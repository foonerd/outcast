//! The hub: between the ring and the uplinks. It turns each ring into a
//! segment the encoders take, hands every piece of the stream to every
//! uplink without waiting for any of them (an uplink that falls behind
//! loses the piece and the loss is counted), carries the title, and starts
//! and stops the uplink workers as the configuration says.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tap::audio::Shape;

use crate::config::{Monitor, Uplinks};
use crate::status::{self, Status};
use crate::uplink::{self, Ctx};
use stream::feed::{self, Conversion, Event, Segment};

/// Pieces of the stream an uplink may hold unread before it loses the next.
const QUEUE: usize = 64;

/// What the control socket asks the hub.
#[derive(Debug)]
pub enum Command {
    Title(String),
    Reload,
    /// An insert into the stream: a WAV file by path, its name for the page,
    /// the programme's level under it in dB.
    Insert {
        name: String,
        file: String,
        duck_db: f32,
    },
}

/// One uplink's worker thread and the two ways in: pieces of the stream,
/// never waited on, and the rare events, always delivered.
struct Worker {
    id: String,
    pcm: SyncSender<Arc<Vec<u8>>>,
    ctl: Sender<Event>,
    handle: Option<JoinHandle<()>>,
    /// Told when the worker's body returned, so a stop can wait with a bound.
    done: Receiver<()>,
}

/// How long a worker may take to stop before the hub goes on without it: a
/// worker that does not answer is a bug, and the bug must never hold the core.
const STOP_PATIENCE: Duration = Duration::from_secs(10);

/// How long a title is held before the uplinks hear it: the player pushes a
/// state with the previous track's title for a moment when a queue changes,
/// and a title that lasts less than this was never the screen's.
const TITLE_HOLD: Duration = Duration::from_secs(2);

pub struct Hub {
    status: Arc<Mutex<Status>>,
    evopeg: PathBuf,
    config: PathBuf,
    workers: Vec<Worker>,
    title: String,
    /// A title just given, and when; told to the uplinks once it has stood.
    pending_title: Option<(String, Instant)>,
    segment: Option<(Segment, Conversion)>,
    converted: Vec<u8>,
    /// What the status watches the programme for, from the last load.
    monitor: Monitor,
    /// Counted at every reload; the workers and the entries of one reload share it.
    generation: u64,
    /// The core's own page, while the settings want it.
    dashboard: Option<crate::dashboard::Server>,
    /// Inserts into the stream, mixed over the programme before the fan-out.
    mixer: crate::mixer::Mixer,
}

impl Hub {
    pub fn new(status: Arc<Mutex<Status>>, evopeg: &Path, config: &Path) -> Self {
        Self {
            status,
            evopeg: evopeg.to_path_buf(),
            config: config.to_path_buf(),
            workers: Vec::new(),
            title: String::new(),
            pending_title: None,
            segment: None,
            converted: Vec::new(),
            monitor: Monitor::default(),
            generation: 0,
            dashboard: None,
            mixer: crate::mixer::Mixer::new(),
        }
    }

    pub fn monitor(&self) -> Monitor {
        self.monitor
    }

