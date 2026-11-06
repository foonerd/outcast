//! The SRT uplink: the stream as an MPEG transport stream to an SRT listener,
//! this side the caller. The transport is the core's own, over UDP, with the
//! SRT protocol's state machines (`srt-protocol`, pure Rust); the encoder
//! is evopeg making the transport stream. A link that goes away is made
//! again with a backoff; the stream is kept up with silence while the
//! player stands still, as for Icecast.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use srt_protocol::connection::{Action, DuplexConnection, Input};
use srt_protocol::options::{Passphrase, SocketOptions};
use srt_protocol::packet::{Packet, ReceivePacketError, ReceivePacketResult};
use srt_protocol::protocol::pending_connection::connect::Connect;
use srt_protocol::protocol::pending_connection::ConnectionResult;
use srt_protocol::settings::ConnInitSettings;
use stream::conform::Conformer;
use stream::feed::{Event, Segment};

use crate::config::Srt;
use crate::encoder::{Container, Encoder, Output, Recipe};
use crate::uplink::{self, Ctx, Next, Silence, GRACE, STILL};

/// Bytes of one SRT payload: seven transport packets, what every SRT sender sends.
const PAYLOAD: usize = 7 * 188;
/// Encoded pieces waiting between the encoder and the link.
const QUEUE: usize = 256;
/// How long the handshake may take.
const CONNECT: Duration = Duration::from_secs(10);
/// The backoff's ceiling, in seconds.
const BACKOFF_MAX: u64 = 30;

/// What the link's thread tells the worker.
#[derive(Default, Clone, Debug)]
pub struct LinkState {
    pub open: bool,
    pub error: Option<String>,
    pub bytes: u64,
    pub rtt_ms: u32,
    pub lost: u64,
    pub retransmitted: u64,
}

/// One SRT connection on a thread of its own: the handshake, then the
/// payloads from `data` out and the protocol's own traffic in and out, until
/// the peer goes or the worker drops the sender.
struct Link {
    state: Arc<Mutex<LinkState>>,
    tx: SyncSender<Arc<Vec<u8>>>,
    thread: Option<JoinHandle<()>>,
}

impl Link {
    fn open(cfg: &Srt, id: &str) -> Link {
        let (tx, rx) = mpsc::sync_channel::<Arc<Vec<u8>>>(QUEUE);
        let state = Arc::new(Mutex::new(LinkState::default()));
        let shared = Arc::clone(&state);
        let cfg = cfg.clone();
        let id = id.to_string();
        let thread = std::thread::Builder::new()
            .name(format!("{id}-link"))
            .spawn(move || {
                let outcome = drive(&cfg, rx, &shared, &id);
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                s.open = false;
                if let Err(e) = outcome {
                    s.error = Some(e);
                }
            })
            .expect("srt link thread");
        Link {
            state,
            tx,
            thread: Some(thread),
        }
    }

