//! The listeners: every WebSocket client, each on a thread of its own, fed
//! from a bounded queue that the reader never waits on. A client that
//! falls behind loses frames and is told so; one that stops taking bytes
//! for a while is dropped.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tungstenite::{Message, WebSocket};

use crate::wire;

/// Frames a client may hold unread before it loses the next: at 20 ms a
/// frame, two seconds.
const QUEUE: usize = 100;
/// How long a write may make no progress before the client is dropped.
const STALL: Duration = Duration::from_secs(3);

/// What a listener's thread is given to send.
pub enum Out {
    Text(String),
    Pcm(Arc<Vec<u8>>),
}

struct Listener {
    id: u64,
    tx: SyncSender<Out>,
    /// Frames lost since the last the client was told.
    dropped: Arc<AtomicU64>,
}

pub struct Shared {
    listeners: Vec<Listener>,
    /// The segment on air, as announced, for a client that joins late.
    pub current: Option<String>,
    next_id: u64,
    max: usize,
    /// Set when the count of listeners changed, for the main thread's line.
    changed: AtomicBool,
}

pub type Registry = Arc<Mutex<Shared>>;

pub fn registry(max: usize) -> Registry {
    Arc::new(Mutex::new(Shared {
        listeners: Vec::new(),
        current: None,
        next_id: 0,
        max,
        changed: AtomicBool::new(false),
    }))
}

impl Shared {
    pub fn count(&self) -> usize {
        self.listeners.len()
    }

    /// Whether the count changed since the last ask.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }

    /// A text frame to every listener. One whose queue is full is beyond
    /// help and is dropped.
    pub fn text(&mut self, s: &str) {
        self.listeners
            .retain(|l| l.tx.try_send(Out::Text(s.to_string())).is_ok());
    }

    /// A frame of the stream to every listener that has room; the others
    /// lose it, and the loss is counted for them.
    pub fn pcm(&mut self, piece: Arc<Vec<u8>>) {
        self.listeners
            .retain(|l| match l.tx.try_send(Out::Pcm(Arc::clone(&piece))) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    l.dropped.fetch_add(1, Ordering::Relaxed);
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            });
    }

    fn leave(&mut self, id: u64) {
        let before = self.listeners.len();
        self.listeners.retain(|l| l.id != id);
        if self.listeners.len() != before {
            self.changed.store(true, Ordering::Relaxed);
        }
    }
}

/// The acceptor, on a thread of its own, for as long as the listening socket
/// is open; each client on a thread of its own.
pub fn accept(listener: TcpListener, shared: Registry, hello: String) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("lite-accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let peer = stream.peer_addr().ok();
                let shared = Arc::clone(&shared);
                let hello = hello.clone();
                let _ = std::thread::Builder::new()
                    .name("lite-listener".into())
                    .spawn(move || serve(stream, peer, shared, hello));
            }
        })
        .expect("lite accept thread")
}

/// One client, from its handshake to its end.
fn serve(stream: TcpStream, peer: Option<SocketAddr>, shared: Registry, hello: String) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut ws = match tungstenite::accept(stream) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("outcast-lite: a client's handshake failed: {e}");
            return;
        }
    };
    let (tx, rx) = mpsc::sync_channel::<Out>(QUEUE);
    let dropped = Arc::new(AtomicU64::new(0));
    let (id, first) = {
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        if s.listeners.len() >= s.max {
            drop(s);
            let _ = ws.send(Message::Text(wire::word("full").into()));
            let _ = ws.close(None);
            return;
        }
        s.next_id += 1;
        let id = s.next_id;
        s.listeners.push(Listener {
            id,
            tx,
            dropped: Arc::clone(&dropped),
        });
        s.changed.store(true, Ordering::Relaxed);
        (id, s.current.clone())
    };
    let who = peer.map_or_else(|| "a client".to_string(), |p| p.to_string());
    eprintln!("outcast-lite: {who} listens");
    // The socket is read without waiting, so the client's close is seen
    // between frames sent.
    let _ = ws.get_ref().set_nonblocking(true);
    let mut ok = write(&mut ws, Message::Text(hello.into())).is_ok();
    if ok {
        if let Some(first) = first {
            ok = write(&mut ws, Message::Text(first.into())).is_ok();
        }
    }
    while ok {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(out) => {
                let lost = dropped.swap(0, Ordering::Relaxed);
                if lost > 0 {
                    let gap = format!("{{\"type\":\"gap\",\"dropped\":{lost}}}");
                    ok = write(&mut ws, Message::Text(gap.into())).is_ok();
                }
                let msg = match out {
                    Out::Text(s) => Message::Text(s.into()),
                    Out::Pcm(p) => Message::Binary(p.as_slice().to_vec().into()),
                };
                ok = ok && write(&mut ws, msg).is_ok();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // What the client sent: a close ends it; pings are answered by the
        // library on the next write; anything else is ignored.
        match ws.read() {
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(tungstenite::Error::Io(e)) if e.kind() == io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    let _ = ws.close(None);
    shared.lock().unwrap_or_else(|e| e.into_inner()).leave(id);
    eprintln!("outcast-lite: {who} left");
}

/// A message written whole: a socket that cannot take it at once is given
/// `STALL` to, then the client counts as gone.
fn write(ws: &mut WebSocket<TcpStream>, msg: Message) -> Result<(), ()> {
    let until = Instant::now() + STALL;
    let mut pending = Some(msg);
    loop {
        let r = match pending.take() {
            Some(m) => ws.send(m),
            None => ws.flush(),
        };
        match r {
            Ok(()) => return Ok(()),
            Err(tungstenite::Error::Io(e))
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                if Instant::now() > until {
                    return Err(());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return Err(()),
        }
    }
}

/// Everything shut: the listening socket, so the acceptor leaves, and every
/// listener's queue, so each thread ends.
pub fn shutdown(listener: &TcpListener, acceptor: JoinHandle<()>, shared: &Registry) {
    {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::shutdown(listener.as_raw_fd(), libc::SHUT_RDWR) };
    }
    shared
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .listeners
        .clear();
    let _ = acceptor.join();
}
