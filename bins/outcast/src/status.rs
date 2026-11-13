//! What the daemon knows, as the plugin's page and the control socket see
//! it: the ring being followed, the segments and bytes so far, the title
//! the player last gave, and each uplink's state.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// The ring being followed, as its header says.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Ring {
    pub path: String,
    pub writer_pid: u32,
    pub rate: u32,
    pub channels: u32,
    /// ALSA's name for the format, as `S32_LE`.
    pub format: String,
    /// FFmpeg's name for the raw bytes, where it has one.
    pub ffmpeg: Option<String>,
    pub frame_bytes: u32,
    pub one_bit: bool,
    /// Bytes the tap dropped on the audio thread (a transfer longer than the ring).
    pub dropped: u64,
    /// Times the tap made the ring again after finding it gone.
    pub healed: u32,
}

/// The whole of it.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub version: &'static str,
    pub pid: u32,
    pub tag: String,
    /// Seconds since the epoch when the daemon started.
    pub started: u64,
    /// Seconds since the epoch of the last change.
    pub updated: u64,
    /// The ring being followed; none when nothing is playing.
    pub ring: Option<Ring>,
    /// Segments followed so far: one per ring, one ring per shape.
    pub segments: u64,
    /// Bytes of the stream taken from the rings so far.
    pub bytes: u64,
    /// Bytes of the stream lost to this reader (it fell behind the tap).
    pub lost: u64,
    /// The title the player last gave, what its screen shows.
    pub title: String,
    /// The last thing that went wrong outside an uplink: a configuration
    /// that did not read, for one.
    pub last_error: String,
    /// The uplinks, each with its own state.
    pub uplinks: Vec<Uplink>,
    /// The programme's level from the tap's measurement ring; none when the
    /// tap measures nothing.
    pub level: Option<Level>,
    /// The programme has been below the dead-air level for the dead-air time.
    pub dead_air: bool,
    /// Seconds since the programme last stood above the dead-air level.
    pub quiet_seconds: u64,
    /// The machine under the uplinks, read once a second; none until the
    /// second reading.
    pub system: Option<crate::system::System>,
    /// The dead-air floor as set, in dBFS, for the page's meter.
    pub dead_air_level: f32,
    /// Counts, peaks and the last events since a mark: the core's start, or
    /// the dashboard's Clear.
    pub since: Since,
    /// The titles the player showed, newest last, twenty at most.
    pub played: VecDeque<Played>,
    /// DJ mode as the plugin reports it: the block in force, the next, whether
    /// it stands back for the presenter. Null when the plugin has said nothing.
    pub dj: serde_json::Value,
    /// The insert playing into the stream now, by name; empty when none.
    pub insert: String,
}

/// Events kept and titles kept.
const KEPT: usize = 20;

/// One thing that happened: dead air, an uplink lost or back, an error, a
/// mark. `kind` is `dead`, `err`, `ok` or `info`, as the page colours them.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Event {
    pub at: u64,
    pub kind: String,
    pub text: String,
}

/// A title the player showed, and when it began.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Played {
    pub at: u64,
    pub title: String,
}

/// What happened since the mark.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Since {
    /// Seconds since the epoch of the mark.
    pub at: u64,
    pub dead_air_events: u32,
    /// Seconds the programme stood silent in those events, in all.
    pub dead_air_seconds: u64,
    pub dead_air_last: u64,
    pub reconnects: u32,
    pub reconnects_by: BTreeMap<String, u32>,
    pub errors: u32,
    pub error_last: String,
    pub error_last_at: u64,
    pub cpu_peak: f32,
    pub outcast_peak: f32,
    pub temp_peak: Option<f32>,
    pub listeners_peak: u32,
    /// Bytes the uplinks sent since the mark, together.
    pub bytes: u64,
    pub uplinks_kbps_avg: f32,
    pub lost: u64,
    pub dropped: u64,
    /// Meter hops at full scale.
    pub overs: u32,
    /// Inserts played into the stream.
    pub inserts: u32,
    pub events: VecDeque<Event>,
}

impl Since {
    pub fn new(at: u64) -> Self {
        Self {
            at,
            ..Self::default()
        }
    }

    pub fn event(&mut self, kind: &str, text: String) {
        self.events.push_back(Event {
            at: now(),
            kind: kind.to_string(),
            text,
        });
        while self.events.len() > KEPT {
            self.events.pop_front();
        }
    }
}

impl Status {
    /// Something that happened, for the page's list.
    pub fn event(&mut self, kind: &str, text: String) {
        self.since.event(kind, text);
        self.touch();
    }

    /// The counts started again from now, at the dashboard's word.
    pub fn clear_since(&mut self) {
        self.since = Since::new(now());
        self.event("info", "counts cleared".into());
    }

    /// A title the player showed, kept when it is new.
    pub fn played_title(&mut self, title: &str) {
        if title.is_empty() || self.played.back().is_some_and(|p| p.title == title) {
            return;
        }
        self.played.push_back(Played {
            at: now(),
            title: title.to_string(),
        });
        while self.played.len() > KEPT {
            self.played.pop_front();
        }
        self.touch();
    }
}

/// The level per channel, in dBFS: the highest of the hops since the meter last looked.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Level {
    pub peak_db: Vec<f32>,
    pub rms_db: Vec<f32>,
    /// Seconds since the epoch of the reading.
    pub at: u64,
}