    fn state(&self) -> LinkState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Encoded bytes to the link; false when it is gone.
    fn push(&self, piece: Arc<Vec<u8>>) -> bool {
        match self.tx.try_send(piece) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,
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

fn send(socket: &UdpSocket, buf: &mut BytesMut, packet: (Packet, SocketAddr)) {
    buf.clear();
    packet.0.serialize(buf);
    let _ = socket.send_to(buf, packet.1);
}

fn receive(socket: &UdpSocket, scratch: &mut [u8]) -> Option<ReceivePacketResult> {
    match socket.recv_from(scratch) {
        Ok((n, from)) => {
            let mut cursor = std::io::Cursor::new(&scratch[..n]);
            Some(
                Packet::parse(&mut cursor, from.is_ipv6())
                    .map(|p| (p, from))
                    .map_err(ReceivePacketError::from),
            )
        }
        Err(e)
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
        {
            None
        }
        Err(e) => Some(Err(ReceivePacketError::Io(e))),
    }
}

/// The link's whole life on its thread.
fn drive(
    cfg: &Srt,
    rx: Receiver<Arc<Vec<u8>>>,
    shared: &Arc<Mutex<LinkState>>,
    id: &str,
) -> Result<(), String> {
    let remote = (cfg.host.as_str(), cfg.port)
        .to_socket_addrs()
        .map_err(|e| format!("{}: {e}", cfg.host))?
        .next()
        .ok_or_else(|| format!("{}: no address", cfg.host))?;
    let socket = UdpSocket::bind(if remote.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    })
    .map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(Duration::from_millis(5)))
        .map_err(|e| e.to_string())?;
    let mut options = SocketOptions::default();
    let latency = Duration::from_millis(u64::from(cfg.latency_ms));
    options.sender.peer_latency = latency;
    options.receiver.latency = latency;
    if !cfg.passphrase.is_empty() {
        options.encryption.passphrase = Some(
            Passphrase::try_from(cfg.passphrase.clone())
                .map_err(|e| format!("passphrase: {e:?}"))?,
        );
    }
    let settings: ConnInitSettings = options.into();
    let stream_id = (!cfg.stream_id.is_empty()).then(|| cfg.stream_id.clone());
    let mut connect = Connect::new(
        remote,
        socket.local_addr().map_err(|e| e.to_string())?.ip(),
        settings,
        stream_id,
        rand::random(),
    );
    let mut buf = BytesMut::with_capacity(2048);
    let mut scratch = vec![0u8; 2048];

    // The handshake: the caller's induction, the listener's cookie, the
    // conclusion; a tick every 100 ms resends what went unanswered.
    let started = Instant::now();
    let mut last_tick = Instant::now();
    let mut pending = Some(connect.handle_tick(Instant::now()));
    let connection = loop {
        if started.elapsed() > CONNECT {
            return Err(format!(
                "no answer from {remote} within {} s",
                CONNECT.as_secs()
            ));
        }
        let result = match pending.take() {
            Some(r) => r,
            None => match receive(&socket, &mut scratch) {
                Some(p) => connect.handle_packet(p, Instant::now()),
                None if last_tick.elapsed() >= Duration::from_millis(100) => {
                    last_tick = Instant::now();
                    connect.handle_tick(Instant::now())
                }
                None => ConnectionResult::NoAction,
            },
        };
        match result {
            ConnectionResult::SendPacket(p) => send(&socket, &mut buf, p),
            ConnectionResult::Connected(p, c) => {
                if let Some(p) = p {
                    send(&socket, &mut buf, p);
                }
                break c;
            }
            ConnectionResult::Reject(p, why) => {
                if let Some(p) = p {
                    send(&socket, &mut buf, p);
                }
                return Err(format!("refused: {why:?}"));
            }
            ConnectionResult::Failure(e) => return Err(e.to_string()),
            ConnectionResult::NotHandled(e) => eprintln!("outcast: {id}: srt: {e}"),
            ConnectionResult::NoAction | ConnectionResult::RequestAccess(_) => {}
        }
    };
    {
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        s.open = true;
    }
    let mut conn = DuplexConnection::new(connection);

    // The stream: encoded pieces cut into payloads of seven transport
    // packets, each handed to the protocol with the moment it came.
    let mut held: Vec<u8> = Vec::with_capacity(PAYLOAD * 2);
    let mut input = Input::Timer;
    let mut gone = false;
    loop {
        let now = Instant::now();
        input = match conn.handle_input(now, input) {
            Action::Close => break,
            Action::ReleaseData(_) => Input::DataReleased,
            Action::SendPacket(p) => {
                send(&socket, &mut buf, p);
                Input::PacketSent
            }
            Action::UpdateStatistics(st) => {
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                s.bytes = st.tx_all_bytes;
                s.rtt_ms = st.tx_average_rtt.as_millis() as u32;
                s.lost = st.tx_loss_data;
                s.retransmitted = st.tx_retransmit_data;
                Input::StatisticsUpdated
            }
            Action::WaitForData(wait) => {
                if held.len() >= PAYLOAD {
                    let chunk = Bytes::copy_from_slice(&held[..PAYLOAD]);
                    held.drain(..PAYLOAD);
                    Input::Data(Some((now, chunk)))
                } else if gone {
                    // The worker closed the link: what is held goes, then the end.
                    if held.is_empty() {
                        Input::Data(None)
                    } else {
                        let chunk = Bytes::copy_from_slice(&held);
                        held.clear();
                        Input::Data(Some((now, chunk)))
                    }
                } else {
                    let left = wait.clamp(Duration::from_millis(1), Duration::from_millis(20));
                    match rx.recv_timeout(left) {
                        Ok(piece) => {
                            held.extend_from_slice(&piece);
                            Input::Timer
                        }
                        Err(RecvTimeoutError::Timeout) => match receive(&socket, &mut scratch) {
                            Some(p) => Input::Packet(p),
                            None => Input::Timer,
                        },
                        Err(RecvTimeoutError::Disconnected) => {
                            gone = true;
                            Input::Timer
                        }
                    }
                }
            }
        };
        // What the peer sent is read whenever a write just went.
        if matches!(input, Input::PacketSent | Input::DataReleased) {
            if let Some(p) = receive(&socket, &mut scratch) {
                input = Input::Packet(p);
            }
        }
    }
    if gone {
        Ok(())
    } else {
        Err("the peer closed the link".to_string())
    }
}

