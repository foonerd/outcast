//! The dashboard: a page of the core's own, served from its memory on a port
//! of the player, for anyone on the network who wants to watch the uplinks:
//! the programme, the level, dead air, every uplink's state and counters, a
//! second at a time. The page is one file built into the binary (no asset of
//! anyone's fetched), its script polls `/status.json`, which is the status
//! the core keeps for the plugin, rendered on request. The Volumio page
//! carries a button that opens it. Beside it, DJ mode's programme page, and
//! the inserts' files: listed, uploaded (converted to the WAV the mixer plays
//! as they land) and deleted, from the page, so a jingle or a recording on a
//! phone reaches the player with nothing but a browser.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::status::{self, Status};

/// The page, whole.
const PAGE: &str = include_str!("../assets/dashboard.html");
/// DJ mode's editor, served beside it; it talks to the plugin over the player's
/// own socket, as the Volumio page does.
const DJ_PAGE: &str = include_str!("../assets/dj.html");
/// About: what this is, the evo framework it comes from (with a code to scan), what it
/// is built of, and how to support it. The QR code is an SVG built into the page.
const ABOUT_PAGE: &str = include_str!("../assets/about.html");
/// Connections answered at once; the rest wait in the listen queue.
const AT_ONCE: usize = 8;
/// An upload larger than this is refused.
const MAX_UPLOAD: u64 = 256 * 1024 * 1024;

