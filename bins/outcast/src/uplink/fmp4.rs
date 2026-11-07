//! Fragmented MP4 as evopeg makes it (CMAF: `ftyp` and `moov` once, then a
//! `moof` and `mdat` per fragment), read box by box for the HLS and DASH
//! segmenter: the init segment, each fragment with its decode time and its
//! length on the track's clock; and the boxes the core writes itself: an
//! `emsg` carrying an ID3 tag (the title, as Apple and hls.js take timed
//! metadata in fMP4) and the DASH manifest.

/// What the reader found whole.
pub enum Part {
    /// `ftyp` and `moov`: the init segment, with the track's timescale.
    Init { bytes: Vec<u8>, timescale: u32 },
    /// A `moof` and its `mdat`: one fragment.
    Fragment {
        bytes: Vec<u8>,
        /// The fragment's decode time on the track's clock (`tfdt`).
        tfdt: u64,
        /// Its length on the track's clock (the samples' durations summed).
        duration: u64,
    },
}

/// A box's header read at `at`: its size and type; None when not whole.
fn header(b: &[u8], at: usize) -> Option<(usize, [u8; 4], usize)> {
    if b.len() < at + 8 {
        return None;
    }
    let size32 = u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
    let typ = [b[at + 4], b[at + 5], b[at + 6], b[at + 7]];
    match size32 {
        1 => {
            if b.len() < at + 16 {
                return None;
            }
            let size = u64::from_be_bytes(b[at + 8..at + 16].try_into().ok()?) as usize;
            Some((size, typ, 16))
        }
        0 => Some((b.len() - at, typ, 8)),
        s => Some((s, typ, 8)),
    }
}

/// The first box of `typ` inside `b` (searched through the nesting), as its
/// payload after the header.
fn find<'a>(b: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
    let mut at = 0;
    while let Some((size, t, head)) = header(b, at) {
        if size < head || at + size > b.len() {
            return None;
        }
        if &t == typ {
            return Some(&b[at + head..at + size]);
        }
        // The containers of the boxes looked for.
        if matches!(
            &t,
            b"moov" | b"trak" | b"mdia" | b"mvex" | b"moof" | b"traf"
        ) {
            if let Some(found) = find(&b[at + head..at + size], typ) {
                return Some(found);
            }
        }
        at += size;
    }
    None
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map(u64::from_be_bytes)
}

/// The track's timescale from `mdhd` (version 0 or 1).
fn timescale(moov: &[u8]) -> Option<u32> {
    let mdhd = find(moov, b"mdhd")?;
    match mdhd.first()? {
        1 => u32_at(mdhd, 4 + 8 + 8),
        _ => u32_at(mdhd, 4 + 4 + 4),
    }
}

/// `trex`'s default sample duration.
fn trex_default(moov: &[u8]) -> u32 {
    // Version and flags, the track id, the description index, then the duration.
    find(moov, b"trex")
        .and_then(|t| u32_at(t, 4 + 4 + 4))
        .unwrap_or(0)
}

/// A fragment's decode time and length from its `moof`.
fn fragment_times(moof: &[u8], default_duration: u32) -> Option<(u64, u64)> {
    let tfdt = find(moof, b"tfdt")?;
    let decode_time = match tfdt.first()? {
        1 => u64_at(tfdt, 4)?,
        _ => u64::from(u32_at(tfdt, 4)?),
    };
    // tfhd: flags in the low 24 bits; 0x000008 says a default sample duration follows
    // the track id and, where flagged, the base data offset and the description index.
    let mut default = default_duration;
    if let Some(tfhd) = find(moof, b"tfhd") {
        let flags = u32_at(tfhd, 0)? & 0x00FF_FFFF;
        let mut at = 8;
        if flags & 0x01 != 0 {
            at += 8;
        }
        if flags & 0x02 != 0 {
            at += 4;
        }
        if flags & 0x08 != 0 {
            default = u32_at(tfhd, at)?;
        }
    }
    // trun: the sample count, then per sample the fields its flags name.
    let trun = find(moof, b"trun")?;
    let flags = u32_at(trun, 0)? & 0x00FF_FFFF;
    let count = u32_at(trun, 4)?;
    let mut at = 8;
    if flags & 0x001 != 0 {
        at += 4;
    }
    if flags & 0x004 != 0 {
        at += 4;
    }
    let per_sample = [
        (0x100, true),
        (0x200, false),
        (0x400, false),
        (0x800, false),
    ];
    let mut duration: u64 = 0;
    if flags & 0x100 != 0 {
        for _ in 0..count {
            for (flag, is_duration) in per_sample {
                if flags & flag != 0 {
                    if is_duration {
                        duration += u64::from(u32_at(trun, at)?);
                    }
                    at += 4;
                }
            }
        }
    } else {
        duration = u64::from(count) * u64::from(default);
    }
    Some((decode_time, duration))
}

