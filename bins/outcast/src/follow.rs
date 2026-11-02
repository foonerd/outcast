//! Following the ring: the newest live `evoaudio` ring of the tag is read
//! from the moment it is found to the moment it goes (the player closed
//! the stream or changed its shape), then the next one. The bytes are
//! taken whole frames at a time, counted, and handed to the hub for the
//! uplinks; between takes the hub's commands are heard.

use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tap::audio::Reader;
use tap::sample::{alsa_name, ffmpeg_name};

use crate::hub::{Command, Hub};
use crate::status::{Ring, Status};

/// Bytes taken from the ring in one go.
const TAKE: usize = 1 << 18;
/// How long to sleep when the ring has nothing new.
const NAP: Duration = Duration::from_millis(4);
/// How often to look for a ring when there is none.
const LOOK: Duration = Duration::from_millis(100);

fn ring_of(reader: &Reader) -> Ring {
    let shape = reader.shape();
    let info = reader.info();
    Ring {
        path: reader.path().display().to_string(),
        writer_pid: info.pid,
        rate: shape.rate,
        channels: shape.channels,
        format: alsa_name(shape.format).to_string(),
        ffmpeg: ffmpeg_name(shape.format).map(str::to_string),
        frame_bytes: shape.frame_bytes,
        one_bit: shape.one_bit,
        dropped: info.dropped,
        healed: info.healed,
    }
}

/// Every command waiting, given to the hub. False when the control side is gone.
fn commands(hub: &mut Hub, rx: &Receiver<Command>) -> bool {
    loop {
        match rx.try_recv() {
            Ok(c) => hub.command(c),
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => return false,
        }
    }
}

/// Follow rings for ever.
pub fn run(shm: &Path, tag: &str, status: Arc<Mutex<Status>>, mut hub: Hub, rx: Receiver<Command>) {
    let mut buf: Vec<u8> = Vec::with_capacity(TAKE);
    loop {
        if !commands(&mut hub, &rx) {
            break;
        }
        let Some(mut reader) = Reader::open_live(shm, Some(tag)) else {
            std::thread::sleep(LOOK);
            continue;
        };
        {
            let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
            s.ring = Some(ring_of(&reader));
            s.segments += 1;
            s.touch();
        }
        let ring = ring_of(&reader);
        eprintln!(
            "outcast: segment {} rate={} channels={} format={} frame={} one_bit={}",
            ring.path, ring.rate, ring.channels, ring.format, ring.frame_bytes, ring.one_bit
        );
        // A reader starts at the head: what played before it opened is not wanted.
        hub.segment(&reader.shape());
        let mut bytes: u64 = 0;
        let mut lost: u64 = 0;
        loop {
            if !commands(&mut hub, &rx) {
                hub.end();
                hub.shutdown();
                return;
            }
            buf.clear();
            let took = reader.take(&mut buf, TAKE);
            if took.lost > 0 || took.bytes > 0 {
                bytes += took.bytes as u64;
                lost += took.lost;
                if took.bytes > 0 {
                    hub.pcm(&buf);
                }
                let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
                s.bytes += took.bytes as u64;
                s.lost += took.lost;
                if let Some(r) = s.ring.as_mut() {
                    let info = reader.info();
                    r.dropped = info.dropped;
                    r.healed = info.healed;
                }
                s.touch();
                continue;
            }
            if !reader.is_live() {
                break;
            }
            std::thread::sleep(NAP);
        }
        eprintln!("outcast: segment ended after {bytes} bytes, {lost} lost");
        hub.end();
        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
        s.ring = None;
        s.touch();
    }
    hub.shutdown();
}
