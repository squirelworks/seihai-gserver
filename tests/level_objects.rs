//! Parity rows from #56: hit-objects, show-image, throws and shots reach
//! the others in the level with the sender's id first; a shot reaches each
//! build in the form it reads (`shoot.rs`). Also the word filter on chat,
//! dropping a weapon, and `PLI_VERIFYWANTSEND`. Seihai's own client code
//! on 127.0.0.1; no real client has sent these to this server yet.

mod common;

use common::raw::{has, has_gstring, RawClient, Seen};
use common::*;
use seihai_core::protocol::ClientVersion;

fn in_level(m: &Seen) -> bool {
    m.iter().any(|(id, _)| *id == 6)
}

fn id_of(c: &RawClient, account: &str) -> Vec<u8> {
    c.seen
        .iter()
        .find(|(id, p)| *id == 8 && has_gstring(p, account.as_bytes()))
        .map(|(_, p)| p[..2].to_vec())
        .unwrap_or_else(|| panic!("no {account}"))
}

fn named(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend_from_slice(body);
    v
}

/// A 40: shooter 0, (30.5, 20), z 0, angle, zangle, power,
/// gani `arrow`, no params.
fn shot40() -> Vec<u8> {
    let mut b = b"   ".to_vec();
    b.extend([
        61 + 0x20,
        40 + 0x20,
        50 + 0x20,
        55 + 0x20,
        110 + 0x20,
        44 + 0x20,
    ]);
    b.push(5 + 0x20);
    b.extend_from_slice(b"arrow");
    b.push(0x20);
    b
}

