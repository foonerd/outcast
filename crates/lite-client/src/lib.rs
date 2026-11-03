//! A client of outcast-lite's wire, for a native remote on Linux, Windows or
//! Android: the WebSocket opened, every frame given back as an [`Event`],
//! the segment's [`Shape`] parsed, and the PCM of each frame decoded to
//! 16-bit or float samples for an audio device. Pure Rust; it builds for
//! every target the remote builds for, the Android ABIs included.
//!
//! ```no_run
//! use lite_client::{Client, Event};
//! let mut client = Client::connect("ws://player:5582/anymote/listen").unwrap();
//! let mut shape = None;
//! let mut samples = Vec::new();
//! loop {
//!     match client.event().unwrap() {
//!         Event::Segment(s) => shape = Some(s),   // open the device at s.rate, s.channels
//!         Event::Pcm(pcm) => {
//!             if let Some(s) = &shape {
//!                 s.to_f32(&pcm, &mut samples);    // queue `samples` on the device
//!             }
//!         }
//!         Event::Gap { .. } | Event::End => {}    // clear the device's queue
//!         Event::Closed => break,                 // reconnect after a moment
//!         _ => {}
//!     }
//! }
//! ```
//!
//! The rules a remote follows with these events (the lead before the device
//! starts, the reopen on a new segment, the drift) are in outcast's
//! `docs/outcast-lite.md`, "Playing it in a native remote".

use std::net::TcpStream;

use serde::Deserialize;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// What a segment is, as the lite announces it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Shape {
    pub rate: u32,
    pub channels: u32,
    /// `s16le`, `s24le`, `s32le` or `f32le`: interleaved, little-endian.
    pub format: String,
    pub bits: u32,
    pub frame_bytes: u32,
    /// One-bit audio (DSD): announced, and no frames follow.
    pub one_bit: bool,
    /// Resampled down to the lite's rate cap.
    #[serde(default)]
    pub capped: bool,
    /// The most audio one binary frame carries.
    #[serde(default = "default_chunk_ms")]
    pub chunk_ms: u32,
}

fn default_chunk_ms() -> u32 {
    20
}

impl Shape {
    /// Bytes of one sample.
    pub fn sample_bytes(&self) -> usize {
        match self.format.as_str() {
            "s16le" => 2,
            "s24le" => 3,
            _ => 4,
        }
    }

    /// Whether this crate decodes the format.
    pub fn known(&self) -> bool {
        matches!(self.format.as_str(), "s16le" | "s24le" | "s32le" | "f32le")
    }

    /// Frames in `bytes` of PCM.
    pub fn frames_in(&self, bytes: usize) -> usize {
        bytes / self.frame_bytes.max(1) as usize
    }

    /// Seconds of audio in `bytes` of PCM.
    pub fn seconds_in(&self, bytes: usize) -> f64 {
        self.frames_in(bytes) as f64 / f64::from(self.rate.max(1))
    }

    /// Every sample of `pcm` as a 16-bit integer, onto `out` (cleared
    /// first): the top sixteen bits of a wider integer, a float clamped and
    /// scaled. A trailing part of a sample is left out.
    pub fn to_i16(&self, pcm: &[u8], out: &mut Vec<i16>) {
        out.clear();
        let n = self.sample_bytes();
        out.reserve(pcm.len() / n);
        match self.format.as_str() {
            "s16le" => out.extend(
                pcm.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|s| i16::from_le_bytes([s[0], s[1]])),
            ),
            "s24le" => out.extend(
                pcm.as_chunks::<3>()
                    .0
                    .iter()
                    .map(|s| (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 16) as i16),
            ),
            "s32le" => out.extend(
                pcm.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|s| (i32::from_le_bytes([s[0], s[1], s[2], s[3]]) >> 16) as i16),
            ),
            "f32le" => out.extend(pcm.as_chunks::<4>().0.iter().map(|s| {
                let v = f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                (v.clamp(-1.0, 1.0) * 32767.0) as i16
            })),
            _ => {}
        }
    }

    /// Every sample of `pcm` as a float in -1 to 1, onto `out` (cleared
    /// first), nothing lost on the way.
    pub fn to_f32(&self, pcm: &[u8], out: &mut Vec<f32>) {
        out.clear();
        let n = self.sample_bytes();
        out.reserve(pcm.len() / n);
        match self.format.as_str() {
            "s16le" => out.extend(
                pcm.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0),
            ),
            "s24le" => out.extend(
                pcm.as_chunks::<3>()
                    .0
                    .iter()
                    .map(|s| (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0),
            ),
            "s32le" => out.extend(
                pcm.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0),
            ),
            "f32le" => out.extend(
                pcm.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]])),
            ),
            _ => {}
        }
    }
}

/// What the lite sends, one event per frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The first frame: the lite's version and the ring's tag.
    Hello { version: String, tag: String },
    /// A stream begins (or is on, when joining late).
    Segment(Shape),
    /// A piece of the stream, at most `chunk_ms` of PCM in the segment's shape.
    Pcm(Vec<u8>),
    /// The stream ended; a new `Segment` may follow.
    End,
    /// This client fell behind and `dropped` frames were lost for it: clear
    /// the queue and go on.
    Gap { dropped: u64 },
    /// Too many listeners; the lite closes the connection.
    Full,
    /// A text frame of a type this version does not know: ignore it.
    Other(String),
    /// The connection closed.
    Closed,
}

#[derive(Deserialize)]
struct Kind {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    dropped: u64,
}

