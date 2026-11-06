//! The HLS uplink: the stream as HTTP Live Streaming, packed audio (AAC in
//! ADTS or MP3 frames) cut into segments of a few seconds with a sliding
//! playlist, the way a browser, a phone, a smart speaker and a CDN take it.
//! The segments and the playlist live in the core's memory, never on the
//! player's card: served from there on a port of the player's for the LAN,
//! and put on the operator's origin by an uploader of their own, which never
//! holds the stream up. Each segment carries an ID3 tag at its head with the
//! timestamp HLS wants of packed audio, and the title the player shows.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use stream::conform::Conformer;
use stream::feed::{Event, Segment as Shape};

use crate::config::{Codec, Hls, OriginKind};
use crate::encoder::{Container, Encoder, Output, Recipe};
use crate::uplink::http::{Client, Target};
use crate::uplink::{self, Ctx, Next, Silence, GRACE, STILL};

/// Segments kept past the playlist's window, for a player still fetching the
/// one the playlist just dropped.
const HOLD: usize = 3;
/// The segment numbers and the timelines run on across the workers of one core:
/// a save on the page makes the worker anew, and a player mid-stream must see
/// the sequence go on upward, with a discontinuity, never start again at nought.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static TIMELINES: AtomicU64 = AtomicU64::new(0);
/// The backoff's ceiling for an origin that refuses, in seconds.
const BACKOFF_MAX: u64 = 30;

/// One encoded frame and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub bytes: Vec<u8>,
    pub samples: u32,
    pub rate: u32,
}

/// Bare frames found in the encoder's bytes: ADTS for AAC, MPEG audio for MP3;
/// what is not yet a whole frame waits for the next piece.
pub struct Frames {
    codec: Codec,
    held: Vec<u8>,
}

const ADTS_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];
const MP3_RATES: [[u32; 3]; 3] = [
    [11025, 12000, 8000],
    [22050, 24000, 16000],
    [44100, 48000, 32000],
];
const MP3_BITRATES_V1: [u32; 15] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const MP3_BITRATES_V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// The length and the samples of the frame at the head of `b`, if a header
/// is there whole and sound; None for a header not yet whole; Some(None) for
/// bytes that are no header.
#[allow(clippy::type_complexity)]
fn frame_at(codec: Codec, b: &[u8]) -> Option<Option<(usize, u32, u32)>> {
    match codec {
        Codec::Aac => {
            if b.len() < 7 {
                return None;
            }
            if b[0] != 0xFF || b[1] & 0xF6 != 0xF0 {
                return Some(None);
            }
            let rate_index = ((b[2] >> 2) & 0x0F) as usize;
            let Some(&rate) = ADTS_RATES.get(rate_index) else {
                return Some(None);
            };
            let len =
                ((b[3] as usize & 0x03) << 11) | ((b[4] as usize) << 3) | ((b[5] as usize) >> 5);
            let blocks = (b[6] & 0x03) as u32 + 1;
            if len < 7 {
                return Some(None);
            }
            Some(Some((len, 1024 * blocks, rate)))
        }
        _ => {
            if b.len() < 4 {
                return None;
            }
            if b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
                return Some(None);
            }
            let version = (b[1] >> 3) & 0x03; // 0: 2.5, 2: 2, 3: 1
            let layer = (b[1] >> 1) & 0x03; // 1: III
            if version == 1 || layer != 1 {
                return Some(None);
            }
            let bitrate_index = (b[2] >> 4) as usize;
            let rate_index = ((b[2] >> 2) & 0x03) as usize;
            let padding = ((b[2] >> 1) & 0x01) as usize;
            if bitrate_index == 0 || bitrate_index > 14 || rate_index > 2 {
                return Some(None);
            }
            let (bitrate, samples, rates) = match version {
                3 => (MP3_BITRATES_V1[bitrate_index], 1152u32, MP3_RATES[2]),
                2 => (MP3_BITRATES_V2[bitrate_index], 576u32, MP3_RATES[1]),
                _ => (MP3_BITRATES_V2[bitrate_index], 576u32, MP3_RATES[0]),
            };
            let rate = rates[rate_index];
            let len = (samples as usize / 8) * (bitrate as usize * 1000) / rate as usize + padding;
            Some(Some((len, samples, rate)))
        }
    }
}