    /// The configuration read again and the uplinks made anew from it: the
    /// old workers told to stop and joined, the new ones started, and the
    /// current segment and title given to them so a save in the middle of a
    /// stream is a short gap and not a silence.
    pub fn reload(&mut self) -> Result<(), String> {
        let uplinks = Uplinks::load(&self.config)?;
        self.monitor = uplinks.monitor;
        self.dashboard(uplinks.dashboard);
        self.stop_all();
        self.generation += 1;
        // The entries are in the status before any worker runs, since a worker
        // says its state at once and would be written over.
        let mut listed = Vec::new();
        // The Icecast slots: `icecast`, `icecast2`, `icecast3`, each enabled one its
        // own worker, encoder and connection.
        let icecasts: Vec<_> = uplinks
            .slots()
            .into_iter()
            .enumerate()
            .filter(|(_, cfg)| cfg.enabled)
            .map(|(n, cfg)| {
                let id = if n == 0 {
                    "icecast".to_string()
                } else {
                    format!("icecast{}", n + 1)
                };
                let cfg = cfg.clone();
                let ctx = self.ctx(&id, "icecast");
                let detail = match cfg.protocol {
                    crate::config::Protocol::Icecast2 => {
                        format!("{}:{}{}", cfg.host, cfg.port, cfg.mount)
                    }
                    crate::config::Protocol::Shoutcast1 => {
                        format!(
                            "{}:{} SHOUTcast v1, stream {}",
                            cfg.host, cfg.port, cfg.stream_id
                        )
                    }
                };
                listed.push(ctx.entry(&detail));
                (id, cfg, ctx)
            })
            .collect();
        let lan = uplinks.lan.enabled.then(|| {
            let cfg = uplinks.lan.clone();
            let ctx = self.ctx("lan", "lan");
            listed.push(ctx.entry(&format!("port {}", cfg.port)));
            (cfg, ctx)
        });
        let record = uplinks.record.enabled.then(|| {
            let cfg = uplinks.record.clone();
            let ctx = self.ctx("record", "record");
            listed.push(ctx.entry(&cfg.folder.clone()));
            (cfg, ctx)
        });
        let srt = uplinks.srt.enabled.then(|| {
            let cfg = uplinks.srt.clone();
            let ctx = self.ctx("srt", "srt");
            listed.push(ctx.entry(&format!("srt://{}:{}", cfg.host, cfg.port)));
            (cfg, ctx)
        });
        let rtmp = uplinks.rtmp.enabled.then(|| {
            let cfg = uplinks.rtmp.clone();
            let ctx = self.ctx("rtmp", "rtmp");
            listed.push(ctx.entry(&cfg.url.clone()));
            (cfg, ctx)
        });
        let ts = uplinks.ts.enabled.then(|| {
            let cfg = uplinks.ts.clone();
            let ctx = self.ctx("ts", "ts");
            let scheme = match cfg.transport {
                crate::config::Transport::Tcp => "tcp",
                crate::config::Transport::Udp => "udp",
            };
            listed.push(ctx.entry(&format!("{scheme}://{}:{}", cfg.host, cfg.port)));
            (cfg, ctx)
        });
        let hls = uplinks.hls.enabled.then(|| {
            let cfg = uplinks.hls.clone();
            let ctx = self.ctx("hls", "hls");
            let origin = match cfg.origin {
                crate::config::OriginKind::None => String::new(),
                crate::config::OriginKind::Http => cfg.origin_url.clone(),
                crate::config::OriginKind::S3 => {
                    format!(
                        "{}/{}",
                        cfg.s3_endpoint.trim_end_matches('/'),
                        cfg.s3_bucket
                    )
                }
            };
            let detail = match (cfg.port, origin.is_empty()) {
                (0, _) => origin,
                (p, true) => format!("port {p}"),
                (p, false) => format!("port {p}, {origin}"),
            };
            listed.push(ctx.entry(&detail));
            (cfg, ctx)
        });
        {
            let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
            s.uplinks = listed;
            s.touch();
        }
        for (id, cfg, ctx) in icecasts {
            self.start(&id, move |ctl, pcm| {
                uplink::icecast::run(cfg, ctx, ctl, pcm)
            });
        }
        if let Some((cfg, ctx)) = lan {
            self.start("lan", move |ctl, pcm| uplink::lan::run(cfg, ctx, ctl, pcm));
        }
        if let Some((cfg, ctx)) = record {
            self.start("record", move |ctl, pcm| {
                uplink::record::run(cfg, ctx, ctl, pcm)
            });
        }
        if let Some((cfg, ctx)) = srt {
            self.start("srt", move |ctl, pcm| uplink::srt::run(cfg, ctx, ctl, pcm));
        }
        if let Some((cfg, ctx)) = rtmp {
            self.start("rtmp", move |ctl, pcm| {
                uplink::rtmp::run(cfg, ctx, ctl, pcm)
            });
        }
        if let Some((cfg, ctx)) = ts {
            self.start("ts", move |ctl, pcm| uplink::ts::run(cfg, ctx, ctl, pcm));
        }
        if let Some((cfg, ctx)) = hls {
            self.start("hls", move |ctl, pcm| uplink::hls::run(cfg, ctx, ctl, pcm));
        }
        if !self.title.is_empty() {
            self.broadcast(Event::Title(self.title.clone()));
        }
        if let Some((segment, _)) = self.segment {
            self.broadcast(Event::Segment(segment));
        }
        eprintln!("outcast: uplinks: {}", self.names());
        {
            let names = self.names();
            let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
            s.dead_air_level = self.monitor.dead_air_level;
            s.event("info", format!("uplinks: {names}"));
        }
        Ok(())
    }

