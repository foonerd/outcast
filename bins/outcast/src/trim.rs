//! The programme's trim: one attenuation, set by the operator, for everything
//! sent out. The tap takes the player's stream bit for bit, at the level its
//! tracks were mastered to, which for most music is full scale; a lossy
//! encoder fed that has no room for the peaks it makes of it, and they clip
//! at the listener. The trim takes the level down by so many dB before the
//! fan-out, after an insert is mixed in, so every uplink carries the same.
//! At 0 dB nothing is touched: what goes out is the player's stream, bit for
//! bit.

use tap::sample::Layout;

use crate::mixer::put_i32;

/// The furthest the trim goes down, in dB.
pub const MOST: f32 = -12.0;

/// A trim, with its gain worked out once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trim {
    db: f32,
    /// The gain in 31 fractional bits, for linear samples.
    q31: i64,
    /// The gain as a factor, for float samples.
    factor: f32,
}

impl Trim {
    /// A trim of `db`, to a tenth: between `MOST` and 0, anything beyond brought
    /// to the nearer end, and what is not a number to none. It never adds level.
    pub fn new(db: f32) -> Self {
        let db = if db.is_finite() {
            (db.clamp(MOST, 0.0) * 10.0).round() / 10.0
        } else {
            0.0
        };
        let factor = 10f64.powf(f64::from(db) / 20.0);
        Self {
            db: if db == 0.0 { 0.0 } else { db },
            q31: (factor * 2_147_483_648.0).round() as i64,
            factor: factor as f32,
        }
    }

    pub fn none() -> Self {
        Self::new(0.0)
    }

    /// The trim in dB: 0 or below.
    pub fn db(&self) -> f32 {
        self.db
    }

    pub fn is_none(&self) -> bool {
        self.db == 0.0
    }

    /// Every sample of `bytes`, laid out as `layout`, taken down by the trim
    /// in place. A linear sample is scaled in 64 bits and rounded to the
    /// nearest; a float is multiplied, so one past full scale comes back under
    /// it rather than being cut off first. One-bit audio has no level to take
    /// down and is left.
    pub fn apply(&self, layout: &Layout, bytes: &mut [u8]) {
        if self.is_none() {
            return;
        }
        match *layout {
            Layout::Linear { bytes: n, .. } => {
                for slot in bytes.chunks_exact_mut(usize::from(n).max(1)) {
                    let sample = i64::from(layout.to_i32(slot));
                    let trimmed = (sample * self.q31 + (1 << 30)) >> 31;
                    put_i32(layout, trimmed as i32, slot);
                }
            }
            Layout::Float { little } => {
                let (samples, _) = bytes.as_chunks_mut::<4>();
                for slot in samples {
                    if little {
                        *slot = (f32::from_le_bytes(*slot) * self.factor).to_le_bytes();
                    } else {
                        *slot = (f32::from_be_bytes(*slot) * self.factor).to_be_bytes();
                    }
                }
            }
            Layout::Dsd { .. } => {}
        }
    }
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
    const S32: Layout = Layout::Linear {
        bytes: 4,
        valid: 32,
        signed: true,
        little: true,
    };

    #[test]
    fn a_trim_is_between_none_and_the_most_to_a_tenth_and_never_adds() {
        assert!(Trim::new(0.0).is_none() && Trim::none().is_none());
        assert!(Trim::new(-0.0).is_none() && Trim::new(-0.04).is_none());
        assert_eq!(Trim::new(-0.04).db().to_bits(), 0f32.to_bits());
        assert!(
            Trim::new(3.0).is_none(),
            "level is never added: a clip is no cure"
        );
        assert!(Trim::new(f32::NAN).is_none() && Trim::new(f32::INFINITY).is_none());
        assert_eq!(Trim::new(-3.04).db(), -3.0);
        assert_eq!(Trim::new(-3.25).db(), -3.3);
        assert_eq!(Trim::new(-40.0).db(), MOST);
        assert_eq!(Trim::new(f32::NEG_INFINITY).db(), 0.0);
    }

    #[test]
    fn no_trim_touches_no_byte() {
        let mut bytes: Vec<u8> = (0..=255).collect();
        let before = bytes.clone();
        Trim::none().apply(&S16, &mut bytes);
        Trim::none().apply(&Layout::Float { little: true }, &mut bytes);
        assert_eq!(bytes, before);
    }

    #[test]
    fn linear_samples_come_down_by_the_trim_rounded_to_the_nearest() {
        // Six dB down is a factor of 0.501187: full scale either way, a small
        // sample, one that rounds up and one that rounds down, and nothing.
        let trim = Trim::new(-6.0);
        let samples: [i32; 7] = [i32::MAX, i32::MIN, 1_000_000, -1_000_000, 3, -3, 0];
        let mut bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        trim.apply(&S32, &mut bytes);
        let got: Vec<i32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes(*c))
            .collect();
        let want: Vec<i32> = samples
            .iter()
            .map(|s| (f64::from(*s) * 10f64.powf(-6.0 / 20.0)).round() as i32)
            .collect();
        for ((g, w), s) in got.iter().zip(&want).zip(&samples) {
            assert!((g - w).abs() <= 1, "{s} came to {g}, not {w}");
        }
        assert_eq!(got[6], 0, "silence stays silence");
        assert_eq!(got[4], 2, "3 at -6 dB is 1.5, rounded to the nearest");
        assert!(
            got[0] < i32::MAX / 2 + 3_000_000 && got[0] > i32::MAX / 2,
            "full scale comes to half and a little: {}",
            got[0]
        );
        // The most there is: twelve dB, a quarter and a little.
        let mut bytes = i32::MAX.to_le_bytes().to_vec();
        Trim::new(MOST).apply(&S32, &mut bytes);
        let v = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert!((f64::from(v) / f64::from(i32::MAX) - 0.251_188_6).abs() < 1e-6);
    }

    #[test]
    fn a_narrower_linear_sample_is_trimmed_in_its_own_width() {
        // What the hub does not send (it widens first), kept right all the same.
        let mut bytes: Vec<u8> = [20_000i16, -20_000, 1]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        Trim::new(-6.0).apply(&S16, &mut bytes);
        let got: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect();
        assert!(
            (got[0] - 10_024).abs() <= 1 && (got[1] + 10_024).abs() <= 1,
            "{got:?}"
        );
    }

    #[test]
    fn a_float_past_full_scale_comes_back_under_it() {
        let trim = Trim::new(-3.0);
        let samples = [1.2f32, -1.2, 0.5, 0.0];
        for little in [true, false] {
            let mut bytes: Vec<u8> = samples
                .iter()
                .flat_map(|s| {
                    if little {
                        s.to_le_bytes()
                    } else {
                        s.to_be_bytes()
                    }
                })
                .collect();
            trim.apply(&Layout::Float { little }, &mut bytes);
            let got: Vec<f32> = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| {
                    if little {
                        f32::from_le_bytes(*c)
                    } else {
                        f32::from_be_bytes(*c)
                    }
                })
                .collect();
            assert!((got[0] - 0.849_5).abs() < 1e-3, "{got:?}");
            assert!((got[1] + 0.849_5).abs() < 1e-3, "{got:?}");
            assert!((got[2] - 0.354).abs() < 1e-3, "{got:?}");
            assert_eq!(got[3], 0.0);
        }
    }

    #[test]
    fn one_bit_audio_is_left() {
        let mut bytes = vec![0x69u8; 8];
        Trim::new(-6.0).apply(&Layout::Dsd { bytes: 1 }, &mut bytes);
        assert_eq!(bytes, vec![0x69u8; 8]);
    }
}
