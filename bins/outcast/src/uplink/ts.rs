//! The transport-stream uplink: the stream as an MPEG transport stream to a
//! host and port, over TCP (the receiver listens; a connection lost is made
//! again with a backoff) or UDP (datagrams of seven transport packets,
//! unicast or multicast; nothing answers, so the status counts what was
//! sent and says when the network refused a datagram). The plain
//! contribution feed a broadcast chain, a decoder or a player such as VLC
//! takes.

use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use stream::conform::Conformer;
use stream::feed::{Event, Segment};

use crate::config::{Transport, Ts};
use crate::encoder::{Container, Encoder, Output, Recipe};
use crate::uplink::{
    self, fix_send_buffer, unsent, Ctx, Next, Silence, GRACE, STALL_THRESHOLD, STILL,
};

/// Bytes of one datagram: seven transport packets.
const DATAGRAM: usize = 7 * 188;
/// Encoded pieces waiting between the encoder and the socket.
const QUEUE: usize = 256;
/// How long a refused network is left before the socket is made again.
const RETRY: Duration = Duration::from_secs(5);
/// How long the TCP connection may take.
const CONNECT: Duration = Duration::from_secs(15);
/// A TCP write that makes no progress for this long is a dead receiver.
const STALL: Duration = Duration::from_secs(8);

enum Socket {
    Tcp(TcpStream),
    Udp(UdpSocket),
}

struct Sender {
    socket: Socket,
    to: SocketAddr,
    held: Vec<u8>,
    bytes: u64,
    /// Since when the unsent bytes have stood above the threshold (TCP).
    stall_since: Option<Instant>,
}

