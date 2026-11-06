//! The Icecast uplink: the source protocol to one mount, as mpd-output's
//! relay did it, in the core's own socket. The connection is made when a
//! stream begins and kept while it plays; a server that stops taking bytes
//! (a frozen host, a black hole, a half-open connection) is found by a write
//! that makes no progress for `STALL` and dropped, and every drop is retried
//! with a backoff that doubles to half a minute, or waits a minute after a
//! refused password. While the server is away the stream is thrown away,
//! never queued: the player plays on, and the listeners miss what they miss.
//! The title goes to the server's admin interface for MP3 and AAC; an Ogg
//! stream carries it itself, so a new title there is a new chain from the
//! encoder on the same connection.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{Codec, Icecast, Protocol};
use crate::encoder::{Container, Encoder, Output, Recipe};
use crate::uplink::{self, clean_line, fix_send_buffer, unsent, Ctx, Next, Silence, GRACE, STILL};
use stream::conform::Conformer;
use stream::feed::{Event, Segment};

/// A write that makes no progress for this long means the peer is gone.
const STALL: Duration = Duration::from_secs(8);
/// How long the connect and the server's answer may take.
const CONNECT: Duration = Duration::from_secs(15);
/// The longest wait between retries.
const BACKOFF_MAX: u64 = 30;
/// The wait after the server refused the password.
const AUTH_BACKOFF: u64 = 60;
/// After a stream ends, how long the connection is kept for the next one:
/// a format change between two tracks is one connection, not two. Within
/// Icecast's own source timeout of ten seconds.
const LINGER: Duration = Duration::from_secs(5);
/// Encoded pieces held between the encoder and the socket.
const ENCODED_QUEUE: usize = 64;

/// The outcome of asking the server for the mount.
enum Refusal {
    /// 401: the password.
    Auth(String),
    /// Anything else: the mount in use, the content type, a bad gateway.
    Other(String),
}

/// `SOURCE` request headers for a mount.
pub fn source_request(cfg: &Icecast, content_type: &str, version: &str) -> String {
    let mut h = vec![
        format!("SOURCE {} HTTP/1.0", cfg.mount),
        format!("Host: {}:{}", cfg.host, cfg.port),
        format!(
            "Authorization: Basic {}",
            basic_auth(&cfg.user, &cfg.password)
        ),
        format!("User-Agent: outcast/{version}"),
        format!("Content-Type: {content_type}"),
        format!("ice-public: {}", u8::from(cfg.public)),
        format!("ice-name: {}", clean_line(&cfg.name)),
    ];
    if !cfg.description.is_empty() {
        h.push(format!("ice-description: {}", clean_line(&cfg.description)));
    }
    if !cfg.genre.is_empty() {
        h.push(format!("ice-genre: {}", clean_line(&cfg.genre)));
    }
    if !cfg.url.is_empty() {
        h.push(format!("ice-url: {}", clean_line(&cfg.url)));
    }
    // What the server tells directories and listeners of the stream: the bitrate
    // for a lossy codec, the rate where it is fixed, the channels.
    let mut info: Vec<String> = Vec::new();
    if cfg.codec.lossy() {
        info.push(format!("bitrate={}", cfg.bitrate));
    }
    if cfg.rate != 0 {
        info.push(format!("samplerate={}", cfg.rate));
    }
    info.push("channels=2".to_string());
    h.push(format!("ice-audio-info: {}", info.join(";")));
    h.join("\r\n") + "\r\n\r\n"
}

/// The `GET` that sets the mount's title.
pub fn metadata_request(cfg: &Icecast, title: &str, version: &str) -> String {
    format!(
        "GET /admin/metadata?mode=updinfo&charset=UTF-8&mount={}&song={} HTTP/1.0\r\nHost: {}:{}\r\nAuthorization: Basic {}\r\nUser-Agent: outcast/{version}\r\n\r\n",
        percent(&cfg.mount),
        percent(title),
        cfg.host,
        cfg.port,
        basic_auth(&cfg.user, &cfg.password)
    )
}