impl Frames {
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            held: Vec::new(),
        }
    }

    /// The encoder's bytes in, the whole frames out.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Frame>) {
        self.held.extend_from_slice(bytes);
        let mut at = 0;
        while at < self.held.len() {
            match frame_at(self.codec, &self.held[at..]) {
                None => break,
                Some(None) => at += 1,
                Some(Some((len, samples, rate))) => {
                    if self.held.len() < at + len {
                        break;
                    }
                    out.push(Frame {
                        bytes: self.held[at..at + len].to_vec(),
                        samples,
                        rate,
                    });
                    at += len;
                }
            }
        }
        self.held.drain(..at);
    }
}

/// One segment as served: its bytes with the ID3 tag at the head.
pub struct Piece {
    pub seq: u64,
    pub name: String,
    pub bytes: Arc<Vec<u8>>,
    pub seconds: f64,
    /// The wall clock when it began, seconds since the epoch.
    pub began: f64,
    /// The timeline it belongs to; a change between two pieces is a discontinuity
    /// (the encoder started anew), and the number is the playlist's discontinuity
    /// sequence.
    pub timeline: u64,
}

/// A syncsafe 28-bit size as ID3v2 writes it.
fn syncsafe(n: u32) -> [u8; 4] {
    [
        ((n >> 21) & 0x7F) as u8,
        ((n >> 14) & 0x7F) as u8,
        ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

/// The ID3v2.4 tag at a segment's head: the timestamp HLS asks of packed audio
/// (a PRIV frame of Apple's, the 90 kHz clock in 8 bytes) and, when there is
/// one, the title as TIT2.
pub fn id3_tag(timestamp_90k: u64, title: &str) -> Vec<u8> {
    let mut frames: Vec<u8> = Vec::new();
    let owner = b"com.apple.streaming.transportStreamTimestamp\0";
    let mut priv_data = owner.to_vec();
    priv_data.extend_from_slice(&(timestamp_90k & 0x1_FFFF_FFFF).to_be_bytes());
    frames.extend_from_slice(b"PRIV");
    frames.extend_from_slice(&syncsafe(priv_data.len() as u32));
    frames.extend_from_slice(&[0, 0]);
    frames.extend_from_slice(&priv_data);
    if !title.is_empty() {
        let mut text = vec![0x03u8]; // UTF-8
        text.extend_from_slice(title.as_bytes());
        frames.extend_from_slice(b"TIT2");
        frames.extend_from_slice(&syncsafe(text.len() as u32));
        frames.extend_from_slice(&[0, 0]);
        frames.extend_from_slice(&text);
    }
    let mut tag = b"ID3\x04\x00\x00".to_vec();
    tag.extend_from_slice(&syncsafe(frames.len() as u32));
    tag.extend_from_slice(&frames);
    tag
}

/// Frames gathered into segments of the wanted length, each cut on a frame
/// boundary at the first frame past it; the timeline in samples from the
/// uplink's start.
pub struct Segmenter {
    name: String,
    extension: &'static str,
    target: f64,
    frames: Frames,
    gathered: Vec<u8>,
    gathered_samples: u64,
    rate: u32,
    /// Samples since the start, the timeline the ID3 timestamp follows.
    position: u64,
    next_seq: u64,
    timeline: u64,
    began: f64,
    pub title: String,
}

fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

impl Segmenter {
    /// A segmenter numbering from `first_seq` on timeline `timeline`.
    pub fn new(
        name: &str,
        codec: Codec,
        segment_seconds: u32,
        first_seq: u64,
        timeline: u64,
    ) -> Self {
        Self {
            name: name.to_string(),
            extension: codec.extension(),
            target: f64::from(segment_seconds.max(1)),
            frames: Frames::new(codec),
            gathered: Vec::new(),
            gathered_samples: 0,
            rate: 0,
            position: 0,
            next_seq: first_seq,
            timeline,
            began: 0.0,
            title: String::new(),
        }
    }

    /// The next segment begins a new timeline, the next of the core's.
    pub fn discontinuity(&mut self) {
        self.timeline = TIMELINES.fetch_add(1, Ordering::Relaxed) + 1;
    }

    /// The encoder's bytes in, the segments that became whole out.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Piece>) {
        let mut frames = Vec::new();
        self.frames.push(bytes, &mut frames);
        for f in frames {
            if self.gathered.is_empty() {
                self.began = now_seconds();
                self.rate = f.rate;
            }
            self.gathered.extend_from_slice(&f.bytes);
            self.gathered_samples += u64::from(f.samples);
            let seconds = self.gathered_samples as f64 / f64::from(self.rate.max(1));
            if seconds >= self.target {
                out.push(self.cut());
            }
        }
    }

    /// Whatever is gathered, as a segment, when the stream ends.
    pub fn flush(&mut self, out: &mut Vec<Piece>) {
        if !self.gathered.is_empty() {
            out.push(self.cut());
        }
    }

    fn cut(&mut self) -> Piece {
        let rate = u64::from(self.rate.max(1));
        let timestamp = self.position * 90_000 / rate;
        let mut bytes = id3_tag(timestamp, &self.title);
        bytes.append(&mut self.gathered);
        let seconds = self.gathered_samples as f64 / rate as f64;
        self.position += self.gathered_samples;
        self.gathered_samples = 0;
        let seq = self.next_seq;
        self.next_seq += 1;
        SEQUENCE.fetch_max(self.next_seq, Ordering::Relaxed);
        Piece {
            seq,
            name: format!("{}-{seq}.{}", self.name, self.extension),
            bytes: Arc::new(bytes),
            seconds,
            began: self.began,
            timeline: self.timeline,
        }
    }
}

/// The segments kept, the playlist as it stands: what the player serves and the
/// uploader reads.
pub struct Store {
    pub name: String,
    content_type: &'static str,
    window: usize,
    target: u32,
    pieces: VecDeque<Arc<Piece>>,
    /// The number the next piece gets: the core's count of segments cut.
    pub next_seq: u64,
}

impl Store {
    pub fn new(cfg: &Hls) -> Self {
        Self {
            name: cfg.name.clone(),
            content_type: cfg.codec.content_type(),
            window: cfg.window as usize,
            target: cfg.segment_seconds,
            pieces: VecDeque::new(),
            next_seq: SEQUENCE.load(Ordering::Relaxed),
        }
    }

    pub fn playlist_name(&self) -> String {
        format!("{}.m3u8", self.name)
    }

    pub fn push(&mut self, piece: Piece) {
        self.next_seq = piece.seq + 1;
        self.pieces.push_back(Arc::new(piece));
        while self.pieces.len() > self.window + HOLD {
            self.pieces.pop_front();
        }
    }

    /// The pieces the playlist lists: the newest `window`.
    pub fn listed(&self) -> Vec<Arc<Piece>> {
        let skip = self.pieces.len().saturating_sub(self.window);
        self.pieces.iter().skip(skip).cloned().collect()
    }

    /// The playlist for a set of pieces in sequence order, as the player serves it
    /// or as the origin gets it (for the pieces that landed there).
    pub fn playlist(&self, pieces: &[Arc<Piece>]) -> String {
        let mut target = f64::from(self.target);
        for p in pieces {
            target = target.max(p.seconds);
        }
        let mut out = String::new();
        out.push_str("#EXTM3U\n#EXT-X-VERSION:3\n");
        out.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", target.ceil() as u64));
        let first = pieces.first();
        out.push_str(&format!(
            "#EXT-X-MEDIA-SEQUENCE:{}\n",
            first.map(|p| p.seq).unwrap_or(0)
        ));
        out.push_str(&format!(
            "#EXT-X-DISCONTINUITY-SEQUENCE:{}\n",
            first.map(|p| p.timeline).unwrap_or(0)
        ));
        let mut timeline = first.map(|p| p.timeline).unwrap_or(0);
        for p in pieces {
            if p.timeline != timeline {
                out.push_str("#EXT-X-DISCONTINUITY\n");
                timeline = p.timeline;
            }
            out.push_str(&format!("#EXT-X-PROGRAM-DATE-TIME:{}\n", iso8601(p.began)));
            out.push_str(&format!("#EXTINF:{:.3},\n{}\n", p.seconds, p.name));
        }
        out
    }

    pub fn piece(&self, name: &str) -> Option<Arc<Piece>> {
        self.pieces.iter().find(|p| p.name == name).cloned()
    }
}

/// A moment as the playlist dates it: `2026-11-06T16:40:05.250Z`.
fn iso8601(seconds: f64) -> String {
    let whole = seconds.floor() as i64;
    let millis = ((seconds - whole as f64) * 1000.0).round() as i64;
    let days = whole.div_euclid(86_400);
    let secs = whole.rem_euclid(86_400);
    // Civil from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// The player's own server for the playlist and the segments, from memory.
fn serve(mut stream: TcpStream, store: Arc<Mutex<Store>>) {
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
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");
    let path = path
        .split('?')
        .next()
        .unwrap_or("/")
        .trim_start_matches('/');
    let common = "Server: outcast\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n";
    if method != "GET" && method != "HEAD" {
        let _ = stream.write_all(
            format!("HTTP/1.0 405 Method Not Allowed\r\n{common}Content-Length: 0\r\n\r\n")
                .as_bytes(),
        );
        return;
    }
    let (content_type, cache, body): (&str, &str, Arc<Vec<u8>>) = {
        let s = store.lock().unwrap_or_else(|e| e.into_inner());
        if path.is_empty() || path == s.playlist_name() {
            (
                "application/vnd.apple.mpegurl",
                "no-cache, no-store",
                Arc::new(s.playlist(&s.listed()).into_bytes()),
            )
        } else if let Some(p) = s.piece(path) {
            (s.content_type, "max-age=3600", Arc::clone(&p.bytes))
        } else {
            drop(s);
            let _ = stream.write_all(
                format!("HTTP/1.0 404 Not Found\r\n{common}Content-Length: 0\r\n\r\n").as_bytes(),
            );
            return;
        }
    };
    let head = format!(
        "HTTP/1.0 200 OK\r\n{common}Content-Type: {content_type}\r\nCache-Control: {cache}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if method == "GET" {
        let _ = stream.write_all(&body);
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// The uploader: the window's pieces put on the origin as they come, oldest
/// first, then the playlist naming those that landed; pieces that fell out of
/// the window and the hold removed. An origin that refuses is tried again with
/// a backoff; the playlist on it is only ever written after its pieces.
fn upload(
    cfg: Hls,
    store: Arc<Mutex<Store>>,
    nudge: Receiver<()>,
    landed_count: Arc<AtomicU64>,
    ctx: Ctx,
) {
    let target = match Target::parse(&cfg.origin_url) {
        Ok(t) => t,
        Err(e) => {
            ctx.state("error", Some(&format!("origin: {e}")));
            ctx.log(&format!("origin: {e}"));
            while nudge.recv().is_ok() {}
            return;
        }
    };
    let mut client = Client::new(target, &cfg.origin_user, &cfg.origin_password, &cfg.ca_file);
    let playlist_type = "application/vnd.apple.mpegurl";
    let mut landed: VecDeque<Arc<Piece>> = VecDeque::new();
    let mut attempt: u32 = 0;
    let mut next_try = Instant::now();
    let mut told_ok = false;
    // The files a worker before this one left on the origin, under the same name:
    // the numbers just below this worker's first, swept once the origin answers.
    let mut sweep: Option<std::ops::Range<u64>> = {
        let first = SEQUENCE.load(Ordering::Relaxed);
        (first > 0).then(|| first.saturating_sub((cfg.window as usize + HOLD) as u64 * 2)..first)
    };
    let extension = cfg.codec.extension();
    // A nudge for every segment cut; the worker gone ends the uploader.
    while let Ok(()) | Err(RecvTimeoutError::Timeout) =
        nudge.recv_timeout(Duration::from_millis(500))
    {
        if Instant::now() < next_try {
            continue;
        }
        let (listed, content_type, playlist_name) = {
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            (s.listed(), s.content_type, s.playlist_name())
        };
        let Some(newest) = listed.last().map(|p| p.seq) else {
            continue;
        };
        let last_landed = landed.back().map(|p| p.seq);
        if last_landed == Some(newest) {
            continue;
        }
        // What is listed and not yet on the origin, oldest first.
        let due: Vec<Arc<Piece>> = listed
            .iter()
            .filter(|p| last_landed.is_none_or(|l| p.seq > l))
            .cloned()
            .collect();
        let mut failed = None;
        for p in due {
            match client.put(&p.name, content_type, &p.bytes) {
                Ok(code) if (200..300).contains(&code) => {
                    landed_count.store(p.seq + 1, Ordering::Relaxed);
                    landed.push_back(p);
                    while landed.len() > cfg.window as usize + HOLD {
                        if let Some(old) = landed.pop_front() {
                            let _ = client.delete(&old.name);
                        }
                    }
                }
                Ok(code) => {
                    failed = Some(format!("{}: the origin answered {code}", p.name));
                    break;
                }
                Err(e) => {
                    failed = Some(format!("{}: {e}", p.name));
                    break;
                }
            }
        }
        if failed.is_none() {
            if let Some(range) = sweep.take() {
                for n in range {
                    let _ = client.delete(&format!("{}-{n}.{extension}", cfg.name));
                }
            }
            // The playlist of what landed, in the window.
            let skip = landed.len().saturating_sub(cfg.window as usize);
            let on_origin: Vec<Arc<Piece>> = landed.iter().skip(skip).cloned().collect();
            let text = store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .playlist(&on_origin);
            match client.put(&playlist_name, playlist_type, text.as_bytes()) {
                Ok(code) if (200..300).contains(&code) => {}
                Ok(code) => failed = Some(format!("{playlist_name}: the origin answered {code}")),
                Err(e) => failed = Some(format!("{playlist_name}: {e}")),
            }
        }
        match failed {
            Some(e) => {
                attempt += 1;
                let delay = BACKOFF_MAX.min(1u64 << attempt.min(6));
                next_try = Instant::now() + Duration::from_secs(delay);
                if attempt == 1 || attempt.is_multiple_of(10) {
                    ctx.log(&format!("origin: {e}; retry in {delay} s"));
                }
                told_ok = false;
                ctx.update(|u| u.last_error = format!("origin: {e}"));
            }
            None => {
                if attempt > 0 || !told_ok {
                    ctx.log(&format!("origin: {} up to date", cfg.origin_url));
                    told_ok = true;
                }
                attempt = 0;
                ctx.update(|u| u.last_error.clear());
            }
        }
    }
}

pub fn run(cfg: Hls, ctx: Ctx, ctl: Receiver<Event>, pcm: Receiver<Arc<Vec<u8>>>) {
    let store = Arc::new(Mutex::new(Store::new(&cfg)));
    ctx.state("idle", None);
    ctx.log(&format!(
        "{} as {} in segments of {} s, {} listed{}{}{}",
        store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .playlist_name(),
        cfg.codec.extension(),
        cfg.segment_seconds,
        cfg.window,
        if cfg.port != 0 {
            format!(", served on port {}", cfg.port)
        } else {
            String::new()
        },
        match cfg.origin {
            OriginKind::None => String::new(),
            OriginKind::Http => format!(", put on {}", cfg.origin_url),
        },
        if cfg.keep_alive { ", kept alive" } else { "" }
    ));

    // The player's server, where a port is set.
    let listener = if cfg.port != 0 {
        match TcpListener::bind(("0.0.0.0", cfg.port)) {
            Ok(l) => Some(l),
            Err(e) => {
                ctx.state("error", Some(&format!("port {}: {e}", cfg.port)));
                ctx.log(&format!("cannot listen on port {}: {e}", cfg.port));
                loop {
                    match uplink::next(&ctl, &pcm, Duration::from_millis(200)) {
                        Next::Gone | Next::Event(Event::Stop) => return,
                        _ => {}
                    }
                }
            }
        }
    } else {
        None
    };
    let acceptor: Option<JoinHandle<()>> = listener.as_ref().map(|l| {
        let store = Arc::clone(&store);
        let acceptor = l.try_clone().expect("listener clone");
        std::thread::Builder::new()
            .name("hls-accept".into())
            .spawn(move || {
                for stream in acceptor.incoming() {
                    let Ok(stream) = stream else { break };
                    let store = Arc::clone(&store);
                    let _ = std::thread::Builder::new()
                        .name("hls-serve".into())
                        .spawn(move || serve(stream, store));
                }
            })
            .expect("hls accept thread")
    });

    // The uploader, where an origin is set; what landed is counted for the status.
    let landed_count = Arc::new(AtomicU64::new(0));
    let (nudge, uploader): (Option<SyncSender<()>>, Option<JoinHandle<()>>) =
        if cfg.origin == OriginKind::Http {
            let (tx, rx) = mpsc::sync_channel::<()>(4);
            let store = Arc::clone(&store);
            let cfg2 = cfg.clone();
            let ctx2 = ctx.clone();
            let landed = Arc::clone(&landed_count);
            let t = std::thread::Builder::new()
                .name("hls-upload".into())
                .spawn(move || upload(cfg2, store, rx, landed, ctx2))
                .expect("hls upload thread");
            (Some(tx), Some(t))
        } else {
            (None, None)
        };

    let mut segmenter = Segmenter::new(
        &cfg.name,
        cfg.codec,
        cfg.segment_seconds,
        SEQUENCE.load(Ordering::Relaxed),
        TIMELINES.fetch_add(1, Ordering::Relaxed),
    );
    let mut segment: Option<Shape> = None;
    let mut encoder: Option<Encoder> = None;
    let mut encoded: Option<Receiver<Arc<Vec<u8>>>> = None;
    let mut shape: Option<Shape> = None;
    let mut silence: Option<Silence> = None;
    let mut last_pcm = Instant::now();
    let mut conformer = Conformer::new(cfg.rate, cfg.channels);
    let mut conformed: Vec<u8> = Vec::new();
    let mut bytes: u64 = 0;
    let mut first_encoder = true;
    let mut pieces: Vec<Piece> = Vec::new();

    // The encoder's bytes through the segmenter into the store.
    let take = |encoded: &Option<Receiver<Arc<Vec<u8>>>>,
                segmenter: &mut Segmenter,
                pieces: &mut Vec<Piece>,
                bytes: &mut u64| {
        let Some(rx) = encoded.as_ref() else { return };
        while let Ok(out) = rx.try_recv() {
            *bytes += out.len() as u64;
            segmenter.push(&out, pieces);
        }
        if pieces.is_empty() {
            return;
        }
        let (cut, listed) = {
            let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
            for p in pieces.drain(..) {
                s.push(p);
            }
            (s.next_seq, s.listed().len())
        };
        let b = *bytes;
        let pending = if nudge.is_some() {
            cut.saturating_sub(landed_count.load(Ordering::Relaxed))
        } else {
            0
        };
        ctx.update(|u| {
            u.segments = cut;
            u.bytes = b;
            u.pending = pending;
        });
        if let Some(n) = nudge.as_ref() {
            match n.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
            }
        }
        if listed > 0 {
            ctx.state("streaming", None);
        }
    };

    loop {
        if encoder.is_none() && (segment.is_some() || cfg.keep_alive) {
            let wanted = conformer.shape();
            let (tx, rx) = mpsc::sync_channel(64);
            let recipe = Recipe {
                codec: cfg.codec,
                bitrate: cfg.bitrate,
                rate: cfg.rate,
                title: String::new(),
                output: Output::Pipe,
                container: Container::Frames,
            };
            match Encoder::start(&ctx.evopeg, &wanted, &recipe, Some(tx), &ctx.id) {
                Ok(e) => {
                    encoder = Some(e);
                    encoded = Some(rx);
                    shape = Some(wanted);
                    if !first_encoder {
                        segmenter.discontinuity();
                    }
                    first_encoder = false;
                    ctx.update(|u| u.last_error.clear());
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

        let feed = |bytes_in: &[u8],
                    encoder: &mut Option<Encoder>,
                    encoded: &mut Option<Receiver<Arc<Vec<u8>>>>,
                    shape: &mut Option<Shape>| {
            let Some(e) = encoder.as_mut() else { return };
            if let Err(err) = e.feed(bytes_in) {
                ctx.log(&format!("encoder gone: {err}"));
                if let Some(enc) = encoder.take() {
                    enc.kill();
                }
                *encoded = None;
                *shape = None;
            }
        };

        match uplink::next(&ctl, &pcm, Duration::from_millis(20)) {
            Next::Gone | Next::Event(Event::Stop) => break,
            Next::Nothing => {
                take(&encoded, &mut segmenter, &mut pieces, &mut bytes);
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
                feed(&zeros, &mut encoder, &mut encoded, &mut shape);
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
                    feed(&conformed, &mut encoder, &mut encoded, &mut shape);
                }
                if shape.is_some_and(|s| s != conformer.shape()) {
                    if let Some(e) = encoder.take() {
                        e.finish(GRACE);
                        take(&encoded, &mut segmenter, &mut pieces, &mut bytes);
                    }
                    encoded = None;
                    shape = None;
                }
            }
            Next::Event(Event::End) => {
                segment = None;
                conformed.clear();
                conformer.end(&mut conformed);
                if !conformed.is_empty() {
                    feed(&conformed, &mut encoder, &mut encoded, &mut shape);
                }
                if cfg.keep_alive {
                    if let (Some(s), true) = (shape, silence.is_none()) {
                        silence = Some(Silence::new(&s));
                        ctx.update(|u| u.silence = true);
                    }
                } else {
                    if let Some(e) = encoder.take() {
                        e.finish(GRACE);
                        take(&encoded, &mut segmenter, &mut pieces, &mut bytes);
                    }
                    encoded = None;
                    shape = None;
                    segmenter.flush(&mut pieces);
                    take(&None, &mut segmenter, &mut pieces, &mut bytes);
                    ctx.state("idle", None);
                }
            }
            Next::Event(Event::Title(t)) => {
                segmenter.title = t.clone();
                ctx.update(|u| u.title = t);
            }
            Next::Pcm(piece) => {
                last_pcm = Instant::now();
                if silence.take().is_some() {
                    ctx.update(|u| u.silence = false);
                }
                conformer.push(&piece, &mut conformed);
                if !conformed.is_empty() {
                    feed(&conformed, &mut encoder, &mut encoded, &mut shape);
                }
                take(&encoded, &mut segmenter, &mut pieces, &mut bytes);
            }
        }
    }
    if let Some(e) = encoder.take() {
        e.kill();
    }
    drop(nudge);
    if let Some(t) = uploader {
        let _ = t.join();
    }
    if let Some(l) = listener {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::shutdown(l.as_raw_fd(), libc::SHUT_RDWR) };
        drop(l);
    }
    if let Some(a) = acceptor {
        let _ = a.join();
    }
    ctx.state("off", None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adts(rate_index: u8, len: usize) -> Vec<u8> {
        let mut f = vec![0xFF, 0xF1, (1 << 6) | (rate_index << 2), 0x80, 0, 0, 0xFC];
        f[3] |= ((len >> 11) & 0x03) as u8;
        f[4] = ((len >> 3) & 0xFF) as u8;
        f[5] = ((len & 0x07) << 5) as u8;
        f.resize(len, 0xAB);
        f
    }

    fn mp3(bitrate_index: u8, padding: u8) -> Vec<u8> {
        // MPEG-1 Layer III, 44.1 kHz.
        let mut f = vec![0xFF, 0xFB, (bitrate_index << 4) | (padding << 1), 0xC0];
        let len = 144 * MP3_BITRATES_V1[bitrate_index as usize] as usize * 1000 / 44100
            + padding as usize;
        f.resize(len, 0xCD);
        f
    }

    #[test]
    fn adts_frames_are_found_across_pieces_and_garbage_is_skipped() {
        let mut frames = Frames::new(Codec::Aac);
        let a = adts(3, 300); // 48 kHz
        let b = adts(4, 120); // 44.1 kHz
        let mut all = vec![0x00, 0x11];
        all.extend_from_slice(&a);
        all.extend_from_slice(&b);
        let mut out = Vec::new();
        frames.push(&all[..100], &mut out);
        assert!(out.is_empty());
        frames.push(&all[100..], &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(
            (out[0].bytes.len(), out[0].samples, out[0].rate),
            (300, 1024, 48000)
        );
        assert_eq!(
            (out[1].bytes.len(), out[1].samples, out[1].rate),
            (120, 1024, 44100)
        );
        assert_eq!(out[0].bytes, a);
    }

    #[test]
    fn mp3_frames_are_measured_from_their_headers() {
        let mut frames = Frames::new(Codec::Mp3);
        let a = mp3(9, 0); // 128 kbit/s: 417 bytes
        let b = mp3(9, 1); // 418 with padding
        let mut all = a.clone();
        all.extend_from_slice(&b);
        let mut out = Vec::new();
        frames.push(&all, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(
            (out[0].bytes.len(), out[0].samples, out[0].rate),
            (417, 1152, 44100)
        );
        assert_eq!(out[1].bytes.len(), 418);
    }

    #[test]
    fn the_id3_tag_carries_the_timestamp_and_the_title() {
        let tag = id3_tag(90_000 * 5, "Tone One - Outcast");
        assert_eq!(&tag[..3], b"ID3");
        assert_eq!(tag[3], 4);
        let size = ((tag[6] as usize) << 21)
            | ((tag[7] as usize) << 14)
            | ((tag[8] as usize) << 7)
            | tag[9] as usize;
        assert_eq!(size + 10, tag.len());
        assert_eq!(&tag[10..14], b"PRIV");
        let owner = b"com.apple.streaming.transportStreamTimestamp\0";
        assert_eq!(&tag[20..20 + owner.len()], owner);
        let ts = &tag[20 + owner.len()..20 + owner.len() + 8];
        assert_eq!(u64::from_be_bytes(ts.try_into().unwrap()), 450_000);
        let tit2 = tag.windows(4).position(|w| w == b"TIT2").unwrap();
        assert_eq!(&tag[tit2 + 11..tit2 + 11 + 18], b"Tone One - Outcast");
        assert!(id3_tag(0, "").windows(4).all(|w| w != b"TIT2"));
    }

    #[test]
    fn segments_are_cut_on_a_frame_past_the_length_with_a_running_timeline() {
        let mut seg = Segmenter::new("live", Codec::Aac, 1, 0, 0);
        let mut out = Vec::new();
        // 48 frames of 1024 at 48 kHz are 1.024 s: the 47th frame crosses 1 s.
        for _ in 0..100 {
            seg.push(&adts(3, 50), &mut out);
        }
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "live-0.aac");
        assert_eq!(out[1].name, "live-1.aac");
        assert!((out[0].seconds - 47.0 * 1024.0 / 48000.0).abs() < 1e-9);
        assert_eq!(out[0].bytes.len(), 73 + 47 * 50);
        // The second segment's timestamp is the first's length in 90 kHz.
        let owner_len = b"com.apple.streaming.transportStreamTimestamp\0".len();
        let ts = &out[1].bytes[20 + owner_len..20 + owner_len + 8];
        assert_eq!(
            u64::from_be_bytes(ts.try_into().unwrap()),
            47 * 1024 * 90_000 / 48_000
        );
        assert_eq!(out[1].timeline, 0);
        seg.discontinuity();
        seg.flush(&mut out);
        assert_eq!(out.len(), 3);
        assert!(out[2].timeline > 0, "a new timeline");
        // A segmenter made after another numbers on from it.
        let later = Segmenter::new("live", Codec::Aac, 1, 7, 3);
        assert_eq!((later.next_seq, later.timeline), (7, 3));
    }

    #[test]
    fn the_playlist_lists_the_window_with_its_sequence_and_discontinuities() {
        let cfg = Hls {
            window: 3,
            segment_seconds: 4,
            ..Hls::default()
        };
        let mut store = Store::new(&cfg);
        for seq in 0..8u64 {
            store.push(Piece {
                seq,
                name: format!("stream-{seq}.aac"),
                bytes: Arc::new(vec![0; 10]),
                seconds: 4.0 + seq as f64 * 0.1,
                began: 1_762_447_205.25,
                timeline: match seq {
                    0 | 1 => 0,
                    2..=5 => 1,
                    _ => 2,
                },
            });
        }
        let listed = store.listed();
        assert_eq!(
            listed.iter().map(|p| p.seq).collect::<Vec<_>>(),
            vec![5, 6, 7]
        );
        let text = store.playlist(&listed);
        assert!(text.starts_with("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:5\n#EXT-X-DISCONTINUITY-SEQUENCE:1\n"), "{text}");
        assert!(text.contains("stream-5.aac\n#EXT-X-DISCONTINUITY\n#EXT-X-PROGRAM-DATE-TIME:2025-11-06T16:40:05.250Z\n#EXTINF:4.600,\nstream-6.aac\n"), "{text}");
        // Listed from a piece that begins a timeline: its own count includes it.
        let from_six: Vec<Arc<Piece>> = listed.iter().skip(1).cloned().collect();
        let text = store.playlist(&from_six);
        assert!(
            text.contains("#EXT-X-MEDIA-SEQUENCE:6\n#EXT-X-DISCONTINUITY-SEQUENCE:2\n"),
            "{text}"
        );
        assert!(!text.contains("#EXT-X-DISCONTINUITY\n"), "{text}");
        assert!(text.ends_with("stream-7.aac\n"));
        assert!(store.piece("stream-7.aac").is_some());
        assert!(
            store.piece("stream-1.aac").is_none(),
            "dropped beyond the hold"
        );
        assert!(
            store.piece("stream-2.aac").is_some(),
            "held past the window"
        );
        assert_eq!(store.next_seq, 8);
    }

    #[test]
    fn moments_are_dated_as_the_playlist_wants() {
        assert_eq!(iso8601(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_762_447_205.25), "2025-11-06T16:40:05.250Z");
    }
}