    /// The dashboard as the settings want it: started, moved to another port, or
    /// stopped; left alone when nothing changed.
    fn dashboard(&mut self, wanted: crate::config::Dashboard) {
        let have = self.dashboard.as_ref().map(|s| s.port);
        let want = wanted.enabled.then_some(wanted.port);
        if have == want {
            return;
        }
        if let Some(s) = self.dashboard.take() {
            s.stop();
        }
        if let Some(port) = want {
            match crate::dashboard::Server::start(port, Arc::clone(&self.status)) {
                Ok(s) => {
                    eprintln!("outcast: dashboard on port {port}");
                    self.dashboard = Some(s);
                }
                Err(e) => {
                    // The page's status says why there is no dashboard: the port taken,
                    // most often; the uplinks go on without it.
                    eprintln!("outcast: dashboard: {e}");
                    let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
                    s.last_error = format!("dashboard: {e}");
                    s.touch();
                }
            }
        }
    }

    fn ctx(&self, id: &str, kind: &str) -> Ctx {
        Ctx {
            id: id.to_string(),
            kind: kind.to_string(),
            evopeg: self.evopeg.clone(),
            status: Arc::clone(&self.status),
            generation: self.generation,
        }
    }

    fn start<F>(&mut self, id: &str, body: F)
    where
        F: FnOnce(Receiver<Event>, Receiver<Arc<Vec<u8>>>) + Send + 'static,
    {
        let (ctl_tx, ctl_rx) = mpsc::channel();
        let (pcm_tx, pcm_rx) = mpsc::sync_channel(QUEUE);
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let handle = std::thread::Builder::new()
            .name(format!("uplink-{id}"))
            .spawn(move || {
                body(ctl_rx, pcm_rx);
                let _ = done_tx.send(());
            })
            .expect("uplink thread");
        self.workers.push(Worker {
            id: id.to_string(),
            pcm: pcm_tx,
            ctl: ctl_tx,
            handle: Some(handle),
            done: done_rx,
        });
    }