pub fn basic_auth(user: &str, password: &str) -> String {
    base64(format!("{user}:{password}").as_bytes())
}

/// SHOUTcast's name for a stream's content: `audio/aacp` for AAC, as a DNAS
/// says it; MP3 as everywhere.
pub fn shoutcast_content_type(codec: Codec) -> &'static str {
    match codec {
        Codec::Aac => "audio/aacp",
        _ => "audio/mpeg",
    }
}

/// A SHOUTcast v1 source's first line: the password, with the stream id
/// where a DNAS 2 has several streams.
pub fn shoutcast_password_line(cfg: &Icecast) -> String {
    if cfg.stream_id > 1 {
        format!("{}:#{}\r\n", clean_line(&cfg.password), cfg.stream_id)
    } else {
        format!("{}\r\n", clean_line(&cfg.password))
    }
}

/// A SHOUTcast v1 source's headers, sent after the server's `OK2`: the
/// `icy-*` lines a DNAS 1 expects, every one of them, then an empty line.
pub fn shoutcast_headers(cfg: &Icecast, content_type: &str) -> String {
    let h = [
        format!("icy-name:{}", clean_line(&cfg.name)),
        format!("icy-genre:{}", clean_line(&cfg.genre)),
        format!("icy-url:{}", clean_line(&cfg.url)),
        format!("icy-pub:{}", u8::from(cfg.public)),
        format!("icy-br:{}", cfg.bitrate),
        "icy-irc:".to_string(),
        "icy-icq:".to_string(),
        "icy-aim:".to_string(),
        format!("content-type:{content_type}"),
    ];
    h.join("\r\n") + "\r\n\r\n"
}

/// The `GET` that sets a SHOUTcast stream's title, on the server's own
/// port; a DNAS takes it from a client that calls itself Mozilla and from
/// no other.
pub fn shoutcast_metadata_request(cfg: &Icecast, title: &str) -> String {
    let sid = if cfg.stream_id > 1 {
        format!("&sid={}", cfg.stream_id)
    } else {
        String::new()
    };
    format!(
        "GET /admin.cgi?pass={}&mode=updinfo&song={}{sid} HTTP/1.0\r\nHost: {}:{}\r\nUser-Agent: (Mozilla Compatible)\r\n\r\n",
        percent(&cfg.password),
        percent(title),
        cfg.host,
        cfg.port
    )
}

