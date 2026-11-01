//! The control socket: a Unix stream socket the plugin talks to, one JSON
//! object a line, one JSON object back. `{"op":"status"}` answers with the
//! status; `{"op":"title","title":"…"}` sets the title the uplinks carry
//! and answers `{"ok":true}`; anything else answers `{"ok":false,"error":"…"}`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::status::{self, Status};

#[derive(Deserialize)]
struct Request {
    op: String,
    #[serde(default)]
    title: String,
}

/// The answer to one request line.
pub fn answer(line: &str, shared: &Arc<Mutex<Status>>) -> String {
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
        "status" => status::render(shared),
        "title" => {
            let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
            if s.title != request.title {
                s.title = request.title;
                s.touch();
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

fn serve_one(stream: UnixStream, shared: &Arc<Mutex<Status>>) {
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
        if writer.write_all(answer(&line, shared).as_bytes()).is_err() {
            break;
        }
    }
}

/// Listen for ever; each connection is served on this thread, one after
/// another, since each asks one or two things and goes.
pub fn serve(socket: &Path, shared: Arc<Mutex<Status>>) {
    let _ = std::fs::remove_file(socket);
    let listener = match UnixListener::bind(socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("outcast: cannot listen on {}: {e}", socket.display());
            return;
        }
    };
    for stream in listener.incoming().flatten() {
        serve_one(stream, &shared);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_kept_and_status_shows_it() {
        let shared = Arc::new(Mutex::new(Status::new("t")));
        assert_eq!(
            answer("{\"op\":\"title\",\"title\":\"A - B\"}", &shared),
            "{\"ok\":true}\n"
        );
        let text = answer("{\"op\":\"status\"}", &shared);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["title"], "A - B");
    }

    #[test]
    fn a_bad_line_is_refused_in_json() {
        let shared = Arc::new(Mutex::new(Status::new("t")));
        for line in ["not json", "{\"op\":\"dance\"}"] {
            let text = answer(line, &shared);
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["ok"], false);
            assert!(v["error"].as_str().unwrap().len() > 3);
        }
    }
}
