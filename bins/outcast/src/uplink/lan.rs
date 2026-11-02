//! The LAN stream: an HTTP server on the player that any listener on the
//! network opens, as MPD's httpd output was. One encoder runs while a
//! stream plays; every listener takes the encoded stream from the live
//! point, with the stream's own headers first where the container has them
//! (an Ogg chain's first pages), and the title in-band the way a listener
//! asked for it (`Icy-MetaData: 1`). A listener that does not keep up is
//! dropped; the player never waits.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::{Codec, Lan};
use crate::encoder::{Encoder, Output, Recipe};
use crate::feed::{Event, Segment};
use crate::uplink::{self, clean_line, Ctx, Next, GRACE};

/// Bytes of audio between two metadata blocks, for a listener that asked.
pub const METAINT: usize = 8192;
/// Encoded pieces a listener may hold unread before it is dropped.
const LISTENER_QUEUE: usize = 128;
/// Encoded pieces held between the encoder and the server.
const ENCODED_QUEUE: usize = 64;

/// What every listener sees: the headers to send first, the title now.
struct Shared {
    header: Vec<u8>,
    title: String,
    listeners: Vec<Listener>,
    max: usize,
    next_id: u64,
}

struct Listener {
    id: u64,
    tx: SyncSender<Arc<Vec<u8>>>,
}

/// What a listener asked for, from its request.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub head: bool,
    pub icy: bool,
}

/// The request line and headers read.
pub fn parse_request(text: &str) -> Option<Request> {
    let mut lines = text.lines();
    let first = lines.next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?;
    let head = match method {
        "GET" => false,
        "HEAD" => true,
        _ => return None,
    };
    let icy = lines.any(|l| {
        let l = l.trim();
        l.len() > 13 && l[..13].eq_ignore_ascii_case("icy-metadata:") && l[13..].trim() == "1"
    });
    Some(Request { head, icy })
}

/// The response headers for a listener.
pub fn response(cfg: &Lan, icy: bool, version: &str) -> String {
    let mut h = vec![
        "HTTP/1.0 200 OK".to_string(),
        format!("Content-Type: {}", cfg.codec.content_type()),
        format!("Server: outcast/{version}"),
        format!("icy-name: {}", clean_line(&cfg.name)),
        "icy-pub: 0".to_string(),
        "Cache-Control: no-cache, no-store".to_string(),
        "Pragma: no-cache".to_string(),
        "Connection: close".to_string(),
    ];
    if cfg.codec.lossy() {
        h.push(format!("icy-br: {}", cfg.bitrate));
    }
    if icy {
        h.push(format!("icy-metaint: {METAINT}"));
    }
    h.join("\r\n") + "\r\n\r\n"
}

/// An ICY metadata block: one length byte (in sixteens) and the text padded
/// with zeros; an empty block says nothing changed.
pub fn icy_block(title: Option<&str>) -> Vec<u8> {
    let Some(title) = title else { return vec![0] };
    let text = format!(
        "StreamTitle='{}';",
        title.replace('\'', " ").replace(['\r', '\n'], " ")
    );
    let bytes = text.as_bytes();
    let len = bytes.len().min(16 * 255);
    let blocks = len.div_ceil(16);
    let mut out = Vec::with_capacity(1 + blocks * 16);
    out.push(blocks as u8);
    out.extend_from_slice(&bytes[..len]);
    out.resize(1 + blocks * 16, 0);
    out
}

/// An Ogg stream cut into whole pages, with the chain's header pages kept.
/// A page is 27 bytes of header, the segment table, and the segments; its
/// granule position is -1 or 0 for the header packets of a chain and above
/// zero once audio begins; the first page of a chain carries the BOS flag.
#[derive(Default)]
pub struct OggPages {
    pending: Vec<u8>,
    header: Vec<u8>,
    in_header: bool,
}

impl OggPages {
    /// Bytes in; whole pages out, and the header pages of the chain noted.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(len) = page_len(&self.pending[at..]) {
            let page = &self.pending[at..at + len];
            let flags = page[5];
            let granule = i64::from_le_bytes(page[6..14].try_into().unwrap_or([0; 8]));
            if flags & 0x02 != 0 {
                // Beginning of a stream: a new chain, a new header.
                self.header.clear();
                self.in_header = true;
            }
            if self.in_header {
                if granule <= 0 {
                    self.header.extend_from_slice(page);
                } else {
                    self.in_header = false;
                }
            }
            out.extend_from_slice(page);
            at += len;
        }
        self.pending.drain(..at);
        out
    }

    pub fn header(&self) -> &[u8] {
        &self.header
    }
}