/// One uplink's state, as the page shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Uplink {
    /// The reload that made the entry: a worker of an earlier one, still ending,
    /// may not write over it.
    #[serde(skip)]
    pub generation: u64,
    pub id: String,
    pub kind: String,
    /// `starting`, `idle`, `connecting`, `waiting`, `connected`, `listening`,
    /// `streaming`, `recording`, `error`, `off`.
    pub state: String,
    /// Where it goes: the mount, the port, the file.
    pub detail: String,
    /// Seconds since the epoch when the state last changed.
    pub since: u64,
    /// Encoded bytes sent or written.
    pub bytes: u64,
    /// Connections made again after the first.
    pub reconnects: u32,
    /// Pieces of the stream this uplink lost for not keeping up.
    pub dropped: u64,
    pub last_error: String,
    /// Listeners on a LAN stream.
    pub listeners: u32,
    /// Files a recording wrote.
    pub files: u64,
    /// The title it carries now.
    pub title: String,
    /// The stream is silence of the uplink's making: the player stands still.
    pub silence: bool,
    /// An SRT link's round trip, in milliseconds.
    pub rtt_ms: u32,
    /// Packets an SRT link lost on the way, as the peer reported them.
    pub lost_packets: u64,
    /// Segments an HLS uplink cut.
    pub segments: u64,
    /// Segments cut and not yet on the origin.
    pub pending: u64,
    /// This uplink's encoder, percent of all the player's cores, last second.
    pub cpu_pct: f32,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Status {
    pub fn new(tag: &str) -> Self {
        let started = now();
        Self {
            version: env!("CARGO_PKG_VERSION"),
            pid: std::process::id(),
            tag: tag.to_string(),
            started,
            updated: started,
            ring: None,
            segments: 0,
            bytes: 0,
            lost: 0,
            title: String::new(),
            last_error: String::new(),
            uplinks: Vec::new(),
            level: None,
            dead_air: false,
            quiet_seconds: 0,
            system: None,
            dead_air_level: -60.0,
            since: Since::new(started),
            played: VecDeque::new(),
            dj: serde_json::Value::Null,
            insert: String::new(),
        }
    }

    pub fn touch(&mut self) {
        self.updated = now();
    }
}

/// The status as one JSON document.
pub fn render(shared: &Arc<Mutex<Status>>) -> String {
    let status = shared.lock().unwrap_or_else(|e| e.into_inner());
    serde_json::to_string_pretty(&*status).unwrap_or_else(|_| "{}".to_string()) + "\n"
}

/// The file written whole and moved into place, so a reader never sees half.
pub fn write_file(path: &Path, text: &str) -> std::io::Result<()> {
    let part = path.with_extension("part");
    std::fs::write(&part, text)?;
    std::fs::rename(&part, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_as_json_with_the_fields_the_page_reads() {
        let shared = Arc::new(Mutex::new(Status::new("outcast")));
        {
            let mut s = shared.lock().unwrap();
            s.ring = Some(Ring {
                path: "/dev/shm/evoaudio.outcast.1.1".into(),
                rate: 48000,
                channels: 2,
                format: "S32_LE".into(),
                ffmpeg: Some("s32le".into()),
                frame_bytes: 8,
                ..Ring::default()
            });
            s.segments = 3;
            s.bytes = 1024;
            s.title = "Artist - Title".into();
            s.uplinks.push(Uplink {
                id: "icecast".into(),
                kind: "icecast".into(),
                state: "connected".into(),
                detail: "ice.example.com:8000/volumio.mp3".into(),
                ..Uplink::default()
            });
        }
        let text = render(&shared);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["tag"], "outcast");
        assert_eq!(v["ring"]["rate"], 48000);
        assert_eq!(v["ring"]["ffmpeg"], "s32le");
        assert_eq!(v["segments"], 3);
        assert_eq!(v["bytes"], 1024);
        assert_eq!(v["title"], "Artist - Title");
        assert_eq!(v["uplinks"][0]["state"], "connected");
        assert_eq!(v["uplinks"][0]["listeners"], 0);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn the_counts_the_events_and_the_titles_are_kept_and_cleared() {
        let mut s = Status::new("outcast");
        let start = s.since.at;
        s.played_title("Artist - One");
        s.played_title("Artist - One");
        s.played_title("");
        s.played_title("Artist - Two");
        assert_eq!(s.played.len(), 2, "a repeated or empty title is not kept");
        for i in 0..25 {
            s.played_title(&format!("Title {i}"));
            s.event("info", format!("event {i}"));
        }
        assert_eq!(s.played.len(), KEPT);
        assert_eq!(s.since.events.len(), KEPT);
        assert_eq!(s.since.events.back().unwrap().text, "event 24");
        s.since.reconnects = 3;
        s.since.overs = 2;
        std::thread::sleep(std::time::Duration::from_millis(1100));
        s.clear_since();
        assert!(s.since.at > start);
        assert_eq!((s.since.reconnects, s.since.overs), (0, 0));
        assert_eq!(
            s.since.events.len(),
            1,
            "the clearing is the first event after it"
        );
        assert_eq!(s.played.len(), KEPT, "the titles are not the counts");
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(v["since"]["events"][0]["kind"], "info");
        assert_eq!(v["played"][19]["title"], "Title 24");
        assert_eq!(v["dead_air_level"], -60.0);
    }

    #[test]
    fn the_file_is_written_whole() {
        let dir = std::env::temp_dir().join(format!("outcast-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("status.json");
        write_file(&file, "{\"a\":1}\n").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{\"a\":1}\n");
        assert!(!dir.join("status.part").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