/// The server, alive while the hub holds it.
pub struct Server {
    pub port: u16,
    listener: TcpListener,
    acceptor: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(
        port: u16,
        status: Arc<Mutex<Status>>,
        jingles: PathBuf,
    ) -> Result<Server, String> {
        let listener =
            TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("port {port}: {e}"))?;
        let acceptor = listener.try_clone().map_err(|e| e.to_string())?;
        let open = Arc::new(AtomicUsize::new(0));
        let jingles = Arc::new(jingles);
        let thread = std::thread::Builder::new()
            .name("dashboard".into())
            .spawn(move || {
                for stream in acceptor.incoming() {
                    let Ok(mut stream) = stream else { break };
                    if open.load(Ordering::Relaxed) >= AT_ONCE {
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.write_all(
                            b"HTTP/1.0 503 Service Unavailable\r\nRetry-After: 1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                        );
                        continue;
                    }
                    open.fetch_add(1, Ordering::Relaxed);
                    let status = Arc::clone(&status);
                    let open = Arc::clone(&open);
                    let jingles = Arc::clone(&jingles);
                    let _ = std::thread::Builder::new()
                        .name("dashboard-serve".into())
                        .spawn(move || {
                            serve(stream, &status, &jingles);
                            open.fetch_sub(1, Ordering::Relaxed);
                        });
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Server {
            port,
            listener,
            acceptor: Some(thread),
        })
    }

    /// The listening socket shut, so the acceptor's wait ends and the port is free.
    pub fn stop(mut self) {
        {
            use std::os::unix::io::AsRawFd;
            unsafe { libc::shutdown(self.listener.as_raw_fd(), libc::SHUT_RDWR) };
        }
        if let Some(t) = self.acceptor.take() {
            let _ = t.join();
        }
    }
}

/// The answer to one request of the pages: the status code, the content type,
/// the caching and the body.
pub fn answer(
    method: &str,
    path: &str,
    status_json: &str,
) -> (u16, &'static str, &'static str, String) {
    let path = path.split('?').next().unwrap_or("/");
    if method != "GET" && method != "HEAD" {
        return (405, "text/plain", "no-store", String::new());
    }
    match path {
        "/" | "/index.html" => (
            200,
            "text/html; charset=utf-8",
            "no-cache",
            PAGE.to_string(),
        ),
        "/status.json" => (200, "application/json", "no-store", status_json.to_string()),
        "/dj" | "/dj/" | "/dj.html" => (
            200,
            "text/html; charset=utf-8",
            "no-cache",
            DJ_PAGE.to_string(),
        ),
        "/about" | "/about/" | "/about.html" => (
            200,
            "text/html; charset=utf-8",
            "no-cache",
            ABOUT_PAGE.to_string(),
        ),
        _ => (404, "text/plain", "no-store", String::new()),
    }
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "",
    }
}

fn respond(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    cache: &str,
    body: &[u8],
    head_only: bool,
) {
    let head = format!(
        "HTTP/1.0 {code} {}\r\nServer: outcast\r\nContent-Type: {content_type}\r\nCache-Control: {cache}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        reason(code),
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if !head_only {
        let _ = stream.write_all(body);
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// A file's name as the page gave it, made the WAV name it is kept under:
/// percent-decoded, the extension dropped, letters, digits, spaces, dots,
/// dashes and underscores kept, nothing that walks a path.
pub fn clean_name(raw: &str) -> Option<String> {
    let decoded = percent_decode(raw);
    let stem = match decoded.rfind('.') {
        Some(i) if i > 0 => &decoded[..i],
        _ => &decoded[..],
    };
    let kept: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'))
        .collect();
    let kept = kept.trim().trim_start_matches('.').trim().to_string();
    if kept.is_empty() || kept.len() > 80 {
        return None;
    }
    Some(format!("{kept}.wav"))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// The files in the folder, as JSON for the page: name, bytes, seconds.
fn list(jingles: &Path) -> String {
    let mut items: Vec<(String, u64, f64)> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(jingles) {
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.to_lowercase().ends_with(".wav") || name.starts_with('.') {
                continue;
            }
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            items.push((name, bytes, wav_seconds(&entry.path()).unwrap_or(0.0)));
        }
    }
    items.sort_by_key(|a| a.0.to_lowercase());
    let parts: Vec<String> = items
        .iter()
        .map(|(n, b, s)| {
            format!(
                "{{\"name\":{},\"bytes\":{b},\"seconds\":{s:.1}}}",
                serde_json::to_string(n).unwrap_or_else(|_| "\"\"".into())
            )
        })
        .collect();
    format!("[{}]\n", parts.join(","))
}

/// A WAV's length from its header alone.
fn wav_seconds(path: &Path) -> Option<f64> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = vec![0u8; 4096];
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    if n < 12 || &head[..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut byte_rate = 0u32;
    while at + 8 <= head.len() {
        let id = &head[at..at + 4];
        let len = u32::from_le_bytes([head[at + 4], head[at + 5], head[at + 6], head[at + 7]]);
        if id == b"fmt " && at + 16 <= head.len() {
            byte_rate =
                u32::from_le_bytes([head[at + 16], head[at + 17], head[at + 18], head[at + 19]]);
        } else if id == b"data" {
            return (byte_rate > 0).then(|| f64::from(len) / f64::from(byte_rate));
        }
        at += 8 + len as usize + (len as usize & 1);
    }
    None
}

fn serve(mut stream: TcpStream, status: &Arc<Mutex<Status>>, jingles: &Path) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head, body_start) = loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break (String::from_utf8_lossy(&buf[..i]).to_string(), i + 4);
        }
        if let Some(i) = buf.windows(2).position(|w| w == b"\n\n") {
            break (String::from_utf8_lossy(&buf[..i]).to_string(), i + 2);
        }
        if buf.len() > 16384 {
            return;
        }
    };
    let mut lines = head.lines();
    let mut parts = lines.next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let content_length: Option<u64> = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok());
    let bare = path.split('?').next().unwrap_or("/").to_string();

    // The page's one write of the counts: started again from now.
    if method == "POST" && bare == "/since/clear" {
        status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear_since();
        respond(&mut stream, 204, "text/plain", "no-store", b"", true);
        return;
    }
    // The inserts' files.
    if bare == "/jingles" || bare == "/jingles/" {
        if method == "GET" || method == "HEAD" {
            respond(
                &mut stream,
                200,
                "application/json",
                "no-store",
                list(jingles).as_bytes(),
                method == "HEAD",
            );
        } else {
            respond(&mut stream, 405, "text/plain", "no-store", b"", false);
        }
        return;
    }
    if let Some(raw) = bare.strip_prefix("/jingles/") {
        let Some(name) = clean_name(raw) else {
            respond(
                &mut stream,
                400,
                "text/plain",
                "no-store",
                b"a name of letters, digits, spaces, dots, dashes and underscores",
                false,
            );
            return;
        };
        match method.as_str() {
            "DELETE" => {
                let code = match std::fs::remove_file(jingles.join(&name)) {
                    Ok(()) => {
                        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
                        s.event("info", format!("insert file deleted: {name}"));
                        204
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => 404,
                    Err(_) => 500,
                };
                respond(&mut stream, code, "text/plain", "no-store", b"", true);
            }
            "POST" | "PUT" => {
                let Some(len) = content_length else {
                    respond(
                        &mut stream,
                        411,
                        "text/plain",
                        "no-store",
                        b"the upload needs its length",
                        false,
                    );
                    return;
                };
                if len > MAX_UPLOAD {
                    respond(
                        &mut stream,
                        413,
                        "text/plain",
                        "no-store",
                        b"an upload of 256 MB at most",
                        false,
                    );
                    return;
                }
                match upload(
                    &mut stream,
                    &buf[body_start.min(buf.len())..],
                    len,
                    jingles,
                    &name,
                ) {
                    Ok(info) => {
                        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
                        s.event(
                            "info",
                            format!("insert file uploaded: {name}, {:.0} s", info.seconds()),
                        );
                        drop(s);
                        let body = format!(
                            "{{\"name\":{},\"rate\":{},\"channels\":{},\"seconds\":{:.1}}}\n",
                            serde_json::to_string(&name).unwrap_or_default(),
                            info.rate,
                            info.channels,
                            info.seconds()
                        );
                        respond(
                            &mut stream,
                            200,
                            "application/json",
                            "no-store",
                            body.as_bytes(),
                            false,
                        );
                    }
                    Err(e) => {
                        eprintln!("outcast: upload {name}: {e}");
                        respond(
                            &mut stream,
                            400,
                            "text/plain; charset=utf-8",
                            "no-store",
                            e.as_bytes(),
                            false,
                        );
                    }
                }
            }
            _ => respond(&mut stream, 405, "text/plain", "no-store", b"", false),
        }
        return;
    }

    let json = if bare == "/status.json" {
        status::render(status)
    } else {
        String::new()
    };
    let (code, content_type, cache, body) = answer(&method, &path, &json);
    respond(
        &mut stream,
        code,
        content_type,
        cache,
        body.as_bytes(),
        method == "HEAD",
    );
}

