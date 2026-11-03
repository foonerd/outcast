//! A reference client for outcast-lite, for a native remote to lift: every
//! text frame as a line on standard error, the stream's PCM on standard
//! output as it comes, or as 16-bit whatever it comes as with `--s16`. Pipe
//! it into a player to hear it:
//!
//! `lite-client ws://player:5582/anymote/listen --s16 | aplay -f S16_LE -r 44100 -c 2`
//!
//! Built with `cargo build -p lite-client --example lite-client`, for this
//! machine or, with cargo-ndk, for an Android ABI. What a remote does
//! instead of writing to standard output: queue the samples on its audio
//! device at the segment's rate and channels, start the device when 200 ms
//! are queued, and clear the queue on `Gap`, `End` and a new `Segment`; see
//! README.md, "Playing it in a native remote".

use std::io::Write;

use lite_client::{Client, Event, Shape};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let url = args.iter().find(|a| a.starts_with("ws")).cloned();
    let s16 = args.iter().any(|a| a == "--s16");
    let Some(url) = url else {
        eprintln!("lite-client ws://host:port/path [--s16]");
        std::process::exit(64);
    };
    let mut client = match Client::connect(&url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lite-client: cannot connect to {url}: {e}");
            std::process::exit(1);
        }
    };
    let mut out = std::io::stdout().lock();
    let mut shape: Option<Shape> = None;
    let mut samples: Vec<i16> = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        let event = match client.event() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("lite-client: the lite went away: {e}");
                break;
            }
        };
        match event {
            Event::Hello { version, tag } => {
                eprintln!("lite-client: hello from outcast-lite {version}, tag {tag}")
            }
            Event::Segment(s) => {
                eprintln!(
                    "lite-client: segment {} Hz, {} ch, {}{}{}",
                    s.rate,
                    s.channels,
                    s.format,
                    if s.capped { ", capped" } else { "" },
                    if s.one_bit {
                        ", one-bit: plays on the player only"
                    } else {
                        ""
                    }
                );
                if !s.known() {
                    eprintln!("lite-client: a format this client does not take");
                }
                shape = Some(s);
            }
            Event::Pcm(pcm) => {
                let Some(s) = shape.as_ref() else { continue };
                if s.one_bit || !s.known() {
                    continue;
                }
                let written = if s16 {
                    s.to_i16(&pcm, &mut samples);
                    bytes.clear();
                    for v in &samples {
                        bytes.extend_from_slice(&v.to_le_bytes());
                    }
                    out.write_all(&bytes)
                } else {
                    out.write_all(&pcm)
                };
                if written.is_err() {
                    // The reader of standard output is gone: a pipe closed.
                    break;
                }
            }
            Event::End => eprintln!("lite-client: end"),
            Event::Gap { dropped } => eprintln!("lite-client: gap, {dropped} frames dropped"),
            Event::Full => eprintln!("lite-client: the lite is full"),
            Event::Other(kind) => eprintln!("lite-client: a frame of type {kind}, ignored"),
            Event::Closed => {
                eprintln!("lite-client: closed");
                break;
            }
        }
    }
    let _ = out.flush();
    client.close();
}
