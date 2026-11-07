//! An encoder: one evopeg child per segment and uplink, reading raw PCM on
//! its standard input and writing the encoded, muxed stream on its standard
//! output, or into a file it names itself when a recording asks for one.
//! The child is told nothing of the network. Its standard error is read on
//! a thread and logged a line at a time.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::Codec;
use stream::feed::Segment;

/// Bytes read from the child's output in one go.
const READ: usize = 1 << 14;

/// What an encoder is asked to make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipe {
    pub codec: Codec,
    /// Kilobits per second, for a lossy codec; 0 for the codec's own choice.
    pub bitrate: u32,
    /// The output sample rate, 0 for the stream's own.
    pub rate: u32,
    /// The title written into the stream where the container carries one.
    pub title: String,
    /// Where the output goes: a pipe the caller reads, or a file the child writes.
    pub output: Output,
    /// The container: the codec's own, or an MPEG transport stream for SRT.
    pub container: Container,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Container {
    /// What the codec is usually carried in: MP3 and ADTS bare, Ogg for Opus,
    /// Vorbis and streamed FLAC, FLAC and WAV files.
    #[default]
    Native,
    /// An MPEG transport stream, every codec the muxer takes.
    MpegTs,
    /// FLV, for RTMP: AAC or MP3.
    Flv,
    /// Bare frames for a segmenter: ADTS for AAC, MP3 frames without an ID3 tag
    /// or a Xing frame.
    Frames,
    /// Fragmented MP4 as CMAF wants it (evopeg 0.2.0 or later): the init segment
    /// once, then a fragment every so many seconds, each on the pipe as it closes.
    Fmp4 { fragment_seconds: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// Standard output, read by the caller.
    Pipe,
    /// A file, written by the child; `WAV` and `FLAC` finish their headers
    /// when the file is closed, which a pipe would not allow.
    File(PathBuf),
}

/// evopeg's arguments for a segment and a recipe.
pub fn arguments(segment: &Segment, recipe: &Recipe) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-nostdin".into(),
        "-f".into(),
        segment.format.into(),
        "-ar".into(),
        segment.rate.to_string(),
        "-ac".into(),
        segment.channels.to_string(),
        "-i".into(),
        "pipe:0".into(),
    ];
    if recipe.rate != 0 && recipe.rate != segment.rate {
        a.push("-ar".into());
        a.push(recipe.rate.to_string());
    }
    let (encoder, muxer): (&str, &str) = match (recipe.codec, &recipe.output) {
        (Codec::Mp3, _) => ("libmp3lame", "mp3"),
        (Codec::Mp2, _) => ("mp2", "mp2"),
        (Codec::Aac, _) => ("aac", "adts"),
        (Codec::Opus, _) => ("libopus", "ogg"),
        (Codec::Vorbis, _) => ("libvorbis", "ogg"),
        // FLAC streamed goes in Ogg, so a listener joining late can take the
        // headers; a FLAC file is a FLAC file.
        (Codec::Flac, Output::Pipe) => ("flac", "ogg"),
        (Codec::Flac, Output::File(_)) => ("flac", "flac"),
        (Codec::Wav, _) => ("pcm_s16le", "wav"),
    };
    a.push("-c:a".into());
    a.push(encoder.into());
    if recipe.codec.lossy() && recipe.bitrate > 0 {
        a.push("-b:a".into());
        a.push(format!("{}k", recipe.bitrate));
    }
    if !recipe.title.is_empty() {
        a.push("-metadata".into());
        a.push(format!("title={}", recipe.title));
    }
    if recipe.container == Container::Frames && recipe.codec == Codec::Mp3 {
        // Bare frames: no ID3 tag at the head, no Xing frame, so a cut on a frame
        // boundary is a cut on a frame boundary.
        a.push("-id3v2_version".into());
        a.push("0".into());
        a.push("-write_xing".into());
        a.push("0".into());
    }
    a.push("-f".into());
    a.push(match recipe.container {
        Container::Native | Container::Frames => muxer.into(),
        Container::MpegTs => "mpegts".into(),
        Container::Flv => "flv".into(),
        Container::Fmp4 { .. } => "mp4".into(),
    });
    if let Container::Fmp4 { fragment_seconds } = recipe.container {
        // The moov before any sample (a pipe cannot seek back), fragments by
        // duration and never by keyframe (every audio sample is one), each fragment
        // out as it closes.
        a.push("-movflags".into());
        a.push("+cmaf".into());
        a.push("-frag_duration".into());
        a.push(format!(
            "{}",
            u64::from(fragment_seconds.max(1)) * 1_000_000
        ));
        a.push("-flush_packets".into());
        a.push("1".into());
    }
    match &recipe.output {
        Output::Pipe => a.push("pipe:1".into()),
        Output::File(path) => {
            a.push("-y".into());
            a.push(path.to_string_lossy().into_owned());
        }
    }
    a
}

