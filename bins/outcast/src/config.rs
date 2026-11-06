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
    /// MPEG-1 layer II, the broadcast transport stream's own; in a transport
    /// stream only.
    Mp2,
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
        matches!(
            self,
            Codec::Mp3 | Codec::Mp2 | Codec::Aac | Codec::Opus | Codec::Vorbis
        )
    }

    /// The file extension a recording of it takes.
    pub fn extension(self) -> &'static str {
        match self {
            Codec::Mp3 => "mp3",
            Codec::Mp2 => "mp2",
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
            Codec::Mp3 | Codec::Mp2 => "audio/mpeg",
            Codec::Aac => "audio/aac",
            Codec::Opus | Codec::Vorbis | Codec::Flac => "audio/ogg",
            Codec::Wav => "audio/wav",
        }
    }
}

fn default_true() -> bool {
    true
}

/// How the uplink talks to the server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// Icecast 2's `SOURCE` request to a mount, with Basic authentication.
    #[default]
    Icecast2,
    /// SHOUTcast's v1 source protocol: the password on the port above the
    /// server's, `OK2`, the `icy-*` headers; what a DNAS 1 takes, and a
    /// DNAS 2 on its legacy port.
    Shoutcast1,
}

/// The Icecast uplink: the source protocol to one mount, or SHOUTcast's.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Icecast {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default)]
    pub host: String,
    /// The server's own port; a SHOUTcast source connects one above it.
    #[serde(default = "Icecast::default_port")]
    pub port: u16,
    #[serde(default = "Icecast::default_mount")]
    pub mount: String,
    /// The stream a SHOUTcast DNAS 2 with several takes this one as; 1 for a
    /// DNAS 1 or a DNAS 2 with one.
    #[serde(default = "Icecast::default_stream_id")]
    pub stream_id: u32,
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
    /// The channels sent, 0 for the stream's own.
    #[serde(default)]
    pub channels: u32,
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
    fn default_stream_id() -> u32 {
        1
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

impl Icecast {
    /// What cannot run turned off, what is empty filled.
    pub fn tidy(&mut self) {
        self.host = self.host.trim().to_string();
        self.mount = self.mount.trim().to_string();
        if self.enabled && self.host.is_empty() {
            self.enabled = false;
        }
        match self.protocol {
            Protocol::Icecast2 => {
                if self.enabled && !self.mount.starts_with('/') {
                    self.enabled = false;
                }
            }
            Protocol::Shoutcast1 => {
                // A stream id is 1 or more; the source port is the server's plus one.
                self.stream_id = self.stream_id.max(1);
                if self.port == u16::MAX {
                    self.enabled = false;
                }
            }
        }
        if self.user.trim().is_empty() {
            self.user = Self::default_user();
        }
    }
}

impl Default for Icecast {
    fn default() -> Self {
        Self {
            enabled: false,
            protocol: Protocol::Icecast2,
            host: String::new(),
            port: Self::default_port(),
            mount: Self::default_mount(),
            stream_id: Self::default_stream_id(),
            user: Self::default_user(),
            password: String::new(),
            codec: Self::default_codec(),
            bitrate: Self::default_bitrate(),
            rate: Self::default_rate(),
            channels: 0,
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
    /// The channels sent, 0 for the stream's own.
    #[serde(default)]
    pub channels: u32,
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
            channels: 0,
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
    /// 0: the stream's own channels.
    #[serde(default)]
    pub channels: u32,
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
            channels: 0,
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
    /// Further Icecast or SHOUTcast destinations, each its own uplink.
    #[serde(default)]
    pub icecasts: Vec<Icecast>,
    #[serde(default)]
    pub lan: Lan,
    #[serde(default)]
    pub record: Record,
    #[serde(default)]
    pub srt: Srt,
    #[serde(default)]
    pub rtmp: Rtmp,
    #[serde(default)]
    pub ts: Ts,
    #[serde(default)]
    pub monitor: Monitor,
}

/// How a transport stream travels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// A TCP connection to the receiver, which listens; lost, it is made again.
    #[default]
    Tcp,
    /// UDP datagrams of seven transport packets, unicast or multicast.
    Udp,
}

/// A transport-stream uplink: the stream as an MPEG transport stream to a
/// host and port, over TCP or UDP, the plain contribution feed a broadcast
/// chain, a decoder or a player such as VLC takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ts {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub transport: Transport,
    #[serde(default)]
    pub host: String,
    #[serde(default = "Ts::default_port")]
    pub port: u16,
    /// The time to live of a multicast datagram: how many routers it may cross.
    #[serde(default = "Ts::default_ttl")]
    pub ttl: u8,
    #[serde(default = "Ts::default_codec")]
    pub codec: Codec,
    #[serde(default = "Ts::default_bitrate")]
    pub bitrate: u32,
    /// The sample rate sent, 0 for the stream's own.
    #[serde(default = "Ts::default_rate")]
    pub rate: u32,
    /// The channels sent, 0 for the stream's own.
    #[serde(default)]
    pub channels: u32,
    #[serde(default = "default_true")]
    pub keep_alive: bool,
}

impl Ts {
    fn default_port() -> u16 {
        5004
    }
    fn default_ttl() -> u8 {
        1
    }
    fn default_codec() -> Codec {
        Codec::Aac
    }
    fn default_bitrate() -> u32 {
        128
    }
    fn default_rate() -> u32 {
        48000
    }
    pub fn tidy(&mut self) {
        self.host = self.host.trim().to_string();
        if self.enabled && self.host.is_empty() {
            self.enabled = false;
        }
    }
}

impl Default for Ts {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: Transport::Tcp,
            host: String::new(),
            port: Self::default_port(),
            ttl: Self::default_ttl(),
            codec: Self::default_codec(),
            bitrate: Self::default_bitrate(),
            rate: Self::default_rate(),
            channels: 0,
            keep_alive: true,
        }
    }
}

