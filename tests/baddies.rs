//! Stage 3.2: baddies, with Seihai's own client code on 127.0.0.1. The
//! first player in a level leads it (`PLO_ISLEADER`) and gets the level's
//! baddies; a second player gets the baddies and no leader flag. The
//! leader's changes reach the follower, the follower's hit reaches the
//! leader, a dying baddy is dead for both two seconds later, and when the
//! leader leaves, the follower leads.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key, GserverSession};
use seihai_core::protocol::ClientVersion;
use std::io;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const LEVEL: &str = "GLEVNW01\n\
BADDY 40 30 0\nHalt!\nOuch!\nTake that!\nBADDYEND\n\
BADDY 20 40 7\n\n\n\nBADDYEND\n";

const ISLEADER: u8 = 10;
const BADDYPROPS: u8 = 2;
const BADDYHURT: u8 = 27;

struct Client {
    sess: GserverSession,
    rx: Receiver<io::Result<Vec<u8>>>,
    seen: Vec<(u8, Vec<u8>)>,
}

impl Client {
    fn join(addr: &str, v: ClientVersion, account: &str) -> Client {
        let mut sess = login_gserver(addr, v, random_key(), account, PASSWORD, false)
            .unwrap_or_else(|e| panic!("{v:?}: {e}"));
        let rx = sess.conn.spawn_reader().unwrap();
        let mut c = Client {
            sess,
            rx,
            seen: Vec::new(),
        };
        c.until("both baddies", |m| {
            m.iter().filter(|(id, _)| *id == BADDYPROPS).count() >= 2
        });
        c
    }

    /// Read until `done` holds for what came since the call, then a moment
    /// more; returns what came.
    fn until(&mut self, what: &str, done: impl Fn(&[(u8, Vec<u8>)]) -> bool) -> Vec<(u8, Vec<u8>)> {
        let from = self.seen.len();
        let end = Instant::now() + Duration::from_secs(10);
        while !done(&self.seen[from..]) {
            assert!(Instant::now() < end, "no {what}: {:?}", &self.seen[from..]);
            if let Ok(Ok(b)) = self.rx.recv_timeout(Duration::from_millis(100)) {
                self.seen.extend(messages(&b));
            }
        }
        while let Ok(Ok(b)) = self.rx.recv_timeout(Duration::from_millis(200)) {
            self.seen.extend(messages(&b));
        }
        self.seen[from..].to_vec()
    }

    fn send(&mut self, id: u8, payload: &[u8]) {
        let mut m = vec![id + 32];
        m.extend_from_slice(payload);
        m.push(b'\n');
        self.sess.conn.send_messages(&[m]).unwrap();
    }
}

/// The messages in a frame body, as (id, payload); a `PLO_RAWDATA` block
/// is skipped whole.
fn messages(b: &[u8]) -> Vec<(u8, Vec<u8>)> {
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

fn has(m: &[(u8, Vec<u8>)], id: u8, payload: &[u8]) -> bool {
    m.iter().any(|(i, p)| *i == id && p == payload)
}

#[test]
fn the_leader_runs_the_baddies() {
    let w = world("baddies", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), LEVEL).unwrap();
    let (addr, _server) = start(&w);

    let mut lea = Client::join(&addr, ClientVersion::V222, "lea");
    assert!(lea.seen.iter().any(|(id, _)| *id == ISLEADER));
    let gray = lea
        .seen
        .iter()
        .find(|(id, p)| *id == BADDYPROPS && p[0] == 33)
        .unwrap();
    assert!(gray
        .1
        .starts_with(&[0x21, 0x21, 0x70, 0x22, 0x5c, 0x23, 0x20, 0x24, 0x22, 0x2d]));
    assert!(gray.1.windows(13).any(|w| w == b"baddygray.png"));

    let mut fay = Client::join(&addr, ClientVersion::V139, "fay");
    assert!(!fay.seen.iter().any(|(id, _)| *id == ISLEADER));
    assert!(fay
        .seen
        .iter()
        .any(|(id, p)| *id == BADDYPROPS && p.windows(13).any(|w| w == b"baddygray.gif")));

    // The leader moves baddy 1 to x 41: the follower sees it.
    lea.send(15, &[0x21, 0x21, 0x72]);
    let got = fay.until("the move", |m| has(m, BADDYPROPS, &[0x21, 0x21, 0x72]));
    assert!(!got.iter().any(|(id, _)| *id == ISLEADER));

    // The follower hits it: the leader gets the hit, as sent.
    fay.send(16, &[0x21, 0x28, 0x60, 0x21]);
    lea.until("the hit", |m| has(m, BADDYHURT, &[0x21, 0x28, 0x60, 0x21]));

    // The leader kills it: the follower sees it die, and two seconds later
    // both see it dead.
    let start = Instant::now();
    lea.send(15, &[0x21, 0x25, 0x25]);
    fay.until("the death", |m| has(m, BADDYPROPS, &[0x21, 0x25, 0x25]));
    lea.until("mode 9", |m| has(m, BADDYPROPS, &[0x21, 0x25, 0x29]));
    fay.until("mode 9", |m| has(m, BADDYPROPS, &[0x21, 0x25, 0x29]));
    assert!(
        start.elapsed() >= Duration::from_millis(1900),
        "{:?}",
        start.elapsed()
    );

    // A later arrival sees it dead and moved.
    let late = Client::join(&addr, ClientVersion::V205, "lou");
    let b1 = late
        .seen
        .iter()
        .find(|(id, p)| *id == BADDYPROPS && p[0] == 33)
        .unwrap();
    assert!(b1.1.starts_with(&[0x21, 0x21, 0x72]), "{:?}", b1.1);
    assert!(b1.1.windows(2).any(|w| w == [0x25, 0x29]));
    assert!(b1.1.windows(13).any(|w| w == b"baddygray.png"));
    assert!(!late.seen.iter().any(|(id, _)| *id == ISLEADER));

    // The leader goes: the follower who came first leads now.
    drop(lea);
    fay.until("the lead", |m| m.iter().any(|(id, _)| *id == ISLEADER));
    let _ = std::fs::remove_dir_all(&w);
}
