//! The conformer: between the hub's pieces and an uplink's encoder, the
//! stream in the shape the operator chose for the uplink, whatever the
//! player plays. A rate that is set is held: a track at another rate is
//! resampled here (a windowed sinc, `rubato`, at its full quality), so the
//! encoder runs on across the player's changes and a listener hears no seam.
//! A rate left "as played" follows the player, and the encoder starts anew at
//! a change. Channels likewise: set, held (a mono track doubled, a stereo one
//! folded to mono, the first two of more); as played, followed. Samples go
//! to the encoder as 32-bit integers, every width widened without loss.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Resampler, SincInterpolationParameters};
use tap::sample::Layout;

use crate::feed::Segment;

/// Frames the resampler takes in one go: 23 ms at 44.1 kHz, the latency the
/// conversion adds beside the filter's own few milliseconds.
const CHUNK: usize = 1024;

const S32: Layout = Layout::Linear {
    bytes: 4,
    valid: 32,
    signed: true,
    little: true,
};

/// The shape an encoder is fed for a rate and a channel count: 32-bit
/// samples.
pub fn target(rate: u32, channels: u32) -> Segment {
    let channels = channels.max(1);
    Segment {
        format: "s32le",
        layout: S32,
        rate: rate.max(1),
        channels,
        frame_bytes: 4 * channels,
        dsd_silence: false,
    }
}

/// The stream of one segment on its way to the target shape.
struct Input {
    layout: Layout,
    channels: usize,
    frame_bytes: usize,
    /// Bytes as they are when the segment is already the target's shape.
    pass: bool,
    /// A trailing part of a frame, kept for the next piece.
    rest: Vec<u8>,
    /// The resampler, where the rates differ, and the frames waiting for a
    /// full chunk of it.
    resampler: Option<Async<f32>>,
    pending: Vec<f32>,
}

pub struct Conformer {
    /// The operator's rate, 0 for the player's own.
    rate: u32,
    /// The operator's channels, 0 for the player's own.
    channels: u32,
    /// The shape of the stream now going to the encoder.
    current: Segment,
    input: Option<Input>,
    /// Scratch for the samples of one piece.
    samples: Vec<f32>,
}

impl Conformer {
    /// `rate` and `channels` as set for the uplink, 0 for "as played".
    pub fn new(rate: u32, channels: u32) -> Self {
        Self {
            rate,
            channels,
            current: target(
                if rate == 0 { 44100 } else { rate },
                if channels == 0 { 2 } else { channels },
            ),
            input: None,
            samples: Vec::new(),
        }
    }

    /// The shape the encoder is fed now: fixed where the operator set it,
    /// the player's where not. An encoder running at another shape is to be
    /// ended and started anew.
    pub fn shape(&self) -> Segment {
        self.current
    }

    /// Whether the current segment is resampled on its way.
    #[cfg(test)]
    pub fn resampling(&self) -> bool {
        self.input.as_ref().is_some_and(|i| i.resampler.is_some())
    }

    /// A segment begins: the one before is flushed onto `out`, the way for
    /// this one is laid, and `shape()` says what the encoder must be.
    pub fn segment(&mut self, seg: &Segment, out: &mut Vec<u8>) {
        self.end(out);
        let in_channels = seg.channels.max(1);
        let rate = if self.rate == 0 {
            seg.rate.max(1)
        } else {
            self.rate
        };
        let channels = if self.channels == 0 {
            in_channels
        } else {
            self.channels
        };
        let frame_bytes = seg.frame_bytes.max(1) as usize;
        // Where the rate and the channels are held, the encoder's shape is
        // one for its whole life: 32-bit, so a 16-bit track and a 24-bit one
        // go through the same encoder. Where either follows the player, the
        // encoder starts anew at a change anyway, and a track that needs
        // nothing done passes in its own format, every bit as it was.
        let held = self.rate != 0 && self.channels != 0;
        let unchanged = seg.rate == rate && in_channels == channels;
        let pass = unchanged && (!held || seg.format == "s32le");
        self.current = if pass {
            Segment {
                format: seg.format,
                layout: seg.layout,
                rate,
                channels,
                frame_bytes: frame_bytes as u32,
                dsd_silence: false,
            }
        } else {
            target(rate, channels)
        };
        let resampler = (seg.rate != rate && seg.rate > 0).then(|| {
            Async::<f32>::new_sinc(
                f64::from(rate) / f64::from(seg.rate),
                1.0,
                &SincInterpolationParameters::default(),
                CHUNK,
                channels as usize,
                FixedAsync::Input,
            )
            .expect("a sinc resampler for two rates")
        });
        self.input = Some(Input {
            layout: seg.layout,
            channels: in_channels as usize,
            frame_bytes,
            pass,
            rest: Vec::new(),
            resampler,
            pending: Vec::new(),
        });
    }

