//! The mixer: an insert (a jingle, a station ID, an announcement) played into
//! the stream over the programme, the programme ducked under it and brought
//! back after; the room's audio is the player's and untouched. It sits in the
//! hub between the ring's pieces and the fan-out to the uplinks, mixing in
//! place in the segment's own sample layout, so every uplink carries the same
//! mix and nothing is added to the latency. Inserts are WAV files read whole;
//! one plays at a time, the next waits.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use stream::conform::fit_clip;
use stream::feed::Segment;
use tap::sample::Layout;

/// The programme is taken down to the duck over this long before the insert.
const DUCK_IN_MS: u32 = 300;
/// And brought back over this long after it.
const DUCK_OUT_MS: u32 = 600;
/// The insert's own edges.
const EDGE_MS: u32 = 10;
/// A file larger than this is not an insert.
const MAX_WAV: u64 = 64 * 1024 * 1024;

/// A clip as read: interleaved floats between minus one and one.
pub struct Clip {
    pub rate: u32,
    pub channels: u32,
    pub samples: Vec<f32>,
}

impl Clip {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
}

/// An insert waiting its turn.
pub struct Insert {
    pub name: String,
    pub clip: Arc<Clip>,
    /// The programme's level under the insert, in dB; −60 is a cut.
    pub duck_db: f32,
}

/// What happened in a piece, for the status and the page.
#[derive(Debug, PartialEq)]
pub enum Moment {
    Started(String),
    Ended(String),
    /// Not played: the stream is one-bit audio, or its shape changed under it.
    Skipped(String, String),
}

struct Playing {
    name: String,
    /// The clip in the segment's channels and rate.
    fitted: Vec<f32>,
    channels: usize,
    rate: u32,
    /// Frames of duck-in before the clip, frames of the clip, frames of duck-out after.
    lead: usize,
    body: usize,
    tail: usize,
    /// The frame of the whole (lead, body, tail) the next piece starts at.
    pos: usize,
    duck: f32,
}

#[derive(Default)]
pub struct Mixer {
    queue: VecDeque<Insert>,
    playing: Option<Playing>,
}

