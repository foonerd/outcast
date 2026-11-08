//! The dashboard: a page of the core's own, served from its memory on a port
//! of the player, for anyone on the network who wants to watch the uplinks:
//! the programme, the level, dead air, every uplink's state and counters, a
//! second at a time. The page is one file built into the binary (no asset of
//! anyone's fetched), its script polls `/status.json`, which is the status
//! the core keeps for the plugin, rendered on request. The Volumio page
//! carries a button that opens it.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::status::{self, Status};

/// The page, whole.
const PAGE: &str = include_str!("../assets/dashboard.html");
/// Connections answered at once; the rest wait in the listen queue.
const AT_ONCE: usize = 8;

/// The server, alive while the hub holds it.
pub struct Server {
    pub port: u16,
    listener: TcpListener,
    acceptor: Option<JoinHandle<()>>,
}

impl Server {
    pub fn start(port: u16, status: Arc<Mutex<Status>>) -> Result<Server, String> {
        let listener =
            TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("port {port}: {e}"))?;
        let acceptor = listener.try_clone().map_err(|e| e.to_string())?;
        let open = Arc::new(AtomicUsize::new(0));
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
                    let _ = std::thread::Builder::new()
                        .name("dashboard-serve".into())
                        .spawn(move || {
                            serve(stream, &status);
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

/// The answer to one request: the status code, the content type, the caching
/// and the body.
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
        _ => (404, "text/plain", "no-store", String::new()),
    }
}

fn serve(mut stream: TcpStream, status: &Arc<Mutex<Status>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let head = loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        let text = String::from_utf8_lossy(&buf);
        if let Some(i) = text.find("\r\n\r\n").or_else(|| text.find("\n\n")) {
            break text[..i].to_string();
        }
        if buf.len() > 8192 {
            return;
        }
    };
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let json = if path.starts_with("/status.json") {
        status::render(status)
    } else {
        String::new()
    };
    let (code, content_type, cache, body) = answer(&method, &path, &json);
    let reason = match code {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "",
    };
    let head = format!(
        "HTTP/1.0 {code} {reason}\r\nServer: outcast\r\nContent-Type: {content_type}\r\nCache-Control: {cache}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if method == "GET" {
        let _ = stream.write_all(body.as_bytes());
    }
    let _ = stream.shutdown(Shutdown::Both);
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
            body.contains(r#"id="close""#) && body.contains("window.parent === window"),
            "a way out when the page is the window itself"
        );
        assert!(
            body.contains(r#"id="player""#) && body.contains("s.system"),
            "the machine's card, from the status's system reading"
        );
        assert!(
            !body.contains("<script src") && !body.contains("<link ") && !body.contains("@import"),
            "nothing fetched from anywhere else"
        );
        let (code, kind, cache, body) = answer("GET", "/status.json?t=1", r#"{"version":"x"}"#);
        assert_eq!(
            (code, kind, cache, body.as_str()),
            (200, "application/json", "no-store", r#"{"version":"x"}"#)
        );
        assert_eq!(answer("GET", "/secret", "{}").0, 404);
        assert_eq!(answer("POST", "/", "{}").0, 405);
        assert_eq!(answer("HEAD", "/", "{}").0, 200);
    }

    #[test]
    fn the_server_serves_on_its_port_and_frees_it() {
        let status = Arc::new(Mutex::new(Status::new("test")));
        let s = Server::start(0, Arc::clone(&status)).unwrap();
        let port = s.listener.local_addr().unwrap().port();
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(b"GET /status.json HTTP/1.0\r\n\r\n").unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.0 200 OK"));
        assert!(out.contains("\"tag\"") && out.contains("\"test\""), "{out}");
        s.stop();
        assert!(
            TcpListener::bind(("0.0.0.0", port)).is_ok(),
            "the port is free again"
        );
    }
}