/// An RTMP uplink: the stream published to an RTMP server, this side the
/// publisher, as the audio of an FLV stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rtmp {
    #[serde(default)]
    pub enabled: bool,
    /// `rtmp://host[:port]/application`, or `rtmps://` for a server behind TLS.
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub stream_key: String,
    /// A PEM file of certificate authorities to trust beside the public ones, for a
    /// server with a certificate of a private authority; empty for the public ones alone.
    #[serde(default)]
    pub ca_file: String,
    #[serde(default = "Rtmp::default_codec")]
    pub codec: Codec,
    #[serde(default = "Rtmp::default_bitrate")]
    pub bitrate: u32,
    /// The sample rate sent, 0 for the stream's own.
    #[serde(default = "Rtmp::default_rate")]
    pub rate: u32,
    /// The channels sent, 0 for the stream's own.
    #[serde(default)]
    pub channels: u32,
    #[serde(default = "default_true")]
    pub keep_alive: bool,
}

impl Rtmp {
    fn default_codec() -> Codec {
        Codec::Aac
    }
    fn default_bitrate() -> u32 {
        128
    }
    fn default_rate() -> u32 {
        44100
    }
    pub fn tidy(&mut self) {
        self.url = self.url.trim().to_string();
        self.stream_key = self.stream_key.trim().to_string();
        if self.enabled && self.url.is_empty() {
            self.enabled = false;
        }
    }
    /// The stream key for a log line: its first characters and no more.
    pub fn stream_key_shown(&self) -> String {
        let k = &self.stream_key;
        if k.len() <= 4 {
            "…".to_string()
        } else {
            format!("{}…", &k[..4])
        }
    }
}

impl Default for Rtmp {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            stream_key: String::new(),
            ca_file: String::new(),
            codec: Self::default_codec(),
            bitrate: Self::default_bitrate(),
            rate: Self::default_rate(),
            channels: 0,
            keep_alive: true,
        }
    }
}

