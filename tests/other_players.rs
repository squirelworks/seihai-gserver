//! Stage 2.3: players in one level see each other arrive, walk and log out,
//! with Seihai's own client code (`seihai_core::net`) on 127.0.0.1. A 2.05
//! and a 2.31 share the start level; the 2.31 walks with X2/Y2 only, as
//! the real 2.31 does, and the 2.05 sees the move in half tiles.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key, walk_message_for};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::Duration;

#[test]
fn two_players_see_each_other() {
    let w = world("others", "*\n");
    let (addr, server) = start(&w);

    let mut a = login_gserver(
        &addr,
        ClientVersion::V205,
        random_key(),
        "amy",
        PASSWORD,
        false,
    )
    .unwrap();
    let ra = a.conn.spawn_reader().unwrap();
    let mut sa = SessionState::for_version(ClientVersion::V205);
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| s.board.is_some()).unwrap();
    assert!(sa.others.is_empty());

    let mut b = login_gserver(
        &addr,
        ClientVersion::V231,
        random_key(),
        "ben",
        PASSWORD,
        false,
    )
    .unwrap();
    let rb = b.conn.spawn_reader().unwrap();
    let mut sb = SessionState::for_version(ClientVersion::V231);
    // Ben gets Amy after his burst, and Amy gets Ben.
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| {
        s.others.iter().any(|o| o.account.as_deref() == Some("amy"))
    })
    .unwrap();
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.iter().any(|o| o.nick.as_deref() == Some("ben"))
    })
    .unwrap();
    let ben = sa.others[0].id;

    // Ben walks a few tiles from the start (30, 30).
    b.conn
        .send_messages(&[walk_message_for(ClientVersion::V231, 26.5, 33.0, 2)])
        .unwrap();
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others
            .iter()
            .any(|o| o.id == ben && o.x == 26.5 && o.y == 33.0)
    })
    .unwrap();

    // Ben logs out; Amy sees him go.
    drop(rb);
    drop(b);
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.is_empty()
    })
    .unwrap();
    assert!(sa.ignored.is_empty(), "{:?}", sa.ignored);
    assert_eq!(server.online_accounts(), ["amy"]);
    let _ = std::fs::remove_dir_all(&w);
}
