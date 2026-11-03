//! The uplinks as the plugin's page set them: one JSON file the shell writes
//! and the core reads at its start and at every `reload`. Each uplink's
//! settings are its own; what the file leaves out takes the default here,
//! and a value the core cannot use is replaced by the default and said.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The codecs an uplink may encode with, by the name the page uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Mp3,
    Aac,
    Opus,
    Vorbis,
    Flac,
    Wav,
}

impl Codec {
    /// Whether the stream carries its titles in-band (Ogg), so a new title
    /// means a new chain from the encoder rather than a word to the server.
    pub fn in_band_titles(self) -> bool {
        matches!(self, Codec::Opus | Codec::Vorbis | Codec::Flac)
    }

    /// Whether a bitrate means anything.
    pub fn lossy(self) -> bool {
        matches!(self, Codec::Mp3 | Codec::Aac | Codec::Opus | Codec::Vorbis)
    }

    /// The file extension a recording of it takes.
    pub fn extension(self) -> &'static str {
        match self {
            Codec::Mp3 => "mp3",
            Codec::Aac => "aac",
            Codec::Opus => "opus",
            Codec::Vorbis => "ogg",
            Codec::Flac => "flac",
            Codec::Wav => "wav",
        }
    }

    /// The content type a stream of it is served as.
    pub fn content_type(self) -> &'static str {
        match self {
            Codec::Mp3 => "audio/mpeg",
            Codec::Aac => "audio/aac",
            Codec::Opus | Codec::Vorbis | Codec::Flac => "audio/ogg",
            Codec::Wav => "audio/wav",
        }
    }
}

fn default_true() -> bool {
    true
}

/// The Icecast uplink: the source protocol to one mount.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Icecast {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "Icecast::default_port")]
    pub port: u16,
    #[serde(default = "Icecast::default_mount")]
    pub mount: String,
    #[serde(default = "Icecast::default_user")]
    pub user: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "Icecast::default_codec")]
    pub codec: Codec,
    #[serde(default = "Icecast::default_bitrate")]
    pub bitrate: u32,
    /// The sample rate sent, 0 for the stream's own.
    #[serde(default = "Icecast::default_rate")]
    pub rate: u32,
    #[serde(default = "Icecast::default_name")]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub genre: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub public: bool,
    /// The mount kept up with silence while the player pauses or stops, and
    /// from the moment the uplink is on; off, the connection follows the stream.
    #[serde(default = "default_true")]
    pub keep_alive: bool,
}

impl Icecast {
    fn default_port() -> u16 {
        8000
    }
    fn default_mount() -> String {
        "/volumio.mp3".into()
    }
    fn default_user() -> String {
        "source".into()
    }
    fn default_codec() -> Codec {
        Codec::Mp3
    }
    fn default_bitrate() -> u32 {
        192
    }
    fn default_rate() -> u32 {
        44100
    }
    fn default_name() -> String {
        "Volumio".into()
    }
}

impl Default for Icecast {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: Self::default_port(),
            mount: Self::default_mount(),
            user: Self::default_user(),
            password: String::new(),
            codec: Self::default_codec(),
            bitrate: Self::default_bitrate(),
            rate: Self::default_rate(),
            name: Self::default_name(),
            description: String::new(),
            genre: String::new(),
            url: String::new(),
            public: false,
            keep_alive: true,
        }
    }
}

/// The LAN stream: an HTTP server on the player any listener on the network opens.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Lan {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "Lan::default_port")]
    pub port: u16,
    #[serde(default = "Lan::default_codec")]
    pub codec: Codec,
    #[serde(default = "Lan::default_bitrate")]
    pub bitrate: u32,
    #[serde(default = "Lan::default_rate")]
    pub rate: u32,
    #[serde(default = "Lan::default_name")]
    pub name: String,
    #[serde(default = "Lan::default_max_listeners")]
    pub max_listeners: u32,
    /// The stream kept going with silence while the player pauses or stops.
    #[serde(default = "default_true")]
    pub keep_alive: bool,
}

impl Lan {
    fn default_port() -> u16 {
        8000
    }
    fn default_codec() -> Codec {
        Codec::Mp3
    }
    fn default_bitrate() -> u32 {
        192
    }
    fn default_rate() -> u32 {
        44100
    }
    fn default_name() -> String {
        "Volumio".into()
    }
    fn default_max_listeners() -> u32 {
        5
    }
}

impl Default for Lan {
    fn default() -> Self {
        Self {
            enabled: false,
            port: Self::default_port(),
            codec: Self::default_codec(),
            bitrate: Self::default_bitrate(),
            rate: Self::default_rate(),
            name: Self::default_name(),
            max_listeners: Self::default_max_listeners(),
            keep_alive: true,
        }
    }
}

/// How a recording is cut into files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    /// A new file at every title the player shows.
    Track,
    /// One file per stream, from the moment it starts to the moment it ends.
    Stream,
}

/// Recording: the stream to files under a folder of the player.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Record {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "Record::default_folder")]
    pub folder: String,
    #[serde(default = "Record::default_codec")]
    pub codec: Codec,
    #[serde(default)]
    pub bitrate: u32,
    /// 0: the stream's own rate, nothing resampled.
    #[serde(default)]
    pub rate: u32,
    #[serde(default = "Record::default_split")]
    pub split: Split,
    #[serde(default = "default_true")]
    pub title_in_name: bool,
}

