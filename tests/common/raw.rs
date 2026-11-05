//! A client that keeps every server message as sent: `(id, payload)`.

use super::PASSWORD;
use seihai_core::net::{login_gserver, random_key, GserverSession};
use seihai_core::protocol::ClientVersion;
use std::io;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

pub type Seen = [(u8, Vec<u8>)];

pub struct RawClient {
    pub sess: GserverSession,
    pub rx: Receiver<io::Result<Vec<u8>>>,
    pub seen: Vec<(u8, Vec<u8>)>,
}

impl RawClient {
    /// Log in and read until `ready` holds for what came.
    pub fn join(
        addr: &str,
        v: ClientVersion,
        account: &str,
        ready: impl Fn(&Seen) -> bool,
    ) -> RawClient {
        let mut sess = login_gserver(addr, v, random_key(), account, PASSWORD, false)
            .unwrap_or_else(|e| panic!("{v:?}: {e}"));
        let rx = sess.conn.spawn_reader().unwrap();
        let mut c = RawClient {
            sess,
            rx,
            seen: Vec::new(),
        };
        c.until("the login", ready);
        c
    }

    /// Read until `done` holds for what came since the call, then a moment
    /// more; returns what came.
    pub fn until(&mut self, what: &str, done: impl Fn(&Seen) -> bool) -> Vec<(u8, Vec<u8>)> {
        let from = self.seen.len();
        let end = Instant::now() + Duration::from_secs(10);
        while !done(&self.seen[from..]) {
            assert!(Instant::now() < end, "no {what}: {:?}", &self.seen[from..]);
            if let Ok(Ok(b)) = self.rx.recv_timeout(Duration::from_millis(100)) {
                self.seen.extend(messages(&b));
            }
        }
        self.settle();
        self.seen[from..].to_vec()
    }

    /// Whatever comes within a moment.
    pub fn settle(&mut self) -> Vec<(u8, Vec<u8>)> {
        let from = self.seen.len();
        while let Ok(Ok(b)) = self.rx.recv_timeout(Duration::from_millis(300)) {
            self.seen.extend(messages(&b));
        }
        self.seen[from..].to_vec()
    }

    pub fn send(&mut self, id: u8, payload: &[u8]) {
        let mut m = vec![id + 32];
        m.extend_from_slice(payload);
        m.push(b'\n');
        self.sess.conn.send_messages(&[m]).unwrap();
    }
}

/// The messages in a frame body, as (id, payload); a `PLO_RAWDATA` block
/// is skipped whole.
pub fn messages(b: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let id = b[i].wrapping_sub(32);
        let j = b[i..]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(b.len(), |p| i + p);
        if id == 100 {
            let n = b[i + 1..i + 4]
                .iter()
                .fold(0usize, |a, c| a << 7 | usize::from(c - 32));
            i = j + 1 + n;
            continue;
        }
        out.push((id, b[i + 1..j].to_vec()));
        i = j + 1;
    }
    out
}

pub fn has(m: &Seen, id: u8, payload: &[u8]) -> bool {
    m.iter().any(|(i, p)| *i == id && p == payload)
}
