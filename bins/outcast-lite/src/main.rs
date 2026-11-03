//! outcast-lite: the player's sound for a page or a remote, as it is. The
//! tap's audio ring of a tag is read and served over a WebSocket as PCM,
//! a text frame per segment saying its shape, binary frames of at most
//! 20 ms each, no encoder anywhere. Started when something asks for the
//! sound and gone when nothing has listened for a while; what it does goes
//! to standard output as one JSON line per event, for whoever started it.
//!
//! `outcast-lite [--tag TAG] [--shm DIR] [--bind ADDR] [--port N] [--max N] [--idle SECONDS] [--cap RATE] [--version]`
//!
//! `TAG` is the ring's tag (`glass` unless given); `--shm` is where the
//! rings are (`/dev/shm` unless given); `--bind` and `--port` are where to
//! listen (`127.0.0.1` and `0`, a port of the system's choosing, said on
//! standard output); `--max` is the most listeners at once (8); `--idle`
//! is how many seconds without a listener end the process (30; 0 never);
//! `--cap` is a sample rate above which the stream is resampled down to
//! it and sent as 16-bit (0, no cap).

mod listeners;
mod wire;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use stream::conform::Conformer;
use stream::feed::{self, Conversion, Segment};
use tap::audio::Reader;

use wire::Announced;

/// Bytes taken from the ring in one go.
const TAKE: usize = 1 << 16;
/// How long to sleep when the ring has nothing new.
const NAP: Duration = Duration::from_millis(4);
/// How often to look for a ring when there is none.
const LOOK: Duration = Duration::from_millis(100);

struct Args {
    tag: String,
    shm: PathBuf,
    bind: String,
    port: u16,
    max: usize,
    idle: u64,
    cap: u32,
}

fn usage() -> ! {
    eprintln!("outcast-lite [--tag TAG] [--shm DIR] [--bind ADDR] [--port N] [--max N] [--idle SECONDS] [--cap RATE] [--version]");
    std::process::exit(64);
}

fn args() -> Args {
    let mut out = Args {
        tag: "glass".to_string(),
        shm: PathBuf::from(tap::ring::DIR),
        bind: "127.0.0.1".to_string(),
        port: 0,
        max: 8,
        idle: 30,
        cap: 0,
    };
    let mut it = std::env::args().skip(1);
    let next = |it: &mut std::iter::Skip<std::env::Args>| it.next().unwrap_or_else(|| usage());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tag" => out.tag = next(&mut it),
            "--shm" => out.shm = PathBuf::from(next(&mut it)),
            "--bind" => out.bind = next(&mut it),
            "--port" => out.port = next(&mut it).parse().unwrap_or_else(|_| usage()),
            "--max" => out.max = next(&mut it).parse().unwrap_or_else(|_| usage()),
            "--idle" => out.idle = next(&mut it).parse().unwrap_or_else(|_| usage()),
            "--cap" => out.cap = next(&mut it).parse().unwrap_or_else(|_| usage()),
            "--version" => {
                println!("outcast-lite {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => usage(),
        }
    }
    if out.tag.is_empty() {
        usage();
    }
    out
}

/// One JSON line on standard output, for whoever started the process.
fn say(line: &str) {
    use std::io::Write;
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{line}");
    let _ = o.flush();
}

/// The way from the ring's bytes to the wire for one segment: the feed's
/// conversion to a named format, and the cap's conformer where the rate is
/// above it.
struct Way {
    segment: Segment,
    conversion: Conversion,
    conformer: Option<Conformer>,
    announced: Segment,
    converted: Vec<u8>,
    conformed: Vec<u8>,
    narrowed: Vec<u8>,
    pieces: Vec<Vec<u8>>,
}

impl Way {
    fn new(shape: &tap::audio::Shape, cap: u32) -> Self {
        let (segment, conversion) = feed::plan(shape);
        let capped = cap > 0 && segment.rate > cap && !segment.dsd_silence;
        let mut conformer = capped.then(|| Conformer::new(cap, 0));
        let announced = match conformer.as_mut() {
            Some(c) => {
                let mut sink = Vec::new();
                c.segment(&segment, &mut sink);
                // The cap's stream is 16-bit: the rate came down for a weak
                // link and the width goes with it.
                let s = c.shape();
                Segment {
                    format: "s16le",
                    layout: tap::sample::Layout::Linear {
                        bytes: 2,
                        valid: 16,
                        signed: true,
                        little: true,
                    },
                    rate: s.rate,
                    channels: s.channels,
                    frame_bytes: 2 * s.channels,
                    dsd_silence: false,
                }
            }
            None => segment,
        };
        Self {
            segment,
            conversion,
            conformer,
            announced,
            converted: Vec::new(),
            conformed: Vec::new(),
            narrowed: Vec::new(),
            pieces: Vec::new(),
        }
    }

