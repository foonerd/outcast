//! The programme's level and dead air, from the tap's measurement ring: the
//! peak and RMS per channel in dBFS, and how long the programme has been
//! below the operator's dead-air level. Dead air is counted whether the
//! player plays silence or plays nothing: a listener hears the same either
//! way.
//!
//! The tap publishes a hop of measurements every 1024 frames, faster than
//! anything here looks. A look therefore takes every hop published since the
//! last one and gives them as one, the highest of each, so a peak or an over
//! between two looks is never missed: the meter's ten looks a second and the
//! dashboard's stream of the level both read the ring this way.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tap::ring::{merge_hops, now_ns, Reader, MAX_CHANNELS, PREFIX};

use crate::config::Monitor;
use crate::status::{self, Level, Status};

/// How often the meter looks at the ring.
const SAMPLE: Duration = Duration::from_millis(100);
/// The floor a silent sample is said at, in dBFS.
const FLOOR: f32 = -120.0;
/// A peak at or above this is an over: full scale, within the meter's step.
const OVER_DB: f32 = -0.3;
/// A ring not written for this long has no level: the player stopped.
const QUIET_NS: u64 = 500_000_000;
/// How often a ring is looked for while there is none.
const LOOK: Duration = Duration::from_millis(500);

/// Linear, 1.0 full scale, as dBFS.
pub fn dbfs(linear: f32) -> f32 {
    if linear <= 0.0 {
        FLOOR
    } else {
        (20.0 * linear.log10()).max(FLOOR)
    }
}

/// What a look at the ring found.
#[derive(Debug, PartialEq)]
pub enum Look {
    /// The hops published since the last look as one, the highest of each
    /// reading, and how many of them stood at full scale.
    Hops(Level, u32),
    /// The ring is being written and holds nothing new since the last look.
    Same,
    /// No ring, or one not written for half a second: there is no level.
    Quiet,
}

/// The tag's measurement ring, read so that no hop is missed between looks.
pub struct Hops {
    shm: PathBuf,
    tag: String,
    reader: Option<Reader>,
    /// The sequence of the last hop taken from the ring held.
    last_seq: u64,
    looked: Option<Instant>,
}

impl Hops {
    pub fn new(shm: &Path, tag: &str) -> Self {
        Self {
            shm: shm.to_path_buf(),
            tag: tag.to_string(),
            reader: None,
            last_seq: 0,
            looked: None,
        }
    }

    /// The live measurement ring of the tag held, or found: looked for at
    /// most twice a second while there is none.
    fn hold(&mut self) {
        if self.reader.as_ref().is_some_and(Reader::is_live) {
            return;
        }
        self.reader = None;
        if self.looked.is_some_and(|at| at.elapsed() < LOOK) {
            return;
        }
        self.looked = Some(Instant::now());
        let prefix = format!("{PREFIX}{}.", self.tag);
        self.reader = Reader::find(&self.shm)
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with(&prefix))
                    .unwrap_or(false)
            })
            .find_map(|p| Reader::open(&p).ok().filter(Reader::is_live));
        self.last_seq = 0;
    }

    /// Every hop published since the last look, as one. The first look at a
    /// ring takes its newest hop alone; a reader that stayed away longer
    /// than the ring holds takes what it still holds.
    pub fn look(&mut self) -> Look {
        self.hold();
        let Some(reader) = self.reader.as_ref() else {
            return Look::Quiet;
        };
        let info = reader.info();
        if now_ns().saturating_sub(info.written_ns) > QUIET_NS {
            return Look::Quiet;
        }
        if info.seq == self.last_seq {
            return Look::Same;
        }
        let held = u64::from(info.slots.max(1));
        let first = if self.last_seq == 0 || info.seq < self.last_seq {
            info.seq
        } else {
            (self.last_seq + 1).max(info.seq.saturating_sub(held - 1))
        };
        let channels = (info.channels as usize).clamp(1, MAX_CHANNELS);
        let mut overs = 0u32;
        let merged = merge_hops(
            (first..=info.seq)
                .filter_map(|seq| reader.slot(seq))
                .inspect(|f| {
                    if f.peak[..channels].iter().any(|v| dbfs(*v) >= OVER_DB) {
                        overs += 1;
                    }
                }),
        );
        self.last_seq = info.seq;
        match merged {
            Some(f) => Look::Hops(
                Level {
                    peak_db: f.peak[..channels].iter().map(|v| dbfs(*v)).collect(),
                    rms_db: f.rms[..channels].iter().map(|v| dbfs(*v)).collect(),
                    at: status::now(),
                },
                overs,
            ),
            // Every hop was caught mid-write: the next look has them.
            None => Look::Same,
        }
    }
}