impl Sender {
    fn open(cfg: &Ts) -> Result<Sender, String> {
        let to = (cfg.host.as_str(), cfg.port)
            .to_socket_addrs()
            .map_err(|e| format!("{}: {e}", cfg.host))?
            .next()
            .ok_or_else(|| format!("{}: no address", cfg.host))?;
        let socket = match cfg.transport {
            Transport::Tcp => {
                let stream =
                    TcpStream::connect_timeout(&to, CONNECT).map_err(|e| format!("{to}: {e}"))?;
                let _ = stream.set_nodelay(true);
                fix_send_buffer(&stream);
                stream
                    .set_write_timeout(Some(STALL))
                    .map_err(|e| e.to_string())?;
                Socket::Tcp(stream)
            }
            Transport::Udp => {
                let socket = UdpSocket::bind(if to.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" })
                    .map_err(|e| e.to_string())?;
                if let SocketAddr::V4(v4) = to {
                    if v4.ip().is_multicast() {
                        socket
                            .set_multicast_ttl_v4(u32::from(cfg.ttl.max(1)))
                            .map_err(|e| e.to_string())?;
                        let _ = socket.set_multicast_loop_v4(false);
                        let _ = socket.join_multicast_v4(v4.ip(), &Ipv4Addr::UNSPECIFIED);
                    }
                }
                socket.connect(to).map_err(|e| e.to_string())?;
                Socket::Udp(socket)
            }
        };
        Ok(Sender {
            socket,
            to,
            held: Vec::with_capacity(DATAGRAM * 4),
            bytes: 0,
            stall_since: None,
        })
    }

    /// Encoded bytes out: over TCP as they come; over UDP as whole datagrams,
    /// a part of one waiting.
    fn push(&mut self, piece: &[u8]) -> Result<(), String> {
        match &mut self.socket {
            Socket::Tcp(stream) => {
                stream.write_all(piece).map_err(|e| {
                    if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut
                    {
                        format!("{}: the receiver stopped taking bytes", self.to)
                    } else {
                        format!("{}: {e}", self.to)
                    }
                })?;
                self.bytes += piece.len() as u64;
                // A receiver that stops taking bytes shows in the kernel's queue long
                // before a write blocks.
                if unsent(stream) > STALL_THRESHOLD {
                    let since = *self.stall_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= STALL {
                        return Err(format!(
                            "{}: the receiver stopped taking bytes for {} s",
                            self.to,
                            STALL.as_secs()
                        ));
                    }
                } else {
                    self.stall_since = None;
                }
            }
            Socket::Udp(socket) => {
                self.held.extend_from_slice(piece);
                let mut at = 0;
                while self.held.len() - at >= DATAGRAM {
                    socket
                        .send(&self.held[at..at + DATAGRAM])
                        .map_err(|e| format!("{}: {e}", self.to))?;
                    self.bytes += DATAGRAM as u64;
                    at += DATAGRAM;
                }
                self.held.drain(..at);
            }
        }
        Ok(())
    }
}

pub fn run(cfg: Ts, ctx: Ctx, ctl: Receiver<Event>, pcm: Receiver<Arc<Vec<u8>>>) {
    let mut segment: Option<Segment> = None;
    let mut sender: Option<Sender> = None;
    let mut next_try = Instant::now();
    let mut encoder: Option<Encoder> = None;
    let mut encoded: Option<Receiver<Arc<Vec<u8>>>> = None;
    let mut shape: Option<Segment> = None;
    let mut silence: Option<Silence> = None;
    let mut last_pcm = Instant::now();
    let mut conformer = Conformer::new(cfg.rate, cfg.channels);
    let mut conformed: Vec<u8> = Vec::new();
    ctx.state("idle", None);
    ctx.log(&format!(
        "{}://{}:{} as {} in a transport stream{}{}",
        match cfg.transport {
            Transport::Tcp => "tcp",
            Transport::Udp => "udp",
        },
        cfg.host,
        cfg.port,
        cfg.codec.extension(),
        if cfg.transport == Transport::Udp && cfg.ttl > 1 {
            format!(", ttl {}", cfg.ttl)
        } else {
            String::new()
        },
        if cfg.keep_alive { ", kept alive" } else { "" }
    ));

    loop {
        // The socket, made when wanted and made again after a refusal.
        if (segment.is_some() || cfg.keep_alive) && sender.is_none() && Instant::now() >= next_try {
            match Sender::open(&cfg) {
                Ok(s) => {
                    ctx.log(&format!("sending to {}", s.to));
                    ctx.state("connected", Some(""));
                    sender = Some(s);
                }
                Err(e) => {
                    ctx.log(&format!("{e}; retry in {} s", RETRY.as_secs()));
                    ctx.state("waiting", Some(&e));
                    ctx.update(|u| u.reconnects += 1);
                    next_try = Instant::now() + RETRY;
                }
            }
        }
        if sender.is_some() && encoder.is_none() && (segment.is_some() || cfg.keep_alive) {
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
        // Where the stream follows the player, the socket and the encoder end with it.
        if !cfg.keep_alive && segment.is_none() && encoder.is_some() {
            if let Some(e) = encoder.take() {
                e.finish(GRACE);
            }
            encoded = None;
            shape = None;
            sender = None;
            ctx.state("idle", None);
        }

        let feed = |bytes: &[u8],
                    encoder: &mut Option<Encoder>,
                    encoded: &mut Option<Receiver<Arc<Vec<u8>>>>,
                    shape: &mut Option<Segment>,
                    sender: &mut Option<Sender>,
                    next_try: &mut Instant| {
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
            let Some(rx) = encoded.as_ref() else { return };
            let Some(s) = sender.as_mut() else {
                // No receiver: the encoder's output is let go, never left to fill the
                // pipe, which would hold the encoder and this worker with it.
                while rx.try_recv().is_ok() {}
                return;
            };
            while let Ok(out) = rx.try_recv() {
                if let Err(err) = s.push(&out) {
                    ctx.log(&format!("lost: {err}"));
                    ctx.state("waiting", Some(&err));
                    ctx.update(|u| u.reconnects += 1);
                    *sender = None;
                    *next_try = Instant::now() + RETRY;
                    // The encoder goes with the connection and starts anew with the
                    // next one, as the other uplinks do: an encoder kept running with
                    // nowhere to send would fill its pipe and block the feed.
                    if let Some(enc) = encoder.take() {
                        enc.kill();
                    }
                    *encoded = None;
                    *shape = None;
                    return;
                }
            }
            let b = s.bytes;
            ctx.update(|u| u.bytes = b);
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
                feed(
                    &zeros,
                    &mut encoder,
                    &mut encoded,
                    &mut shape,
                    &mut sender,
                    &mut next_try,
                );
            }
            Next::Event(Event::Segment(seg)) => {
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
                        &mut sender,
                        &mut next_try,
                    );
                }
                if shape.is_some_and(|s| s != conformer.shape()) {
                    if let Some(e) = encoder.take() {
                        e.finish(GRACE);
                    }
                    encoded = None;
                    shape = None;
                }
                if sender.is_some() {
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
                        &mut sender,
                        &mut next_try,
                    );
                }
                if cfg.keep_alive {
                    if let (Some(s), true) = (shape, silence.is_none()) {
                        silence = Some(Silence::new(&s));
                        ctx.update(|u| u.silence = true);
                    }
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
                    &mut sender,
                    &mut next_try,
                );
            }
        }
    }
    if let Some(e) = encoder.take() {
        e.kill();
    }
    ctx.state("off", None);
}
