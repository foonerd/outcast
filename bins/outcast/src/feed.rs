//! What flows from the ring to the uplinks: a segment's shape in the words an
//! encoder takes, then its bytes, then its end. The ring's native bytes are
//! turned into one of the shapes evopeg has a name for once, here, for every
//! uplink: a linear format evopeg knows passes as it is; one it does not is
//! widened to S32_LE; one-bit audio (DSD) becomes silence of the same
//! length, with the flag set, until a decimation of its own exists.

use tap::audio::Shape;
use tap::sample::{ffmpeg_name, widen_to_s32, Layout};

/// The raw formats evopeg demuxes: every other linear format is widened.
const EVOPEG_FORMATS: [&str; 4] = ["s16le", "s24le", "s32le", "f32le"];

/// A segment as an encoder reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// evopeg's name for the raw bytes, as `s32le`.
    pub format: &'static str,
    /// The samples' layout, for the conformer.
    pub layout: Layout,
    pub rate: u32,
    pub channels: u32,
    /// Bytes of one frame as sent.
    pub frame_bytes: u32,
    /// The player plays one-bit audio and the uplinks carry silence.
    pub dsd_silence: bool,
}

/// How the ring's bytes become the segment's.
#[derive(Clone, Copy, Debug)]
pub enum Conversion {
    /// As they are.
    Pass,
    /// Every sample widened to S32_LE.
    Widen(Layout),
    /// Zeros, as many as the ring's bytes are long in time at the silence's shape.
    Silence { in_frame: u32, in_rate: u32 },
}

/// The segment an encoder gets for a ring of this shape, and the conversion.
pub fn plan(shape: &Shape) -> (Segment, Conversion) {
    let channels = shape.channels.max(1);
    let s32 = Layout::Linear {
        bytes: 4,
        valid: 32,
        signed: true,
        little: true,
    };
    if shape.one_bit {
        return (
            Segment {
                format: "s16le",
                layout: Layout::Linear {
                    bytes: 2,
                    valid: 16,
                    signed: true,
                    little: true,
                },
                rate: 44100,
                channels,
                frame_bytes: 2 * channels,
                dsd_silence: true,
            },
            Conversion::Silence {
                in_frame: shape.frame_bytes.max(1),
                in_rate: shape.rate.max(1),
            },
        );
    }
    if let (Some(name), Some(layout)) = (ffmpeg_name(shape.format), Layout::from_alsa(shape.format))
    {
        if EVOPEG_FORMATS.contains(&name) {
            return (
                Segment {
                    format: name,
                    layout,
                    rate: shape.rate,
                    channels,
                    frame_bytes: shape.frame_bytes,
                    dsd_silence: false,
                },
                Conversion::Pass,
            );
        }
    }
    let layout = Layout::from_alsa(shape.format).unwrap_or(Layout::Float { little: true });
    (
        Segment {
            format: "s32le",
            layout: s32,
            rate: shape.rate,
            channels,
            frame_bytes: 4 * channels,
            dsd_silence: false,
        },
        Conversion::Widen(layout),
    )
}

/// The ring's bytes as the segment's.
pub fn convert(conversion: &Conversion, segment: &Segment, bytes: &[u8], out: &mut Vec<u8>) {
    out.clear();
    match conversion {
        Conversion::Pass => out.extend_from_slice(bytes),
        Conversion::Widen(layout) => widen_to_s32(layout, bytes, out),
        Conversion::Silence { in_frame, in_rate } => {
            let seconds = bytes.len() as f64 / *in_frame as f64 / *in_rate as f64;
            let frames = (seconds * segment.rate as f64).round() as usize;
            out.resize(frames * segment.frame_bytes as usize, 0);
        }
    }
}

/// What an uplink hears beside the stream's own bytes, which come on a
/// channel of their own and are never waited on.
#[derive(Clone, Debug)]
pub enum Event {
    /// A stream of this shape begins: start an encoder.
    Segment(Segment),
    /// The stream ended: the encoder is done.
    End,
    /// The player shows this title now.
    Title(String),
    /// The uplink is being taken down.
    Stop,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(format: i32, rate: u32, channels: u32, frame_bytes: u32, one_bit: bool) -> Shape {
        Shape {
            rate,
            channels,
            format,
            frame_bytes,
            one_bit,
        }
    }

    #[test]
    fn a_known_format_passes_as_it_is() {
        let (seg, conv) = plan(&shape(2, 48000, 2, 4, false));
        assert_eq!(seg.format, "s16le");
        assert_eq!((seg.rate, seg.channels, seg.frame_bytes), (48000, 2, 4));
        assert!(matches!(conv, Conversion::Pass));
        let mut out = Vec::new();
        convert(&conv, &seg, &[1, 2, 3, 4], &mut out);
        assert_eq!(out, vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_format_evopeg_has_no_name_for_is_widened_too() {
        // S16_BE is ALSA format 3: ffmpeg knows it as s16be, evopeg does not demux it.
        let (seg, conv) = plan(&shape(3, 44100, 2, 4, false));
        assert_eq!(seg.format, "s32le");
        assert!(matches!(conv, Conversion::Widen(_)));
        let mut out = Vec::new();
        convert(&conv, &seg, &[0x40, 0x00, 0x40, 0x00], &mut out);
        assert_eq!(&out[..4], &[0, 0, 0, 0x40]);
    }

    #[test]
    fn a_24_in_32_format_is_widened() {
        // S24_LE is ALSA format 6: 24 valid bits in a 4-byte word, no name in evopeg.
        let (seg, conv) = plan(&shape(6, 96000, 2, 8, false));
        assert_eq!(seg.format, "s32le");
        assert_eq!(seg.frame_bytes, 8);
        assert!(matches!(conv, Conversion::Widen(_)));
        let mut out = Vec::new();
        convert(&conv, &seg, &[0, 0, 0x80, 0x00, 0, 0, 0x80, 0x00], &mut out);
        assert_eq!(out.len(), 8);
        // The valid bits left-justified: 0x008000 in 24 bits becomes 0x80000000.
        assert_eq!(&out[..4], &[0, 0, 0, 0x80]);
    }

    #[test]
    fn one_bit_audio_becomes_silence_of_the_same_length() {
        // DSD_U32_LE at an ALSA rate of 88200 frames of 4 bytes a channel.
        let (seg, conv) = plan(&shape(50, 88200, 2, 8, true));
        assert!(seg.dsd_silence);
        assert_eq!((seg.format, seg.rate, seg.frame_bytes), ("s16le", 44100, 4));
        let mut out = Vec::new();
        // One second of the ring's bytes: 88200 frames of 8 bytes.
        convert(&conv, &seg, &vec![0xAA; 88200 * 8], &mut out);
        assert_eq!(out.len(), 44100 * 4);
        assert!(out.iter().all(|b| *b == 0));
        assert_eq!(out.len() as u32 / seg.frame_bytes, seg.rate);
    }
}
