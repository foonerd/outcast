//! A small HTTP/1.1 client for an origin that takes files: PUT and DELETE,
//! over the bare socket or TLS, basic authentication where the operator set a
//! user or an S3 signature where the origin is a bucket, the connection kept
//! for the next request and made again when it is gone. Enough for an HLS
//! origin (a WebDAV folder, a CDN's ingest, a media server's HTTP origin, an
//! S3-compatible bucket); nothing is fetched.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::uplink::s3::{self, Signer};
use crate::uplink::tls::{wrap_tls, Wire};

/// How long a connection, a request or an answer may take.
const PATIENCE: Duration = Duration::from_secs(15);

/// Where the files go: scheme, host, port, the path they go under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// The path the file names are put under, with its trailing slash.
    pub path: String,
}

impl Target {
    /// `http://host[:port]/path/` or `https://…` taken apart; the path is made to
    /// end with a slash, so the file names append.
    pub fn parse(url: &str) -> Result<Target, String> {
        let url = url.trim();
        let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = url.strip_prefix("http://") {
            (false, r)
        } else {
            return Err("the origin must start with http:// or https://".to_string());
        };
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.contains(']') || h.ends_with(']') => (
                h.trim_matches(|c| c == '[' || c == ']').to_string(),
                p.parse::<u16>()
                    .map_err(|_| format!("the port in {authority} is not a number"))?,
            ),
            _ => (
                authority.trim_matches(|c| c == '[' || c == ']').to_string(),
                if tls { 443 } else { 80 },
            ),
        };
        if host.is_empty() {
            return Err("the origin has no host".to_string());
        }
        let mut path = format!("/{}", path.trim_matches('/'));
        if !path.ends_with('/') {
            path.push('/');
        }
        Ok(Target {
            tls,
            host,
            port,
            path,
        })
    }

    fn host_header(&self) -> String {
        let default = if self.tls { 443 } else { 80 };
        if self.port == default {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// An answer: the status code and whether the server wants the connection closed.
struct Answer {
    code: u16,
    close: bool,
}

/// The client: one connection, used again while the server keeps it.
pub struct Client {
    target: Target,
    auth: Option<String>,
    signer: Option<Signer>,
    ca_file: String,
    wire: Option<Wire>,
}

impl Client {
    pub fn new(target: Target, user: &str, password: &str, ca_file: &str) -> Client {
        let auth = (!user.is_empty()).then(|| {
            format!(
                "Basic {}",
                crate::uplink::icecast::base64(format!("{user}:{password}").as_bytes())
            )
        });
        Client {
            target,
            auth,
            signer: None,
            ca_file: ca_file.to_string(),
            wire: None,
        }
    }

    /// A client for an S3-compatible bucket: every request signed.
    pub fn signed(target: Target, signer: Signer, ca_file: &str) -> Client {
        Client {
            target,
            auth: None,
            signer: Some(signer),
            ca_file: ca_file.to_string(),
            wire: None,
        }
    }

    fn connect(&mut self) -> Result<(), String> {
        let addr = (self.target.host.as_str(), self.target.port)
            .to_socket_addrs()
            .map_err(|e| format!("{}: {e}", self.target.host))?
            .next()
            .ok_or_else(|| format!("{}: no address", self.target.host))?;
        let sock = TcpStream::connect_timeout(&addr, PATIENCE).map_err(|e| e.to_string())?;
        let _ = sock.set_nodelay(true);
        sock.set_read_timeout(Some(PATIENCE))
            .map_err(|e| e.to_string())?;
        sock.set_write_timeout(Some(PATIENCE))
            .map_err(|e| e.to_string())?;
        self.wire = Some(if self.target.tls {
            wrap_tls(sock, &self.target.host, &self.ca_file)?
        } else {
            Wire::Plain(sock)
        });
        Ok(())
    }

    /// A file put under the target's path, with the caching a CDN should give
    /// it; the status code the server gave.
    pub fn put(
        &mut self,
        name: &str,
        content_type: &str,
        cache_control: &str,
        body: &[u8],
    ) -> Result<u16, String> {
        self.request("PUT", name, Some((content_type, cache_control, body)))
    }

    /// A file removed from under the target's path; the status code.
    pub fn delete(&mut self, name: &str) -> Result<u16, String> {
        self.request("DELETE", name, None)
    }

    /// The request sent and the answer read; a connection the server dropped since
    /// the last request is made again once.
    fn request(
        &mut self,
        method: &str,
        name: &str,
        body: Option<(&str, &str, &[u8])>,
    ) -> Result<u16, String> {
        let head = self.head(method, name, body, s3::now_seconds());
        let had = self.wire.is_some();
        for attempt in 0..2 {
            if self.wire.is_none() {
                self.connect()?;
            }
            let wire = self.wire.as_mut().expect("a wire");
            let sent = wire
                .write_all(head.as_bytes())
                .and_then(|()| match body {
                    Some((_, _, b)) => wire.write_all(b),
                    None => Ok(()),
                })
                .and_then(|()| wire.flush());
            match sent.and_then(|()| read_answer(wire)) {
                Ok(a) => {
                    if a.close {
                        self.wire = None;
                    }
                    return Ok(a.code);
                }
                Err(e) => {
                    self.wire = None;
                    // A kept connection the server closed meanwhile fails at once; once
                    // more, fresh. A server that does not answer is not tried twice.
                    let timed_out = matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    );
                    if had && attempt == 0 && !timed_out {
                        continue;
                    }
                    return Err(if timed_out {
                        format!("no answer within {} s", PATIENCE.as_secs())
                    } else {
                        e.to_string()
                    });
                }
            }
        }
        unreachable!()
    }

    fn head(
        &self,
        method: &str,
        name: &str,
        body: Option<(&str, &str, &[u8])>,
        now: u64,
    ) -> String {
        let path = format!("{}{name}", self.target.path);
        let host = self.target.host_header();
        let mut h = vec![
            format!("{method} {path} HTTP/1.1"),
            format!("Host: {host}"),
            format!("User-Agent: outcast/{}", env!("CARGO_PKG_VERSION")),
        ];
        if let Some(a) = &self.auth {
            h.push(format!("Authorization: {a}"));
        }
        match body {
            Some((t, cache, b)) => {
                h.push(format!("Content-Type: {t}"));
                h.push(format!("Content-Length: {}", b.len()));
                if !cache.is_empty() {
                    h.push(format!("Cache-Control: {cache}"));
                }
            }
            None => h.push("Content-Length: 0".to_string()),
        }
        if let Some(signer) = &self.signer {
            let payload = body.map(|(_, _, b)| b).unwrap_or(&[]);
            for (k, v) in signer.headers(method, &path, &host, payload, now) {
                h.push(format!("{k}: {v}"));
            }
        }
        h.join("\r\n") + "\r\n\r\n"
    }
}

/// The answer's head read and its body drained, so the connection can serve again.
fn read_answer<R: Read>(wire: &mut R) -> io::Result<Answer> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = wire.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the origin closed before answering",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 65536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the answer's head is too long",
            ));
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let status = lines.next().unwrap_or("");
    let code: u16 = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("not an answer: {status}"),
            )
        })?;
    let mut length: Option<usize> = Some(0);
    let mut chunked = false;
    let mut close = status.starts_with("HTTP/1.0");
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let v = v.trim();
        if k.eq_ignore_ascii_case("content-length") {
            length = v.parse().ok();
        } else if k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked") {
            chunked = true;
        } else if k.eq_ignore_ascii_case("connection") && v.eq_ignore_ascii_case("close") {
            close = true;
        }
    }
    let mut rest = buf[head_end..].to_vec();
    if chunked {
        // Each chunk's size line, its bytes and the blank; the last chunk is 0.
        loop {
            let line_end = loop {
                if let Some(i) = find(&rest, b"\r\n") {
                    break i;
                }
                let n = wire.read(&mut chunk)?;
                if n == 0 {
                    return Ok(Answer { code, close: true });
                }
                rest.extend_from_slice(&chunk[..n]);
            };
            let size_text = String::from_utf8_lossy(&rest[..line_end]).into_owned();
            let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("0").trim(), 16)
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "a chunk size that is not one")
                })?;
            rest.drain(..line_end + 2);
            let want = size + 2;
            while rest.len() < want {
                let n = wire.read(&mut chunk)?;
                if n == 0 {
                    return Ok(Answer { code, close: true });
                }
                rest.extend_from_slice(&chunk[..n]);
            }
            rest.drain(..want);
            if size == 0 {
                break;
            }
        }
    } else if let Some(len) = length {
        while rest.len() < len {
            let n = wire.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            rest.extend_from_slice(&chunk[..n]);
        }
    } else {
        close = true;
    }
    Ok(Answer { code, close })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_origin_address_is_taken_apart_with_its_path_slashed() {
        let t = Target::parse("https://origin.example.com/radio/live").unwrap();
        assert_eq!(
            (t.tls, t.host.as_str(), t.port, t.path.as_str()),
            (true, "origin.example.com", 443, "/radio/live/")
        );
        let t = Target::parse("http://10.0.0.9:8081").unwrap();
        assert_eq!((t.tls, t.port, t.path.as_str()), (false, 8081, "/"));
        assert_eq!(t.host_header(), "10.0.0.9:8081");
        assert_eq!(
            Target::parse("https://cdn.example.com/")
                .unwrap()
                .host_header(),
            "cdn.example.com"
        );
        assert!(Target::parse("ftp://x/y").unwrap_err().contains("http://"));
    }

    #[test]
    fn the_request_head_names_the_file_under_the_path_with_the_credentials() {
        let c = Client::new(
            Target::parse("http://origin.example.com/radio/").unwrap(),
            "ingest",
            "secret",
            "",
        );
        let h = c.head(
            "PUT",
            "live.m3u8",
            Some(("application/vnd.apple.mpegurl", "no-cache", b"#EXTM3U\n")),
            0,
        );
        assert!(h.starts_with("PUT /radio/live.m3u8 HTTP/1.1\r\nHost: origin.example.com\r\n"));
        assert!(h.contains("\r\nAuthorization: Basic aW5nZXN0OnNlY3JldA==\r\n"));
        assert!(h.contains("\r\nContent-Length: 8\r\nCache-Control: no-cache\r\n"));
        assert!(h.ends_with("\r\n\r\n"));
        let h = c.head("DELETE", "live-7.aac", None, 0);
        assert!(h.starts_with("DELETE /radio/live-7.aac HTTP/1.1\r\n"));
        assert!(h.contains("Content-Length: 0"));
        // A bucket: the request signed, the host as sent.
        let c = Client::signed(
            Target::parse("http://10.0.0.9:9000/radio/live/").unwrap(),
            Signer {
                access_key: "AKIAIOSFODNN7EXAMPLE".into(),
                secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
                region: "us-east-1".into(),
            },
            "",
        );
        let h = c.head(
            "PUT",
            "live-7.aac",
            Some(("audio/aac", "max-age=3600", b"abc")),
            1_369_353_600,
        );
        assert!(h.contains("\r\nx-amz-date: 20130524T000000Z\r\n"));
        assert!(h.contains("\r\nx-amz-content-sha256: ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\r\n"));
        assert!(h.contains("\r\nAuthorization: AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="));
    }

    #[test]
    fn answers_are_read_whole_with_their_bodies() {
        use std::io::Cursor;
        // A plain body of known length, then a chunked one, then a 1.0 close.
        struct Fake(Cursor<Vec<u8>>);
        impl Read for Fake {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                self.0.read(b)
            }
        }
        let pipe = |text: &[u8]| -> io::Result<Answer> {
            let mut f = Fake(Cursor::new(text.to_vec()));
            read_answer(&mut f)
        };
        let a = pipe(b"HTTP/1.1 201 Created\r\nContent-Length: 5\r\n\r\nhello").unwrap();
        assert_eq!((a.code, a.close), (201, false));
        let a = pipe(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").unwrap();
        assert_eq!((a.code, a.close), (204, true));
        let a =
            pipe(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nabcd\r\n0\r\n\r\n")
                .unwrap();
        assert_eq!((a.code, a.close), (200, false));
        let a = pipe(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert_eq!((a.code, a.close), (404, true));
        assert!(pipe(b"garbage\r\n\r\n").is_err());
    }
}