/// The length of the Ogg page at the start of `b`, when all of it is there.
fn page_len(b: &[u8]) -> Option<usize> {
    if b.len() < 27 || &b[..4] != b"OggS" {
        return None;
    }
    let segments = b[26] as usize;
    if b.len() < 27 + segments {
        return None;
    }
    let body: usize = b[27..27 + segments].iter().map(|s| *s as usize).sum();
    let len = 27 + segments + body;
    (b.len() >= len).then_some(len)
}

pub fn run(cfg: Lan, ctx: Ctx, ctl: Receiver<Event>, pcm: Receiver<Arc<Vec<u8>>>) {
    let shared = Arc::new(Mutex::new(Shared {
        header: Vec::new(),
        title: String::new(),
        listeners: Vec::new(),
        max: cfg.max_listeners as usize,
        next_id: 0,
    }));
    let listener = match TcpListener::bind(("0.0.0.0", cfg.port)) {
        Ok(l) => l,
        Err(e) => {
            ctx.state("error", Some(&format!("port {}: {e}", cfg.port)));
            ctx.log(&format!("cannot listen on port {}: {e}", cfg.port));
            // The worker stays, so the status says why, until it is stopped.
            loop {
                match uplink::next(&ctl, &pcm, Duration::from_millis(200)) {
                    Next::Gone | Next::Event(Event::Stop) => return,
                    _ => {}
                }
            }
        }
    };
    ctx.log(&format!(
        "listening on port {} as {}",
        cfg.port,
        cfg.codec.content_type()
    ));
    ctx.state("listening", Some(""));
    // The acceptor, on a thread of its own, for as long as the socket lives.
    {
        let shared = Arc::clone(&shared);
        let cfg = cfg.clone();
        let ctx = ctx.clone();
        let _ = listener.set_nonblocking(false);
        let acceptor = listener.try_clone().expect("listener clone");
        std::thread::Builder::new()
            .name("lan-accept".into())
            .spawn(move || {
                for stream in acceptor.incoming() {
                    let Ok(stream) = stream else { break };
                    let shared = Arc::clone(&shared);
                    let cfg = cfg.clone();
                    let ctx = ctx.clone();
                    let _ = std::thread::Builder::new()
                        .name("lan-listener".into())
                        .spawn(move || serve(stream, cfg, shared, ctx));
                }
            })
            .expect("lan accept thread");
    }

    let ogg = matches!(cfg.codec, Codec::Opus | Codec::Vorbis | Codec::Flac);
    let mut pages = OggPages::default();
    let mut segment: Option<Segment> = None;
    let mut encoder: Option<Encoder> = None;
    let mut encoded: Option<Receiver<Arc<Vec<u8>>>> = None;
    let mut title = String::new();
    let mut bytes: u64 = 0;

    loop {
        if let (Some(seg), None) = (segment.as_ref(), encoder.as_ref()) {
            let (tx, rx) = mpsc::sync_channel(ENCODED_QUEUE);
            let recipe = Recipe {
                codec: cfg.codec,
                bitrate: cfg.bitrate,
                rate: cfg.rate,
                title: if ogg { title.clone() } else { String::new() },
                output: Output::Pipe,
            };
            match Encoder::start(&ctx.evopeg, seg, &recipe, Some(tx), &ctx.id) {
                Ok(e) => {
                    encoder = Some(e);
                    encoded = Some(rx);
                    ctx.state("streaming", None);
                }
                Err(e) => {
                    ctx.state("error", Some(&format!("encoder: {e}")));
                    ctx.log(&format!("encoder: {e}"));
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
        match uplink::next(&ctl, &pcm, Duration::from_millis(20)) {
            Next::Gone | Next::Event(Event::Stop) => break,
            Next::Nothing => {}
            Next::Event(Event::Segment(seg)) => {
                if let Some(e) = encoder.take() {
                    e.finish(GRACE);
                    if let Some(rx) = encoded.take() {
                        drain(&rx, &mut pages, ogg, &shared, &mut bytes);
                    }
                }
                segment = Some(seg);
            }
            Next::Event(Event::End) => {
                if let Some(e) = encoder.take() {
                    e.finish(GRACE);
                    if let Some(rx) = encoded.take() {
                        drain(&rx, &mut pages, ogg, &shared, &mut bytes);
                    }
                }
                segment = None;
                ctx.state("listening", None);
            }
            Next::Event(Event::Title(t)) => {
                title = t.clone();
                shared.lock().unwrap_or_else(|e| e.into_inner()).title = t.clone();
                ctx.update(|u| u.title = t);
                if ogg && segment.is_some() {
                    // A new chain with the new title.
                    if let Some(e) = encoder.take() {
                        e.finish(GRACE);
                        if let Some(rx) = encoded.take() {
                            drain(&rx, &mut pages, ogg, &shared, &mut bytes);
                        }
                    }
                }
            }
            Next::Pcm(piece) => {
                let Some(e) = encoder.as_mut() else { continue };
                if let Err(err) = e.feed(&piece) {
                    ctx.log(&format!("encoder gone: {err}"));
                    if let Some(enc) = encoder.take() {
                        enc.kill();
                    }
                    encoded = None;
                    continue;
                }
                if let Some(rx) = encoded.as_ref() {
                    while let Ok(out) = rx.try_recv() {
                        publish(&out, &mut pages, ogg, &shared, &mut bytes);
                    }
                }
                let b = bytes;
                let n = shared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .listeners
                    .len() as u32;
                ctx.update(|u| {
                    u.bytes = b;
                    u.listeners = n;
                });
            }
        }
    }
    if let Some(e) = encoder.take() {
        e.kill();
    }
    shared
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .listeners
        .clear();
    drop(listener);
    ctx.state("off", None);
}

/// A piece of the encoded stream to every listener that has room; for Ogg,
/// whole pages only, with the chain's header pages kept for the next listener.
fn publish(
    out: &[u8],
    pages: &mut OggPages,
    ogg: bool,
    shared: &Arc<Mutex<Shared>>,
    bytes: &mut u64,
) {
    let piece: Vec<u8> = if ogg { pages.push(out) } else { out.to_vec() };
    if piece.is_empty() {
        return;
    }
    *bytes += piece.len() as u64;
    let piece = Arc::new(piece);
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    if ogg {
        s.header = pages.header().to_vec();
    }
    s.listeners
        .retain(|l| match l.tx.try_send(Arc::clone(&piece)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        });
}

fn drain(
    rx: &Receiver<Arc<Vec<u8>>>,
    pages: &mut OggPages,
    ogg: bool,
    shared: &Arc<Mutex<Shared>>,
    bytes: &mut u64,
) {
    while let Ok(out) = rx.recv_timeout(Duration::from_millis(200)) {
        publish(&out, pages, ogg, shared, bytes);
    }
}

/// One listener, from its request to its end.
fn serve(mut stream: TcpStream, cfg: Lan, shared: Arc<Mutex<Shared>>, ctx: Ctx) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_nodelay(true);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let request = loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        let text = String::from_utf8_lossy(&buf);
        if let Some(i) = text.find("\r\n\r\n").or_else(|| text.find("\n\n")) {
            break parse_request(&text[..i]);
        }
        if buf.len() > 8192 {
            return;
        }
    };
    let Some(request) = request else {
        let _ = stream.write_all(b"HTTP/1.0 400 Bad Request\r\nConnection: close\r\n\r\n");
        return;
    };
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let (rx, header, id) = {
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        if request.head {
            drop(s);
            let _ =
                stream.write_all(response(&cfg, request.icy, env!("CARGO_PKG_VERSION")).as_bytes());
            return;
        }
        if s.listeners.len() >= s.max {
            drop(s);
            let _ =
                stream.write_all(b"HTTP/1.0 503 Service Unavailable\r\nConnection: close\r\n\r\n");
            ctx.log(&format!(
                "listener {peer} refused: {} already",
                cfg.max_listeners
            ));
            return;
        }
        let (tx, rx) = mpsc::sync_channel(LISTENER_QUEUE);
        s.next_id += 1;
        let id = s.next_id;
        s.listeners.push(Listener { id, tx });
        (rx, s.header.clone(), id)
    };
    ctx.log(&format!(
        "listener {peer} joined{}",
        if request.icy { " (titles in band)" } else { "" }
    ));
    let count = shared
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .listeners
        .len() as u32;
    ctx.update(|u| u.listeners = count);
    let result = stream_to(&mut stream, &request, &cfg, &shared, rx, header);
    let _ = stream.shutdown(Shutdown::Both);
    ctx.log(&format!(
        "listener {peer} left{}",
        result.err().map(|e| format!(": {e}")).unwrap_or_default()
    ));
    let count = {
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        s.listeners.retain(|l| l.id != id);
        s.listeners.len() as u32
    };
    ctx.update(|u| u.listeners = count);
}

fn stream_to(
    stream: &mut TcpStream,
    request: &Request,
    cfg: &Lan,
    shared: &Arc<Mutex<Shared>>,
    rx: Receiver<Arc<Vec<u8>>>,
    header: Vec<u8>,
) -> io::Result<()> {
    stream.write_all(response(cfg, request.icy, env!("CARGO_PKG_VERSION")).as_bytes())?;
    let mut icy_sent = String::new();
    let mut to_next = METAINT;
    let mut write = |stream: &mut TcpStream, bytes: &[u8]| -> io::Result<()> {
        if !request.icy {
            return stream.write_all(bytes);
        }
        let mut at = 0;
        while at < bytes.len() {
            let n = to_next.min(bytes.len() - at);
            stream.write_all(&bytes[at..at + n])?;
            at += n;
            to_next -= n;
            if to_next == 0 {
                let title = shared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .title
                    .clone();
                let block = if title != icy_sent {
                    icy_sent = title.clone();
                    icy_block(Some(&title))
                } else {
                    icy_block(None)
                };
                stream.write_all(&block)?;
                to_next = METAINT;
            }
        }
        Ok(())
    };
    if !header.is_empty() {
        write(stream, &header)?;
    }
    // A listener that leaves while nothing plays would never be written to and never
    // noticed: between pieces its socket is looked at, and one closed ends it.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
    let mut probe = [0u8; 1];
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(piece) => write(stream, &piece)?,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => match stream.peek(&mut probe) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "closed by the listener",
                    ))
                }
                Ok(_) => {}
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e),
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_read_for_the_method_and_the_icy_wish() {
        assert_eq!(
            parse_request("GET / HTTP/1.1\r\nHost: x\r\nIcy-MetaData: 1\r\n"),
            Some(Request {
                head: false,
                icy: true
            })
        );
        assert_eq!(
            parse_request("GET /stream HTTP/1.0\r\nicy-metadata:1\r\n"),
            Some(Request {
                head: false,
                icy: true
            })
        );
        assert_eq!(
            parse_request("HEAD / HTTP/1.1\r\n"),
            Some(Request {
                head: true,
                icy: false
            })
        );
        assert_eq!(parse_request("POST / HTTP/1.1\r\n"), None);
        assert_eq!(parse_request(""), None);
    }

    #[test]
    fn the_response_names_the_stream_and_the_metaint_when_asked() {
        let cfg = Lan {
            name: "Living room".into(),
            bitrate: 192,
            ..Lan::default()
        };
        let r = response(&cfg, true, "0.1.0");
        assert!(r.starts_with("HTTP/1.0 200 OK\r\nContent-Type: audio/mpeg\r\n"));
        assert!(
            r.contains("icy-name: Living room\r\n")
                && r.contains("icy-br: 192\r\n")
                && r.contains("icy-metaint: 8192\r\n")
        );
        assert!(!response(&cfg, false, "0.1.0").contains("icy-metaint"));
    }

    #[test]
    fn icy_blocks_are_padded_to_sixteens() {
        assert_eq!(icy_block(None), vec![0]);
        let b = icy_block(Some("Artist - Title"));
        // "StreamTitle='Artist - Title';" is 29 bytes: two sixteens.
        assert_eq!(b[0], 2);
        assert_eq!(b.len(), 1 + 32);
        assert!(b[1..].starts_with(b"StreamTitle='Artist - Title';"));
        assert_eq!(b[30], 0);
        let q = icy_block(Some("it's"));
        assert!(q[1..].starts_with(b"StreamTitle='it s';"));
    }

    fn page(bos: bool, granule: i64, body: &[u8]) -> Vec<u8> {
        let mut p = b"OggS".to_vec();
        p.push(0);
        p.push(if bos { 0x02 } else { 0 });
        p.extend_from_slice(&granule.to_le_bytes());
        p.extend_from_slice(&[0; 4 + 4 + 4]);
        p.push(1);
        p.push(body.len() as u8);
        p.extend_from_slice(body);
        p
    }

    #[test]
    fn ogg_pages_are_cut_whole_and_the_chain_header_kept() {
        let mut pages = OggPages::default();
        let id = page(true, 0, b"OpusHead");
        let tags = page(false, 0, b"OpusTags");
        let audio1 = page(false, 960, b"audio1");
        let audio2 = page(false, 1920, b"audio2");
        let mut stream = Vec::new();
        for p in [&id, &tags, &audio1, &audio2] {
            stream.extend_from_slice(p);
        }
        // Delivered in pieces that cut pages in two.
        let mut out = Vec::new();
        for chunk in stream.chunks(13) {
            out.extend_from_slice(&pages.push(chunk));
        }
        assert_eq!(out, stream);
        let mut header = id.clone();
        header.extend_from_slice(&tags);
        assert_eq!(pages.header(), &header[..]);
        // A new chain replaces the header.
        let id2 = page(true, 0, b"OpusHead2");
        pages.push(&id2);
        assert_eq!(pages.header(), &id2[..]);
    }
}