/// A text frame as an event.
pub fn parse_text(text: &str) -> Result<Event, serde_json::Error> {
    let k: Kind = serde_json::from_str(text)?;
    Ok(match k.kind.as_str() {
        "hello" => Event::Hello {
            version: k.version,
            tag: k.tag,
        },
        "segment" => Event::Segment(serde_json::from_str(text)?),
        "end" => Event::End,
        "gap" => Event::Gap { dropped: k.dropped },
        "full" => Event::Full,
        other => Event::Other(other.to_string()),
    })
}

/// A connection to the lite.
pub struct Client {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
}

impl Client {
    /// Connected to `url` (`ws://host:port/path`); the `hello` is the first event.
    pub fn connect(url: &str) -> Result<Self, tungstenite::Error> {
        let (ws, _) = tungstenite::connect(url)?;
        Ok(Self { ws })
    }

    /// The next event, waited for. Pings are answered on the way. An error
    /// is the connection gone; `Event::Closed` is its orderly end.
    pub fn event(&mut self) -> Result<Event, tungstenite::Error> {
        loop {
            match self.ws.read()? {
                Message::Text(t) => {
                    return Ok(parse_text(&t).unwrap_or_else(|_| Event::Other(t.to_string())))
                }
                Message::Binary(b) => return Ok(Event::Pcm(b.to_vec())),
                Message::Close(_) => return Ok(Event::Closed),
                _ => {}
            }
        }
    }

    /// The connection closed from this side.
    pub fn close(mut self) {
        let _ = self.ws.close(None);
        let _ = self.ws.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(format: &str) -> Shape {
        let bytes = match format {
            "s16le" => 2,
            "s24le" => 3,
            _ => 4,
        };
        Shape {
            rate: 48000,
            channels: 2,
            format: format.into(),
            bits: bytes * 8,
            frame_bytes: bytes * 2,
            one_bit: false,
            capped: false,
            chunk_ms: 20,
        }
    }

    #[test]
    fn the_text_frames_parse() {
        assert_eq!(
            parse_text(r#"{"type":"hello","version":"0.1.6","tag":"glass"}"#).unwrap(),
            Event::Hello {
                version: "0.1.6".into(),
                tag: "glass".into()
            }
        );
        let s = parse_text(r#"{"type":"segment","rate":44100,"channels":2,"format":"s16le","bits":16,"frame_bytes":4,"one_bit":false,"capped":false,"chunk_ms":20}"#).unwrap();
        match s {
            Event::Segment(s) => {
                assert_eq!(
                    (s.rate, s.channels, s.format.as_str(), s.frame_bytes),
                    (44100, 2, "s16le", 4)
                );
                assert!(s.known());
                assert_eq!(s.frames_in(4 * 441), 441);
                assert!((s.seconds_in(44100 * 4) - 1.0).abs() < 1e-9);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(parse_text(r#"{"type":"end"}"#).unwrap(), Event::End);
        assert_eq!(
            parse_text(r#"{"type":"gap","dropped":7}"#).unwrap(),
            Event::Gap { dropped: 7 }
        );
        assert_eq!(parse_text(r#"{"type":"full"}"#).unwrap(), Event::Full);
        // A later lite's new type and new fields are taken in stride.
        assert_eq!(
            parse_text(r#"{"type":"tempo","bpm":120}"#).unwrap(),
            Event::Other("tempo".into())
        );
        let s = parse_text(r#"{"type":"segment","rate":96000,"channels":2,"format":"s24le","bits":24,"frame_bytes":6,"one_bit":false,"colour":"blue"}"#).unwrap();
        assert!(matches!(s, Event::Segment(s) if s.chunk_ms == 20 && !s.capped));
    }

    #[test]
    fn every_format_decodes_to_the_same_values() {
        // One frame, left 0x4000 of full scale, right -0x4000: in each format.
        let mut i = Vec::new();
        let mut f = Vec::new();
        shape("s16le").to_i16(&[0x00, 0x40, 0x00, 0xC0], &mut i);
        assert_eq!(i, vec![0x4000, -0x4000]);
        shape("s16le").to_f32(&[0x00, 0x40, 0x00, 0xC0], &mut f);
        assert_eq!(f, vec![0.5, -0.5]);
        shape("s24le").to_i16(&[0x00, 0x00, 0x40, 0x00, 0x00, 0xC0], &mut i);
        assert_eq!(i, vec![0x4000, -0x4000]);
        shape("s24le").to_f32(&[0x00, 0x00, 0x40, 0x00, 0x00, 0xC0], &mut f);
        assert_eq!(f, vec![0.5, -0.5]);
        shape("s32le").to_i16(&[0, 0, 0, 0x40, 0, 0, 0, 0xC0], &mut i);
        assert_eq!(i, vec![0x4000, -0x4000]);
        shape("s32le").to_f32(&[0, 0, 0, 0x40, 0, 0, 0, 0xC0], &mut f);
        assert_eq!(f, vec![0.5, -0.5]);
        let mut pcm = Vec::new();
        pcm.extend_from_slice(&0.5f32.to_le_bytes());
        pcm.extend_from_slice(&(-0.5f32).to_le_bytes());
        shape("f32le").to_i16(&pcm, &mut i);
        assert_eq!(i, vec![16383, -16383]);
        shape("f32le").to_f32(&pcm, &mut f);
        assert_eq!(f, vec![0.5, -0.5]);
        // A trailing part of a sample is left out.
        shape("s16le").to_i16(&[0x00, 0x40, 0x12], &mut i);
        assert_eq!(i, vec![0x4000]);
    }
}
