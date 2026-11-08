//! An uploaded insert made the WAV the mixer plays: whatever a phone or a desk
//! hands over (AAC in M4A, MP3, FLAC, Ogg Vorbis, WAV) decoded once with
//! symphonia and written as 32-bit float WAV at its own rate and channels.
//! Run on an upload, outside the audio path; also as `outcast convert IN OUT`.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// What came out.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub rate: u32,
    pub channels: u32,
    pub frames: u64,
}

impl Info {
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / f64::from(self.rate.max(1))
    }
}

/// A clip longer than this is not an insert.
const MAX_SECONDS: f64 = 3.0 * 3600.0;

pub fn to_wav(input: &Path, output: &Path) -> Result<Info, String> {
    let file = File::open(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = input.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("not a sound file that is read: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("no sound track in it")?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| format!("a codec that is not read: {e}"))?;

    let out = File::create(output).map_err(|e| format!("{}: {e}", output.display()))?;
    let mut w = BufWriter::new(out);
    // The header, filled in at the end when the sizes are known.
    let mut rate = params.sample_rate.unwrap_or(0);
    let mut channels = params.channels.map_or(0, |c| c.count() as u32);
    let mut header_written = false;
    let mut frames: u64 = 0;
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(format!("reading it: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decoding it: {e}")),
        };
        let spec = *decoded.spec();
        if !header_written {
            rate = spec.rate;
            channels = spec.channels.count() as u32;
            if rate == 0 || channels == 0 {
                return Err("no rate or no channels in it".into());
            }
            write_header(&mut w, rate, channels, 0).map_err(|e| e.to_string())?;
            header_written = true;
        }
        let b =
            buf.get_or_insert_with(|| SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        if b.capacity() < decoded.capacity() * channels as usize {
            *b = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        }
        b.copy_interleaved_ref(decoded);
        let samples = b.samples();
        frames += (samples.len() / channels as usize) as u64;
        if frames as f64 / f64::from(rate) > MAX_SECONDS {
            return Err("longer than three hours".into());
        }
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        w.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    if !header_written {
        return Err("no sound in it".into());
    }
    w.flush().map_err(|e| e.to_string())?;
    let mut out = w.into_inner().map_err(|e| e.to_string())?;
    out.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    write_header(&mut out, rate, channels, frames).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())?;
    Ok(Info {
        rate,
        channels,
        frames,
    })
}

/// A WAV header for 32-bit float samples; `frames` 0 while the data is written.
fn write_header<W: Write>(w: &mut W, rate: u32, channels: u32, frames: u64) -> std::io::Result<()> {
    let data = (frames * u64::from(channels) * 4).min(u64::from(u32::MAX - 44)) as u32;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&3u16.to_le_bytes())?; // IEEE float
    w.write_all(&(channels as u16).to_le_bytes())?;
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * channels * 4).to_le_bytes())?;
    w.write_all(&((channels * 4) as u16).to_le_bytes())?;
    w.write_all(&32u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data.to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wav_comes_out_as_float_wav_with_its_rate_and_channels() {
        let dir = std::env::temp_dir().join(format!("outcast-convert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 100 ms of 16-bit stereo at 44.1 kHz, a ramp.
        let mut data = Vec::new();
        for i in 0..4410i32 {
            let v = ((i % 1000) * 30) as i16;
            data.extend_from_slice(&v.to_le_bytes());
            data.extend_from_slice(&(-v).to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&44100u32.to_le_bytes());
        wav.extend_from_slice(&(44100u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        let input = dir.join("in.wav");
        let output = dir.join("out.wav");
        std::fs::write(&input, &wav).unwrap();
        let info = to_wav(&input, &output).unwrap();
        assert_eq!((info.rate, info.channels, info.frames), (44100, 2, 4410));
        let clip = crate::mixer::load_wav(&output).unwrap();
        assert_eq!((clip.rate, clip.channels, clip.frames()), (44100, 2, 4410));
        assert!(
            (clip.samples[2] - 30.0 / 32768.0).abs() < 1e-4,
            "{}",
            clip.samples[2]
        );
        assert!(to_wav(&dir.join("absent.m4a"), &output).is_err());
        std::fs::write(dir.join("text.m4a"), b"not sound at all").unwrap();
        assert!(to_wav(&dir.join("text.m4a"), &output).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
