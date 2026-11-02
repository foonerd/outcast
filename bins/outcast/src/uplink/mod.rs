//! The uplinks: each a worker thread fed by the hub, owning its encoder and
//! its transport. What they share is here: the status entry they keep, the
//! encoder they run per segment, the way they wait on their two channels.

pub mod icecast;
pub mod lan;
pub mod record;

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::feed::Event;
use crate::status::{self, Status};

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

/// A value fit for one header line: every run of line breaks is one space,
/// so nothing a user typed can end the line and start another header.
pub fn clean_line(s: &str) -> String {
    s.split(['\r', '\n'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