impl Mixer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, insert: Insert) {
        self.queue.push_back(insert);
    }

    /// The insert playing now, by name.
    pub fn playing(&self) -> Option<&str> {
        self.playing.as_ref().map(|p| p.name.as_str())
    }

    pub fn waiting(&self) -> usize {
        self.queue.len()
    }

    /// One piece of the stream in the segment's shape, mixed in place.
    pub fn process(&mut self, seg: &Segment, bytes: &mut [u8]) -> Vec<Moment> {
        let mut moments = Vec::new();
        let channels = seg.channels.max(1) as usize;
        let frame_bytes = seg.frame_bytes.max(1) as usize;
        let sample_bytes = seg.layout.bytes();
        // An insert whose stream changed shape under it ends here.
        if let Some(p) = self.playing.as_ref() {
            if p.channels != channels || p.rate != seg.rate {
                let p = self.playing.take().expect("playing");
                moments.push(Moment::Skipped(p.name, "the stream changed shape".into()));
            }
        }
        // The next insert, when nothing plays.
        if self.playing.is_none() {
            if let Some(next) = self.queue.pop_front() {
                if seg.layout.is_dsd() || seg.dsd_silence || sample_bytes * channels != frame_bytes
                {
                    moments.push(Moment::Skipped(
                        next.name,
                        "the stream is one-bit audio, which is not mixed".into(),
                    ));
                    return moments;
                }
                let fitted = fit_clip(
                    &next.clip.samples,
                    next.clip.channels as usize,
                    next.clip.rate,
                    channels,
                    seg.rate,
                );
                let ms = |n: u32| (u64::from(seg.rate) * u64::from(n) / 1000) as usize;
                let body = fitted.len() / channels;
                self.playing = Some(Playing {
                    name: next.name.clone(),
                    fitted,
                    channels,
                    rate: seg.rate,
                    lead: ms(DUCK_IN_MS),
                    body,
                    tail: ms(DUCK_OUT_MS),
                    pos: 0,
                    duck: 10f32.powf(next.duck_db.clamp(-60.0, 0.0) / 20.0),
                });
                moments.push(Moment::Started(next.name));
            }
        }
        let Some(p) = self.playing.as_mut() else {
            return moments;
        };
        let edge = (u64::from(p.rate) * u64::from(EDGE_MS) / 1000).max(1) as usize;
        let total = p.lead + p.body + p.tail;
        let mut ended = false;
        for frame in bytes.chunks_exact_mut(frame_bytes) {
            if p.pos >= total {
                ended = true;
                break;
            }
            let t = p.pos;
            // The programme's gain: down over the lead, held under the body, back over the tail.
            let gain = if t < p.lead {
                1.0 - (1.0 - p.duck) * (t as f32 / p.lead.max(1) as f32)
            } else if t < p.lead + p.body {
                p.duck
            } else {
                p.duck + (1.0 - p.duck) * ((t - p.lead - p.body) as f32 / p.tail.max(1) as f32)
            };
            // The insert's sample, its edges faded.
            let in_body = t >= p.lead && t < p.lead + p.body;
            let (jframe, jgain) = if in_body {
                let i = t - p.lead;
                let from_end = p.body - 1 - i;
                let g = (i.min(from_end).min(edge) as f32 / edge as f32).min(1.0);
                (Some(i), g)
            } else {
                (None, 0.0)
            };
            for c in 0..channels {
                let at = c * sample_bytes;
                let slot = &mut frame[at..at + sample_bytes];
                let programme = seg.layout.to_i32(slot) as f32 / 2_147_483_648.0;
                let insert = jframe.map_or(0.0, |i| p.fitted[i * channels + c] * jgain);
                put(&seg.layout, programme * gain + insert, slot);
            }
            p.pos += 1;
        }
        if ended || p.pos >= total {
            let p = self.playing.take().expect("playing");
            moments.push(Moment::Ended(p.name));
        }
        moments
    }
}

/// A float between minus one and one into the sample at the start of `out`,
/// in the layout's own width and order: the inverse of `Layout::to_i32`.
pub fn put(layout: &Layout, value: f32, out: &mut [u8]) {
    let v = (f64::from(value) * 2_147_483_648.0).clamp(-2_147_483_648.0, 2_147_483_647.0) as i32;
    match *layout {
        Layout::Linear {
            bytes: n,
            valid,
            signed,
            little,
        } => {
            let n = n as usize;
            if out.len() < n {
                return;
            }
            let mut s = v;
            if !signed {
                s ^= i32::MIN;
            }
            let u = (s as u32).wrapping_shr(32 - valid as u32);
            for i in 0..n {
                let b = ((u >> (8 * i)) & 0xff) as u8;
                if little {
                    out[i] = b;
                } else {
                    out[n - 1 - i] = b;
                }
            }
        }
        Layout::Float { little } => {
            if out.len() < 4 {
                return;
            }
            let f = value.clamp(-1.0, 1.0);
            let raw = if little {
                f.to_le_bytes()
            } else {
                f.to_be_bytes()
            };
            out[..4].copy_from_slice(&raw);
        }
        Layout::Dsd { .. } => {}
    }
}

