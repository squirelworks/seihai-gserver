//! Stage 2.6: what a player does stays with the account. With Seihai's own
//! client code on 127.0.0.1, a 2.22 opens a chest and gets its rupees,
//! sets and clears player flags and logs out; the account file then has
//! the chest, the flags and the rupees, and the next login (2.22, and 1.39
//! in gen 2) sees the chest opened and gets the flags back.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key, GserverSession};
use seihai_core::protocol::flagset::flag_message;
use seihai_core::protocol::{ClientVersion, SessionState};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const START: &str = "GLEVNW01\nCHEST 10 12 redrupee -1\nCHEST 20 21 bombs -1\n";

type Session = (
    GserverSession,
    Receiver<std::io::Result<Vec<u8>>>,
    SessionState,
);

fn join(addr: &str, v: ClientVersion) -> Session {
    let mut sess = login_gserver(addr, v, random_key(), "cora", PASSWORD, false)
        .unwrap_or_else(|e| panic!("{v:?}: {e}"));
    let rx = sess.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.board.is_some() && s.chests.len() == 2
    })
    .unwrap_or_else(|e| panic!("{v:?}: {e}"));
    (sess, rx, st)
}

/// The account file once it has `line`.
fn account_with(w: &std::path::Path, line: &str) -> String {
    let path = w.join("accounts/cora.txt");
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.lines().any(|l| l == line) {
            return text;
        }
        assert!(Instant::now() < end, "no {line:?} in:\n{text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn chests_and_flags_stay_with_the_account() {
    let w = world("chests", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), START).unwrap();
    let (addr, server) = start(&w);

    let v = ClientVersion::V222;
    let (mut s, rx, mut st) = join(&addr, v);
    assert!(st.opened_chests.is_empty());
    assert_eq!(st.own_rupees, Some(0));

    // The red rupee chest: 30 rupees.
    s.conn.send_open_chest(10, 12).unwrap();
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.own_rupees == Some(30)
    })
    .unwrap();
    // Opening it again, or a chest that is not there, gives nothing.
    s.conn.send_open_chest(10, 12).unwrap();
    s.conn.send_open_chest(5, 5).unwrap();
    s.conn
        .send_messages(&[
            flag_message("quest", Some("2")),
            flag_message("door", Some("")),
            flag_message("door", None),
            flag_message("met.npc", Some("")),
        ])
        .unwrap();
    let _ = read_until(&rx, &mut st, Duration::from_millis(300), |_| false);
    assert_eq!(st.own_rupees, Some(30));
    assert!(st.ignored.is_empty(), "{:?}", st.ignored);
    drop((s, rx));

    let text = account_with(&w, "CHEST onlinestartlocal.nw 10 12");
    assert!(text.lines().any(|l| l == "RUPEES 30"), "{text}");
    assert!(text.lines().any(|l| l == "FLAG quest=2"), "{text}");
    assert!(text.lines().any(|l| l == "FLAG met.npc"), "{text}");
    assert!(!text.contains("door"), "{text}");
    assert!(!text.contains("20 21"), "{text}");

    // Back in: the chest is open, the flags and rupees are back.
    for v in [ClientVersion::V222, ClientVersion::V139] {
        let (_s, rx, mut st) = join(&addr, v);
        let flags_sent = v.profile().protocol.plo.contains(28);
        read_until(&rx, &mut st, Duration::from_secs(10), |s| {
            !flags_sent || s.flags.len() == 2
        })
        .unwrap_or_else(|e| panic!("{v:?}: {e}"));
        assert_eq!(st.opened_chests, [(10, 12)], "{v:?}");
        assert_eq!(st.own_rupees, Some(30), "{v:?}");
        if flags_sent {
            assert_eq!(st.flags.get("quest").map(String::as_str), Some("2"));
            assert_eq!(st.flags.get("met.npc").map(String::as_str), Some(""));
        }
        assert!(st.ignored.is_empty(), "{v:?}: {:?}", st.ignored);
        drop((_s, rx));
        // Wait for the logout before the next build logs in.
        let end = Instant::now() + Duration::from_secs(10);
        while !server.world.online_accounts().is_empty() {
            assert!(Instant::now() < end, "{v:?} still online");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let _ = std::fs::remove_dir_all(&w);
}