    /// A line for the log: the segment's shape and what is done to it.
    pub fn describe(&self, seg: &Segment) -> String {
        let what = match self.input.as_ref() {
            Some(i) if i.resampler.is_some() => {
                format!("resampled to {} Hz", self.current.rate)
            }
            Some(i) if i.pass => "as it is".to_string(),
            Some(i) if i.channels as u32 != self.current.channels => format!(
                "widened, {} channels made {}",
                i.channels, self.current.channels
            ),
            _ => "widened".to_string(),
        };
        format!(
            "stream {} Hz, {} ch, {}: {what}",
            seg.rate, seg.channels, seg.format
        )
    }

    /// A piece of the segment, conformed onto `out` (cleared first). Whole
    /// frames only; a trailing part waits for the next piece.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        out.clear();
        let Some(input) = self.input.as_mut() else {
            return;
        };
        if input.pass {
            out.extend_from_slice(bytes);
            return;
        }
        // Whole frames, the rest kept.
        let mut all: Vec<u8>;
        let bytes = if input.rest.is_empty() {
            bytes
        } else {
            all = std::mem::take(&mut input.rest);
            all.extend_from_slice(bytes);
            &all[..]
        };
        let whole = bytes.len() / input.frame_bytes * input.frame_bytes;
        input.rest = bytes[whole..].to_vec();
        let bytes = &bytes[..whole];
        // Every frame as the target's channels of float samples.
        let out_channels = self.current.channels as usize;
        self.samples.clear();
        let sample_bytes = input.layout.bytes();
        let mut frame_in = vec![0.0f32; input.channels];
        for frame in bytes.chunks_exact(input.frame_bytes) {
            for (c, s) in frame_in.iter_mut().enumerate() {
                let at = c * sample_bytes;
                *s = input.layout.to_i32(&frame[at..at + sample_bytes]) as f32 / 2_147_483_648.0;
            }
            map_channels(&frame_in, out_channels, &mut self.samples);
        }
        match input.resampler.as_mut() {
            None => write_s32(&self.samples, out),
            Some(r) => {
                input.pending.extend_from_slice(&self.samples);
                let need = r.input_frames_next() * out_channels;
                while input.pending.len() >= need {
                    let chunk = InterleavedSlice::new(
                        &input.pending[..need],
                        out_channels,
                        need / out_channels,
                    )
                    .expect("a chunk of frames");
                    if let Ok(made) = r.process(&chunk, None) {
                        write_s32(&made.take_data(), out);
                    }
                    input.pending.drain(..need);
                }
            }
        }
    }

    /// The segment ends: what the resampler still holds, onto `out`
    /// (appended), and the way cleared.
    pub fn end(&mut self, out: &mut Vec<u8>) {
        let Some(mut input) = self.input.take() else {
            return;
        };
        let Some(mut r) = input.resampler.take() else {
            return;
        };
        let out_channels = self.current.channels as usize;
        let frames = input.pending.len() / out_channels;
        if frames == 0 {
            return;
        }
        let need = r.input_frames_next() * out_channels;
        input.pending.resize(need, 0.0);
        let chunk =
            InterleavedSlice::new(&input.pending[..need], out_channels, need / out_channels)
                .expect("a chunk of frames");
        let indexing = rubato::Indexing::new().partial_len(frames);
        if let Ok(made) = r.process(&chunk, Some(&indexing)) {
            // Of the chunk made, the frames that came from the segment's own
            // samples; the rest is the padding.
            let keep = (frames as f64 * r.resample_ratio()).round() as usize;
            let data = made.take_data();
            let keep = keep.min(data.len() / out_channels);
            write_s32(&data[..keep * out_channels], out);
        }
    }
}

/// One frame's channels made `out` channels, onto `samples`: the same
/// count as they are; one channel to every output; more than wanted folded
/// to one (the first two averaged) or cut to the first two; fewer than
/// wanted padded with silence beyond the first.
fn map_channels(frame: &[f32], out: usize, samples: &mut Vec<f32>) {
    let inn = frame.len();
    if inn == out {
        samples.extend_from_slice(frame);
    } else if inn == 1 {
        samples.extend(std::iter::repeat_n(frame[0], out));
    } else if out == 1 {
        samples.push((frame[0] + frame[1]) * 0.5);
    } else if inn > out {
        samples.extend_from_slice(&frame[..out]);
    } else {
        samples.extend_from_slice(frame);
        samples.extend(std::iter::repeat_n(0.0, out - inn));
    }
}

