//! Stage 3.2: baddies, with Seihai's own client code on 127.0.0.1. The
//! first player in a level leads it (`PLO_ISLEADER`) and gets the level's
//! baddies; a second player gets the baddies and no leader flag. The
//! leader's changes reach the follower, the follower's hit reaches the
//! leader, a dying baddy is dead for both two seconds later, and when the
//! leader leaves, the follower leads.

mod common;

use common::raw::{has, RawClient, Seen};
use common::*;
use seihai_core::protocol::ClientVersion;
use std::time::{Duration, Instant};

const LEVEL: &str = "GLEVNW01\n\
BADDY 40 30 0\nHalt!\nOuch!\nTake that!\nBADDYEND\n\
BADDY 20 40 7\n\n\n\nBADDYEND\n";

const ISLEADER: u8 = 10;
const BADDYPROPS: u8 = 2;
const BADDYHURT: u8 = 27;

fn both_baddies(m: &Seen) -> bool {
    m.iter().filter(|(id, _)| *id == BADDYPROPS).count() >= 2
}

#[test]
fn the_leader_runs_the_baddies() {
    let w = world("baddies", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), LEVEL).unwrap();
    let (addr, _server) = start(&w);

    let mut lea = RawClient::join(&addr, ClientVersion::V222, "lea", both_baddies);
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

    let mut fay = RawClient::join(&addr, ClientVersion::V139, "fay", both_baddies);
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
    let late = RawClient::join(&addr, ClientVersion::V205, "lou", both_baddies);
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
