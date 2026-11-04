//! Stage 2.4: links between levels (`PLI_LEVELWARP`, `PLI_LEVELWARPMOD`),
//! with Seihai's own client code (`seihai_core::net`) on 127.0.0.1. A link
//! loads the new level's board and links, players in the old level see the
//! player leave, and a link with the client's cached mod time gets the name
//! alone, so the client fills the level from its own copy.

mod common;

use common::*;
use seihai_core::net::{
    levelwarp_message, levelwarpmod_message, login_gserver, random_key, wantfile_message,
};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::sync::mpsc::Receiver;
use std::time::Duration;

const START: &str = "GLEVNW01\nLINK house.nw 30 0 2 1 30 60\n";
const HOUSE: &str = "GLEVNW01\n\
BOARD 0 0 64 0 AB\n\
LINK onlinestartlocal.nw 30 63 2 1 30.5 2\n\
SIGN 20 21\nWelcome home.\nSIGNEND\n";

type Session = (
    seihai_core::net::GserverSession,
    Receiver<std::io::Result<Vec<u8>>>,
    SessionState,
);

fn join(addr: &str, v: ClientVersion, account: &str) -> Session {
    let mut sess = login_gserver(addr, v, random_key(), account, PASSWORD, false)
        .unwrap_or_else(|e| panic!("{v:?}: {e}"));
    let rx = sess.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| s.board.is_some())
        .unwrap_or_else(|e| panic!("{v:?}: {e}"));
    (sess, rx, st)
}

fn in_house(s: &SessionState, board: &[u16]) -> bool {
    s.current_level_name().as_deref() == Some("house.nw") && s.board.as_deref() == Some(board)
}

#[test]
fn links_load_the_next_level() {
    let w = world("warps", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), START).unwrap();
    std::fs::write(w.join("levels/house.nw"), HOUSE).unwrap();
    // Players start on the link into the house: the anti-cheat answers a
    // warp only through a link the player stands on.
    let cfg = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("start_y = 1\n");
    std::fs::write(&cfg, text).unwrap();
    let house = seihai_core::assets::Level::parse(HOUSE).unwrap().tile_ids();
    let start_board = seihai_core::assets::Level::parse(START).unwrap().tile_ids();
    let (addr, _server) = start(&w);

    // Amy (2.05) waits in the start level.
    let (_a, ra, mut sa) = join(&addr, ClientVersion::V205, "amy");

    // Ben (2.22) arrives there, then takes the link into the house.
    let (mut b, rb, mut sb) = join(&addr, ClientVersion::V222, "ben");
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.iter().any(|o| o.nick.as_deref() == Some("ben"))
    })
    .unwrap();
    b.conn
        .send_messages(&[levelwarp_message(30.0, 60.0, "house.nw")])
        .unwrap();
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| {
        in_house(s, &house) && s.signs.len() == 1 && s.active_level.is_some()
    })
    .unwrap();
    assert_eq!(sb.links, ["onlinestartlocal.nw 30 63 2 1 30.5 2"]);
    assert_eq!(sb.active_level.as_deref(), Some("house.nw"));
    // Amy sees him leave her level; he no longer sees her.
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.is_empty()
    })
    .unwrap();
    assert!(sb
        .others
        .iter()
        .all(|o| o.account.as_deref() != Some("amy")));

    // Back out, then in again with the cached copy's mod time: the answer
    // has the name alone, and the client fills the house from its cache.
    let mod_time = sb.server_mod_time("house.nw").expect("house mod time");
    b.conn
        .send_messages(&[levelwarp_message(30.5, 2.0, "onlinestartlocal.nw")])
        .unwrap();
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| {
        s.board.as_deref() == Some(&start_board[..])
    })
    .unwrap();
    // Amy sees him come back.
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.iter().any(|o| o.nick.as_deref() == Some("ben"))
    })
    .unwrap();
    let restores = sb.cache_restores;
    sb.active_level = None;
    sb.expect_cached_level("house.nw");
    b.conn
        .send_messages(&[levelwarpmod_message(mod_time, 30.0, 60.0, "house.nw")])
        .unwrap();
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| {
        in_house(s, &house) && s.cache_restores > restores && s.active_level.is_some()
    })
    .unwrap();
    assert_eq!(sb.cache_restores, restores + 1);
    assert_eq!(sb.signs.len(), 1);
    assert!(sb.ignored.is_empty(), "{:?}", sb.ignored);

    // Cal (1.39, gen 2) takes the same link.
    let (mut c, rc, mut sc) = join(&addr, ClientVersion::V139, "cal");
    c.conn
        .send_messages(&[levelwarp_message(30.0, 60.0, "house.nw")])
        .unwrap();
    read_until(&rc, &mut sc, Duration::from_secs(10), |s| {
        in_house(s, &house)
    })
    .unwrap();
    // Back and forth with a file request in flight each time: every board
    // arrives whole and every request gets its answer (the suite stress
    // run lost answers to the gen-2 board slot).
    let start_board = seihai_core::assets::Level::parse(START).unwrap().tile_ids();
    for i in 0..10 {
        let (to, x, y, board) = if i % 2 == 0 {
            ("onlinestartlocal.nw", 30.5, 2.0, &start_board)
        } else {
            ("house.nw", 30.0, 60.0, &house)
        };
        let asked = sc.file_requests.len();
        c.conn
            .send_messages(&[
                wantfile_message("head0.png"),
                levelwarp_message(x, y, to),
                wantfile_message("body.png"),
            ])
            .unwrap();
        read_until(&rc, &mut sc, Duration::from_secs(10), |s| {
            s.current_level_name().as_deref() == Some(to)
                && s.board.as_deref() == Some(&board[..])
                && s.file_requests.len() >= asked + 2
        })
        .unwrap_or_else(|e| panic!("round {i}: {e}"));
    }
    assert!(sc.ignored.is_empty(), "{:?}", sc.ignored);
    assert!(sc.disconnect_message.is_none());

    // A link to a level that is not there changes nothing.
    b.conn
        .send_messages(&[levelwarp_message(1.0, 1.0, "nowhere.nw")])
        .unwrap();
    let _ = read_until(&rb, &mut sb, Duration::from_millis(500), |_| false);
    assert_eq!(sb.current_level_name().as_deref(), Some("house.nw"));
    assert!(sb.disconnect_message.is_none());
    let _ = std::fs::remove_dir_all(&w);
}