/// A whole clip (interleaved floats, `channels` wide, at `rate`) made the
/// shape of a segment: the channels mapped as a stream's are, the rate
/// converted with the same windowed sinc. For an insert that is read once
/// and played into the stream; the filter's few milliseconds of delay lead it.
pub fn fit_clip(
    samples: &[f32],
    channels: usize,
    rate: u32,
    to_channels: usize,
    to_rate: u32,
) -> Vec<f32> {
    let channels = channels.max(1);
    let to_channels = to_channels.max(1);
    let mut mapped = Vec::with_capacity(samples.len() / channels * to_channels);
    for frame in samples.chunks_exact(channels) {
        map_channels(frame, to_channels, &mut mapped);
    }
    if rate == 0 || to_rate == 0 || rate == to_rate {
        return mapped;
    }
    let mut r = Async::<f32>::new_sinc(
        f64::from(to_rate) / f64::from(rate),
        1.0,
        &SincInterpolationParameters::default(),
        CHUNK,
        to_channels,
        FixedAsync::Input,
    )
    .expect("a sinc resampler for two rates");
    let mut out: Vec<f32> = Vec::with_capacity(mapped.len() * 2);
    let need = r.input_frames_next() * to_channels;
    let mut at = 0;
    while at + need <= mapped.len() {
        let chunk = InterleavedSlice::new(&mapped[at..at + need], to_channels, need / to_channels)
            .expect("a chunk of frames");
        if let Ok(made) = r.process(&chunk, None) {
            out.extend_from_slice(&made.take_data());
        }
        at += need;
    }
    let frames = (mapped.len() - at) / to_channels;
    if frames > 0 {
        let mut pending = mapped[at..].to_vec();
        pending.resize(need, 0.0);
        let chunk = InterleavedSlice::new(&pending[..need], to_channels, need / to_channels)
            .expect("a chunk of frames");
        let indexing = rubato::Indexing::new().partial_len(frames);
        if let Ok(made) = r.process(&chunk, Some(&indexing)) {
            let keep = (frames as f64 * r.resample_ratio()).round() as usize;
            let data = made.take_data();
            let keep = keep.min(data.len() / to_channels);
            out.extend_from_slice(&data[..keep * to_channels]);
        }
    }
    out
}

