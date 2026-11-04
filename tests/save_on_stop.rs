//! A stop saves the players still online. With Seihai's own client code on
//! 127.0.0.1, a 2.22 opens a chest and stays online; the stop's save (what
//! Ctrl-C and SIGTERM run) writes the chest and the rupees to the account
//! file while the player is still connected.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::Duration;

#[test]
fn a_stop_saves_who_is_online() {
    let w = world("savestop", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(
        w.join("levels/onlinestartlocal.nw"),
        "GLEVNW01\nCHEST 10 12 redrupee -1\n",
    )
    .unwrap();
    let (addr, server) = start(&w);

    let v = ClientVersion::V222;
    let mut s = login_gserver(&addr, v, random_key(), "stan", PASSWORD, false).unwrap();
    let rx = s.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.board.is_some() && s.chests.len() == 1
    })
    .unwrap();
    s.conn.send_open_chest(10, 12).unwrap();
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.own_rupees == Some(30)
    })
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(rt.block_on(server.world.save_all()), 1);
    let text = std::fs::read_to_string(w.join("accounts/stan.txt")).unwrap();
    assert!(
        text.lines().any(|l| l == "CHEST onlinestartlocal.nw 10 12"),
        "{text}"
    );
    assert!(text.lines().any(|l| l == "RUPEES 30"), "{text}");
    // Still online: the save is not a logout.
    assert_eq!(server.world.online_accounts(), ["stan"]);
    drop((s, rx));
    let _ = std::fs::remove_dir_all(&w);
}
