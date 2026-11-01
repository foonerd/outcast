//! The core of Outcast Uplink: one daemon, started by the plugin as its own
//! systemd unit, that follows the player's audio ring (evotap's `evoaudio`
//! ring of the tag the plugin's ALSA snippet names), keeps a status file
//! for the plugin's page, and answers a control socket. The uplinks are
//! carried on top of this; the ring is read whether any uplink wants it
//! or not, so the status always says what the player is playing.
//!
//! `outcast [--tag TAG] [--run DIR] [--shm DIR] [--version]`
//!
//! `TAG` is the ring's tag (`outcast` unless given); `DIR` under `--run`
//! holds `status.json` and `control.sock` (`/run/outcast` unless given);
//! `--shm` is where the rings are (`/dev/shm` unless given).

mod control;
mod follow;
mod status;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use status::Status;

/// What the command line said.
struct Args {
    tag: String,
    run: PathBuf,
    shm: PathBuf,
}

fn usage() -> ! {
    eprintln!("outcast [--tag TAG] [--run DIR] [--shm DIR] [--version]");
    std::process::exit(64);
}

fn args() -> Args {
    let mut out = Args {
        tag: "outcast".to_string(),
        run: PathBuf::from("/run/outcast"),
        shm: PathBuf::from(tap::ring::DIR),
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
            "--version" => {
                println!("outcast {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => usage(),
        }
    }
    out
}

fn main() {
    let a = args();
    if let Err(e) = std::fs::create_dir_all(&a.run) {
        eprintln!("outcast: cannot make {}: {e}", a.run.display());
        std::process::exit(1);
    }
    let status = Arc::new(Mutex::new(Status::new(&a.tag)));
    eprintln!(
        "outcast {}: ring tag {}, status in {}",
        env!("CARGO_PKG_VERSION"),
        a.tag,
        a.run.display()
    );

    // The control socket, answered on a thread of its own.
    let socket = a.run.join("control.sock");
    let shared = Arc::clone(&status);
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || control::serve(&socket, shared))
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

    // The ring, followed on this thread for as long as the daemon runs.
    follow::run(&a.shm, &a.tag, status);
}