#[test]
fn objects_and_shots_reach_the_level() {
    let w = world("objects", "*\n");
    let (addr, _server) = start(&w);

    let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", in_level);
    let mut bob = RawClient::join(&addr, ClientVersion::V205, "bob", in_level);
    let mut fin = RawClient::join(&addr, ClientVersion::V500R7, "fin", in_level);
    let mut six = RawClient::join(&addr, ClientVersion::V603R7, "six", in_level);
    let mut old = RawClient::join(&addr, ClientVersion::V139, "old", in_level);
    for c in [&mut amy, &mut bob, &mut fin, &mut six, &mut old] {
        c.settle();
    }
    let amy_id = id_of(&fin, "amy");
    let bob_id = id_of(&fin, "bob");
    let six_id = id_of(&amy, "six");

    // Hit-objects (36 -> 46): 2.01 on read it; 1.39 does not.
    let hit = [0x22, 0x5c, 0x5c];
    bob.send(36, &hit);
    fin.until("46", |m| has(m, 46, &named(&bob_id, &hit)));
    six.until("46", |m| has(m, 46, &named(&bob_id, &hit)));
    // Show-image (24 -> 32) and a throw (11 -> 21, the id alone).
    bob.send(24, &[0x29]);
    bob.send(11, b"");
    fin.until("32 and 21", |m| {
        has(m, 32, &named(&bob_id, &[0x29])) && has(m, 21, &bob_id)
    });
    old.until("32", |m| has(m, 32, &named(&bob_id, &[0x29])));

    // A 40 (2.22): as sent to readers of 175, converted to 191 for
    // 6.03r7, which reads no 175.
    let s = shot40();
    amy.send(40, &s);
    fin.until("175", |m| has(m, 175, &named(&amy_id, &s)));
    let two = seihai_gserver::shoot::shoot_to_shoot2(&s, (0, 0)).unwrap();
    six.until("191", |m| has(m, 191, &named(&amy_id, &two)));
    // A 48 (6.03r7): 191 as sent where read, 175 for 2.22.
    six.send(48, &two);
    fin.until("191", |m| has(m, 191, &named(&six_id, &two)));
    amy.until("175", |m| has(m, 175, &named(&six_id, &s)));
    let echo = six.settle();
    assert!(!echo.iter().any(|(id, _)| *id == 191), "echo: {echo:?}");
    let away = old.settle();
    assert!(
        !away.iter().any(|(id, _)| matches!(id, 46 | 175 | 191)),
        "1.39 got {away:?}"
    );
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn chat_goes_through_the_word_filter() {
    let w = world("wordfilter", "*\n");
    std::fs::write(w.join("config/wordfilter.txt"), "heck\ndarn = drat\n").unwrap();
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", in_level);
    let mut bob = RawClient::join(&addr, ClientVersion::V222, "bob", in_level);
    amy.settle();
    bob.settle();
    let text = b"Heck, darn";
    let mut p = vec![12 + 0x20, text.len() as u8 + 0x20];
    p.extend_from_slice(text);
    amy.send(2, &p);
    bob.until("filtered chat", |m| {
        m.iter()
            .any(|(id, p)| *id == 8 && has_gstring(p, b"****, drat"))
    });
    assert!(
        !bob.seen
            .iter()
            .any(|(id, p)| *id == 8 && p.windows(4).any(|w| w == b"Heck")),
        "unfiltered chat reached bob"
    );
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn verify_want_send_compares_crcs() {
    let w = world("verify", "*\n");
    std::fs::create_dir_all(w.join("files")).unwrap();
    let data = b"\x89PNG verify me".to_vec();
    std::fs::write(w.join("files/v.png"), &data).unwrap();
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V420R8, "amy", in_level);
    amy.settle();
    let crc = seihai_gserver::files::crc32(&data);
    let ask = |c: u32| {
        let mut b = Vec::new();
        seihai_gserver::wire::gint5(c as u64, &mut b);
        b.extend_from_slice(b"v.png");
        b
    };
    // Same CRC: up to date (45). Missing: no answer (GServer-v2's rule).
    amy.send(47, &ask(crc));
    amy.until("45", |m| has(m, 45, b"v.png"));
    // A stale copy: the file. 4.20r8 gets it inside `PLO_RAWDATA`, which
    // the test reader skips whole, so the answer is checked directly.
    let found = seihai_gserver::files::Found {
        name: "v.png".into(),
        data: data.clone(),
        mod_time: 1,
    };
    let sent = seihai_gserver::files::verify(
        ClientVersion::V420R8,
        b"v.png",
        "v.png",
        Some(&found),
        crc ^ 1,
        None,
    );
    assert!(
        sent.concat().windows(9).any(|w| w == b"verify me"),
        "{sent:?}"
    );
    assert!(seihai_gserver::files::verify(
        ClientVersion::V420R8,
        b"none.png",
        "none.png",
        None,
        1,
        None
    )
    .is_empty());
    let _ = std::fs::remove_dir_all(&w);
}

/// `PLI_NPCWEAPONDEL` (29): the weapon is gone from the saved account;
/// another weapon added in the same session is kept.
#[test]
fn a_dropped_weapon_leaves_the_account() {
    let w = world("weapondel", "*\n");
    let (addr, _server) = start(&w);
    let mut ann = RawClient::join(&addr, ClientVersion::V222, "ann", in_level);
    ann.settle();
    ann.send(33, &[0x20, 12 + 0x20]);
    ann.send(33, &[0x20, 20 + 0x20]);
    ann.send(29, b"superbomb");
    ann.settle();
    drop(ann);
    let account = w.join("accounts/ann.txt");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(&account).unwrap_or_default();
        if text.contains("WEAPON fireball\n") {
            assert!(!text.contains("WEAPON superbomb"), "{text}");
            break;
        }
        assert!(std::time::Instant::now() < end, "not saved: {text}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = std::fs::remove_dir_all(&w);
}

/// `PLI_BADDYADD` (17): the put baddy goes to everyone in the level, the
/// sender too, as `PLO_BADDYPROPS` (2) with the next free id.
#[test]
fn a_put_baddy_reaches_the_level() {
    let w = world("baddyadd", "*\n");
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V205, "amy", in_level);
    let mut bob = RawClient::join(&addr, ClientVersion::V222, "bob", in_level);
    amy.settle();
    bob.settle();
    // (30, 20), type 2, the type's own power and image.
    amy.send(17, &[0x5c, 0x48, 0x22, 0x20]);
    let put = |m: &Seen| {
        m.iter().any(|(id, p)| {
            *id == 2 && p.first() == Some(&0x21) && p.windows(8).any(|w| w == b"baddyred")
        })
    };
    amy.until("the put baddy (sender)", put);
    bob.until("the put baddy", put);
    // A second one gets id 2.
    amy.send(17, &[0x5c, 0x48, 0x25, 0x20]);
    bob.until("the second baddy", |m| {
        m.iter().any(|(id, p)| *id == 2 && p.first() == Some(&0x22))
    });
    let _ = std::fs::remove_dir_all(&w);
}