    fn capped(&self) -> bool {
        self.conformer.is_some()
    }

    /// The ring's bytes as frames for the wire, onto `pieces` (cleared first).
    fn pieces(&mut self, bytes: &[u8]) -> &[Vec<u8>] {
        self.pieces.clear();
        if self.segment.dsd_silence {
            return &self.pieces;
        }
        feed::convert(&self.conversion, &self.segment, bytes, &mut self.converted);
        let on_wire: &[u8] = match self.conformer.as_mut() {
            Some(c) => {
                c.push(&self.converted, &mut self.conformed);
                wire::narrow_to_s16(&self.conformed, &mut self.narrowed);
                &self.narrowed
            }
            None => &self.converted,
        };
        wire::chunks(on_wire, &self.announced, &mut self.pieces);
        &self.pieces
    }
}

fn main() {
    let a = args();
    let listener = match TcpListener::bind((a.bind.as_str(), a.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("outcast-lite: cannot listen on {}:{}: {e}", a.bind, a.port);
            std::process::exit(1);
        }
    };
    let address = listener
        .local_addr()
        .map(|s| s.to_string())
        .unwrap_or_default();
    let shared = listeners::registry(a.max);
    let hello = format!(
        "{{\"type\":\"hello\",\"version\":\"{}\",\"tag\":\"{}\"}}",
        env!("CARGO_PKG_VERSION"),
        a.tag
    );
    let acceptor = listeners::accept(
        listener.try_clone().expect("listener clone"),
        Arc::clone(&shared),
        hello,
    );
    eprintln!(
        "outcast-lite {}: ring tag {}, listening on {address}, {} listeners at most, idle {} s, cap {}",
        env!("CARGO_PKG_VERSION"),
        a.tag,
        a.max,
        a.idle,
        a.cap
    );
    say(&format!(
        "{{\"type\":\"listening\",\"address\":\"{address}\",\"tag\":\"{}\"}}",
        a.tag
    ));
    let mut last_heard = Instant::now();
    let mut buf: Vec<u8> = Vec::with_capacity(TAKE);
    loop {
        if idle_out(&shared, &mut last_heard, a.idle) {
            break;
        }
        let Some(mut reader) = Reader::open_live(&a.shm, Some(&a.tag)) else {
            std::thread::sleep(LOOK);
            continue;
        };
        let mut way = Way::new(&reader.shape(), a.cap);
        let announced = Announced::of(&way.announced, way.capped()).json();
        eprintln!(
            "outcast-lite: segment {}: {announced}",
            reader.path().display()
        );
        say(&announced);
        {
            let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
            s.current = Some(announced.clone());
            s.text(&announced);
        }
        let mut bytes: u64 = 0;
        loop {
            if idle_out(&shared, &mut last_heard, a.idle) {
                listeners::shutdown(&listener, acceptor, &shared);
                return;
            }
            buf.clear();
            let took = reader.take(&mut buf, TAKE);
            if took.bytes > 0 {
                bytes += took.bytes as u64;
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                if s.count() > 0 {
                    for piece in way.pieces(&buf) {
                        s.pcm(Arc::new(piece.clone()));
                    }
                }
                continue;
            }
            if !reader.is_live() {
                break;
            }
            std::thread::sleep(NAP);
        }
        eprintln!("outcast-lite: segment ended after {bytes} bytes");
        say(&wire::word("end"));
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        s.current = None;
        s.text(&wire::word("end"));
    }
    listeners::shutdown(&listener, acceptor, &shared);
}

/// The listeners' count said when it changed, and whether the idle time
/// without any has run out.
fn idle_out(shared: &listeners::Registry, last_heard: &mut Instant, idle: u64) -> bool {
    let (count, changed) = {
        let s = shared.lock().unwrap_or_else(|e| e.into_inner());
        (s.count(), s.take_changed())
    };
    if changed {
        say(&format!("{{\"type\":\"listeners\",\"count\":{count}}}"));
    }
    if count > 0 {
        *last_heard = Instant::now();
        return false;
    }
    if idle > 0 && last_heard.elapsed() >= Duration::from_secs(idle) {
        say(&format!(
            "{{\"type\":\"exit\",\"reason\":\"idle\",\"seconds\":{idle}}}"
        ));
        eprintln!("outcast-lite: nothing listened for {idle} s; leaving");
        return true;
    }
    false
}
