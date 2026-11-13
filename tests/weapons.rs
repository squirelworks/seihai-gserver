//! Weapons, with Seihai's own client code on 127.0.0.1. A 2.22 player
//! touches an NPC whose script runs `toweapons`, sends `PLI_WEAPONADD`
//! (33) with the NPC's id, and gets the weapon back as
//! `PLO_NPCWEAPONADD` with the NPC's image and script. The world writes
//! the weapon to `weapons/` and the account keeps it, so the next login
//! gets it again after the burst. A 5.24r4 login with the same account
//! gets the default weapons but not the GS1 weapon.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::{Duration, Instant};

const START: &str = "GLEVNW01\n\
NPC lantern.png 30 30\nif (playertouchsme) {\n  toweapons Lantern;\n  hide;\n}\nNPCEND\n";

fn gc(v: u8) -> u8 {
    v + 0x20
}

#[test]
fn a_weapon_from_an_npc_is_kept() {
    let w = world("weapons", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), START).unwrap();
    let (addr, _server) = start(&w);

    let v = ClientVersion::V222;
    let mut sess = login_gserver(&addr, v, random_key(), "ann", PASSWORD, false).unwrap();
    let rx = sess.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.board.is_some() && !s.server_npcs.is_empty()
    })
    .unwrap();
    assert!(st.weapons.is_empty());
    let npc = *st.server_npcs.keys().next().unwrap();

    // PLI_WEAPONADD: type 1, the NPC's GINT3 id.
    let n = npc as usize;
    let m = vec![
        gc(33),
        gc(1),
        gc((n >> 14) as u8 & 0x7F),
        gc((n >> 7) as u8 & 0x7F),
        gc(n as u8 & 0x7F),
        b'\n',
    ];
    sess.conn.send_messages(&[m]).unwrap();
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        !s.weapons.is_empty()
    })
    .unwrap();
    let got = &st.weapons[0];
    assert_eq!(got.name, "Lantern");
    assert_eq!(got.image.as_deref(), Some("lantern.png"));
    assert!(got
        .script
        .as_deref()
        .unwrap()
        .contains("toweapons Lantern;"));
    let file = std::fs::read_to_string(w.join("weapons/weaponLantern.txt")).unwrap();
    assert!(
        file.starts_with("GRAWP001\nREALNAME Lantern\nIMAGE lantern.png\n"),
        "{file}"
    );

    // A type 0 for an id that is no default weapon is ignored; the bomb is
    // kept by name.
    sess.conn
        .send_messages(&[
            vec![gc(33), gc(0), gc(3), b'\n'],
            vec![gc(33), gc(0), gc(12), b'\n'],
        ])
        .unwrap();
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.default_weapons.contains(&12)
    })
    .unwrap();
    drop(rx);
    drop(sess);

    // The account file is written when the player leaves.
    let account = w.join("accounts/ann.txt");
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(&account).unwrap_or_default();
        if text.contains("WEAPON Lantern\n") && text.contains("WEAPON superbomb\n") {
            assert!(!text.contains("WEAPON bombs"), "{text}");
            break;
        }
        assert!(Instant::now() < end, "not saved: {text}");
        std::thread::sleep(Duration::from_millis(50));
    }

    // The next 2.22 login gets the weapon after the burst.
    let mut sess = login_gserver(&addr, v, random_key(), "ann", PASSWORD, false).unwrap();
    let rx = sess.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.weapons.iter().any(|w| w.name == "Lantern") && s.default_weapons.contains(&12)
    })
    .unwrap();
    drop(rx);
    drop(sess);

    // A 5.24r4 runs no GS1 and is sent no GS1 weapon. It still gets its
    // default weapons.
    let end = Instant::now() + Duration::from_secs(10);
    let v = ClientVersion::V524R4;
    let (mut sess, rx) = loop {
        match login_gserver(&addr, v, random_key(), "ann", PASSWORD, false) {
            Ok(mut s) => {
                let rx = s.conn.spawn_reader().unwrap();
                break (s, rx);
            }
            // The 2.22 session may still be leaving.
            Err(e) if Instant::now() < end => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("{e}"),
        }
    };
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.default_weapons.contains(&12)
    })
    .unwrap();
    let _ = read_until(&rx, &mut st, Duration::from_millis(500), |_| false);
    assert!(st.weapons.is_empty(), "{:?}", st.weapons);
    let _ = &mut sess;
}

