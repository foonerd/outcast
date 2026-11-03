//! What goes down the WebSocket: text frames of JSON that say what the
//! stream is, binary frames of PCM that are the stream. One stream at a
//! time; the page takes the PCM as the segment said and plays it.

use serde::Serialize;
use stream::feed::Segment;

/// The most audio one binary frame carries: 20 ms, so a page's buffer can
/// be short and a dropped frame costs little.
pub const CHUNK_MS: u32 = 20;

/// A segment as the page is told of it.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Announced {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub rate: u32,
    pub channels: u32,
    /// `s16le`, `s24le`, `s32le` or `f32le`: little-endian, interleaved.
    pub format: &'static str,
    /// Bits in one sample: 16, 24 or 32.
    pub bits: u32,
    pub frame_bytes: u32,
    /// One-bit audio (DSD) plays on the player only: the segment is
    /// announced and no frames follow.
    pub one_bit: bool,
    /// The stream was resampled to the rate cap.
    pub capped: bool,
    pub chunk_ms: u32,
}

impl Announced {
    pub fn of(seg: &Segment, capped: bool) -> Self {
        Self {
            kind: "segment",
            rate: seg.rate,
            channels: seg.channels,
            format: seg.format,
            bits: match seg.format {
                "s16le" => 16,
                "s24le" => 24,
                _ => 32,
            },
            frame_bytes: seg.frame_bytes,
            one_bit: seg.dsd_silence,
            capped,
            chunk_ms: CHUNK_MS,
        }
    }

    pub fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// A text frame of one word, as `{"type":"end"}`.
pub fn word(kind: &str) -> String {
    format!("{{\"type\":\"{kind}\"}}")
}

/// Bytes of a stream cut into frames of at most `CHUNK_MS`, whole PCM
/// frames each, onto `out`.
pub fn chunks(bytes: &[u8], seg: &Segment, out: &mut Vec<Vec<u8>>) {
    let frame = seg.frame_bytes.max(1) as usize;
    let per_chunk = (seg.rate.max(1) * CHUNK_MS / 1000).max(1) as usize * frame;
    let whole = bytes.len() / frame * frame;
    for piece in bytes[..whole].chunks(per_chunk) {
        out.push(piece.to_vec());
    }
}

/// 32-bit samples made 16-bit, the top sixteen bits of each: for the
/// capped stream, where the rate was reduced for a weak link and the width
/// goes with it.
pub fn narrow_to_s16(s32: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(s32.len() / 2);
    for s in s32.as_chunks::<4>().0 {
        out.extend_from_slice(&[s[2], s[3]]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tap::sample::Layout;

    fn seg(rate: u32, channels: u32) -> Segment {
        Segment {
            format: "s16le",
            layout: Layout::Linear {
                bytes: 2,
                valid: 16,
                signed: true,
                little: true,
            },
            rate,
            channels,
            frame_bytes: 2 * channels,
            dsd_silence: false,
        }
    }

    #[test]
    fn the_announcement_says_what_the_page_needs() {
        let a = Announced::of(&seg(48000, 2), false);
        assert_eq!(
            a.json(),
            r#"{"type":"segment","rate":48000,"channels":2,"format":"s16le","bits":16,"frame_bytes":4,"one_bit":false,"capped":false,"chunk_ms":20}"#
        );
        assert_eq!(word("end"), r#"{"type":"end"}"#);
    }

    #[test]
    fn chunks_are_twenty_milliseconds_of_whole_frames() {
        let s = seg(48000, 2);
        // 50 ms of frames plus a stray byte.
        let bytes = vec![0u8; 48000 / 20 * 4 + 1];
        let mut out = Vec::new();
        chunks(&bytes, &s, &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].len(), 960 * 4);
        assert_eq!(out[2].len(), 480 * 4);
    }

    #[test]
    fn narrowing_keeps_the_top_bits() {
        let mut out = Vec::new();
        narrow_to_s16(&[0x78, 0x56, 0x34, 0x12, 0x00, 0x00, 0x00, 0x80], &mut out);
        assert_eq!(out, vec![0x34, 0x12, 0x00, 0x80]);
    }
}
