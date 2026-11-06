//! The RTMP uplink: the stream published to an RTMP server (nginx-rtmp,
//! Wowza, Red5, Ant Media, a streaming platform, a custom ingest) as AAC or
//! MP3 audio messages, this side the publisher. The transport is the core's
//! own over TCP, or over TLS (`rtmps://`, rustls) for a server that wants it;
//! the session the `rml_rtmp` crate's (pure Rust); the encoder is evopeg
//! making an FLV stream, whose audio tags become the messages. A server that
//! goes away is tried again with a backoff; silence keeps the stream up while
//! the player stands still, as for Icecast.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
    PublishRequestType, StreamMetadata,
};
use rml_rtmp::time::RtmpTimestamp;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use stream::conform::Conformer;
use stream::feed::{Event, Segment};

use crate::config::{Codec, Rtmp};
use crate::encoder::{Container, Encoder, Output, Recipe};
use crate::uplink::{
    self, fix_send_buffer, unsent, Ctx, Next, Silence, GRACE, STALL_THRESHOLD, STILL,
};

/// Encoded pieces waiting between the encoder and the link.
const QUEUE: usize = 256;
/// How long the connection and each step of the session may take.
const CONNECT: Duration = Duration::from_secs(15);
/// A write that makes no progress for this long is a dead peer.
const STALL: Duration = Duration::from_secs(8);
/// How many bytes the server may take before it owes an acknowledgement: small, so
/// that the stream of silence an idle player sends (a few hundred bytes a second as
/// AAC) still draws one every few seconds; at a music bitrate it is a few a second,
/// each sixteen bytes.
const WINDOW: u32 = 4096;
/// The backoff's ceiling, in seconds.
const BACKOFF_MAX: u64 = 30;

/// What the link's thread tells the worker.
#[derive(Default, Clone, Debug)]
pub struct LinkState {
    pub open: bool,
    pub error: Option<String>,
    pub bytes: u64,
}

/// The parts of an RTMP URL the session needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub app: String,
    pub tc_url: String,
    /// `rtmps://`: the connection inside TLS.
    pub tls: bool,
}

/// `rtmp://host[:port]/app[/...]` or `rtmps://…` taken apart; the stream key is its
/// own setting. The port is 1935 unless given, 443 for rtmps.
pub fn parse_url(url: &str) -> Result<Target, String> {
    let url = url.trim();
    let (scheme, rest) = if let Some(r) = url.strip_prefix("rtmp://") {
        ("rtmp", r)
    } else if let Some(r) = url.strip_prefix("rtmps://") {
        ("rtmps", r)
    } else {
        return Err("the address must start with rtmp:// or rtmps://".to_string());
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
            if scheme == "rtmps" { 443 } else { 1935 },
        ),
    };
    if host.is_empty() {
        return Err("the address has no host".to_string());
    }
    let app = path.trim_matches('/').to_string();
    if app.is_empty() {
        return Err(
            "the address has no application after the host, as rtmp://host/live".to_string(),
        );
    }
    Ok(Target {
        tc_url: format!("{scheme}://{authority}/{app}"),
        host,
        port,
        app,
        tls: scheme == "rtmps",
    })
}

/// The wire: the socket bare, or inside TLS.
enum Wire {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Wire {
    fn socket(&self) -> &TcpStream {
        match self {
            Wire::Plain(s) => s,
            Wire::Tls(t) => t.get_ref(),
        }
    }
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(s) => s.read(buf),
            Wire::Tls(t) => t.read(buf),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(s) => s.write(buf),
            Wire::Tls(t) => t.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Wire::Plain(s) => s.flush(),
            Wire::Tls(t) => t.flush(),
        }
    }
}

