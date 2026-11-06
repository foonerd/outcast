//! The uplinks: each a worker thread fed by the hub, owning its encoder and
//! its transport. What they share is here: the status entry they keep, the
//! encoder they run per segment, the way they wait on their two channels.

pub mod icecast;
pub mod lan;
pub mod record;
pub mod srt;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::status::{self, Status};
use stream::feed::{Event, Segment};

/// What every uplink worker is given.
#[derive(Clone)]
pub struct Ctx {
    pub id: String,
    pub kind: String,
    pub evopeg: PathBuf,
    pub status: Arc<Mutex<Status>>,
}

impl Ctx {
    /// The uplink's own status entry changed in place.
    pub fn update(&self, f: impl FnOnce(&mut status::Uplink)) {
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(u) = s.uplinks.iter_mut().find(|u| u.id == self.id) {
            f(u);
            s.touch();
        }
    }

    /// The state word and, where there is one, the error that put it there.
    pub fn state(&self, state: &str, error: Option<&str>) {
        let state = state.to_string();
        let error = error.map(str::to_string);
        self.update(|u| {
            if u.state != state {
                u.state = state;
                u.since = status::now();
            }
            if let Some(e) = error {
                u.last_error = e;
            }
        });
    }

    pub fn log(&self, line: &str) {
        eprintln!("outcast: {}: {line}", self.id);
    }
}

/// The next thing a worker should act on: an event, a piece of the stream,
/// or nothing within `wait`. Events come first, since they are rare and
/// change what the pieces mean.
pub enum Next {
    Event(Event),
    Pcm(Arc<Vec<u8>>),
    Nothing,
    Gone,
}

pub fn next(ctl: &Receiver<Event>, pcm: &Receiver<Arc<Vec<u8>>>, wait: Duration) -> Next {
    match ctl.try_recv() {
        Ok(e) => return Next::Event(e),
        Err(TryRecvError::Disconnected) => return Next::Gone,
        Err(TryRecvError::Empty) => {}
    }
    match pcm.recv_timeout(wait) {
        Ok(p) => Next::Pcm(p),
        Err(RecvTimeoutError::Timeout) => Next::Nothing,
        Err(RecvTimeoutError::Disconnected) => Next::Gone,
    }
}

/// The time an encoder gets to write what it holds when its input closes.
pub const GRACE: Duration = Duration::from_secs(5);

/// A live ring that brings no bytes for this long is a paused player: the
/// uplink that keeps its stream alive fills with silence from here on.
pub const STILL: Duration = Duration::from_millis(300);

/// Silence at a shape, paced by the clock: as many zero frames as the time
/// since the last call is worth, so an encoder fed from here runs at the
/// stream's own pace while the player stands still.
pub struct Silence {
    rate: u32,
    frame_bytes: usize,
    last: Instant,
    zeros: Vec<u8>,
}

impl Silence {
    pub fn new(shape: &Segment) -> Self {
        Self {
            rate: shape.rate.max(1),
            frame_bytes: shape.frame_bytes.max(1) as usize,
            last: Instant::now(),
            zeros: Vec::new(),
        }
    }

    /// The zeros due since the last call, in whole frames, when at least
    /// 20 ms of them are due; never more than a second's worth, so a thread
    /// held up does not pour a backlog into the encoder.
    pub fn due(&mut self) -> Option<&[u8]> {
        let elapsed = self.last.elapsed().as_secs_f64();
        let frames = (elapsed * f64::from(self.rate)) as usize;
        if frames < self.rate as usize / 50 {
            return None;
        }
        let frames = frames.min(self.rate as usize);
        self.last += Duration::from_secs_f64(frames as f64 / f64::from(self.rate));
        self.zeros.clear();
        self.zeros.resize(frames * self.frame_bytes, 0);
        Some(&self.zeros)
    }
}

/// A value fit for one header line: every run of line breaks is one space,
/// so nothing a user typed can end the line and start another header.
pub fn clean_line(s: &str) -> String {
    s.split(['\r', '\n'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