/// The reader: bytes in, parts out.
#[derive(Default)]
pub struct Boxes {
    held: Vec<u8>,
    init: Vec<u8>,
    timescale: u32,
    default_duration: u32,
    pending_moof: Option<Vec<u8>>,
}

impl Boxes {
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Part>) {
        self.held.extend_from_slice(bytes);
        let mut at = 0;
        while let Some((size, typ, _)) = header(&self.held, at) {
            if size < 8 || self.held.len() < at + size {
                break;
            }
            let b = &self.held[at..at + size];
            match &typ {
                b"ftyp" => {
                    self.init.clear();
                    self.init.extend_from_slice(b);
                }
                b"moov" => {
                    self.init.extend_from_slice(b);
                    self.timescale = timescale(b).unwrap_or(0);
                    self.default_duration = trex_default(b);
                    out.push(Part::Init {
                        bytes: self.init.clone(),
                        timescale: self.timescale,
                    });
                }
                b"moof" => self.pending_moof = Some(b.to_vec()),
                b"mdat" => {
                    if let Some(mut moof) = self.pending_moof.take() {
                        let times = fragment_times(&moof, self.default_duration);
                        moof.extend_from_slice(b);
                        if let Some((tfdt, duration)) = times {
                            out.push(Part::Fragment {
                                bytes: moof,
                                tfdt,
                                duration,
                            });
                        }
                    }
                }
                // styp, sidx, prft, mfra and whatever else: not the core's business.
                _ => {}
            }
            at += size;
        }
        self.held.drain(..at);
    }

    pub fn timescale(&self) -> u32 {
        self.timescale
    }

    /// The default sample duration the init named, for the tests.
    #[cfg(test)]
    pub fn default_duration(&self) -> u32 {
        self.default_duration
    }
}

/// An `emsg` box (version 1) carrying an ID3 tag at `presentation_time` on the
/// track's clock: the title as timed metadata, Apple's and hls.js's way.
pub fn emsg_id3(timescale: u32, presentation_time: u64, id3: &[u8]) -> Vec<u8> {
    let scheme = b"https://aomedia.org/emsg/ID3\0";
    let value = b"\0";
    let size = 8 + 4 + 4 + 8 + 4 + 4 + scheme.len() + value.len() + id3.len();
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(&(size as u32).to_be_bytes());
    b.extend_from_slice(b"emsg");
    b.extend_from_slice(&[1, 0, 0, 0]);
    b.extend_from_slice(&timescale.to_be_bytes());
    b.extend_from_slice(&presentation_time.to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes()); // event duration: until the next
    b.extend_from_slice(&0u32.to_be_bytes()); // id
    b.extend_from_slice(scheme);
    b.extend_from_slice(value);
    b.extend_from_slice(id3);
    b
}

/// One segment as the manifest lists it.
pub struct Listed<'a> {
    pub seq: u64,
    pub timeline: u64,
    pub tfdt: u64,
    pub duration: u64,
    /// The wall clock, seconds since the epoch, when the segment's first byte came.
    pub began: f64,
    pub init_name: &'a str,
}

