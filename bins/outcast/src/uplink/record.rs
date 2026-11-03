//! Recording: the stream to files under a folder of the player, one file per
//! title the player shows or one per stream, named by the moment they begin
//! and the title. The encoder writes the file itself, so a WAV or FLAC file
//! gets its header finished when it closes.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{Record, Split};
use crate::conform::Conformer;
use crate::encoder::{Encoder, Output, Recipe};
use crate::feed::{Event, Segment};
use crate::uplink::{self, Ctx, Next, GRACE};

/// A file name's part made of a title: no path separators, no control
/// characters, no leading dots, not longer than a file system likes.
pub fn clean_name(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        let c = match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        };
        out.push(c);
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let out = out.trim_start_matches('.').to_string();
    out.chars().take(120).collect()
}

/// The local time as `YYYY-MM-DD HH-MM-SS`.
pub fn stamp() -> String {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!(
        "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// The file a recording goes to.
pub fn file_for(cfg: &Record, title: &str, stamp: &str) -> PathBuf {
    let name = if cfg.title_in_name && !title.is_empty() {
        format!("{stamp} - {}.{}", clean_name(title), cfg.codec.extension())
    } else {
        format!("{stamp}.{}", cfg.codec.extension())
    };
    PathBuf::from(&cfg.folder).join(name)
}

/// How long a new stream's first bytes are held before its file begins: the
/// player's title settles in the hub within two seconds of the stream, and a
/// file named by it from its first byte is worth the wait. The bytes held are
/// written first, so nothing is lost.
const PREROLL: Duration = Duration::from_millis(2500);
/// The most the pre-roll holds; beyond it the file begins whatever the title.
const PREROLL_MOST: usize = 16 << 20;

/// A file begun for the segment, with the pre-roll written first.
fn begin(
    cfg: &Record,
    ctx: &Ctx,
    seg: &Segment,
    title: &str,
    preroll: &mut Vec<u8>,
    files: &mut u64,
) -> Option<Encoder> {
    if let Err(e) = std::fs::create_dir_all(&cfg.folder) {
        ctx.state("error", Some(&format!("{}: {e}", cfg.folder)));
        ctx.log(&format!("{}: {e}", cfg.folder));
        std::thread::sleep(Duration::from_secs(5));
        return None;
    }
    let path = file_for(cfg, title, &stamp());
    let recipe = Recipe {
        codec: cfg.codec,
        bitrate: cfg.bitrate,
        rate: cfg.rate,
        title: title.to_string(),
        output: Output::File(path.clone()),
    };
    match Encoder::start(&ctx.evopeg, seg, &recipe, None, &ctx.id) {
        Ok(mut e) => {
            if !preroll.is_empty() {
                if let Err(err) = e.feed(preroll) {
                    ctx.log(&format!("encoder gone: {err}"));
                    e.kill();
                    return None;
                }
                preroll.clear();
            }
            *files += 1;
            let shown = path.display().to_string();
            ctx.log(&format!("recording {shown}"));
            let n = *files;
            ctx.update(|u| {
                u.state = "recording".into();
                u.detail = shown;
                u.files = n;
            });
            Some(e)
        }
        Err(e) => {
            ctx.state("error", Some(&format!("encoder: {e}")));
            ctx.log(&format!("encoder: {e}"));
            std::thread::sleep(Duration::from_secs(2));
            None
        }
    }
}

pub fn run(cfg: Record, ctx: Ctx, ctl: Receiver<Event>, pcm: Receiver<Arc<Vec<u8>>>) {
    let mut segment: Option<Segment> = None;
    let mut since: Option<Instant> = None;
    let mut preroll: Vec<u8> = Vec::new();
    let mut title = String::new();
    let mut recording_title = String::new();
    let mut encoder: Option<Encoder> = None;
    let mut files: u64 = 0;
    // The files' shape: the operator's rate and channels where set, the
    // player's where not.
    let mut conformer = Conformer::new(cfg.rate, cfg.channels);
    let mut conformed: Vec<u8> = Vec::new();
    ctx.state("idle", None);
    ctx.log(&format!("to {} as {}", cfg.folder, cfg.codec.extension()));

    loop {
        // A stream whose first bytes have been held long enough gets its file.
        if let (Some(_), None, Some(at)) = (segment.as_ref(), encoder.as_ref(), since) {
            if at.elapsed() >= PREROLL || preroll.len() >= PREROLL_MOST {
                encoder = begin(
                    &cfg,
                    &ctx,
                    &conformer.shape(),
                    &title,
                    &mut preroll,
                    &mut files,
                );
                recording_title = title.clone();
            }
        }
        match uplink::next(&ctl, &pcm, Duration::from_millis(20)) {
            Next::Gone | Next::Event(Event::Stop) => break,
            Next::Nothing => {}
            Next::Event(Event::Segment(seg)) => {
                // What the stream before still held goes into its file first.
                conformer.segment(&seg, &mut conformed);
                ctx.log(&conformer.describe(&seg));
                if let Some(mut e) = encoder.take() {
                    if !conformed.is_empty() {
                        let _ = e.feed(&conformed);
                    }
                    e.finish(GRACE);
                }
                segment = Some(seg);
                since = Some(Instant::now());
                preroll.clear();
            }
            Next::Event(Event::End) => {
                conformed.clear();
                conformer.end(&mut conformed);
                if let Some(mut e) = encoder.take() {
                    if !conformed.is_empty() {
                        let _ = e.feed(&conformed);
                    }
                    e.finish(GRACE);
                } else if segment.is_some() && !(preroll.is_empty() && conformed.is_empty()) {
                    // A stream shorter than the pre-roll still gets its file.
                    preroll.extend_from_slice(&conformed);
                    if let Some(e) = begin(
                        &cfg,
                        &ctx,
                        &conformer.shape(),
                        &title,
                        &mut preroll,
                        &mut files,
                    ) {
                        e.finish(GRACE);
                    }
                }
                segment = None;
                since = None;
                preroll.clear();
                ctx.update(|u| {
                    u.state = "idle".into();
                    u.detail = cfg.folder.clone();
                });
            }
            Next::Event(Event::Title(t)) => {
                title = t.clone();
                ctx.update(|u| u.title = t);
                if cfg.split == Split::Track
                    && segment.is_some()
                    && encoder.is_some()
                    && recording_title != title
                {
                    // The next file begins with the next bytes, at once: the title
                    // has already settled in the hub.
                    if let Some(e) = encoder.take() {
                        e.finish(GRACE);
                    }
                    since = Some(Instant::now() - PREROLL);
                }
            }
            Next::Pcm(piece) => {
                conformer.push(&piece, &mut conformed);
                if conformed.is_empty() {
                    continue;
                }
                let Some(e) = encoder.as_mut() else {
                    if segment.is_some() && preroll.len() < PREROLL_MOST {
                        preroll.extend_from_slice(&conformed);
                    }
                    continue;
                };
                if let Err(err) = e.feed(&conformed) {
                    ctx.log(&format!("encoder gone: {err}"));
                    ctx.state("error", Some(&format!("encoder: {err}")));
                    if let Some(enc) = encoder.take() {
                        enc.kill();
                    }
                    continue;
                }
                let b = e.bytes_in;
                ctx.update(|u| u.bytes = b);
            }
        }
    }
    if let Some(e) = encoder.take() {
        e.finish(GRACE);
    }
    ctx.state("off", None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Codec;

    #[test]
    fn titles_become_safe_file_names() {
        assert_eq!(clean_name("AC/DC - Back In Black"), "AC-DC - Back In Black");
        assert_eq!(clean_name("  ..hidden:  name?\t"), "hidden- name-");
        assert_eq!(clean_name(&"x".repeat(300)).len(), 120);
        assert_eq!(clean_name("Zoë – Ça va"), "Zoë – Ça va");
    }

    #[test]
    fn the_file_carries_the_stamp_the_title_and_the_extension() {
        let cfg = Record {
            folder: "/data/INTERNAL/outcast/recordings".into(),
            codec: Codec::Flac,
            ..Record::default()
        };
        assert_eq!(
            file_for(&cfg, "Artist - Title", "2026-11-02 10-00-00"),
            PathBuf::from(
                "/data/INTERNAL/outcast/recordings/2026-11-02 10-00-00 - Artist - Title.flac"
            )
        );
        assert_eq!(
            file_for(&cfg, "", "2026-11-02 10-00-00"),
            PathBuf::from("/data/INTERNAL/outcast/recordings/2026-11-02 10-00-00.flac")
        );
        let plain = Record {
            title_in_name: false,
            codec: Codec::Mp3,
            ..cfg
        };
        assert_eq!(
            file_for(&plain, "Artist - Title", "s").file_name().unwrap(),
            "s.mp3"
        );
    }

    #[test]
    fn the_stamp_has_the_form_a_file_system_sorts() {
        let s = stamp();
        assert_eq!(s.len(), 19);
        assert!(s.as_bytes()[4] == b'-' && s.as_bytes()[10] == b' ' && s.as_bytes()[13] == b'-');
    }
}