pub fn run(cfg: Srt, ctx: Ctx, ctl: Receiver<Event>, pcm: Receiver<Arc<Vec<u8>>>) {
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
        "srt://{}:{} as {} in a transport stream, latency {} ms{}{}",
        cfg.host,
        cfg.port,
        cfg.codec.extension(),
        cfg.latency_ms,
        if cfg.passphrase.is_empty() {
            ""
        } else {
            ", encrypted"
        },
        if cfg.keep_alive { ", kept alive" } else { "" }
    ));

    // The link given up, the retry scheduled, the encoder ended.
    let lose = |link: &mut Option<Link>,
                encoder: &mut Option<Encoder>,
                encoded: &mut Option<Receiver<Arc<Vec<u8>>>>,
                shape: &mut Option<Segment>,
                reason: &str,
                attempt: &mut u32,
                next_try: &mut Instant| {
        ctx.log(&format!("link lost: {reason}"));
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
        // A link is wanted while a stream plays, or always where it is kept alive.
        if (segment.is_some() || cfg.keep_alive) && link.is_none() && Instant::now() >= next_try {
            ctx.state("connecting", None);
            let l = Link::open(&cfg, &ctx.id);
            // The handshake, waited for here: the worker has nothing else to do without a link.
            let until = Instant::now() + CONNECT + Duration::from_secs(1);
            loop {
                let s = l.state();
                if s.open {
                    attempt = 0;
                    ctx.log(&format!("link up to {}:{}", cfg.host, cfg.port));
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
        // The link up and no encoder: one at the conformer's shape.
        if link.is_some() && encoder.is_none() && (segment.is_some() || cfg.keep_alive) {
            let wanted = conformer.shape();
            let (tx, rx) = mpsc::sync_channel(QUEUE);
            let recipe = Recipe {
                codec: cfg.codec,
                bitrate: cfg.bitrate,
                rate: cfg.rate,
                title: String::new(),
                output: Output::Pipe,
                container: Container::MpegTs,
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
        // A stream that ended where the link is not kept alive: the link lingers, then goes.
        if let Some(at) = ended_at {
            if !cfg.keep_alive && segment.is_none() && at.elapsed() > Duration::from_secs(5) {
                if let Some(e) = encoder.take() {
                    e.finish(GRACE);
                }
                encoded = None;
                shape = None;
                if let Some(l) = link.take() {
                    ctx.log("stream ended; the link closed");
                    l.close();
                }
                ended_at = None;
                ctx.state("idle", None);
            }
        }
        // The link's own word: gone, or its figures.
        if let Some(l) = link.as_ref() {
            let s = l.state();
            if !s.open {
                let reason = s.error.unwrap_or_else(|| "the link closed".to_string());
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
                ctx.update(|u| {
                    u.bytes = s.bytes;
                    u.rtt_ms = s.rtt_ms;
                    u.lost_packets = s.lost;
                });
            }
        }

        // Bytes to the encoder and what it made to the link.
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