/// An SRT uplink: the stream as an MPEG transport stream to an SRT listener
/// (a switcher, an encoder, a gateway), this side the caller.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Srt {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "Srt::default_port")]
    pub port: u16,
    /// The stream id the listener asks for, if any.
    #[serde(default)]
    pub stream_id: String,
    /// The passphrase for encryption, empty for none.
    #[serde(default)]
    pub passphrase: String,
    /// The latency the link is allowed, in milliseconds.
    #[serde(default = "Srt::default_latency")]
    pub latency_ms: u32,
    #[serde(default = "Srt::default_codec")]
    pub codec: Codec,
    #[serde(default = "Srt::default_bitrate")]
    pub bitrate: u32,
    /// The sample rate sent, 0 for the stream's own.
    #[serde(default = "Srt::default_rate")]
    pub rate: u32,
    /// The channels sent, 0 for the stream's own.
    #[serde(default)]
    pub channels: u32,
    /// The link kept up with silence while the player pauses or stops.
    #[serde(default = "default_true")]
    pub keep_alive: bool,
}

impl Srt {
    fn default_port() -> u16 {
        9000
    }
    fn default_latency() -> u32 {
        120
    }
    fn default_codec() -> Codec {
        Codec::Aac
    }
    fn default_bitrate() -> u32 {
        128
    }
    fn default_rate() -> u32 {
        48000
    }
    /// What cannot run turned off.
    pub fn tidy(&mut self) {
        self.host = self.host.trim().to_string();
        self.stream_id = self.stream_id.trim().to_string();
        if self.enabled && self.host.is_empty() {
            self.enabled = false;
        }
        if self.latency_ms == 0 {
            self.latency_ms = Self::default_latency();
        }
    }
}

impl Default for Srt {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: Self::default_port(),
            stream_id: String::new(),
            passphrase: String::new(),
            latency_ms: Self::default_latency(),
            codec: Self::default_codec(),
            bitrate: Self::default_bitrate(),
            rate: Self::default_rate(),
            channels: 0,
            keep_alive: true,
        }
    }
}

/// What the status watches the programme for.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Monitor {
    /// The level, in dBFS, below which the programme counts as silent.
    #[serde(default = "Monitor::default_level")]
    pub dead_air_level: f32,
    /// Seconds of silence after which the status says dead air; 0 never.
    #[serde(default = "Monitor::default_seconds")]
    pub dead_air_seconds: u64,
}