/// A running encoder.
pub struct Encoder {
    child: Child,
    stdin: Option<ChildStdin>,
    reader: Option<JoinHandle<()>>,
    errors: Option<JoinHandle<()>>,
    pub bytes_in: u64,
}

impl Encoder {
    /// Start evopeg for a segment and a recipe. Output on a pipe is read on a
    /// thread and sent on `out` in pieces; a sender that is gone ends the
    /// reading. `log` prefixes the child's own lines.
    pub fn start(
        binary: &Path,
        segment: &Segment,
        recipe: &Recipe,
        out: Option<SyncSender<Arc<Vec<u8>>>>,
        log: &str,
    ) -> std::io::Result<Self> {
        let args = arguments(segment, recipe);
        let mut child = Command::new(binary)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(if matches!(recipe.output, Output::Pipe) {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let reader = match (child.stdout.take(), out) {
            (Some(mut stdout), Some(tx)) => Some(
                std::thread::Builder::new()
                    .name("encoder-out".into())
                    .spawn(move || {
                        let mut buf = vec![0u8; READ];
                        loop {
                            match stdout.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    if tx.send(Arc::new(buf[..n].to_vec())).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    })?,
            ),
            _ => None,
        };
        let errors = child.stderr.take().map(|stderr| {
            let prefix = log.to_string();
            std::thread::Builder::new()
                .name("encoder-err".into())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        if !line.trim().is_empty() {
                            eprintln!("outcast: {prefix}: evopeg: {line}");
                        }
                    }
                })
                .expect("encoder-err thread")
        });
        Ok(Self {
            child,
            stdin,
            reader,
            errors,
            bytes_in: 0,
        })
    }

