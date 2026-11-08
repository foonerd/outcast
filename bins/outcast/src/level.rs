//! The programme's level and dead air, from the tap's measurement ring: the
//! peak and RMS per channel of the newest hop, in dBFS, and how long the
//! programme has been below the operator's dead-air level. Dead air is
//! counted whether the player plays silence or plays nothing: a listener
//! hears the same either way.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tap::ring::{Reader, PREFIX};

use crate::config::Monitor;
use crate::status::{self, Level, Status};

/// How often the ring is read; the hop is shorter, and the page reads once a second.
const SAMPLE: Duration = Duration::from_millis(100);
/// The floor a silent sample is said at, in dBFS.
const FLOOR: f32 = -120.0;
/// A peak at or above this is an over: full scale, within the meter's step.
const OVER_DB: f32 = -0.3;

pub struct Meter {
    shm: PathBuf,
    tag: String,
    status: Arc<Mutex<Status>>,
    reader: Option<Reader>,
    last_sample: Instant,
    /// The last moment the programme stood above the dead-air level, or
    /// the start.
    last_sound: Instant,
    dead_air: bool,
    /// When dead air was last said, for how long it stood.
    dead_since: Option<Instant>,
}

/// Linear, 1.0 full scale, as dBFS.
pub fn dbfs(linear: f32) -> f32 {
    if linear <= 0.0 {
        FLOOR
    } else {
        (20.0 * linear.log10()).max(FLOOR)
    }
}

impl Meter {
    pub fn new(shm: &Path, tag: &str, status: Arc<Mutex<Status>>) -> Self {
        let now = Instant::now();
        Self {
            shm: shm.to_path_buf(),
            tag: tag.to_string(),
            status,
            reader: None,
            last_sample: now - SAMPLE,
            last_sound: now,
            dead_air: false,
            dead_since: None,
        }
    }

    /// The live measurement ring of the tag, found when there is none held.
    fn reader(&mut self) -> Option<&Reader> {
        if self.reader.as_ref().is_some_and(Reader::is_live) {
            return self.reader.as_ref();
        }
        let prefix = format!("{PREFIX}{}.", self.tag);
        self.reader = Reader::find(&self.shm)
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with(&prefix))
                    .unwrap_or(false)
            })
            .find_map(|p| Reader::open(&p).ok().filter(Reader::is_live));
        self.reader.as_ref()
    }

    /// Called often; reads the ring every `SAMPLE` and keeps the status's
    /// level and dead air current.
    pub fn tick(&mut self, monitor: &Monitor) {
        if self.last_sample.elapsed() < SAMPLE {
            return;
        }
        self.last_sample = Instant::now();
        let level = self.reader().and_then(Reader::latest).map(|f| {
            let channels = self
                .reader
                .as_ref()
                .map_or(2, |r| r.info().channels as usize);
            let n = channels.clamp(1, f.peak.len());
            Level {
                peak_db: f.peak[..n].iter().map(|v| dbfs(*v)).collect(),
                rms_db: f.rms[..n].iter().map(|v| dbfs(*v)).collect(),
                at: status::now(),
            }
        });
        let loud = level
            .as_ref()
            .is_some_and(|l| l.rms_db.iter().any(|v| *v > monitor.dead_air_level));
        if loud {
            self.last_sound = Instant::now();
        }
        let quiet = self.last_sound.elapsed().as_secs();
        let dead_air = monitor.dead_air_seconds > 0 && quiet >= monitor.dead_air_seconds;
        // A hop at full scale: an over, counted since the mark.
        let over = level
            .as_ref()
            .is_some_and(|l| l.peak_db.iter().any(|v| *v >= OVER_DB));
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if over {
            s.since.overs += 1;
        }
        if dead_air != self.dead_air {
            self.dead_air = dead_air;
            if dead_air {
                eprintln!(
                    "outcast: dead air: below {} dBFS for {} s",
                    monitor.dead_air_level, monitor.dead_air_seconds
                );
                self.dead_since = Some(Instant::now());
                s.since.dead_air_events += 1;
                s.since.dead_air_last = status::now();
                s.event(
                    "dead",
                    format!(
                        "dead air: below {} dBFS for {} s",
                        monitor.dead_air_level, monitor.dead_air_seconds
                    ),
                );
            } else {
                // The silence lasted the threshold and then the time dead air stood.
                let stood = self.dead_since.take().map_or(0, |t| t.elapsed().as_secs());
                let silent = monitor.dead_air_seconds + stood;
                eprintln!("outcast: dead air over after {silent} s");
                s.since.dead_air_seconds += silent;
                s.event("ok", format!("on air again after {silent} s of silence"));
            }
        }
        let changed = s.level != level || s.dead_air != dead_air || s.quiet_seconds != quiet;
        s.level = level;
        s.dead_air = dead_air;
        s.quiet_seconds = quiet;
        if changed {
            s.touch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_becomes_dbfs_with_a_floor() {
        assert_eq!(dbfs(1.0), 0.0);
        assert!((dbfs(0.5) + 6.02).abs() < 0.01);
        assert!((dbfs(0.001) + 60.0).abs() < 0.01);
        assert_eq!(dbfs(0.0), FLOOR);
        assert_eq!(dbfs(-1.0), FLOOR);
        assert_eq!(dbfs(1e-9), FLOOR);
    }
}