/// The roots a server's certificate may chain to: the public ones (Mozilla's, as
/// carried by webpki-roots), and those of the operator's PEM file when one is named.
fn roots(ca_file: &str) -> Result<RootCertStore, String> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if !ca_file.trim().is_empty() {
        let path = Path::new(ca_file.trim());
        let mut added = 0;
        for cert in
            CertificateDer::pem_file_iter(path).map_err(|e| format!("{}: {e}", path.display()))?
        {
            let cert = cert.map_err(|e| format!("{}: {e}", path.display()))?;
            roots
                .add(cert)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            added += 1;
        }
        if added == 0 {
            return Err(format!("{}: no certificate in it", path.display()));
        }
    }
    Ok(roots)
}

/// The socket wrapped in TLS and the handshake done, the server's certificate
/// checked against the roots and the host name.
fn wrap_tls(sock: TcpStream, host: &str, ca_file: &str) -> Result<Wire, String> {
    let config = ClientConfig::builder()
        .with_root_certificates(roots(ca_file)?)
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string())
        .map_err(|_| format!("{host}: not a name TLS takes"))?;
    let conn = ClientConnection::new(Arc::new(config), name).map_err(|e| format!("tls: {e}"))?;
    let mut tls = StreamOwned::new(conn, sock);
    while tls.conn.is_handshaking() {
        tls.conn
            .complete_io(&mut tls.sock)
            .map_err(|e| format!("tls handshake with {host}: {e}"))?;
    }
    Ok(Wire::Tls(Box::new(tls)))
}

/// One FLV tag's audio payload and its timestamp.
struct Tag {
    kind: u8,
    timestamp: u32,
    data: Vec<u8>,
}

/// FLV from the encoder cut into tags: the header skipped once, then each
/// tag whole; what is not yet whole stays in `held`.
struct Flv {
    held: Vec<u8>,
    header_seen: bool,
}

impl Flv {
    fn new() -> Self {
        Self {
            held: Vec::new(),
            header_seen: false,
        }
    }

    fn push(&mut self, bytes: &[u8], out: &mut Vec<Tag>) {
        self.held.extend_from_slice(bytes);
        let mut at = 0usize;
        if !self.header_seen {
            // "FLV", version, flags, header size (9), then the first previous-tag size (4).
            if self.held.len() < 13 {
                return;
            }
            if &self.held[..3] != b"FLV" {
                // Not FLV: nothing to do but drop it; the encoder is wrong.
                self.held.clear();
                return;
            }
            let size = u32::from_be_bytes([self.held[5], self.held[6], self.held[7], self.held[8]])
                as usize;
            at = size + 4;
            if self.held.len() < at {
                return;
            }
            self.header_seen = true;
        }
        loop {
            if self.held.len() < at + 11 {
                break;
            }
            let h = &self.held[at..at + 11];
            let kind = h[0] & 0x1f;
            let size = u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
            let timestamp = u32::from_be_bytes([h[7], h[4], h[5], h[6]]);
            let whole = at + 11 + size + 4;
            if self.held.len() < whole {
                break;
            }
            out.push(Tag {
                kind,
                timestamp,
                data: self.held[at + 11..at + 11 + size].to_vec(),
            });
            at = whole;
        }
        self.held.drain(..at);
    }
}

struct Link {
    state: Arc<Mutex<LinkState>>,
    tx: SyncSender<Arc<Vec<u8>>>,
    thread: Option<JoinHandle<()>>,
}

impl Link {
    fn open(cfg: &Rtmp, target: &Target, id: &str) -> Link {
        let (tx, rx) = mpsc::sync_channel::<Arc<Vec<u8>>>(QUEUE);
        let state = Arc::new(Mutex::new(LinkState::default()));
        let shared = Arc::clone(&state);
        let cfg = cfg.clone();
        let target = target.clone();
        let id = id.to_string();
        let thread = std::thread::Builder::new()
            .name(format!("{id}-link"))
            .spawn(move || {
                let outcome = drive(&cfg, &target, rx, &shared, &id);
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                s.open = false;
                if let Err(e) = outcome {
                    s.error = Some(e);
                }
            })
            .expect("rtmp link thread");
        Link {
            state,
            tx,
            thread: Some(thread),
        }
    }