    /// Raw PCM to the child. An error means the child is gone.
    pub fn feed(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match self.stdin.as_mut() {
            Some(stdin) => {
                stdin.write_all(bytes)?;
                self.bytes_in += bytes.len() as u64;
                Ok(())
            }
            None => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "encoder closed",
            )),
        }
    }

    /// The input closed, the child given `grace` to write what it holds and
    /// finish its file, then killed if it has not; the output drained.
    pub fn finish(mut self, grace: Duration) {
        drop(self.stdin.take());
        let until = Instant::now() + grace;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        if let Some(e) = self.errors.take() {
            let _ = e.join();
        }
    }

    /// Killed at once: for an uplink going down or a connection lost, where
    /// what the child holds is not wanted.
    pub fn kill(mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        if let Some(e) = self.errors.take() {
            let _ = e.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment() -> Segment {
        Segment {
            format: "s32le",
            layout: tap::sample::Layout::Linear {
                bytes: 4,
                valid: 32,
                signed: true,
                little: true,
            },
            rate: 96000,
            channels: 2,
            frame_bytes: 8,
            dsd_silence: false,
        }
    }

    #[test]
    fn an_mp3_stream_at_44100_with_a_title() {
        let a = arguments(
            &segment(),
            &Recipe {
                codec: Codec::Mp3,
                bitrate: 192,
                rate: 44100,
                title: "Artist - Title".into(),
                output: Output::Pipe,
                container: Container::Native,
            },
        );
        let s = a.join(" ");
        assert!(s.starts_with("-hide_banner -loglevel error -nostdin -f s32le -ar 96000 -ac 2 -i pipe:0 -ar 44100 -c:a libmp3lame -b:a 192k -metadata title=Artist - Title -f mp3 pipe:1"), "{s}");
    }

    #[test]
    fn bare_mp3_frames_carry_no_tag_and_no_xing() {
        let seg = segment();
        let a = arguments(
            &seg,
            &Recipe {
                codec: Codec::Mp3,
                bitrate: 128,
                rate: 44100,
                title: String::new(),
                output: Output::Pipe,
                container: Container::Frames,
            },
        );
        let s = a.join(" ");
        assert!(
            s.contains("-id3v2_version 0 -write_xing 0 -f mp3 pipe:1"),
            "{s}"
        );
        let a = arguments(
            &seg,
            &Recipe {
                codec: Codec::Aac,
                bitrate: 128,
                rate: 48000,
                title: String::new(),
                output: Output::Pipe,
                container: Container::Frames,
            },
        );
        let s = a.join(" ");
        assert!(s.ends_with("-f adts pipe:1") && !s.contains("xing"), "{s}");
    }

    #[test]
    fn fragmented_mp4_takes_cmaf_fragments_by_duration_flushed() {
        let a = arguments(
            &segment(),
            &Recipe {
                codec: Codec::Flac,
                bitrate: 0,
                rate: 48000,
                title: String::new(),
                output: Output::Pipe,
                container: Container::Fmp4 {
                    fragment_seconds: 4,
                },
            },
        );
        let s = a.join(" ");
        assert!(
            s.ends_with(
                "-c:a flac -f mp4 -movflags +cmaf -frag_duration 4000000 -flush_packets 1 pipe:1"
            ),
            "{s}"
        );
        assert!(!s.contains("frag_keyframe"));
    }

    #[test]
    fn flv_takes_the_flv_muxer() {
        let a = arguments(
            &segment(),
            &Recipe {
                codec: Codec::Aac,
                bitrate: 128,
                rate: 44100,
                title: String::new(),
                output: Output::Pipe,
                container: Container::Flv,
            },
        );
        assert!(a.join(" ").ends_with("-c:a aac -b:a 128k -f flv pipe:1"));
    }

    #[test]
    fn a_transport_stream_takes_the_mpegts_muxer_whatever_the_codec() {
        for (codec, enc) in [
            (Codec::Aac, "aac"),
            (Codec::Mp2, "mp2"),
            (Codec::Mp3, "libmp3lame"),
        ] {
            let a = arguments(
                &segment(),
                &Recipe {
                    codec,
                    bitrate: 128,
                    rate: 48000,
                    title: String::new(),
                    output: Output::Pipe,
                    container: Container::MpegTs,
                },
            );
            let s = a.join(" ");
            assert!(
                s.ends_with(&format!("-ar 48000 -c:a {enc} -b:a 128k -f mpegts pipe:1")),
                "{s}"
            );
        }
    }

    #[test]
    fn a_flac_file_keeps_the_rate_and_takes_no_bitrate() {
        let a = arguments(
            &segment(),
            &Recipe {
                codec: Codec::Flac,
                bitrate: 192,
                rate: 0,
                title: String::new(),
                output: Output::File(PathBuf::from("/tmp/x.flac")),
                container: Container::Native,
            },
        );
        let s = a.join(" ");
        assert!(
            s.ends_with("-i pipe:0 -c:a flac -f flac -y /tmp/x.flac"),
            "{s}"
        );
        assert!(!s.contains("-b:a") && !s.contains("-metadata"));
    }

    #[test]
    fn flac_streamed_goes_in_ogg_and_opus_too() {
        let flac = arguments(
            &segment(),
            &Recipe {
                codec: Codec::Flac,
                bitrate: 0,
                rate: 0,
                title: String::new(),
                output: Output::Pipe,
                container: Container::Native,
            },
        );
        assert!(flac.join(" ").ends_with("-c:a flac -f ogg pipe:1"));
        let opus = arguments(
            &segment(),
            &Recipe {
                codec: Codec::Opus,
                bitrate: 128,
                rate: 48000,
                title: String::new(),
                output: Output::Pipe,
                container: Container::Native,
            },
        );
        assert!(opus
            .join(" ")
            .ends_with("-ar 48000 -c:a libopus -b:a 128k -f ogg pipe:1"));
    }
}
