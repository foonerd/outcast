//! The control socket: a Unix stream socket the plugin talks to, one JSON
//! object a line, one JSON object back. `{"op":"status"}` answers with the
//! status; `{"op":"title","title":"…"}` sets the title the uplinks carry;
//! `{"op":"reload"}` has the uplinks made anew from their file; each of the
//! two answers `{"ok":true}`; anything else answers `{"ok":false,"error":"…"}`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::hub::Command;
use crate::status::{self, Status};

#[derive(Deserialize)]
struct Request {
    op: String,
    #[serde(default)]
    title: String,
    /// DJ mode as the plugin reports it, for the status and the page.
    #[serde(default)]
    dj: serde_json::Value,
    /// A line for the page's events, when something of DJ mode's happened.
    #[serde(default)]
    event: String,
}

/// What answering needs: the status to render and the hub to tell.
pub struct Link {
    pub status: Arc<Mutex<Status>>,
    pub commands: Sender<Command>,
}

/// The answer to one request line.
pub fn answer(line: &str, link: &Link) -> String {
    let request: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return format!(
                "{{\"ok\":false,\"error\":\"{}\"}}\n",
                escape(&e.to_string())
            )
        }
    };
    match request.op.as_str() {
        "status" => status::render(&link.status),
        "title" => {
            let title: String = request.title.chars().take(500).collect();
            {
                let mut s = link.status.lock().unwrap_or_else(|e| e.into_inner());
                if s.title != title {
                    s.title = title.clone();
                    s.touch();
                }
            }
            let _ = link.commands.send(Command::Title(title));
            "{\"ok\":true}\n".to_string()
        }
        "reload" => {
            let _ = link.commands.send(Command::Reload);
            "{\"ok\":true}\n".to_string()
        }
        "dj" => {
            let mut s = link.status.lock().unwrap_or_else(|e| e.into_inner());
            if s.dj != request.dj {
                s.dj = request.dj;
                s.touch();
            }
            let event: String = request.event.chars().take(200).collect();
            if !event.is_empty() {
                s.event("info", event);
            }
            "{\"ok\":true}\n".to_string()
        }
        other => format!(
            "{{\"ok\":false,\"error\":\"unknown op {}\"}}\n",
            escape(other)
        ),
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn serve_one(stream: UnixStream, link: &Link) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if writer.write_all(answer(&line, link).as_bytes()).is_err() {
            break;
        }
    }
}

/// Listen for ever; each connection is served on this thread, one after
/// another, since each asks one or two things and goes.
pub fn serve(socket: &Path, link: Link) {
    let _ = std::fs::remove_file(socket);
    let listener = match UnixListener::bind(socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("outcast: cannot listen on {}: {e}", socket.display());
            return;
        }
    };
    for stream in listener.incoming().flatten() {
        serve_one(stream, &link);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn link() -> (Link, mpsc::Receiver<Command>) {
        let (tx, rx) = mpsc::channel();
        (
            Link {
                status: Arc::new(Mutex::new(Status::new("t"))),
                commands: tx,
            },
            rx,
        )
    }

    #[test]
    fn dj_mode_is_kept_for_the_page_with_its_event() {
        let (link, _rx) = link();
        let line = r#"{"op":"dj","dj":{"enabled":true,"block":"Morning","next":"Midday","next_in":600,"yielded":false},"event":"DJ mode: Morning started"}"#;
        assert_eq!(answer(line, &link), "{\"ok\":true}\n");
        let s = link.status.lock().unwrap();
        assert_eq!(s.dj["block"], "Morning");
        assert_eq!(
            s.since.events.back().unwrap().text,
            "DJ mode: Morning started"
        );
        drop(s);
        // The same report again: nothing new, no event.
        let line = r#"{"op":"dj","dj":{"enabled":true,"block":"Morning","next":"Midday","next_in":600,"yielded":false}}"#;
        assert_eq!(answer(line, &link), "{\"ok\":true}\n");
        assert_eq!(link.status.lock().unwrap().since.events.len(), 1);
    }

    #[test]
    fn a_title_is_kept_told_and_shown() {
        let (link, rx) = link();
        assert_eq!(
            answer("{\"op\":\"title\",\"title\":\"A - B\"}", &link),
            "{\"ok\":true}\n"
        );
        assert!(matches!(rx.try_recv(), Ok(Command::Title(t)) if t == "A - B"));
        let text = answer("{\"op\":\"status\"}", &link);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["title"], "A - B");
    }

    #[test]
    fn a_reload_is_told() {
        let (link, rx) = link();
        assert_eq!(answer("{\"op\":\"reload\"}", &link), "{\"ok\":true}\n");
        assert!(matches!(rx.try_recv(), Ok(Command::Reload)));
    }

    #[test]
    fn a_bad_line_is_refused_in_json() {
        let (link, _rx) = link();
        for line in ["not json", "{\"op\":\"dance\"}"] {
            let text = answer(line, &link);
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["ok"], false);
            assert!(v["error"].as_str().unwrap().len() > 3);
        }
    }
}