/// The DASH manifest for a live stream: one period per timeline in the window,
/// the segments as a timeline of their own under a template by number, so the
/// same files serve DASH and HLS.
#[allow(clippy::too_many_arguments)]
pub fn mpd(
    name: &str,
    extension: &str,
    codecs: &str,
    mime: &str,
    channels: u32,
    sample_rate: u32,
    bitrate_bps: u64,
    segment_seconds: u32,
    window: u32,
    timescale: u32,
    now: f64,
    listed: &[Listed],
) -> String {
    let seg = u64::from(segment_seconds.max(1));
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    // The whole timeline is dated from the first listed segment's wall clock, less
    // its own decode time, so a player's clock arithmetic lands on the segments.
    let ast = listed
        .first()
        .map(|l| l.began - l.tfdt as f64 / f64::from(timescale.max(1)))
        .unwrap_or(now);
    out.push_str(&format!(
        "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" profiles=\"urn:mpeg:dash:profile:isoff-live:2011\" type=\"dynamic\" availabilityStartTime=\"{}\" publishTime=\"{}\" minimumUpdatePeriod=\"PT{seg}S\" minBufferTime=\"PT{}S\" timeShiftBufferDepth=\"PT{}S\" maxSegmentDuration=\"PT{seg}S\">\n",
        super::hls::iso8601(ast),
        super::hls::iso8601(now),
        seg * 2,
        seg * u64::from(window.max(1)),
    ));
    let mut i = 0;
    while i < listed.len() {
        let timeline = listed[i].timeline;
        let end = listed[i..]
            .iter()
            .position(|l| l.timeline != timeline)
            .map(|n| i + n)
            .unwrap_or(listed.len());
        let period = &listed[i..end];
        let first = &period[0];
        // Each period starts where its first segment's wall clock sits against the
        // availability start, with the segments' own decode times inside it.
        let start = (first.began - first.tfdt as f64 / f64::from(timescale.max(1)) - ast).max(0.0);
        out.push_str(&format!(
            "  <Period id=\"{timeline}\" start=\"PT{start:.3}S\">\n    <AdaptationSet mimeType=\"{mime}\" codecs=\"{codecs}\" segmentAlignment=\"true\" startWithSAP=\"1\">\n      <AudioChannelConfiguration schemeIdUri=\"urn:mpeg:dash:23003:3:audio_channel_configuration:2011\" value=\"{channels}\"/>\n      <Representation id=\"audio\" bandwidth=\"{bitrate_bps}\" audioSamplingRate=\"{sample_rate}\">\n        <SegmentTemplate timescale=\"{timescale}\" initialization=\"{}\" media=\"{name}-$Number$.{extension}\" startNumber=\"{}\">\n          <SegmentTimeline>\n",
            first.init_name, first.seq
        ));
        for l in period {
            out.push_str(&format!(
                "            <S t=\"{}\" d=\"{}\"/>\n",
                l.tfdt, l.duration
            ));
        }
        out.push_str("          </SegmentTimeline>\n        </SegmentTemplate>\n      </Representation>\n    </AdaptationSet>\n  </Period>\n");
        i = end;
    }
    out.push_str("</MPD>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(typ);
        b.extend_from_slice(payload);
        b
    }

    fn init_bytes(timescale: u32, default: u32) -> Vec<u8> {
        let mut mdhd = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        mdhd.extend_from_slice(&timescale.to_be_bytes());
        mdhd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        let mdia = boxed(b"mdia", &boxed(b"mdhd", &mdhd));
        let trak = boxed(b"trak", &mdia);
        let mut trex = vec![0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1];
        trex.extend_from_slice(&default.to_be_bytes());
        trex.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        let mvex = boxed(b"mvex", &boxed(b"trex", &trex));
        let mut moov = trak;
        moov.extend_from_slice(&mvex);
        let mut b = boxed(b"ftyp", b"cmfc");
        b.extend_from_slice(&boxed(b"moov", &moov));
        b
    }

    fn fragment_bytes(seq: u32, tfdt: u64, count: u32, durations: Option<&[u32]>) -> Vec<u8> {
        let mut mfhd = vec![0, 0, 0, 0];
        mfhd.extend_from_slice(&seq.to_be_bytes());
        let mut tfdt_b = vec![1, 0, 0, 0];
        tfdt_b.extend_from_slice(&tfdt.to_be_bytes());
        let tfhd = vec![0, 0x02, 0x00, 0x00, 0, 0, 0, 1]; // default-base-is-moof, track 1
        let mut trun = Vec::new();
        let flags: u32 = 0x01 | 0x200 | if durations.is_some() { 0x100 } else { 0 };
        trun.extend_from_slice(&flags.to_be_bytes());
        trun.extend_from_slice(&count.to_be_bytes());
        trun.extend_from_slice(&[0, 0, 0, 0]); // data offset
        for i in 0..count as usize {
            if let Some(d) = durations {
                trun.extend_from_slice(&d[i].to_be_bytes());
            }
            trun.extend_from_slice(&[0, 0, 0, 10]); // size
        }
        let mut traf = boxed(b"tfhd", &tfhd);
        traf.extend_from_slice(&boxed(b"tfdt", &tfdt_b));
        traf.extend_from_slice(&boxed(b"trun", &trun));
        let mut moof = boxed(b"mfhd", &mfhd);
        moof.extend_from_slice(&boxed(b"traf", &traf));
        let mut b = boxed(b"moof", &moof);
        b.extend_from_slice(&boxed(b"mdat", &[0xAB; 30]));
        b
    }

    #[test]
    fn the_init_and_the_fragments_are_read_across_pieces() {
        let mut all = init_bytes(48000, 1024);
        all.extend_from_slice(&fragment_bytes(1, 0, 188, None));
        all.extend_from_slice(&boxed(b"sidx", &[0; 20]));
        all.extend_from_slice(&fragment_bytes(2, 192_512, 3, Some(&[4096, 4096, 100])));
        let mut boxes = Boxes::default();
        let mut parts = Vec::new();
        for chunk in all.chunks(37) {
            boxes.push(chunk, &mut parts);
        }
        assert_eq!(parts.len(), 3);
        match &parts[0] {
            Part::Init { bytes, timescale } => {
                assert_eq!((*timescale, boxes.default_duration()), (48000, 1024));
                assert_eq!(&bytes[4..8], b"ftyp");
                assert_eq!(bytes.len(), init_bytes(48000, 1024).len());
            }
            _ => panic!("the init first"),
        }
        match &parts[1] {
            Part::Fragment {
                tfdt,
                duration,
                bytes,
            } => {
                assert_eq!((*tfdt, *duration), (0, 188 * 1024));
                assert_eq!(&bytes[4..8], b"moof");
                assert_eq!(&bytes[bytes.len() - 34..bytes.len() - 30], b"mdat");
            }
            _ => panic!("a fragment second"),
        }
        match &parts[2] {
            Part::Fragment { tfdt, duration, .. } => {
                assert_eq!((*tfdt, *duration), (192_512, 8292));
            }
            _ => panic!("a fragment third"),
        }
        assert_eq!(boxes.timescale(), 48000);
    }

    #[test]
    fn the_real_streams_boxes_are_read() {
        // evopeg's own AAC stream, 12 s of silence at 48 kHz: ftyp, moov, three
        // fragments of 4.011 s (188 × 1024), mfra; checked by the box lister.
        let real = std::env::var("OUTCAST_FMP4_SAMPLE").ok();
        let Some(path) = real else { return };
        let bytes = std::fs::read(path).unwrap();
        let mut boxes = Boxes::default();
        let mut parts = Vec::new();
        boxes.push(&bytes, &mut parts);
        let frags: Vec<(u64, u64)> = parts
            .iter()
            .filter_map(|p| match p {
                Part::Fragment { tfdt, duration, .. } => Some((*tfdt, *duration)),
                _ => None,
            })
            .collect();
        assert_eq!(frags[0], (0, 192_512));
        assert_eq!(frags[1].0, 192_512);
    }

    #[test]
    fn an_emsg_carries_the_tag_with_its_scheme() {
        let tag = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
        let b = emsg_id3(48000, 192_512, &tag);
        assert_eq!(
            u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize,
            b.len()
        );
        assert_eq!(&b[4..8], b"emsg");
        assert_eq!(b[8], 1);
        assert_eq!(u32_at(&b, 12), Some(48000));
        assert_eq!(u64_at(&b, 16), Some(192_512));
        let scheme = b"https://aomedia.org/emsg/ID3\0";
        assert_eq!(&b[32..32 + scheme.len()], scheme);
        assert!(b.ends_with(&tag));
    }

    #[test]
    fn the_manifest_lists_a_period_per_timeline_with_the_segments_times() {
        let listed = [
            Listed {
                seq: 10,
                timeline: 2,
                tfdt: 385_024,
                duration: 192_512,
                began: 1_762_447_205.0 + 385_024.0 / 48_000.0,
                init_name: "live-init-2.mp4",
            },
            Listed {
                seq: 11,
                timeline: 2,
                tfdt: 577_536,
                duration: 192_512,
                began: 1_762_447_205.0 + 577_536.0 / 48_000.0,
                init_name: "live-init-2.mp4",
            },
            Listed {
                seq: 12,
                timeline: 3,
                tfdt: 0,
                duration: 192_512,
                began: 1_762_447_205.0 + 16.0,
                init_name: "live-init-3.mp4",
            },
        ];
        let m = mpd(
            "live",
            "m4s",
            "mp4a.40.2",
            "audio/mp4",
            2,
            48000,
            128_000,
            4,
            6,
            48000,
            1_762_447_205.0 + 20.0,
            &listed,
        );
        assert!(m.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\""), "{m}");
        assert!(
            m.contains("type=\"dynamic\" availabilityStartTime=\"2025-11-06T16:40:05.000Z\""),
            "{m}"
        );
        assert!(m.contains("timeShiftBufferDepth=\"PT24S\""));
        assert!(m.contains("<Period id=\"2\" start=\"PT0.000S\">"), "{m}");
        assert!(m.contains("<Period id=\"3\" start=\"PT16.000S\">"), "{m}");
        assert!(m.contains(
            "initialization=\"live-init-2.mp4\" media=\"live-$Number$.m4s\" startNumber=\"10\""
        ));
        assert!(m.contains(
            "<S t=\"385024\" d=\"192512\"/>\n            <S t=\"577536\" d=\"192512\"/>"
        ));
        assert!(m.contains("codecs=\"mp4a.40.2\""));
        assert_eq!(m.matches("<Period ").count(), 2);
        assert!(m.ends_with("</MPD>\n"));
    }
}