    fn state(&self) -> LinkState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn push(&self, piece: Arc<Vec<u8>>) -> bool {
        match self.tx.try_send(piece) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn close(mut self) {
        drop(self.tx);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The server's acknowledgements, the proof that it reads what it is sent. The
/// kernel's queue shows a stopped server only once its receive buffer is full, which
/// at a radio bitrate takes a minute or more; a server that acknowledges stops doing
/// so the moment it stops reading. The watch arms itself from the second
/// acknowledgement on, with the server's own interval: one that acknowledges at a
/// window of its own choosing is judged by that, and one that never acknowledges
/// is left to the kernel's queue.
#[derive(Default)]
struct Acks {
    /// Bytes sent when the last acknowledgement came.
    last: Option<u64>,
    /// Bytes between the last two acknowledgements.
    interval: Option<u64>,
    /// Since when the next acknowledgement has been overdue.
    overdue: Option<Instant>,
}

impl Acks {
    fn received(&mut self, sent: u64) {
        if let Some(last) = self.last {
            self.interval = Some(sent.saturating_sub(last).max(WINDOW as u64));
        }
        self.last = Some(sent);
        self.overdue = None;
    }

    /// Whether the server has owed an acknowledgement for `STALL`.
    fn stalled(&mut self, sent: u64) -> bool {
        let (Some(last), Some(interval)) = (self.last, self.interval) else {
            return false;
        };
        // Half an interval of grace beyond the one that is due.
        if sent.saturating_sub(last) <= interval + interval / 2 {
            self.overdue = None;
            return false;
        }
        self.overdue.get_or_insert_with(Instant::now).elapsed() >= STALL
    }
}

/// The session's results acted on: packets written, events judged.
fn act(
    stream: &mut Wire,
    results: Vec<ClientSessionResult>,
    sent: &mut u64,
    state: &mut Phase,
    acks: &mut Acks,
) -> Result<(), String> {
    for r in results {
        match r {
            ClientSessionResult::OutboundResponse(p) => {
                stream.write_all(&p.bytes).map_err(|e| e.to_string())?;
                *sent += p.bytes.len() as u64;
            }
            ClientSessionResult::RaisedEvent(e) => match e {
                ClientSessionEvent::ConnectionRequestAccepted => *state = Phase::Connected,
                ClientSessionEvent::ConnectionRequestRejected { description } => {
                    return Err(format!("refused: {description}"))
                }
                ClientSessionEvent::PublishRequestAccepted => *state = Phase::Publishing,
                ClientSessionEvent::AcknowledgementReceived { .. } => acks.received(*sent),
                _ => {}
            },
            ClientSessionResult::UnhandleableMessageReceived(_) => {}
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Handshaking,
    Connecting,
    Connected,
    Publishing,
}

/// Bytes the peer sent, if any, within the socket's read timeout.
fn read_some(stream: &mut Wire, buf: &mut [u8]) -> Result<Option<usize>, String> {
    match stream.read(buf) {
        Ok(0) => Err("the server closed the connection".to_string()),
        Ok(n) => Ok(Some(n)),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
            Ok(None)
        }
        // TLS: the server went without a close_notify, which the platforms do when
        // they refuse a stream; it is a close all the same.
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            Err("the server closed the connection".to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn drive(
    cfg: &Rtmp,
    target: &Target,
    rx: Receiver<Arc<Vec<u8>>>,
    shared: &Arc<Mutex<LinkState>>,
    id: &str,
) -> Result<(), String> {
    let addr = (target.host.as_str(), target.port)
        .to_socket_addrs()
        .map_err(|e| format!("{}: {e}", target.host))?
        .next()
        .ok_or_else(|| format!("{}: no address", target.host))?;
    let sock = TcpStream::connect_timeout(&addr, CONNECT).map_err(|e| e.to_string())?;
    let _ = sock.set_nodelay(true);
    fix_send_buffer(&sock);
    sock.set_write_timeout(Some(STALL))
        .map_err(|e| e.to_string())?;
    // The TLS handshake first, with the connection's own patience; then the short
    // read timeout the session loop polls with.
    sock.set_read_timeout(Some(CONNECT))
        .map_err(|e| e.to_string())?;
    let mut stream = if target.tls {
        wrap_tls(sock, &target.host, &cfg.ca_file)?
    } else {
        Wire::Plain(sock)
    };
    stream
        .socket()
        .set_read_timeout(Some(Duration::from_millis(5)))
        .map_err(|e| e.to_string())?;
    let mut sent: u64 = 0;
    let mut acks = Acks::default();
    let mut buf = vec![0u8; 16384];

    // The handshake.
    let mut handshake = Handshake::new(PeerType::Client);
    let first = handshake
        .generate_outbound_p0_and_p1()
        .map_err(|e| format!("handshake: {e:?}"))?;
    stream.write_all(&first).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let leftover = loop {
        if started.elapsed() > CONNECT {
            return Err(format!(
                "no handshake from {addr} within {} s",
                CONNECT.as_secs()
            ));
        }
        let Some(n) =
            read_some(&mut stream, &mut buf).map_err(|e| format!("{e} during the handshake"))?
        else {
            continue;
        };
        match handshake
            .process_bytes(&buf[..n])
            .map_err(|e| format!("handshake: {e:?}"))?
        {
            HandshakeProcessResult::InProgress { response_bytes } => {
                if !response_bytes.is_empty() {
                    stream
                        .write_all(&response_bytes)
                        .map_err(|e| e.to_string())?;
                }
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                if !response_bytes.is_empty() {
                    stream
                        .write_all(&response_bytes)
                        .map_err(|e| e.to_string())?;
                }
                break remaining_bytes;
            }
        }
    };

    // The session: connect to the application, create the stream, publish.
    let mut config = ClientSessionConfig::new();
    config.tc_url = Some(target.tc_url.clone());
    config.window_ack_size = WINDOW;
    config.flash_version = format!("outcast/{}", env!("CARGO_PKG_VERSION"));
    let (mut session, results) =
        ClientSession::new(config).map_err(|e| format!("session: {e:?}"))?;
    let mut phase = Phase::Handshaking;
    act(&mut stream, results, &mut sent, &mut phase, &mut acks)?;
    if !leftover.is_empty() {
        let results = session
            .handle_input(&leftover)
            .map_err(|e| format!("session: {e:?}"))?;
        act(&mut stream, results, &mut sent, &mut phase, &mut acks)?;
    }
    let r = session
        .request_connection(target.app.clone())
        .map_err(|e| format!("connect: {e:?}"))?;
    phase = Phase::Connecting;
    act(&mut stream, vec![r], &mut sent, &mut phase, &mut acks)?;
    let started = Instant::now();
    let mut asked = false;
    while phase != Phase::Publishing {
        if started.elapsed() > CONNECT {
            return Err(format!(
                "no answer to {} from the server within {} s",
                if asked { "publish" } else { "connect" },
                CONNECT.as_secs()
            ));
        }
        if phase == Phase::Connected && !asked {
            let r = session
                .request_publishing(cfg.stream_key.clone(), PublishRequestType::Live)
                .map_err(|e| format!("publish: {e:?}"))?;
            act(&mut stream, vec![r], &mut sent, &mut phase, &mut acks)?;
            asked = true;
        }
        let Some(n) = read_some(&mut stream, &mut buf).map_err(|e| {
            format!(
                "{e} while {}",
                if asked {
                    "asking to publish"
                } else {
                    "connecting"
                }
            )
        })?
        else {
            continue;
        };
        let results = session
            .handle_input(&buf[..n])
            .map_err(|e| format!("session: {e:?}"))?;
        act(&mut stream, results, &mut sent, &mut phase, &mut acks)?;
    }
    eprintln!(
        "outcast: {id}: publishing to {} as {}",
        target.tc_url,
        cfg.stream_key_shown()
    );
    let metadata = StreamMetadata {
        audio_codec_id: Some(match cfg.codec {
            Codec::Mp3 => 2,
            _ => 10,
        }),
        audio_bitrate_kbps: Some(cfg.bitrate),
        audio_sample_rate: Some(if cfg.rate == 0 { 44100 } else { cfg.rate }),
        audio_channels: Some(if cfg.channels == 0 { 2 } else { cfg.channels }),
        audio_is_stereo: Some(cfg.channels != 1),
        encoder: Some(format!("outcast/{}", env!("CARGO_PKG_VERSION"))),
        ..StreamMetadata::new()
    };
    let r = session
        .publish_metadata(&metadata)
        .map_err(|e| format!("metadata: {e:?}"))?;
    act(&mut stream, vec![r], &mut sent, &mut phase, &mut acks)?;
    {
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        s.open = true;
    }

    // The stream: FLV from the encoder cut into tags, each audio tag a message.
    let mut flv = Flv::new();
    let mut tags: Vec<Tag> = Vec::new();
    let mut gone = false;
    let mut stall_since: Option<Instant> = None;
    let mut acks_told = false;
    loop {
        // A server that stops reading stops acknowledging at once, and shows in the
        // kernel's queue once its buffer is full; either is a dead peer.
        if acks.stalled(sent) {
            return Err(format!(
                "no acknowledgement from the server for {} s",
                STALL.as_secs()
            ));
        }
        if unsent(stream.socket()) > STALL_THRESHOLD {
            let since = *stall_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= STALL {
                return Err(format!(
                    "the server stopped taking bytes for {} s",
                    STALL.as_secs()
                ));
            }
        } else {
            stall_since = None;
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(piece) => {
                tags.clear();
                flv.push(&piece, &mut tags);
                for t in tags.drain(..) {
                    if t.kind != 8 {
                        continue;
                    }
                    let r = session
                        .publish_audio_data(
                            Bytes::from(t.data),
                            RtmpTimestamp::new(t.timestamp),
                            false,
                        )
                        .map_err(|e| format!("audio: {e:?}"))?;
                    act(&mut stream, vec![r], &mut sent, &mut phase, &mut acks)?;
                }
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                s.bytes = sent;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                gone = true;
            }
        }
        if gone {
            break;
        }
        // What the server sent: acknowledgements, pings, a close.
        if let Some(n) = read_some(&mut stream, &mut buf)? {
            let results = session
                .handle_input(&buf[..n])
                .map_err(|e| format!("session: {e:?}"))?;
            act(&mut stream, results, &mut sent, &mut phase, &mut acks)?;
            if let (Some(interval), false) = (acks.interval, acks_told) {
                eprintln!(
                    "outcast: {id}: the server acknowledges every {} KiB",
                    interval / 1024
                );
                acks_told = true;
            }
        }
    }
    if let Ok(results) = session.stop_publishing() {
        let _ = act(&mut stream, results, &mut sent, &mut phase, &mut acks);
    }
    Ok(())
}

pub fn run(cfg: Rtmp, ctx: Ctx, ctl: Receiver<Event>, pcm: Receiver<Arc<Vec<u8>>>) {
    let target = match parse_url(&cfg.url) {
        Ok(t) => t,
        Err(e) => {
            ctx.state("error", Some(&e));
            ctx.log(&e);
            loop {
                match uplink::next(&ctl, &pcm, Duration::from_millis(200)) {
                    Next::Gone | Next::Event(Event::Stop) => return,
                    _ => {}
                }
            }
        }
    };
    let mut segment: Option<Segment> = None;
    let mut link: Option<Link> = None;
    let mut encoder: Option<Encoder> = None;
    let mut encoded: Option<Receiver<Arc<Vec<u8>>>> = None;
    let mut shape: Option<Segment> = None;
    let mut attempt: u32 = 0;
    let mut next_try = Instant::now();
    let mut silence: Option<Silence> = None;
    let mut last_pcm = Instant::now();
    let mut conformer = Conformer::new(cfg.rate, cfg.channels);
    let mut conformed: Vec<u8> = Vec::new();
    let mut ended_at: Option<Instant> = None;
    ctx.state("idle", None);
    ctx.log(&format!(
        "{} as {}{}",
        target.tc_url,
        cfg.codec.extension(),
        if cfg.keep_alive { ", kept alive" } else { "" }
    ));

    let lose = |link: &mut Option<Link>,
                encoder: &mut Option<Encoder>,
                encoded: &mut Option<Receiver<Arc<Vec<u8>>>>,
                shape: &mut Option<Segment>,
                reason: &str,
                attempt: &mut u32,
                next_try: &mut Instant| {
        ctx.log(&format!("connection lost: {reason}"));
        ctx.state("waiting", Some(reason));
        ctx.update(|u| u.reconnects += 1);
        if let Some(l) = link.take() {
            l.close();
        }
        if let Some(e) = encoder.take() {
            e.kill();
        }
        *encoded = None;
        *shape = None;
        *attempt += 1;
        *next_try =
            Instant::now() + Duration::from_secs(BACKOFF_MAX.min(1u64 << (*attempt).min(6)));
    };

    loop {
        if (segment.is_some() || cfg.keep_alive) && link.is_none() && Instant::now() >= next_try {
            ctx.state("connecting", None);
            let l = Link::open(&cfg, &target, &ctx.id);
            let until = Instant::now() + CONNECT * 3;
            loop {
                let s = l.state();
                if s.open {
                    attempt = 0;
                    ctx.state("connected", Some(""));
                    link = Some(l);
                    break;
                }
                if let Some(e) = s.error {
                    l.close();
                    attempt += 1;
                    let delay = BACKOFF_MAX.min(1u64 << attempt.min(6));
                    ctx.log(&format!("{e}; retry in {delay} s"));
                    ctx.state("waiting", Some(&e));
                    ctx.update(|u| u.reconnects += 1);
                    next_try = Instant::now() + Duration::from_secs(delay);
                    break;
                }
                if Instant::now() > until {
                    l.close();
                    next_try = Instant::now() + Duration::from_secs(2);
                    break;
                }
                match uplink::next(&ctl, &pcm, Duration::from_millis(20)) {
                    Next::Gone | Next::Event(Event::Stop) => {
                        l.close();
                        ctx.state("off", None);
                        return;
                    }
                    Next::Event(Event::Segment(seg)) => {
                        segment = Some(seg);
                        conformer.segment(&seg, &mut conformed);
                    }
                    Next::Event(Event::End) => segment = None,
                    _ => {}
                }
            }
        }
        if link.is_some() && encoder.is_none() && (segment.is_some() || cfg.keep_alive) {
            let wanted = conformer.shape();
            let (tx, rx) = mpsc::sync_channel(QUEUE);
            let recipe = Recipe {
                codec: cfg.codec,
                bitrate: cfg.bitrate,
                rate: cfg.rate,
                title: String::new(),
                output: Output::Pipe,
                container: Container::Flv,
            };
            match Encoder::start(&ctx.evopeg, &wanted, &recipe, Some(tx), &ctx.id) {
                Ok(e) => {
                    encoder = Some(e);
                    encoded = Some(rx);
                    shape = Some(wanted);
                    if segment.is_none() && silence.is_none() {
                        silence = Some(Silence::new(&wanted));
                        ctx.update(|u| u.silence = true);
                    }
                }
                Err(e) => {
                    ctx.state("error", Some(&format!("encoder: {e}")));
                    ctx.log(&format!("encoder: {e}"));
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
        if let Some(at) = ended_at {
            if !cfg.keep_alive && segment.is_none() && at.elapsed() > Duration::from_secs(5) {
                if let Some(e) = encoder.take() {
                    e.finish(GRACE);
                }
                encoded = None;
                shape = None;
                if let Some(l) = link.take() {
                    ctx.log("stream ended; the connection closed");
                    l.close();
                }
                ended_at = None;
                ctx.state("idle", None);
            }
        }
        if let Some(l) = link.as_ref() {
            let s = l.state();
            if !s.open {
                let reason = s
                    .error
                    .unwrap_or_else(|| "the connection closed".to_string());
                lose(
                    &mut link,
                    &mut encoder,
                    &mut encoded,
                    &mut shape,
                    &reason,
                    &mut attempt,
                    &mut next_try,
                );
            } else {
                ctx.update(|u| u.bytes = s.bytes);
            }
        }

        let feed = |bytes: &[u8],
                    encoder: &mut Option<Encoder>,
                    encoded: &mut Option<Receiver<Arc<Vec<u8>>>>,
                    shape: &mut Option<Segment>,
                    link: &mut Option<Link>| {
            let Some(e) = encoder.as_mut() else { return };
            if let Err(err) = e.feed(bytes) {
                ctx.log(&format!("encoder gone: {err}"));
                if let Some(enc) = encoder.take() {
                    enc.kill();
                }
                *encoded = None;
                *shape = None;
                return;
            }
            if let (Some(rx), Some(l)) = (encoded.as_ref(), link.as_ref()) {
                while let Ok(out) = rx.try_recv() {
                    if !l.push(out) {
                        break;
                    }
                }
            }
        };

        match uplink::next(&ctl, &pcm, Duration::from_millis(20)) {
            Next::Gone | Next::Event(Event::Stop) => break,
            Next::Nothing => {
                if !cfg.keep_alive {
                    continue;
                }
                let still = segment.is_none() || last_pcm.elapsed() > STILL;
                let Some(s) = shape else { continue };
                if !still {
                    continue;
                }
                if silence.is_none() {
                    silence = Some(Silence::new(&s));
                    ctx.update(|u| u.silence = true);
                }
                let zeros = silence.as_mut().and_then(|z| z.due().map(<[u8]>::to_vec));
                let Some(zeros) = zeros else { continue };
                feed(&zeros, &mut encoder, &mut encoded, &mut shape, &mut link);
            }
            Next::Event(Event::Segment(seg)) => {
                ended_at = None;
                segment = Some(seg);
                last_pcm = Instant::now();
                if silence.take().is_some() {
                    ctx.update(|u| u.silence = false);
                }
                conformer.segment(&seg, &mut conformed);
                ctx.log(&conformer.describe(&seg));
                if !conformed.is_empty() {
                    feed(
                        &conformed,
                        &mut encoder,
                        &mut encoded,
                        &mut shape,
                        &mut link,
                    );
                }
                if shape.is_some_and(|s| s != conformer.shape()) {
                    if let Some(e) = encoder.take() {
                        e.finish(GRACE);
                    }
                    encoded = None;
                    shape = None;
                }
                if link.is_some() {
                    ctx.state("connected", None);
                }
            }
            Next::Event(Event::End) => {
                segment = None;
                conformed.clear();
                conformer.end(&mut conformed);
                if !conformed.is_empty() {
                    feed(
                        &conformed,
                        &mut encoder,
                        &mut encoded,
                        &mut shape,
                        &mut link,
                    );
                }
                if cfg.keep_alive {
                    if let (Some(s), true) = (shape, silence.is_none()) {
                        silence = Some(Silence::new(&s));
                        ctx.update(|u| u.silence = true);
                    }
                } else {
                    ended_at = Some(Instant::now());
                }
            }
            Next::Event(Event::Title(_)) => {}
            Next::Pcm(piece) => {
                last_pcm = Instant::now();
                if silence.take().is_some() {
                    ctx.update(|u| u.silence = false);
                }
                conformer.push(&piece, &mut conformed);
                if conformed.is_empty() {
                    continue;
                }
                feed(
                    &conformed,
                    &mut encoder,
                    &mut encoded,
                    &mut shape,
                    &mut link,
                );
            }
        }
    }
    if let Some(e) = encoder.take() {
        e.kill();
    }
    if let Some(l) = link.take() {
        l.close();
    }
    ctx.state("off", None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_rtmp_address_is_taken_apart() {
        let t = parse_url("rtmp://ingest.example.com/live").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.app.as_str()),
            ("ingest.example.com", 1935, "live")
        );
        assert_eq!(t.tc_url, "rtmp://ingest.example.com/live");
        assert!(!t.tls);
        let t = parse_url(" rtmp://10.0.0.5:1936/app/sub/ ").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.app.as_str()),
            ("10.0.0.5", 1936, "app/sub")
        );
        let t = parse_url("rtmps://live-api-s.facebook.com:443/rtmp").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.app.as_str(), t.tls),
            ("live-api-s.facebook.com", 443, "rtmp", true)
        );
        assert_eq!(
            parse_url("rtmps://a.rtmps.youtube.com/live2").unwrap().port,
            443
        );
        assert_eq!(
            parse_url("rtmps://a.rtmps.youtube.com/live2")
                .unwrap()
                .tc_url,
            "rtmps://a.rtmps.youtube.com/live2"
        );
        assert!(parse_url("http://x/live").unwrap_err().contains("rtmp://"));
        assert!(parse_url("rtmp://host")
            .unwrap_err()
            .contains("application"));
        assert!(parse_url("rtmp://host:abc/live")
            .unwrap_err()
            .contains("port"));
    }

    #[test]
    fn the_roots_are_the_public_ones_and_the_operators_file() {
        assert!(roots("").unwrap().len() > 100, "the public roots");
        assert!(roots("/nonexistent/ca.pem").unwrap_err().contains("ca.pem"));
        let dir = std::env::temp_dir().join(format!("outcast-roots-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.pem");
        std::fs::write(&empty, "nothing here\n").unwrap();
        assert!(roots(empty.to_str().unwrap())
            .unwrap_err()
            .contains("no certificate"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn flv_is_cut_into_tags_across_pieces() {
        // Header, first previous size, one audio tag of 3 bytes at 20 ms, previous size.
        let mut flv_bytes = vec![b'F', b'L', b'V', 1, 4, 0, 0, 0, 9, 0, 0, 0, 0];
        flv_bytes.extend_from_slice(&[
            8, 0, 0, 3, 0, 0, 20, 0, 0, 0, 0, 0xAF, 0x01, 0x42, 0, 0, 0, 14,
        ]);
        // A script tag of 2 bytes, then a second audio tag at 40 ms, in the next piece.
        let mut more = vec![18, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 1, 2, 0, 0, 0, 13];
        more.extend_from_slice(&[8, 0, 0, 1, 0, 0, 40, 0, 0, 0, 0, 0xAF, 0, 0, 0, 12]);
        let mut flv = Flv::new();
        let mut tags = Vec::new();
        flv.push(&flv_bytes[..20], &mut tags);
        assert!(tags.is_empty(), "a tag only when whole");
        flv.push(&flv_bytes[20..], &mut tags);
        assert_eq!(tags.len(), 1);
        assert_eq!(
            (tags[0].kind, tags[0].timestamp, tags[0].data.as_slice()),
            (8, 20, &[0xAF, 0x01, 0x42][..])
        );
        tags.clear();
        flv.push(&more, &mut tags);
        let kinds: Vec<(u8, u32)> = tags.iter().map(|t| (t.kind, t.timestamp)).collect();
        assert_eq!(kinds, vec![(18, 0), (8, 40)]);
        assert!(flv.held.is_empty());
    }

    #[test]
    fn the_acknowledgement_watch_arms_on_the_second_and_takes_the_servers_interval() {
        let w = WINDOW as u64;
        let mut acks = Acks::default();
        // Nothing owed before the server has acknowledged twice, however much is sent.
        assert!(!acks.stalled(50 * w));
        acks.received(w);
        assert!(!acks.stalled(50 * w));
        // A second acknowledgement at the client's window: the next is due a window on,
        // with half a window of grace, and then only after STALL.
        acks.received(2 * w);
        assert!(!acks.stalled(3 * w + w / 2));
        assert!(
            !acks.stalled(3 * w + w / 2 + 1),
            "overdue, but not yet for STALL"
        );
        assert!(acks.overdue.is_some());
        acks.received(4 * w);
        assert!(acks.overdue.is_none(), "an acknowledgement clears the debt");
        // A server with a larger window of its own is judged by that window.
        acks.received(14 * w);
        assert!(!acks.stalled(29 * w));
        acks.stalled(30 * w);
        assert!(acks.overdue.is_some());
    }
}
