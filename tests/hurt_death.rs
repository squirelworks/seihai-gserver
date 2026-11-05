//! Stage 3.4: players hurting and killing each other, with Seihai's own
//! client code on 127.0.0.1. A hit (`PLI_HURTPLAYER`) reaches its victim
//! in the shape GServer-v2 beta4 sends (lab capture, 2026-11-05). A kill
//! claim (`PLI_CLAIMPKER`) costs the killer alignment, which they and the
//! others in the level are told, and is counted on the killer's account.

mod common;

use common::raw::{RawClient, Seen};
use common::*;
use seihai_core::protocol::ClientVersion;
use std::time::{Duration, Instant};

const OTHERPLPROPS: u8 = 8;
const PLAYERPROPS: u8 = 9;
const HURTPLAYER: u8 = 40;

fn in_level(m: &Seen) -> bool {
    m.iter().any(|(id, _)| *id == 6)
}

/// The id the others know `account` by: an `OTHERPLPROPS` naming them.
fn id_of(c: &RawClient, account: &str) -> Vec<u8> {
    c.seen
        .iter()
        .find(|(id, p)| {
            *id == OTHERPLPROPS && p.windows(account.len()).any(|w| w == account.as_bytes())
        })
        .map(|(_, p)| p[..2].to_vec())
        .unwrap_or_else(|| panic!("no {account}"))
}

#[test]
fn hits_and_kills() {
    let w = world("hurt", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), "GLEVNW01\n").unwrap();
    let mut amy = seihai_gserver::accounts::Account::new("amy", "onlinestartlocal.nw", 30.0, 30.0);
    amy.alignment = 50;
    amy.set_password(PASSWORD.as_bytes());
    seihai_gserver::accounts::Store::new(w.join("accounts"))
        .save(&amy)
        .unwrap();
    let (addr, _server) = start(&w);

    let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", in_level);
    // Her own alignment is in her login props.
    assert!(amy
        .seen
        .iter()
        .any(|(id, p)| *id == PLAYERPROPS && p.windows(2).any(|w| w == [32 + 32, 50 + 32])));
    let mut bob = RawClient::join(&addr, ClientVersion::V205, "bob", in_level);
    amy.until("bob", |m| m.iter().any(|(id, _)| *id == OTHERPLPROPS));
    let (amy_id, bob_id) = (id_of(&bob, "amy"), id_of(&amy, "bob"));

    // Amy hits Bob: he gets her id, the hit, and `ff ff ff`.
    let mut hit = bob_id.clone();
    hit.extend([0x68, 0x60, 0x21]);
    amy.send(26, &hit);
    let mut want = amy_id.clone();
    want.extend([0x68, 0x60, 0x21, 0xFF, 0xFF, 0xFF]);
    bob.until("the hit", |m| {
        m.iter().any(|(id, p)| *id == HURTPLAYER && *p == want)
    });

    // Bob dies and names Amy: at 50 AP the kill costs her 6.
    bob.send(14, &amy_id);
    amy.until("her AP", |m| {
        m.iter()
            .any(|(id, p)| *id == PLAYERPROPS && *p == [32 + 32, 44 + 32])
    });
    let mut seen_by_bob = amy_id.clone();
    seen_by_bob.extend([32 + 32, 44 + 32]);
    bob.until("Amy's AP", |m| {
        m.iter()
            .any(|(id, p)| *id == OTHERPLPROPS && p.starts_with(&seen_by_bob))
    });

    // A claim naming oneself counts for nothing.
    amy.send(14, &amy_id);
    let after = amy.settle();
    assert!(!after.iter().any(|(id, _)| *id == PLAYERPROPS), "{after:?}");

    // Her account has the kill and the AP once she logs out.
    drop(amy);
    let path = w.join("accounts/amy.txt");
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.lines().any(|l| l == "KILLS 1") && text.lines().any(|l| l == "ALIGN 44") {
            break;
        }
        assert!(Instant::now() < end, "{text}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_dir_all(&w);
}