/// `PLI_TOWEAPON` (25), which 1.21 to 1.37 send for a weapon they hold:
/// a name an NPC in the level gives with `toweapons` is kept, with the
/// NPC's script, not the one the client sent; a name nothing gives is
/// not. The next login gets the kept one back.
#[test]
fn a_weapon_upload_is_kept_only_when_the_level_gives_it() {
    use common::raw::RawClient;
    let w = world("toweapon", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), START).unwrap();
    let (addr, _server) = start(&w);
    let in_level = |m: &common::raw::Seen| m.iter().any(|(id, _)| *id == 6);
    let upload = |name: &str, script: &str| {
        let mut b = vec![gc(name.len() as u8)];
        b.extend(name.as_bytes());
        b.extend([gc(0), gc(3)]);
        b.extend(b"x.png");
        b.push(gc(1));
        seihai_gserver::wire::gshort(script.len() as u16, &mut b);
        b.extend(script.as_bytes());
        b
    };
    let mut old = RawClient::join(&addr, ClientVersion::V132, "old", in_level);
    old.send(25, &upload("Lantern", "say2 forged;"));
    old.send(25, &upload("Evil", "setplayerprop #c,cheat;"));
    old.settle();
    drop(old);
    let account = w.join("accounts/old.txt");
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(&account).unwrap_or_default();
        if text.contains("WEAPON Lantern") {
            assert!(!text.contains("Evil"), "{text}");
            break;
        }
        assert!(Instant::now() < end, "Lantern never kept: {text}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let weapon = std::fs::read_to_string(w.join("weapons/weapon_Lantern.txt"))
        .or_else(|_| std::fs::read_to_string(w.join("weapons/weapon_lantern.txt")))
        .unwrap_or_default();
    assert!(!weapon.contains("forged"), "{weapon}");

    let again = RawClient::join(&addr, ClientVersion::V132, "old", |m| {
        m.iter()
            .any(|(id, p)| *id == 33 && p.windows(7).any(|w| w == b"Lantern"))
    });
    let (_, p) = again
        .seen
        .iter()
        .find(|(id, p)| *id == 33 && p.windows(7).any(|w| w == b"Lantern"))
        .unwrap();
    assert!(p.windows(8).any(|w| w == b"toweapon"), "{p:?}");
    assert!(!p.windows(6).any(|w| w == b"forged"));
}

/// A 4.20r8 holding a weapon gets `PLO_NPCWEAPONADD` (33) with its name
/// and image, then `PLO_WEAPONHASH` (193). When it asks with
/// `PLI_UPDATESCRIPT` (158) it gets the bytecode in `PLO_UNKNOWN132`
/// behind `PLO_RAWDATA`, whose CRC-32 is the one 193 gave. A name it does
/// not hold is not answered. (The order GServer-v2 used with a real
/// 4.20r8, lab 2026-11-13.)
#[test]
fn a_gs2_build_gets_the_weapon_hash_and_the_bytecode_it_asks_for() {
    use common::raw::RawClient;
    let w = world("weaponhash", "*\n");
    std::fs::create_dir_all(w.join("weapons")).unwrap();
    std::fs::write(
        w.join("weapons/weaponLantern.txt"),
        "GRAWP001\nREALNAME Lantern\nIMAGE lantern.png\nSCRIPT\nif (weaponfired) {\n  say2 light;\n}\nSCRIPTEND\n",
    )
    .unwrap();
    let (addr, server) = start(&w);
    let v = ClientVersion::V420R8;
    let in_level = |m: &common::raw::Seen| m.iter().any(|(id, _)| *id == 6);

    // The first login makes the account; then it holds the weapon. The
    // account file is there from the login, but the server writes it again
    // when the player leaves, so wait for the leave before editing it: an
    // edit made first is overwritten, and the next login holds nothing.
    drop(RawClient::join(&addr, v, "gs2", in_level));
    let end = Instant::now() + Duration::from_secs(10);
    while server.online_accounts().contains(&"gs2".to_string()) {
        assert!(Instant::now() < end, "gs2 never left");
        std::thread::sleep(Duration::from_millis(20));
    }
    let account = w.join("accounts/gs2.txt");
    let text = std::fs::read_to_string(&account).unwrap();
    assert!(!text.is_empty(), "account not saved");
    std::fs::write(&account, format!("{text}WEAPON Lantern\n")).unwrap();

    let hash = |m: &common::raw::Seen| m.iter().any(|(id, _)| *id == 193);
    let mut c = RawClient::join(&addr, v, "gs2", hash);
    let add = c
        .seen
        .iter()
        .position(|(id, p)| *id == 33 && p.starts_with(b"'Lantern"))
        .unwrap();
    let (_, add_p) = &c.seen[add];
    assert_eq!(add_p.as_slice(), b"'Lantern +lantern.png");
    let at = c.seen.iter().position(|(id, _)| *id == 193).unwrap();
    assert!(add < at);
    let (_, h) = &c.seen[at];
    assert_eq!(&h[5..], b"Lantern");
    let crc = h[..5].iter().fold(0u64, |a, b| a << 7 | u64::from(b - 32));

    // A name the account does not hold gets nothing.
    c.send(158, b"Evil");
    c.send(158, b"Lantern");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut body = Vec::new();
    // PLO_RAWDATA (`0x84`), its GINT3 count and `\n`, then 132 (`0xa4`).
    let raw_at = |b: &[u8]| {
        (0..b.len().saturating_sub(5))
            .find(|&i| b[i] == 0x84 && b[i + 4] == b'\n' && b[i + 5] == 0xa4)
    };
    let found = loop {
        if let Some(i) = raw_at(&body) {
            break i;
        }
        assert!(Instant::now() < deadline, "no 132: {body:?}");
        if let Ok(Ok(b)) = c.rx.recv_timeout(Duration::from_millis(100)) {
            body.extend(b);
        }
    };
    let n = body[found + 1..found + 4]
        .iter()
        .fold(0usize, |a, c| a << 7 | usize::from(c - 32));
    let block = &body[found + 5..found + 5 + n];
    assert_eq!(block[0], 132 + 32);
    assert_eq!(&block[1..9], b"'Lantern");
    // The bytecode as sent is the block less its closing `\n`: the file,
    // its own `\n` included, which is what 193's CRC is of.
    assert!(block.ends_with(b"\n\n"));
    let code = &block[9..block.len() - 1];
    let mut sum = flate2::Crc::new();
    sum.update(code);
    assert_eq!(u64::from(sum.sum()), crc);
    assert!(!body.windows(4).any(|w| w == b"Evil"));
}