impl Monitor {
    fn default_level() -> f32 {
        -60.0
    }
    fn default_seconds() -> u64 {
        30
    }
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            dead_air_level: Self::default_level(),
            dead_air_seconds: Self::default_seconds(),
        }
    }
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
    /// Every Icecast slot, the first and the further ones, in order.
    pub fn slots(&self) -> Vec<&Icecast> {
        std::iter::once(&self.icecast)
            .chain(self.icecasts.iter())
            .collect()
    }

    pub fn tidy(&mut self) {
        for i in std::iter::once(&mut self.icecast).chain(self.icecasts.iter_mut()) {
            i.tidy();
        }
        self.srt.tidy();
        self.rtmp.tidy();
        self.ts.tidy();
        // The codec, the bitrate, the rate and the channels are the operator's,
        // whatever they are: the destination is the one to refuse them, and the
        // encoder's own refusal is shown as it comes.
        let l = &mut self.lan;
        l.max_listeners = l.max_listeners.max(1);
        let r = &mut self.record;
        r.folder = r.folder.trim().to_string();
        if r.enabled && r.folder.is_empty() {
            r.enabled = false;
        }
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
        assert_eq!(
            (u.monitor.dead_air_level, u.monitor.dead_air_seconds),
            (-60.0, 30)
        );
        let m: Uplinks =
            serde_json::from_str(r#"{"monitor":{"dead_air_level":-45.5,"dead_air_seconds":0}}"#)
                .unwrap();
        assert_eq!(
            (m.monitor.dead_air_level, m.monitor.dead_air_seconds),
            (-45.5, 0)
        );
        assert!(u.icecasts.is_empty() && u.slots().len() == 1);
        assert!(!u.srt.enabled);
        assert_eq!(
            (u.srt.port, u.srt.latency_ms, u.srt.codec, u.srt.rate),
            (9000, 120, Codec::Aac, 48000)
        );
        let mut s: Uplinks =
            serde_json::from_str(r#"{"srt":{"enabled":true,"host":" ","latency_ms":0}}"#).unwrap();
        s.tidy();
        assert!(!s.srt.enabled && s.srt.latency_ms == 120);
        assert!(Codec::Mp2.lossy() && Codec::Mp2.content_type() == "audio/mpeg");
        assert!(!u.rtmp.enabled && u.rtmp.codec == Codec::Aac && u.rtmp.rate == 44100);
        let mut r: Uplinks =
            serde_json::from_str(r#"{"rtmp":{"enabled":true,"url":" ","stream_key":"abcdefgh"}}"#)
                .unwrap();
        r.tidy();
        assert!(!r.rtmp.enabled && r.rtmp.stream_key_shown() == "abcd…");
        assert!(
            !u.ts.enabled && u.ts.transport == Transport::Tcp && u.ts.port == 5004 && u.ts.ttl == 1
        );
        let t: Uplinks = serde_json::from_str(
            r#"{"ts":{"enabled":true,"transport":"udp","host":"239.1.1.1","ttl":4}}"#,
        )
        .unwrap();
        assert!(t.ts.transport == Transport::Udp && t.ts.ttl == 4);
    }

    #[test]
    fn further_icecast_slots_are_their_own_uplinks_and_tidied_alike() {
        let mut u: Uplinks = serde_json::from_str(
            r#"{"icecast":{"enabled":true,"host":"a","password":"p"},"icecasts":[{"enabled":true,"host":" ","password":"p"},{"enabled":true,"host":"c","mount":"nomount","password":"p"},{"enabled":true,"protocol":"shoutcast1","host":"d","password":"p","stream_id":0}]}"#,
        )
        .unwrap();
        u.tidy();
        let on: Vec<bool> = u.slots().iter().map(|s| s.enabled).collect();
        assert_eq!(on, vec![true, false, false, true]);
        assert_eq!(u.icecasts[2].stream_id, 1);
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
    fn tidy_turns_off_what_cannot_run_and_leaves_the_operators_values() {
        let mut u = Uplinks::default();
        u.icecast.enabled = true;
        u.icecast.host = "  ".into();
        u.icecast.bitrate = 9999;
        u.icecast.rate = 96000;
        u.icecast.channels = 6;
        u.lan.max_listeners = 0;
        u.record.enabled = true;
        u.record.folder = "".into();
        u.tidy();
        assert!(!u.icecast.enabled);
        // Whatever the operator set goes to the encoder; the destination refuses.
        assert_eq!(u.icecast.bitrate, 9999);
        assert_eq!(u.icecast.rate, 96000);
        assert_eq!(u.icecast.channels, 6);
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
    fn shoutcast_takes_no_mount_and_the_codec_as_set() {
        let mut u: Uplinks = serde_json::from_str(
            r#"{"icecast":{"enabled":true,"protocol":"shoutcast1","host":"h","password":"p","mount":"","codec":"opus","stream_id":0}}"#,
        )
        .unwrap();
        u.tidy();
        assert!(u.icecast.enabled);
        assert_eq!(u.icecast.protocol, Protocol::Shoutcast1);
        assert_eq!(u.icecast.codec, Codec::Opus);
        assert_eq!(u.icecast.stream_id, 1);
        assert_eq!(u.icecast.channels, 0);
        let d = Uplinks::default();
        assert_eq!(d.icecast.protocol, Protocol::Icecast2);
        assert_eq!(d.icecast.stream_id, 1);
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