pub struct Meter {
    hops: Hops,
    status: Arc<Mutex<Status>>,
    last_sample: Instant,
    /// The level of the newest look that had hops, kept while the ring is
    /// written: a hop can be longer than a look at a low sample rate.
    level: Option<Level>,
    /// The last moment the programme stood above the dead-air level, or
    /// the start.
    last_sound: Instant,
    dead_air: bool,
    /// When dead air was last said, for how long it stood.
    dead_since: Option<Instant>,
}

impl Meter {
    pub fn new(shm: &Path, tag: &str, status: Arc<Mutex<Status>>) -> Self {
        let now = Instant::now();
        Self {
            hops: Hops::new(shm, tag),
            status,
            last_sample: now - SAMPLE,
            level: None,
            last_sound: now,
            dead_air: false,
            dead_since: None,
        }
    }

    /// Called often; looks at the ring every `SAMPLE` and keeps the status's
    /// level and dead air current.
    pub fn tick(&mut self, monitor: &Monitor) {
        if self.last_sample.elapsed() < SAMPLE {
            return;
        }
        self.last_sample = Instant::now();
        // The hops at full scale among those just taken: overs, counted since the mark.
        let mut overs = 0;
        match self.hops.look() {
            Look::Hops(level, at_full_scale) => {
                self.level = Some(level);
                overs = at_full_scale;
            }
            Look::Same => {}
            Look::Quiet => self.level = None,
        }
        let level = self.level.clone();
        let loud = level
            .as_ref()
            .is_some_and(|l| l.rms_db.iter().any(|v| *v > monitor.dead_air_level));
        if loud {
            self.last_sound = Instant::now();
        }
        let quiet = self.last_sound.elapsed().as_secs();
        let dead_air = monitor.dead_air_seconds > 0 && quiet >= monitor.dead_air_seconds;
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        s.since.overs += overs;
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
        let changed =
            overs > 0 || s.level != level || s.dead_air != dead_air || s.quiet_seconds != quiet;
        s.level = level;
        s.dead_air = dead_air;
        s.quiet_seconds = quiet;
        if changed {
            s.touch();
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use tap::{Frame, Writer};

    /// A folder of this test's own for rings, empty.
    pub fn ring_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("outcast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A measurement ring as the tap writes one: 48 kHz, two channels, `slots` hops kept.
    pub fn ring(dir: &Path, tag: &str, slots: usize) -> Writer {
        Writer::create(dir, tag, 48_000, 2, tap::Demand::default(), 1024, slots).unwrap()
    }

    /// A hop with these linear peaks, the RMS at half of each.
    pub fn hop(left: f32, right: f32) -> Frame {
        Frame {
            peak: [left, right],
            rms: [left / 2.0, right / 2.0],
            ..Frame::default()
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn linear_becomes_dbfs_with_a_floor() {
        assert_eq!(dbfs(1.0), 0.0);
        assert!((dbfs(0.5) + 6.02).abs() < 0.01);
        assert!((dbfs(0.001) + 60.0).abs() < 0.01);
        assert_eq!(dbfs(0.0), FLOOR);
        assert_eq!(dbfs(-1.0), FLOOR);
        assert_eq!(dbfs(1e-9), FLOOR);
    }

    #[test]
    fn a_look_takes_every_hop_since_the_last_as_one() {
        let dir = ring_dir("hops");
        let mut hops = Hops::new(&dir, "outcast");
        assert_eq!(hops.look(), Look::Quiet, "no ring, no level");

        // A ring of another tag is not this meter's.
        let mut other = ring(&dir, "glass", 64);
        other.publish(&hop(1.0, 1.0));
        hops.looked = None;
        assert_eq!(hops.look(), Look::Quiet, "another tag's ring is left alone");

        let mut writer = ring(&dir, "outcast", 64);
        writer.publish(&hop(0.9, 0.9));
        writer.publish(&hop(0.25, 0.125));
        hops.looked = None;
        let Look::Hops(level, overs) = hops.look() else {
            panic!("the first look at a ring has a level");
        };
        assert!(
            close(level.peak_db[0], -12.04) && close(level.peak_db[1], -18.06),
            "the first look takes the newest hop alone: {level:?}"
        );
        assert_eq!(overs, 0);
        assert_eq!(hops.look(), Look::Same, "nothing new, nothing taken twice");

        // Four hops between two looks, the loud one in the middle and two at full scale.
        for (l, r) in [(0.1, 0.1), (1.0, 0.5), (0.99, 0.2), (0.05, 0.05)] {
            writer.publish(&hop(l, r));
        }
        let Look::Hops(level, overs) = hops.look() else {
            panic!("new hops are a level");
        };
        assert_eq!(level.peak_db.len(), 2);
        assert!(
            close(level.peak_db[0], 0.0) && close(level.peak_db[1], -6.02),
            "the highest peak of the four, per channel: {level:?}"
        );
        assert!(
            close(level.rms_db[0], -6.02) && close(level.rms_db[1], -12.04),
            "the highest RMS of the four, per channel: {level:?}"
        );
        assert_eq!(overs, 2, "each hop at full scale is an over");
        assert_eq!(hops.look(), Look::Same);

        // The player stops: the ring is there and no longer written.
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(hops.look(), Look::Quiet, "a ring gone quiet has no level");
        writer.publish(&hop(0.5, 0.5));
        assert!(
            matches!(hops.look(), Look::Hops(_, 0)),
            "and has one again with the next hop"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reader_away_longer_than_the_ring_holds_takes_what_it_holds() {
        let dir = ring_dir("wrap");
        let mut writer = ring(&dir, "outcast", 4);
        let mut hops = Hops::new(&dir, "outcast");
        writer.publish(&hop(0.1, 0.1));
        assert!(matches!(hops.look(), Look::Hops(_, 0)));
        // Ten hops into a ring of four: the over is long gone, the last four are held.
        writer.publish(&hop(1.0, 1.0));
        for _ in 0..5 {
            writer.publish(&hop(0.1, 0.1));
        }
        for peak in [0.2, 0.5, 0.3, 0.25] {
            writer.publish(&hop(peak, peak));
        }
        let Look::Hops(level, overs) = hops.look() else {
            panic!("the hops still held are a level");
        };
        assert!(
            close(level.peak_db[0], -6.02),
            "the highest of the four held: {level:?}"
        );
        assert_eq!(overs, 0, "a hop the ring no longer holds is not read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_meter_counts_every_over_and_drops_the_level_when_the_ring_goes_quiet() {
        let dir = ring_dir("meter");
        let status = Arc::new(Mutex::new(Status::new("outcast")));
        let mut writer = ring(&dir, "outcast", 64);
        writer.publish(&hop(0.5, 0.5));
        let mut meter = Meter::new(&dir, "outcast", Arc::clone(&status));
        let monitor = Monitor::default();
        meter.tick(&monitor);
        assert!(status.lock().unwrap().level.is_some());
        for peak in [1.0, 0.2, 1.0, 1.0, 0.3] {
            writer.publish(&hop(peak, peak));
        }
        std::thread::sleep(SAMPLE);
        meter.tick(&monitor);
        {
            let s = status.lock().unwrap();
            assert_eq!(s.since.overs, 3, "three hops at full scale in one look");
            let level = s.level.as_ref().expect("a level");
            assert!(close(level.peak_db[0], 0.0), "{level:?}");
        }
        // Nothing new within a look: the level stands while the ring is written.
        std::thread::sleep(SAMPLE);
        meter.tick(&monitor);
        assert!(status.lock().unwrap().level.is_some());
        std::thread::sleep(Duration::from_millis(600));
        meter.tick(&monitor);
        assert!(
            status.lock().unwrap().level.is_none(),
            "the player stopped: no level"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