/// Standard base64, for the one header that wants it.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.len();
        let b = [
            chunk[0],
            if n > 1 { chunk[1] } else { 0 },
            if n > 2 { chunk[2] } else { 0 },
        ];
        let v = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(T[(v >> 18) as usize & 63] as char);
        out.push(T[(v >> 12) as usize & 63] as char);
        out.push(if n > 1 {
            T[(v >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if n > 2 {
            T[v as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Percent-encoding of a query value: the unreserved characters as they are.
pub fn percent(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The server's answer to a request: the status code and, where it refused,
/// the reason it put in the body with the markup taken out.
pub fn parse_response(text: &str) -> (u16, String) {
    let (head, body) = match text.find("\r\n\r\n") {
        Some(i) => (&text[..i], &text[i + 4..]),
        None => (text, ""),
    };
    let status_line = head.lines().next().unwrap_or("");
    let code = status_line
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let mut reason = String::new();
    let mut in_tag = false;
    for c in body.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                reason.push(' ');
            }
            _ if !in_tag => reason.push(c),
            _ => {}
        }
    }
    let reason: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    let reason = reason.chars().take(120).collect::<String>();
    (
        code,
        if reason.is_empty() {
            status_line.to_string()
        } else {
            format!("{status_line} - {reason}")
        },
    )
}

/// Read a response up to the end of its headers and a little body, within the timeout.
fn read_response(stream: &mut TcpStream) -> io::Result<String> {
    stream.set_read_timeout(Some(CONNECT))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let until = Instant::now() + CONNECT;
    loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(e) => return Err(e),
        };
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(i) = text.find("\r\n\r\n") {
            // Icecast answers a refusal with a short HTML body; a little of it
            // is read for the reason, without waiting for a close.
            let status = text.lines().next().unwrap_or("").to_string();
            if status.contains(" 200 ") || buf.len() > i + 4 + 512 || Instant::now() > until {
                break;
            }
            stream.set_read_timeout(Some(Duration::from_millis(300)))?;
            continue;
        }
        if buf.len() > 16384 || Instant::now() > until {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// A connection to the server, with its socket set as every uplink wants it.
fn open(cfg: &Icecast, port: u16) -> Result<TcpStream, Refusal> {
    let addr = (cfg.host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| Refusal::Other(format!("{}: {e}", cfg.host)))?
        .next()
        .ok_or_else(|| Refusal::Other(format!("{}: no address", cfg.host)))?;
    let stream =
        TcpStream::connect_timeout(&addr, CONNECT).map_err(|e| Refusal::Other(e.to_string()))?;
    let _ = stream.set_nodelay(true);
    fix_send_buffer(&stream);
    stream
        .set_write_timeout(Some(CONNECT))
        .map_err(|e| Refusal::Other(e.to_string()))?;
    Ok(stream)
}

/// The stream's socket as it is while the stream flows: no read timeout, a
/// write that makes no progress for `STALL` an error.
fn streaming(stream: TcpStream) -> Result<TcpStream, Refusal> {
    stream
        .set_read_timeout(None)
        .map_err(|e| Refusal::Other(e.to_string()))?;
    stream
        .set_write_timeout(Some(STALL))
        .map_err(|e| Refusal::Other(e.to_string()))?;
    Ok(stream)
}

/// The server's answer, or the refusal a server that never answers earns.
fn answer(stream: &mut TcpStream) -> Result<String, Refusal> {
    let text = read_response(stream).map_err(|e| Refusal::Other(e.to_string()))?;
    // A server that takes the connection and never answers (frozen, or a proxy
    // with nothing behind it) is a refusal with a reason of its own.
    if text.trim().is_empty() {
        return Err(Refusal::Other(format!(
            "no answer from the server within {} s",
            CONNECT.as_secs()
        )));
    }
    Ok(text)
}

fn connect(cfg: &Icecast, content_type: &str) -> Result<TcpStream, Refusal> {
    match cfg.protocol {
        Protocol::Icecast2 => connect_icecast(cfg, content_type),
        Protocol::Shoutcast1 => connect_shoutcast(cfg, content_type),
    }
}

/// Icecast 2: the `SOURCE` request to the mount, answered with a status line.
fn connect_icecast(cfg: &Icecast, content_type: &str) -> Result<TcpStream, Refusal> {
    let mut stream = open(cfg, cfg.port)?;
    stream
        .write_all(source_request(cfg, content_type, env!("CARGO_PKG_VERSION")).as_bytes())
        .map_err(|e| Refusal::Other(e.to_string()))?;
    let text = answer(&mut stream)?;
    let (code, reason) = parse_response(&text);
    match code {
        200 => streaming(stream),
        401 => Err(Refusal::Auth(reason)),
        _ => Err(Refusal::Other(reason)),
    }
}

/// SHOUTcast v1: the password on the port above the server's, the server's
/// `OK2`, then the `icy-*` headers; the stream follows.
fn connect_shoutcast(cfg: &Icecast, content_type: &str) -> Result<TcpStream, Refusal> {
    let port = cfg
        .port
        .checked_add(1)
        .ok_or_else(|| Refusal::Other("the server's port has no port above it".to_string()))?;
    let mut stream = open(cfg, port)?;
    stream
        .write_all(shoutcast_password_line(cfg).as_bytes())
        .map_err(|e| Refusal::Other(e.to_string()))?;
    let text = answer(&mut stream)?;
    let first = text.lines().next().unwrap_or("").trim().to_string();
    if !first.starts_with("OK2") {
        let reason = format!("SHOUTcast said: {first}");
        return if first.to_ascii_lowercase().contains("pass") {
            Err(Refusal::Auth(reason))
        } else {
            Err(Refusal::Other(reason))
        };
    }
    stream
        .write_all(shoutcast_headers(cfg, content_type).as_bytes())
        .map_err(|e| Refusal::Other(e.to_string()))?;
    streaming(stream)
}

/// The stream's title set through the server's admin interface, on a
/// connection of its own: Icecast's `/admin/metadata` with the source's
/// credentials, or SHOUTcast's `admin.cgi` with the password.
fn push_title(cfg: &Icecast, title: &str) -> Result<(), String> {
    let addr = (cfg.host.as_str(), cfg.port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| "no address".to_string())?;
    let mut s =
        TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let request = match cfg.protocol {
        Protocol::Icecast2 => metadata_request(cfg, title, env!("CARGO_PKG_VERSION")),
        Protocol::Shoutcast1 => shoutcast_metadata_request(cfg, title),
    };
    s.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let (code, reason) = parse_response(&String::from_utf8_lossy(&buf));
    if code == 200 {
        Ok(())
    } else {
        Err(reason)
    }
}

struct Connection {
    stream: TcpStream,
    encoder: Option<Encoder>,
    encoded: Option<Receiver<Arc<Vec<u8>>>>,
    /// The shape the encoder was started for; a stream of the same shape
    /// goes on through it without a seam.
    shape: Option<Segment>,
    bytes: u64,
    /// Bytes the kernel may hold unsent before the peer counts as not taking them.
    threshold: u64,
    /// Since when the unsent bytes have stood above the threshold.
    stall_since: Option<Instant>,
}

/// What went wrong on the way to the server.
enum Trouble {
    /// The encoder child is gone; a new one is started.
    Encoder(String),
    /// The connection is lost; it is made again after the backoff.
    Connection(String),
}

/// Bytes of the stream into the encoder, and what it made so far onto the socket.
fn carry(c: &mut Connection, bytes: &[u8]) -> Result<(), Trouble> {
    let Some(e) = c.encoder.as_mut() else {
        return Ok(());
    };
    if let Err(err) = e.feed(bytes) {
        return Err(Trouble::Encoder(err.to_string()));
    }
    if let Some(rx) = c.encoded.as_ref() {
        match pump(&mut c.stream, rx, &mut c.bytes) {
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                return Err(Trouble::Connection("peer stalled".to_string()))
            }
            Err(err) => return Err(Trouble::Connection(err.to_string())),
            Ok(()) => {}
        }
    }
    if stalled(c) {
        return Err(Trouble::Connection(format!(
            "peer stalled: {} bytes not taken for {} s",
            unsent(&c.stream),
            STALL.as_secs()
        )));
    }
    Ok(())
}

/// The encoder ended and what it still held written out; an error means the
/// connection is gone.
fn end_encoder(c: &mut Connection) -> io::Result<()> {
    c.shape = None;
    if let Some(e) = c.encoder.take() {
        e.finish(GRACE);
        if let Some(rx) = c.encoded.take() {
            drain(&mut c.stream, &rx, &mut c.bytes)?;
        }
    }
    Ok(())
}

/// The connection given up, the retry scheduled.
fn lose(
    conn: &mut Option<Connection>,
    ctx: &Ctx,
    reason: &str,
    attempt: &mut u32,
    next_try: &mut Instant,
) {
    ctx.log(&format!("connection lost: {reason}"));
    ctx.state("waiting", Some(reason));
    ctx.update(|u| u.reconnects += 1);
    if let Some(mut c) = conn.take() {
        if let Some(enc) = c.encoder.take() {
            enc.kill();
        }
    }
    *attempt += 1;
    *next_try = Instant::now() + Duration::from_secs(BACKOFF_MAX.min(1u64 << (*attempt).min(6)));
}

/// Bytes of the stream delivered through the connection, and what goes wrong
/// on the way dealt with: an encoder gone is replaced at the next turn, a
/// connection gone is made again after the backoff.
fn deliver(
    conn: &mut Option<Connection>,
    ctx: &Ctx,
    bytes: &[u8],
    attempt: &mut u32,
    next_try: &mut Instant,
) {
    let Some(c) = conn.as_mut() else { return };
    match carry(c, bytes) {
        Ok(()) => {
            let bytes = c.bytes;
            ctx.update(|u| u.bytes = bytes);
        }
        Err(Trouble::Encoder(err)) => {
            ctx.log(&format!("encoder gone: {err}"));
            if let Some(enc) = c.encoder.take() {
                enc.kill();
            }
            c.encoded = None;
            c.shape = None;
        }
        Err(Trouble::Connection(reason)) => {
            lose(conn, ctx, &reason, attempt, next_try);
        }
    }
}

/// Half the send buffer: a peer taking bytes keeps the queue far below it.
fn stall_threshold(_cfg: &Icecast) -> u64 {
    uplink::STALL_THRESHOLD
}

/// Whether the peer has stopped taking bytes for `STALL`.
fn stalled(c: &mut Connection) -> bool {
    if unsent(&c.stream) > c.threshold {
        match c.stall_since {
            None => c.stall_since = Some(Instant::now()),
            Some(since) if since.elapsed() >= STALL => return true,
            Some(_) => {}
        }
    } else {
        c.stall_since = None;
    }
    false
}

pub fn run(cfg: Icecast, ctx: Ctx, ctl: Receiver<Event>, pcm: Receiver<Arc<Vec<u8>>>) {
    let content_type = match cfg.protocol {
        Protocol::Icecast2 => cfg.codec.content_type(),
        Protocol::Shoutcast1 => shoutcast_content_type(cfg.codec),
    };
    let mut segment: Option<Segment> = None;
    let mut title = String::new();
    let mut pushed = String::new();
    let mut conn: Option<Connection> = None;
    let mut attempt: u32 = 0;
    let mut next_try = Instant::now();
    let mut ended_at: Option<Instant> = None;
    // The silence that keeps the mount alive while the player stands still.
    let mut silence: Option<Silence> = None;
    let mut last_pcm = Instant::now();
    // One shape for the encoder, whatever the player plays.
    let mut conformer = Conformer::new(cfg.rate, cfg.channels);
    let mut conformed: Vec<u8> = Vec::new();
    ctx.state("idle", None);
    ctx.log(&format!(
        "{}:{}{} as {}{}{}",
        cfg.host,
        cfg.port,
        match cfg.protocol {
            Protocol::Icecast2 => cfg.mount.clone(),
            Protocol::Shoutcast1 => format!(" SHOUTcast v1 stream {}", cfg.stream_id),
        },
        content_type,
        if cfg.keep_alive { ", kept alive" } else { "" },
        if cfg.protocol == Protocol::Shoutcast1 {
            ", the source on the port above"
        } else {
            ""
        }
    ));

    loop {
        // A connection is wanted while a stream plays, or always where the
        // mount is kept alive; one is tried when its time comes.
        if (segment.is_some() || cfg.keep_alive) && conn.is_none() && Instant::now() >= next_try {
            ctx.state("connecting", None);
            match connect(&cfg, content_type) {
                Ok(stream) => {
                    attempt = 0;
                    pushed.clear();
                    ctx.log(&format!(
                        "source accepted at {}:{}{}",
                        cfg.host, cfg.port, cfg.mount
                    ));
                    ctx.state("connected", Some(""));
                    conn = Some(Connection {
                        stream,
                        encoder: None,
                        encoded: None,
                        shape: None,
                        bytes: 0,
                        threshold: stall_threshold(&cfg),
                        stall_since: None,
                    });
                }
                Err(refusal) => {
                    attempt += 1;
                    let (delay, reason) = match refusal {
                        Refusal::Auth(r) => (AUTH_BACKOFF, format!("refused: {r}")),
                        Refusal::Other(r) => (BACKOFF_MAX.min(1u64 << attempt.min(6)), r),
                    };
                    ctx.log(&format!("{reason}; retry in {delay} s"));
                    ctx.state("waiting", Some(&reason));
                    ctx.update(|u| u.reconnects += 1);
                    next_try = Instant::now() + Duration::from_secs(delay);
                }
            }
        }
        // Connected and no encoder: one at the conformer's shape, while a
        // stream plays or always where the mount is kept alive.
        if let Some(c) = conn.as_mut() {
            if c.encoder.is_none() {
                let shape = (segment.is_some() || cfg.keep_alive).then(|| conformer.shape());
                if let Some(shape) = shape {
                    let (tx, rx) = mpsc::sync_channel(ENCODED_QUEUE);
                    let recipe = Recipe {
                        codec: cfg.codec,
                        bitrate: cfg.bitrate,
                        rate: cfg.rate,
                        title: if cfg.codec.in_band_titles() {
                            title.clone()
                        } else {
                            String::new()
                        },
                        output: Output::Pipe,
                        container: Container::Native,
                    };
                    match Encoder::start(&ctx.evopeg, &shape, &recipe, Some(tx), &ctx.id) {
                        Ok(e) => {
                            c.encoder = Some(e);
                            c.encoded = Some(rx);
                            c.shape = Some(shape);
                            if segment.is_none() && silence.is_none() {
                                silence = Some(Silence::new(&shape));
                                ctx.update(|u| u.silence = true);
                            }
                        }
                        Err(e) => {
                            ctx.state("error", Some(&format!("encoder: {e}")));
                            ctx.log(&format!("encoder: {e}"));
                            std::thread::sleep(Duration::from_secs(2));
                        }
                    }
                    if !cfg.codec.in_band_titles() && !title.is_empty() && pushed != title {
                        match push_title(&cfg, &title) {
                            Ok(()) => {
                                pushed = title.clone();
                                ctx.update(|u| u.title = title.clone());
                            }
                            Err(e) => ctx.log(&format!("title not taken: {e}")),
                        }
                    }
                }
            }
        }
        // A stream that ended, where the mount is not kept alive: the connection
        // lingers for the next, then goes.
        if let Some(at) = ended_at {
            if !cfg.keep_alive && segment.is_none() && at.elapsed() > LINGER {
                if let Some(c) = conn.take() {
                    ctx.log("stream ended; the connection closed");
                    drop(c);
                }
                ended_at = None;
                ctx.state("idle", None);
            }
        }

        match uplink::next(&ctl, &pcm, Duration::from_millis(20)) {
            Next::Gone => break,
            Next::Event(Event::Stop) => break,
            Next::Nothing => {
                // Nothing from the player: where the mount is kept alive and the
                // player stands still, silence of the encoder's shape flows.
                if !cfg.keep_alive {
                    continue;
                }
                let still = segment.is_none() || last_pcm.elapsed() > STILL;
                let Some(shape) = conn.as_ref().and_then(|c| c.shape) else {
                    continue;
                };
                if !still {
                    continue;
                }
                if silence.is_none() {
                    silence = Some(Silence::new(&shape));
                    ctx.update(|u| u.silence = true);
                }
                let zeros = silence.as_mut().and_then(|s| s.due().map(<[u8]>::to_vec));
                let Some(zeros) = zeros else { continue };
                deliver(&mut conn, &ctx, &zeros, &mut attempt, &mut next_try);
            }
            Next::Event(Event::Segment(seg)) => {
                ended_at = None;
                segment = Some(seg);
                last_pcm = Instant::now();
                if silence.take().is_some() {
                    ctx.update(|u| u.silence = false);
                }
                // The stream is conformed to the uplink's shape, and what the
                // stream before still held goes first. The encoder runs on where
                // the shape is held; where it follows the player and changed, the
                // encoder ends and a new one starts at the new shape.
                conformer.segment(&seg, &mut conformed);
                ctx.log(&conformer.describe(&seg));
                if !conformed.is_empty() {
                    deliver(&mut conn, &ctx, &conformed, &mut attempt, &mut next_try);
                }
                if let Some(c) = conn.as_mut() {
                    if c.shape.is_some_and(|s| s != conformer.shape()) && end_encoder(c).is_err() {
                        conn = None;
                    }
                }
                if conn.is_some() {
                    ctx.state("connected", None);
                }
            }
            Next::Event(Event::End) => {
                segment = None;
                conformed.clear();
                conformer.end(&mut conformed);
                if !conformed.is_empty() {
                    deliver(&mut conn, &ctx, &conformed, &mut attempt, &mut next_try);
                }
                if cfg.keep_alive {
                    // The encoder goes on, fed silence until the next stream.
                    if let Some(c) = conn.as_ref() {
                        if let (Some(shape), true) = (c.shape, silence.is_none()) {
                            silence = Some(Silence::new(&shape));
                            ctx.update(|u| u.silence = true);
                        }
                    }
                } else {
                    ended_at = Some(Instant::now());
                    if let Some(c) = conn.as_mut() {
                        if end_encoder(c).is_err() {
                            conn = None;
                        }
                    }
                }
            }
            Next::Event(Event::Title(t)) => {
                title = t;
                match conn.as_mut() {
                    Some(c) if cfg.codec.in_band_titles() && segment.is_some() => {
                        // A new chain with the new title, on the same connection.
                        if end_encoder(c).is_err() {
                            conn = None;
                        }
                        ctx.update(|u| u.title = title.clone());
                    }
                    Some(_) if !title.is_empty() && pushed != title => {
                        match push_title(&cfg, &title) {
                            Ok(()) => {
                                pushed = title.clone();
                                ctx.update(|u| u.title = title.clone());
                            }
                            Err(e) => ctx.log(&format!("title not taken: {e}")),
                        }
                    }
                    _ => {}
                }
            }
            Next::Pcm(piece) => {
                last_pcm = Instant::now();
                if silence.take().is_some() {
                    ctx.update(|u| u.silence = false);
                }
                conformer.push(&piece, &mut conformed);
                if conformed.is_empty() {
                    continue;
                }
                deliver(&mut conn, &ctx, &conformed, &mut attempt, &mut next_try);
            }
        }
    }
    if let Some(c) = conn.take() {
        if let Some(e) = c.encoder {
            e.kill();
        }
    }
    ctx.state("off", None);
}

/// Encoded pieces waiting, written to the socket.
fn pump(stream: &mut TcpStream, rx: &Receiver<Arc<Vec<u8>>>, bytes: &mut u64) -> io::Result<()> {
    while let Ok(piece) = rx.try_recv() {
        stream.write_all(&piece)?;
        *bytes += piece.len() as u64;
    }
    Ok(())
}

/// What an encoder wrote before it ended, written to the socket.
fn drain(stream: &mut TcpStream, rx: &Receiver<Arc<Vec<u8>>>, bytes: &mut u64) -> io::Result<()> {
    while let Ok(piece) = rx.recv_timeout(Duration::from_millis(200)) {
        stream.write_all(&piece)?;
        *bytes += piece.len() as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Icecast {
        Icecast {
            enabled: true,
            protocol: Protocol::Icecast2,
            host: "ice.example.com".into(),
            port: 8000,
            mount: "/volumio.mp3".into(),
            stream_id: 1,
            user: "source".into(),
            password: "hackme".into(),
            codec: Codec::Mp3,
            bitrate: 192,
            rate: 44100,
            channels: 2,
            name: "Volumio".into(),
            description: "a\r\nb".into(),
            genre: String::new(),
            url: String::new(),
            public: false,
            keep_alive: true,
        }
    }

    #[test]
    fn the_source_request_is_what_icecast_expects() {
        let r = source_request(&cfg(), "audio/mpeg", "0.1.0");
        assert!(r.starts_with("SOURCE /volumio.mp3 HTTP/1.0\r\nHost: ice.example.com:8000\r\nAuthorization: Basic c291cmNlOmhhY2ttZQ==\r\nUser-Agent: outcast/0.1.0\r\nContent-Type: audio/mpeg\r\nice-public: 0\r\nice-name: Volumio\r\n"), "{r}");
        assert!(r.contains("ice-description: a b\r\n"));
        assert!(!r.contains("ice-genre"));
        assert!(r.contains("ice-audio-info: bitrate=192;samplerate=44100;channels=2\r\n"));
        assert!(r.ends_with("\r\n\r\n"));
    }

    #[test]
    fn the_metadata_request_encodes_the_title() {
        let r = metadata_request(&cfg(), "Zoë & Co - Ça va?", "0.1.0");
        assert!(r.starts_with("GET /admin/metadata?mode=updinfo&charset=UTF-8&mount=%2Fvolumio.mp3&song=Zo%C3%AB%20%26%20Co%20-%20%C3%87a%20va%3F HTTP/1.0\r\n"), "{r}");
        assert!(r.contains("Authorization: Basic c291cmNlOmhhY2ttZQ==\r\n"));
    }

    #[test]
    fn the_shoutcast_handshake_is_what_a_dnas_expects() {
        let mut c = cfg();
        c.protocol = Protocol::Shoutcast1;
        c.genre = "Spiritual".into();
        assert_eq!(shoutcast_password_line(&c), "hackme\r\n");
        c.stream_id = 2;
        assert_eq!(shoutcast_password_line(&c), "hackme:#2\r\n");
        let h = shoutcast_headers(&c, "audio/mpeg");
        assert_eq!(h, "icy-name:Volumio\r\nicy-genre:Spiritual\r\nicy-url:\r\nicy-pub:0\r\nicy-br:192\r\nicy-irc:\r\nicy-icq:\r\nicy-aim:\r\ncontent-type:audio/mpeg\r\n\r\n");
        let m = shoutcast_metadata_request(&c, "Zoë - Ça va");
        assert!(m.starts_with("GET /admin.cgi?pass=hackme&mode=updinfo&song=Zo%C3%AB%20-%20%C3%87a%20va&sid=2 HTTP/1.0\r\nHost: ice.example.com:8000\r\nUser-Agent: (Mozilla Compatible)\r\n\r\n"), "{m}");
        c.stream_id = 1;
        assert!(!shoutcast_metadata_request(&c, "x").contains("sid="));
        assert_eq!(shoutcast_content_type(Codec::Aac), "audio/aacp");
        assert_eq!(shoutcast_content_type(Codec::Mp3), "audio/mpeg");
    }

    #[test]
    fn base64_and_percent_are_right() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"source:hackme"), "c291cmNlOmhhY2ttZQ==");
        assert_eq!(percent("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(percent("/ ?&=#"), "%2F%20%3F%26%3D%23");
    }

    #[test]
    fn responses_are_read_for_their_code_and_reason() {
        assert_eq!(
            parse_response("HTTP/1.0 200 OK\r\nServer: Icecast 2.4.4\r\n\r\n").0,
            200
        );
        let (code, reason) = parse_response("HTTP/1.0 403 Forbidden\r\nContent-Type: text/html\r\n\r\n<html><head><title>Error</title></head><body><b>Mountpoint in use</b></body></html>");
        assert_eq!(code, 403);
        assert_eq!(reason, "HTTP/1.0 403 Forbidden - Error Mountpoint in use");
        assert_eq!(
            parse_response("HTTP/1.0 401 Authentication Required\r\n\r\n").0,
            401
        );
        assert_eq!(parse_response("garbage").0, 0);
    }
}