/// Float samples onto `out` as S32_LE: a sample that came in as an integer
/// goes out as the same integer, full scale clamped to the top value.
fn write_s32(samples: &[f32], out: &mut Vec<u8>) {
    out.reserve(samples.len() * 4);
    for s in samples {
        let v = (f64::from(*s) * 2_147_483_648.0).clamp(-2_147_483_648.0, 2_147_483_647.0) as i32;
        out.extend_from_slice(&v.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(format: &'static str, layout: Layout, rate: u32, channels: u32, frame: u32) -> Segment {
        Segment {
            format,
            layout,
            rate,
            channels,
            frame_bytes: frame,
            dsd_silence: false,
        }
    }

    fn s16() -> Layout {
        Layout::Linear {
            bytes: 2,
            valid: 16,
            signed: true,
            little: true,
        }
    }

    /// A sine of `hz` for `seconds` at `rate`, S16_LE, `channels` alike.
    fn sine(hz: f64, rate: u32, seconds: f64, channels: usize) -> Vec<u8> {
        let frames = (f64::from(rate) * seconds) as usize;
        let mut v = Vec::with_capacity(frames * 2 * channels);
        for i in 0..frames {
            let s = (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin();
            let s = (s * 32000.0) as i16;
            for _ in 0..channels {
                v.extend_from_slice(&s.to_le_bytes());
            }
        }
        v
    }

    fn channel(out: &[u8], channels: usize, which: usize) -> Vec<i32> {
        out.chunks_exact(4 * channels)
            .map(|f| {
                let at = 4 * which;
                i32::from_le_bytes([f[at], f[at + 1], f[at + 2], f[at + 3]])
            })
            .collect()
    }

    fn zero_crossings(v: &[i32]) -> usize {
        v.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count()
    }

    #[test]
    fn the_shape_is_the_operators_where_set_and_the_players_where_not() {
        let mut c = Conformer::new(48000, 2);
        assert_eq!(
            (
                c.shape().format,
                c.shape().rate,
                c.shape().channels,
                c.shape().frame_bytes
            ),
            ("s32le", 48000, 2, 8)
        );
        let mut out = Vec::new();
        c.segment(&seg("s16le", s16(), 96000, 6, 12), &mut out);
        assert_eq!((c.shape().rate, c.shape().channels), (48000, 2));
        // As played: the shape follows the segment in its own format, every bit
        // as it was, and the idle shape is the usual one.
        let mut f = Conformer::new(0, 0);
        assert_eq!((f.shape().rate, f.shape().channels), (44100, 2));
        f.segment(&seg("s16le", s16(), 96000, 1, 2), &mut out);
        assert_eq!(
            (
                f.shape().format,
                f.shape().rate,
                f.shape().channels,
                f.shape().frame_bytes
            ),
            ("s16le", 96000, 1, 2)
        );
        assert!(!f.resampling());
        f.push(&[1, 2, 3, 4], &mut out);
        assert_eq!(out, vec![1, 2, 3, 4]);
        // As played in rate, held in channels: a mono track is still made stereo.
        let mut g = Conformer::new(0, 2);
        g.segment(&seg("s16le", s16(), 96000, 1, 2), &mut out);
        assert_eq!(
            (g.shape().format, g.shape().rate, g.shape().channels),
            ("s32le", 96000, 2)
        );
    }

    #[test]
    fn a_segment_of_the_target_shape_passes_as_it_is() {
        let mut c = Conformer::new(44100, 2);
        let mut out = Vec::new();
        c.segment(&c.shape(), &mut out);
        assert!(!c.resampling());
        c.push(&[1, 2, 3, 4, 5, 6, 7, 8], &mut out);
        assert_eq!(out, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn s16_is_widened_and_mono_doubled_without_a_resampler() {
        let mut c = Conformer::new(44100, 2);
        let mut out = Vec::new();
        c.segment(&seg("s16le", s16(), 44100, 1, 2), &mut out);
        assert!(!c.resampling());
        // Two frames and a trailing byte: the byte waits.
        c.push(&[0x00, 0x40, 0xFF, 0x7F, 0x12], &mut out);
        assert_eq!(out.len(), 16);
        let l = channel(&out, 2, 0);
        assert_eq!(l, vec![0x4000_0000, 0x7FFF_0000]);
        assert_eq!(l, channel(&out, 2, 1));
        c.push(&[0x34], &mut out);
        assert_eq!(out.len(), 8);
        assert_eq!(channel(&out, 2, 0), vec![0x3412_0000]);
    }

    #[test]
    fn stereo_folds_to_mono_and_six_channels_cut_to_two() {
        let mut c = Conformer::new(44100, 1);
        let mut out = Vec::new();
        c.segment(&seg("s16le", s16(), 44100, 2, 4), &mut out);
        // L = 0x4000, R = 0x2000: the average is 0x3000.
        c.push(&[0x00, 0x40, 0x00, 0x20], &mut out);
        assert_eq!(channel(&out, 1, 0), vec![0x3000_0000]);
        let mut d = Conformer::new(44100, 2);
        d.segment(&seg("s16le", s16(), 44100, 6, 12), &mut out);
        d.push(
            &[0, 0x10, 0, 0x20, 0, 0x30, 0, 0x40, 0, 0x50, 0, 0x60],
            &mut out,
        );
        assert_eq!(out.len(), 8);
        assert_eq!(channel(&out, 2, 0), vec![0x1000_0000]);
        assert_eq!(channel(&out, 2, 1), vec![0x2000_0000]);
    }

    #[test]
    fn a_tone_at_48k_comes_out_at_44k1_as_the_same_tone() {
        let mut c = Conformer::new(44100, 2);
        let mut out = Vec::new();
        c.segment(&seg("s16le", s16(), 48000, 2, 4), &mut out);
        assert!(c.resampling());
        let tone = sine(1000.0, 48000, 1.0, 2);
        let mut all = Vec::new();
        for piece in tone.chunks(4096) {
            c.push(piece, &mut out);
            all.extend_from_slice(&out);
        }
        c.end(&mut all);
        let frames = all.len() / 8;
        // A second of 44.1 kHz, give or take the filter's own delay.
        assert!(
            (44100 - 300..=44100 + 300).contains(&frames),
            "{frames} frames"
        );
        let left = channel(&all, 2, 0);
        // 1 kHz crosses zero 2000 times a second.
        let zc = zero_crossings(&left[400..]);
        assert!((1900..=2100).contains(&zc), "{zc} zero crossings");
        // The level is kept: the peak near 32000/32768 of full scale.
        let peak = left.iter().map(|v| v.abs()).max().unwrap_or(0);
        assert!(peak > 2_000_000_000, "peak {peak}");
    }

    #[test]
    fn a_new_segment_flushes_the_one_before() {
        let mut c = Conformer::new(44100, 2);
        let mut out = Vec::new();
        c.segment(&seg("s16le", s16(), 48000, 2, 4), &mut out);
        // Half a chunk: nothing comes out yet.
        c.push(&sine(500.0, 48000, 0.01, 2), &mut out);
        assert!(out.is_empty());
        // The next segment flushes it: about 440 frames at the new rate.
        c.segment(&c.shape(), &mut out);
        let frames = out.len() / 8;
        assert!((400..=460).contains(&frames), "{frames} frames flushed");
    }
}