    fn stop_all(&mut self) {
        for w in &self.workers {
            let _ = w.ctl.send(Event::Stop);
        }
        let deadline = Instant::now() + STOP_PATIENCE;
        for mut w in self.workers.drain(..) {
            let left = deadline.saturating_duration_since(Instant::now());
            match w.done.recv_timeout(left) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(h) = w.handle.take() {
                        let _ = h.join();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Left behind, not waited for: its entry is gone with the reload
                    // and its words no longer land (the generation), so the core goes on.
                    eprintln!(
                        "outcast: {}: did not stop within {} s; left behind",
                        w.id,
                        STOP_PATIENCE.as_secs()
                    );
                    drop(w.handle.take());
                }
            }
        }
    }

    pub fn names(&self) -> String {
        if self.workers.is_empty() {
            "none".to_string()
        } else {
            self.workers
                .iter()
                .map(|w| w.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }

    fn broadcast(&self, event: Event) {
        for w in &self.workers {
            let _ = w.ctl.send(event.clone());
        }
    }

    /// A new ring: the segment planned and announced.
    pub fn segment(&mut self, shape: &Shape) {
        let planned = feed::plan(shape);
        self.segment = Some(planned);
        self.broadcast(Event::Segment(planned.0));
    }

    /// Bytes of the current ring, converted once and given to every uplink
    /// that has room; the others lose this piece.
    pub fn pcm(&mut self, bytes: &[u8]) {
        let Some((segment, conversion)) = self.segment.as_ref() else {
            return;
        };
        feed::convert(conversion, segment, bytes, &mut self.converted);
        if self.converted.is_empty() {
            return;
        }
        // An insert over the programme, when one plays or waits; a plain pass otherwise.
        if self.mixer.playing().is_some() || self.mixer.waiting() > 0 {
            let segment = *segment;
            let moments = self.mixer.process(&segment, &mut self.converted);
            if !moments.is_empty() {
                let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
                for m in moments {
                    match m {
                        crate::mixer::Moment::Started(name) => {
                            eprintln!("outcast: insert: {name}");
                            s.insert = name.clone();
                            s.since.inserts += 1;
                            s.event("info", format!("insert: {name}"));
                        }
                        crate::mixer::Moment::Ended(_) => {
                            s.insert.clear();
                            s.touch();
                        }
                        crate::mixer::Moment::Skipped(name, why) => {
                            eprintln!("outcast: insert {name} skipped: {why}");
                            s.insert.clear();
                            s.event("err", format!("insert {name} skipped: {why}"));
                        }
                    }
                }
            }
        }
        let piece = Arc::new(std::mem::take(&mut self.converted));
        let mut dropped: Vec<&str> = Vec::new();
        for w in &self.workers {
            match w.pcm.try_send(Arc::clone(&piece)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    dropped.push(&w.id)
                }
            }
        }
        if !dropped.is_empty() {
            let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
            for id in dropped {
                if let Some(u) = s.uplinks.iter_mut().find(|u| u.id == id) {
                    u.dropped += 1;
                }
            }
            s.touch();
        }
    }

    pub fn end(&mut self) {
        self.segment = None;
        self.broadcast(Event::End);
    }

    /// A title from the player: held until it has stood for `TITLE_HOLD`.
    pub fn title(&mut self, title: String) {
        if self.title == title {
            self.pending_title = None;
            return;
        }
        self.pending_title = Some((title, Instant::now()));
    }

    /// A held title that has stood long enough, told to the uplinks. Called
    /// often, between looks at the ring.
    pub fn tick(&mut self) {
        let due = matches!(&self.pending_title, Some((_, at)) if at.elapsed() >= TITLE_HOLD);
        if due {
            if let Some((title, _)) = self.pending_title.take() {
                self.title = title.clone();
                self.status
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .played_title(&title);
                self.broadcast(Event::Title(title));
            }
        }
    }

    /// A command from the control socket.
    pub fn command(&mut self, command: Command) {
        match command {
            Command::Title(t) => self.title(t),
            Command::Reload => {
                if let Err(e) = self.reload() {
                    eprintln!("outcast: reload: {e}");
                    let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
                    s.last_error = e;
                    s.touch();
                }
            }
            Command::Insert {
                name,
                file,
                duck_db,
            } => match crate::mixer::load_wav(Path::new(&file)) {
                Ok(clip) => {
                    eprintln!(
                        "outcast: insert queued: {name} ({} s at {} Hz, {} ch)",
                        clip.frames() / clip.rate.max(1) as usize,
                        clip.rate,
                        clip.channels
                    );
                    self.mixer.insert(crate::mixer::Insert {
                        name,
                        clip: Arc::new(clip),
                        duck_db,
                    });
                }
                Err(e) => {
                    eprintln!("outcast: insert {name}: {file}: {e}");
                    let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
                    s.event("err", format!("insert {name}: {e}"));
                }
            },
        }
    }

    /// Everything down, for the daemon's end.
    pub fn shutdown(&mut self) {
        self.stop_all();
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// A status entry made for an uplink, before its worker says more.
impl Ctx {
    pub fn entry(&self, detail: &str) -> status::Uplink {
        status::Uplink {
            generation: self.generation,
            id: self.id.clone(),
            kind: self.kind.clone(),
            state: "starting".into(),
            detail: detail.to_string(),
            since: status::now(),
            ..status::Uplink::default()
        }
    }
}