/// The body onto a part file in the folder, then converted to the WAV kept
/// under `name`; the part removed either way.
fn upload(
    stream: &mut TcpStream,
    first: &[u8],
    len: u64,
    jingles: &Path,
    name: &str,
) -> Result<crate::convert::Info, String> {
    std::fs::create_dir_all(jingles).map_err(|e| format!("the folder: {e}"))?;
    let part = jingles.join(format!(".{name}.upload"));
    let result = (|| -> Result<crate::convert::Info, String> {
        let mut file =
            std::fs::File::create(&part).map_err(|e| format!("the folder is not writable: {e}"))?;
        let mut received: u64 = 0;
        let take = first.len().min(len as usize);
        file.write_all(&first[..take]).map_err(|e| e.to_string())?;
        received += take as u64;
        let mut chunk = vec![0u8; 65536];
        while received < len {
            let n = stream
                .read(&mut chunk)
                .map_err(|e| format!("the upload broke off: {e}"))?;
            if n == 0 {
                return Err("the upload broke off".into());
            }
            let n = n.min((len - received) as usize);
            file.write_all(&chunk[..n]).map_err(|e| e.to_string())?;
            received += n as u64;
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        let out = jingles.join(name);
        let tmp = jingles.join(format!(".{name}.part"));
        let info = crate::convert::to_wav(&part, &tmp)?;
        std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
        Ok(info)
    })();
    let _ = std::fs::remove_file(&part);
    if result.is_err() {
        let _ = std::fs::remove_file(jingles.join(format!(".{name}.part")));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_routes_answer_the_page_the_status_and_nothing_else() {
        let (code, kind, cache, body) = answer("GET", "/", "{}");
        assert_eq!(
            (code, kind, cache),
            (200, "text/html; charset=utf-8", "no-cache")
        );
        assert!(body.contains("<title>Outcast Uplink</title>"));
        assert!(body.contains("/status.json"), "the page polls the status");
        assert!(
            !body.contains("<script src") && !body.contains("<link ") && !body.contains("@import"),
            "nothing fetched from anywhere else"
        );
        assert!(
            body.contains(r#"id="close""#) && body.contains("window.parent === window"),
            "a way out when the page is the window itself"
        );
        assert!(
            body.contains(r#"id="player""#) && body.contains("s.system"),
            "the machine's card, from the status's system reading"
        );
        assert!(
            body.contains(r#"id="since""#)
                && body.contains(r#"id="played""#)
                && body.contains(r#"id="bridge""#)
                && body.contains(r#"id="history""#)
                && body.contains("/since/clear"),
            "the counts, the titles, the meter bridge and its history, and the one write"
        );
        let (code, kind, cache, body) = answer("GET", "/status.json?t=1", r#"{"version":"x"}"#);
        assert_eq!(
            (code, kind, cache, body.as_str()),
            (200, "application/json", "no-store", r#"{"version":"x"}"#)
        );
        let (code, _, _, body) = answer("GET", "/dj", "{}");
        assert_eq!(code, 200);
        assert!(
            body.contains("<title>Outcast Uplink · DJ mode</title>")
                && body.contains("'/socket.io/socket.io.js'")
                && body.contains("djSave")
                && body.contains("/jingles/")
                && !body.contains("<script src"),
            "the editor talks to the plugin over the player's own socket, uploads to the core, and fetches nothing from outside"
        );
        let (code, _, _, body) = answer("GET", "/about", "{}");
        assert_eq!(code, 200);
        assert!(
            body.contains("<title>Outcast Uplink · About</title>")
                && body.contains("evoframework.org")
                && body.contains("<svg class=\"qr\"")
                && !body.contains("<script src")
                && !body.contains("<img "),
            "the About page names the framework with its code built in, and fetches nothing"
        );
        assert_eq!(answer("GET", "/secret", "{}").0, 404);
        assert_eq!(answer("POST", "/", "{}").0, 405);
        assert_eq!(answer("HEAD", "/", "{}").0, 200);
    }

    #[test]
    fn names_are_cleaned_to_wav_names_that_walk_no_path() {
        assert_eq!(
            clean_name("Top%20Of%20Hour.m4a").as_deref(),
            Some("Top Of Hour.wav")
        );
        assert_eq!(clean_name("news bed.WAV").as_deref(), Some("news bed.wav"));
        assert_eq!(
            clean_name("..%2F..%2Fetc%2Fpasswd").as_deref(),
            None,
            "a path walk is no name"
        );
        assert_eq!(
            clean_name("my.show.intro.m4a").as_deref(),
            Some("my.show.intro.wav")
        );
        assert_eq!(
            clean_name("interview%202026-11-08").as_deref(),
            Some("interview 2026-11-08.wav")
        );
        assert_eq!(clean_name("...").as_deref(), None);
        assert_eq!(clean_name("").as_deref(), None);
        assert_eq!(clean_name("a/b.wav").as_deref(), Some("ab.wav"));
    }

    fn wav16(frames: usize) -> Vec<u8> {
        let mut data = Vec::new();
        for i in 0..frames * 2 {
            data.extend_from_slice(&((i % 100) as i16 * 100).to_le_bytes());
        }
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&2u16.to_le_bytes());
        w.extend_from_slice(&48000u32.to_le_bytes());
        w.extend_from_slice(&(48000u32 * 4).to_le_bytes());
        w.extend_from_slice(&4u16.to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(data.len() as u32).to_le_bytes());
        w.extend_from_slice(&data);
        w
    }

    fn talk(port: u16, request: &[u8]) -> String {
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(request).unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn the_server_serves_on_its_port_takes_files_and_frees_it() {
        let dir = std::env::temp_dir().join(format!("outcast-jingles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let status = Arc::new(Mutex::new(Status::new("test")));
        let s = Server::start(0, Arc::clone(&status), dir.clone()).unwrap();
        let port = s.listener.local_addr().unwrap().port();
        let out = talk(port, b"GET /status.json HTTP/1.0\r\n\r\n");
        assert!(out.starts_with("HTTP/1.0 200 OK"));
        assert!(out.contains("\"tag\"") && out.contains("\"test\""), "{out}");
        // The one write: the counts cleared, the mark moved.
        status.lock().unwrap().since.reconnects = 7;
        let mark = status.lock().unwrap().since.at;
        std::thread::sleep(Duration::from_millis(1100));
        let out = talk(
            port,
            b"POST /since/clear HTTP/1.0\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(out.starts_with("HTTP/1.0 204"), "{out}");
        assert_eq!(status.lock().unwrap().since.reconnects, 0);
        assert!(status.lock().unwrap().since.at > mark);
        // An upload: a WAV under a phone's name, converted and listed; then deleted.
        let wav = wav16(4800);
        let mut req = format!(
            "POST /jingles/Top%20Of%20Hour.m4a HTTP/1.0\r\nContent-Length: {}\r\n\r\n",
            wav.len()
        )
        .into_bytes();
        req.extend_from_slice(&wav);
        let out = talk(port, &req);
        assert!(out.starts_with("HTTP/1.0 200"), "{out}");
        assert!(
            out.contains("\"name\":\"Top Of Hour.wav\"") && out.contains("\"seconds\":0.1"),
            "{out}"
        );
        assert!(dir.join("Top Of Hour.wav").exists());
        let out = talk(port, b"GET /jingles HTTP/1.0\r\n\r\n");
        assert!(
            out.contains("\"name\":\"Top Of Hour.wav\"") && out.contains("\"seconds\":0.1"),
            "{out}"
        );
        assert!(status
            .lock()
            .unwrap()
            .since
            .events
            .iter()
            .any(|e| e.text.starts_with("insert file uploaded: Top Of Hour.wav")));
        let out = talk(
            port,
            b"POST /jingles/bad HTTP/1.0\r\nContent-Length: 5\r\n\r\nhello",
        );
        assert!(out.starts_with("HTTP/1.0 400"), "{out}");
        assert!(!dir.join("bad.wav").exists() && !dir.join(".bad.wav.upload").exists());
        let out = talk(port, b"POST /jingles/x.wav HTTP/1.0\r\n\r\n");
        assert!(out.starts_with("HTTP/1.0 411"), "{out}");
        let out = talk(
            port,
            b"DELETE /jingles/Top%20Of%20Hour.wav HTTP/1.0\r\n\r\n",
        );
        assert!(out.starts_with("HTTP/1.0 204"), "{out}");
        assert!(!dir.join("Top Of Hour.wav").exists());
        let out = talk(
            port,
            b"DELETE /jingles/Top%20Of%20Hour.wav HTTP/1.0\r\n\r\n",
        );
        assert!(out.starts_with("HTTP/1.0 404"), "{out}");
        s.stop();
        assert!(
            TcpListener::bind(("0.0.0.0", port)).is_ok(),
            "the port is free again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
