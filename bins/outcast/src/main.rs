//! The core of Outcast Uplink: one daemon, started by the plugin as its own
//! systemd unit, that follows the player's audio ring (evotap's `evoaudio`
//! ring of the tag the plugin's ALSA snippet names), carries the sound to
//! every uplink the plugin's page set up, keeps a status file for the page,
//! and answers a control socket. The ring is read whether any uplink wants
//! it or not, so the status always says what the player is playing.
//!
//! `outcast [--tag TAG] [--run DIR] [--shm DIR] [--evopeg FILE] [--uplinks FILE] [--version]`
//!
//! `TAG` is the ring's tag (`outcast` unless given); `DIR` under `--run`
//! holds `status.json` and `control.sock` (`/run/outcast` unless given);
//! `--shm` is where the rings are (`/dev/shm` unless given); `--evopeg` is
//! the encoder (`bin/evopeg` beside this binary unless given); `--uplinks`
//! is the file the page writes the uplinks to.

mod config;
mod control;
mod dashboard;
mod encoder;
mod follow;
mod hub;
mod level;
mod status;
mod uplink;

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hub::Hub;
use status::Status;

/// What the command line said.
struct Args {
    tag: String,
    run: PathBuf,
    shm: PathBuf,
    evopeg: Option<PathBuf>,
    uplinks: PathBuf,
}

fn usage() -> ! {
    eprintln!(
        "outcast [--tag TAG] [--run DIR] [--shm DIR] [--evopeg FILE] [--uplinks FILE] [--version]"
    );
    std::process::exit(64);
}

fn args() -> Args {
    let mut out = Args {
        tag: "outcast".to_string(),
        run: PathBuf::from("/run/outcast"),
        shm: PathBuf::from(tap::ring::DIR),
        evopeg: None,
        uplinks: PathBuf::from("/data/configuration/audio_interface/outcast/uplinks.json"),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tag" => {
                out.tag = it
                    .next()
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| usage())
            }
            "--run" => out.run = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--shm" => out.shm = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--evopeg" => {
                out.evopeg = Some(it.next().map(PathBuf::from).unwrap_or_else(|| usage()))
            }
            "--uplinks" => out.uplinks = it.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "--version" => {
                println!("outcast {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => usage(),
        }
    }
    out
}

/// The encoder beside this binary, unless the command line said where.
fn evopeg_path(given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("evopeg")))
            .unwrap_or_else(|| PathBuf::from("/data/plugins/audio_interface/outcast/bin/evopeg"))
    })
}

fn main() {
    let a = args();
    if let Err(e) = std::fs::create_dir_all(&a.run) {
        eprintln!("outcast: cannot make {}: {e}", a.run.display());
        std::process::exit(1);
    }
    let evopeg = evopeg_path(a.evopeg);
    let status = Arc::new(Mutex::new(Status::new(&a.tag)));
    eprintln!(
        "outcast {}: ring tag {}, status in {}, encoder {}, uplinks from {}",
        env!("CARGO_PKG_VERSION"),
        a.tag,
        a.run.display(),
        evopeg.display(),
        a.uplinks.display()
    );
    if !evopeg.is_file() {
        eprintln!(
            "outcast: no encoder at {}; the uplinks will fail to start",
            evopeg.display()
        );
    }

    let (commands, command_rx) = mpsc::channel();

    // The control socket, answered on a thread of its own.
    let socket = a.run.join("control.sock");
    let link = control::Link {
        status: Arc::clone(&status),
        commands,
    };
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || control::serve(&socket, link))
        .expect("control thread");

    // The status file, written when something changed, at most once a second.
    let file = a.run.join("status.json");
    let shared = Arc::clone(&status);
    std::thread::Builder::new()
        .name("status".into())
        .spawn(move || {
            let mut last = String::new();
            loop {
                let now = status::render(&shared);
                if now != last {
                    if let Err(e) = status::write_file(&file, &now) {
                        eprintln!("outcast: cannot write {}: {e}", file.display());
                    }
                    last = now;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        })
        .expect("status thread");

    // The uplinks from their file, then the ring, followed on this thread
    // for as long as the daemon runs.
    let mut hub = Hub::new(Arc::clone(&status), &evopeg, &a.uplinks);
    if let Err(e) = hub.reload() {
        eprintln!("outcast: uplinks: {e}");
        let mut s = status.lock().unwrap_or_else(|e| e.into_inner());
        s.last_error = e;
        s.touch();
    }
    follow::run(&a.shm, &a.tag, status, hub, command_rx);
}