impl Record {
    fn default_folder() -> String {
        "/data/INTERNAL/outcast/recordings".into()
    }
    fn default_codec() -> Codec {
        Codec::Flac
    }
    fn default_split() -> Split {
        Split::Track
    }
}

impl Default for Record {
    fn default() -> Self {
        Self {
            enabled: false,
            folder: Self::default_folder(),
            codec: Self::default_codec(),
            bitrate: 0,
            rate: 0,
            split: Self::default_split(),
            title_in_name: true,
        }
    }
}

/// The whole file.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Uplinks {
    #[serde(default)]
    pub icecast: Icecast,
    #[serde(default)]
    pub lan: Lan,
    #[serde(default)]
    pub record: Record,
}

impl Uplinks {
    /// The file read, or the defaults where there is none; a file that does
    /// not parse is an error, since the page wrote it and the page is wrong.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut uplinks: Uplinks =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        uplinks.tidy();
        Ok(uplinks)
    }

    /// Values the core cannot use, put right: an empty mount or host turns the
    /// uplink off, a bitrate out of the way is clamped, a rate that is not one
    /// of the two the encoders take becomes the stream's own.
    pub fn tidy(&mut self) {
        let i = &mut self.icecast;
        i.host = i.host.trim().to_string();
        i.mount = i.mount.trim().to_string();
        if i.enabled && (i.host.is_empty() || !i.mount.starts_with('/')) {
            i.enabled = false;
        }
        if i.user.trim().is_empty() {
            i.user = Icecast::default_user();
        }
        i.bitrate = i.bitrate.clamp(32, 512);
        i.rate = tidy_rate(i.rate, 44100);
        let l = &mut self.lan;
        l.bitrate = l.bitrate.clamp(32, 512);
        l.rate = tidy_rate(l.rate, 44100);
        l.max_listeners = l.max_listeners.clamp(1, 64);
        let r = &mut self.record;
        r.folder = r.folder.trim().to_string();
        if r.enabled && r.folder.is_empty() {
            r.enabled = false;
        }
        if r.bitrate != 0 {
            r.bitrate = r.bitrate.clamp(32, 512);
        }
        r.rate = tidy_rate(r.rate, 0);
    }
}

fn tidy_rate(rate: u32, fallback: u32) -> u32 {
    match rate {
        0 | 44100 | 48000 => rate,
        _ => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_defaults() {
        let u = Uplinks::load(Path::new("/nonexistent/outcast/uplinks.json")).unwrap();
        assert_eq!(u, Uplinks::default());
        assert!(!u.icecast.enabled && !u.lan.enabled && !u.record.enabled);
        assert_eq!(u.icecast.mount, "/volumio.mp3");
        assert_eq!(u.record.codec, Codec::Flac);
    }

    #[test]
    fn a_partial_file_takes_the_defaults_for_the_rest() {
        let u: Uplinks =
            serde_json::from_str(r#"{"icecast":{"enabled":true,"host":"ice.example.com","password":"x","codec":"opus"}}"#)
                .unwrap();
        assert!(u.icecast.enabled);
        assert_eq!(u.icecast.port, 8000);
        assert_eq!(u.icecast.codec, Codec::Opus);
        assert_eq!(u.icecast.bitrate, 192);
        assert!(u.icecast.keep_alive);
        assert_eq!(u.lan, Lan::default());
        let off: Uplinks =
            serde_json::from_str(r#"{"lan":{"enabled":true,"keep_alive":false}}"#).unwrap();
        assert!(!off.lan.keep_alive);
    }

    #[test]
    fn tidy_turns_off_what_cannot_run_and_clamps_the_rest() {
        let mut u = Uplinks::default();
        u.icecast.enabled = true;
        u.icecast.host = "  ".into();
        u.icecast.bitrate = 9999;
        u.icecast.rate = 96000;
        u.lan.max_listeners = 0;
        u.record.enabled = true;
        u.record.folder = "".into();
        u.tidy();
        assert!(!u.icecast.enabled);
        assert_eq!(u.icecast.bitrate, 512);
        assert_eq!(u.icecast.rate, 44100);
        assert_eq!(u.lan.max_listeners, 1);
        assert!(!u.record.enabled);
        let mut v = Uplinks::default();
        v.icecast.enabled = true;
        v.icecast.host = "h".into();
        v.icecast.mount = "nomount".into();
        v.tidy();
        assert!(!v.icecast.enabled);
    }

    #[test]
    fn codecs_know_their_nature() {
        assert!(Codec::Opus.in_band_titles() && !Codec::Mp3.in_band_titles());
        assert!(Codec::Mp3.lossy() && !Codec::Flac.lossy());
        assert_eq!(Codec::Vorbis.extension(), "ogg");
        assert_eq!(Codec::Aac.content_type(), "audio/aac");
        let c: Codec = serde_json::from_str("\"flac\"").unwrap();
        assert_eq!(c, Codec::Flac);
    }
}