/// A WAV file read whole: PCM 8, 16, 24 or 32 bits, or 32- or 64-bit float,
/// plain or extensible, any rate, any channels.
pub fn load_wav(path: &Path) -> Result<Clip, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_WAV {
        return Err(format!(
            "{} MB is too large for an insert",
            size / 1_048_576
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    read_wav(&bytes)
}

#[allow(clippy::chunks_exact_to_as_chunks)]
pub fn read_wav(bytes: &[u8]) -> Result<Clip, String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let mut at = 12;
    let mut format: Option<(u16, u16, u32, u16)> = None; // tag, channels, rate, bits
    let mut data: Option<&[u8]> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32_at(at + 4) as usize;
        let body_at = at + 8;
        let body_end = body_at.saturating_add(len).min(bytes.len());
        let body = &bytes[body_at..body_end];
        if id == b"fmt " {
            if body.len() < 16 {
                return Err("a WAV whose format chunk is short".into());
            }
            let mut tag = u16_at(body_at);
            let channels = u16_at(body_at + 2);
            let rate = u32_at(body_at + 4);
            let bits = u16_at(body_at + 14);
            if tag == 0xFFFE && body.len() >= 26 {
                // Extensible: the real format leads the sub-format GUID.
                tag = u16_at(body_at + 24);
            }
            format = Some((tag, channels, rate, bits));
        } else if id == b"data" {
            data = Some(body);
        }
        at = body_at + len + (len & 1);
    }
    let (tag, channels, rate, bits) = format.ok_or("a WAV without a format chunk")?;
    let data = data.ok_or("a WAV without a data chunk")?;
    if channels == 0 || rate == 0 {
        return Err("a WAV with no channels or no rate".into());
    }
    let samples: Vec<f32> = match (tag, bits) {
        (1, 8) => data
            .iter()
            .map(|b| (f32::from(*b) - 128.0) / 128.0)
            .collect(),
        (1, 16) => data
            .chunks_exact(2)
            .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
            .collect(),
        (1, 24) => data
            .chunks_exact(3)
            .map(|c| (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f32 / 8_388_608.0)
            .collect(),
        (1, 32) => data
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0)
            .collect(),
        (3, 32) => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]).clamp(-1.0, 1.0))
            .collect(),
        (3, 64) => data
            .chunks_exact(8)
            .map(|c| {
                let mut raw = [0u8; 8];
                raw.copy_from_slice(c);
                (f64::from_le_bytes(raw).clamp(-1.0, 1.0)) as f32
            })
            .collect(),
        _ => {
            return Err(format!(
                "a WAV of format {tag} at {bits} bits, which is not read"
            ))
        }
    };
    Ok(Clip {
        rate,
        channels: u32::from(channels),
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const S16: Layout = Layout::Linear {
        bytes: 2,
        valid: 16,
        signed: true,
        little: true,
    };

    fn wav16(rate: u32, channels: u16, frames: usize, value: f32) -> Vec<u8> {
        let mut data = Vec::new();
        for _ in 0..frames * channels as usize {
            data.extend_from_slice(&((value * 32767.0) as i16).to_le_bytes());
        }
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        w.extend_from_slice(&(channels * 2).to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(data.len() as u32).to_le_bytes());
        w.extend_from_slice(&data);
        w
    }

    fn seg(rate: u32, channels: u32) -> Segment {
        Segment {
            format: "s16le",
            layout: S16,
            rate,
            channels,
            frame_bytes: 2 * channels,
            dsd_silence: false,
        }
    }

    #[test]
    fn a_wav_is_read_and_a_sample_goes_back_as_it_came() {
        let clip = read_wav(&wav16(48000, 2, 480, 0.5)).unwrap();
        assert_eq!((clip.rate, clip.channels, clip.frames()), (48000, 2, 480));
        assert!((clip.samples[0] - 0.5).abs() < 0.001);
        assert!(read_wav(b"RIFF....WAVX").is_err());
        for layout in [
            S16,
            Layout::Linear {
                bytes: 3,
                valid: 24,
                signed: true,
                little: true,
            },
            Layout::Linear {
                bytes: 4,
                valid: 24,
                signed: true,
                little: true,
            },
            Layout::Linear {
                bytes: 4,
                valid: 32,
                signed: true,
                little: false,
            },
            Layout::Linear {
                bytes: 2,
                valid: 16,
                signed: false,
                little: true,
            },
            Layout::Float { little: true },
        ] {
            for v in [-0.75f32, -0.001, 0.0, 0.25, 0.9999] {
                let mut raw = [0u8; 4];
                put(&layout, v, &mut raw);
                let back = layout.to_i32(&raw) as f32 / 2_147_483_648.0;
                assert!(
                    (back - v).abs() < 1.0 / 30000.0,
                    "{layout:?}: {v} came back {back}"
                );
            }
        }
    }

    #[test]
    fn an_insert_ducks_the_programme_plays_and_lets_it_back() {
        let mut m = Mixer::new();
        let clip = Arc::new(read_wav(&wav16(48000, 2, 9600, 0.25)).unwrap()); // 200 ms
        m.insert(Insert {
            name: "ID".into(),
            clip,
            duck_db: -60.0,
        });
        let s = seg(48000, 2);
        // A piece of 10 ms of programme at 0.5.
        let piece = |frames: usize| -> Vec<u8> {
            let mut b = Vec::new();
            for _ in 0..frames * 2 {
                b.extend_from_slice(&((0.5f32 * 32767.0) as i16).to_le_bytes());
            }
            b
        };
        let level = |b: &[u8], frame: usize| {
            S16.to_i32(&b[frame * 4..frame * 4 + 2]) as f32 / 2_147_483_648.0
        };
        // The first piece: the insert starts; the lead ducks the programme from 0.5 down.
        let mut b = piece(480);
        let moments = m.process(&s, &mut b);
        assert_eq!(moments, vec![Moment::Started("ID".into())]);
        assert!(
            level(&b, 0) > 0.49,
            "the first frame is still the programme"
        );
        assert!(
            level(&b, 479) < 0.49 && level(&b, 479) > 0.0,
            "the duck has begun"
        );
        assert_eq!(m.playing(), Some("ID"));
        // 300 ms of lead in all: 30 pieces; then 100 ms into the body of 200 ms, past the insert's own edge: the insert at 0.25 over a cut programme.
        for _ in 0..39 {
            m.process(&s, &mut piece(480));
        }
        let mut b = piece(480);
        m.process(&s, &mut b);
        assert!(
            (level(&b, 240) - 0.25).abs() < 0.02,
            "the insert plays over the ducked programme: {}",
            level(&b, 240)
        );
        // The second half of the body, then the tail of 600 ms: 60 pieces, the programme back at the end.
        let mut ended = false;
        for _ in 0..100 {
            let mut b = piece(480);
            if m.process(&s, &mut b).contains(&Moment::Ended("ID".into())) {
                ended = true;
                assert!(
                    level(&b, 479) > 0.49,
                    "the programme is back when the insert ends"
                );
                break;
            }
        }
        assert!(ended);
        assert_eq!(m.playing(), None);
        // A one-bit stream skips the insert.
        m.insert(Insert {
            name: "ID2".into(),
            clip: Arc::new(read_wav(&wav16(44100, 1, 100, 0.1)).unwrap()),
            duck_db: -12.0,
        });
        let mut dsd = seg(2822400, 2);
        dsd.layout = Layout::Dsd { bytes: 1 };
        let moments = m.process(&dsd, &mut piece(10));
        assert!(matches!(moments.first(), Some(Moment::Skipped(n, _)) if n == "ID2"));
    }

    #[test]
    fn a_clip_at_another_rate_and_width_is_fitted_to_the_stream() {
        let clip = read_wav(&wav16(44100, 1, 4410, 0.3)).unwrap(); // 100 ms mono at 44.1 kHz
        let fitted = fit_clip(&clip.samples, 1, 44100, 2, 48000);
        let frames = fitted.len() / 2;
        assert!(
            (4700..=4900).contains(&frames),
            "100 ms at 48 kHz, give or take the filter: {frames}"
        );
        assert!(
            (fitted[frames] - 0.3).abs() < 0.02,
            "the level is kept: {}",
            fitted[frames]
        );
        assert_eq!(fitted[frames], fitted[frames + 1], "mono made stereo");
    }
}
