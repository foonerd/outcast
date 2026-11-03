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

use crate::config::Uplinks;
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
}

/// One uplink's worker thread and the two ways in: pieces of the stream,
/// never waited on, and the rare events, always delivered.
struct Worker {
    id: String,
    pcm: SyncSender<Arc<Vec<u8>>>,
    ctl: Sender<Event>,
    handle: Option<JoinHandle<()>>,
}

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
        }
    }

    /// The configuration read again and the uplinks made anew from it: the
    /// old workers told to stop and joined, the new ones started, and the
    /// current segment and title given to them so a save in the middle of a
    /// stream is a short gap and not a silence.
    pub fn reload(&mut self) -> Result<(), String> {
        let uplinks = Uplinks::load(&self.config)?;
        self.stop_all();
        // The entries are in the status before any worker runs, since a worker
        // says its state at once and would be written over.
        let mut listed = Vec::new();
        let icecast = uplinks.icecast.enabled.then(|| {
            let cfg = uplinks.icecast.clone();
            let ctx = self.ctx("icecast", "icecast");
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
            (cfg, ctx)
        });
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
        {
            let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
            s.uplinks = listed;
            s.touch();
        }
        if let Some((cfg, ctx)) = icecast {
            self.start("icecast", move |ctl, pcm| {
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
        if !self.title.is_empty() {
            self.broadcast(Event::Title(self.title.clone()));
        }
        if let Some((segment, _)) = self.segment {
            self.broadcast(Event::Segment(segment));
        }
        eprintln!("outcast: uplinks: {}", self.names());
        Ok(())
    }

    fn ctx(&self, id: &str, kind: &str) -> Ctx {
        Ctx {
            id: id.to_string(),
            kind: kind.to_string(),
            evopeg: self.evopeg.clone(),
            status: Arc::clone(&self.status),
        }
    }

    fn start<F>(&mut self, id: &str, body: F)
    where
        F: FnOnce(Receiver<Event>, Receiver<Arc<Vec<u8>>>) + Send + 'static,
    {
        let (ctl_tx, ctl_rx) = mpsc::channel();
        let (pcm_tx, pcm_rx) = mpsc::sync_channel(QUEUE);
        let handle = std::thread::Builder::new()
            .name(format!("uplink-{id}"))
            .spawn(move || body(ctl_rx, pcm_rx))
            .expect("uplink thread");
        self.workers.push(Worker {
            id: id.to_string(),
            pcm: pcm_tx,
            ctl: ctl_tx,
            handle: Some(handle),
        });
    }

    fn stop_all(&mut self) {
        for w in &self.workers {
            let _ = w.ctl.send(Event::Stop);
        }
        for mut w in self.workers.drain(..) {
            if let Some(h) = w.handle.take() {
                let _ = h.join();
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
            id: self.id.clone(),
            kind: self.kind.clone(),
            state: "starting".into(),
            detail: detail.to_string(),
            since: status::now(),
            ..status::Uplink::default()
        }
    }
}
