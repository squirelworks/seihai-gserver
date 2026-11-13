//! Board changes, with Seihai's own client code on 127.0.0.1. A 2.05
//! cuts a bush (`PLI_BOARDMODIFY`, the lab capture's block); a 2.22 in
//! the same level sees the tiles change (`PLO_BOARDMODIFY`), and a 2.31
//! that arrives later gets the change after its level.

mod common;

use common::*;
use seihai_core::net::{board_modify_message, login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::Duration;

const AT: usize = 14 * 64 + 39;

#[test]
fn a_cut_bush_reaches_the_level_and_later_arrivals() {
    let w = world("board", "*\n");
    let (addr, _server) = start(&w);
    let join = |v: ClientVersion, account: &str| {
        let mut sess = login_gserver(&addr, v, random_key(), account, PASSWORD, false)
            .unwrap_or_else(|e| panic!("{v:?}: {e}"));
        let rx = sess.conn.spawn_reader().unwrap();
        let mut st = SessionState::for_version(v);
        read_until(&rx, &mut st, Duration::from_secs(10), |s| s.board.is_some())
            .unwrap_or_else(|e| panic!("{v:?}: {e}"));
        (sess, rx, st)
    };
    let (mut ann, _ra, _sa) = join(ClientVersion::V205, "ann");
    let (_ben, rb, mut sb) = join(ClientVersion::V222, "ben");
    assert_ne!(sb.board.as_ref().unwrap()[AT], 1834);

    ann.conn
        .send_messages(&[board_modify_message(
            39,
            14,
            2,
            2,
            &[1834, 1835, 1850, 1851],
        )])
        .unwrap();
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| {
        s.board.as_ref().is_some_and(|b| b[AT] == 1834)
    })
    .unwrap();
    assert_eq!(sb.board.as_ref().unwrap()[AT + 64 + 1], 1851);

    let (_cat, rc, mut sc) = join(ClientVersion::V231, "cat");
    read_until(&rc, &mut sc, Duration::from_secs(10), |s| {
        s.board.as_ref().is_some_and(|b| b[AT] == 1834)
    })
    .unwrap();

    // A block that does not fit the board is not passed on.
    let mut bad = board_modify_message(63, 14, 2, 2, &[1, 1, 1, 1]);
    bad[1] = 63 + 0x20;
    ann.conn.send_messages(&[bad]).unwrap();
    let _ = read_until(&rb, &mut sb, Duration::from_millis(500), |_| false);
    assert_eq!(
        sb.board.as_ref().unwrap()[14 * 64 + 63],
        sc.board.as_ref().unwrap()[14 * 64 + 63]
    );
}

const GRID: &str = "GRMAP001\nWIDTH 2\nHEIGHT 1\nLEVELNAMES\n\
\"onlinestartlocal.nw\",\"east.nw\",\nLEVELNAMESEND\n";

/// On a gmap: a 2.05 (plain levels) cuts a bush in the west cell; a 2.22
/// playing the gmap gets it as `PLO_BOARDMODIFY2` (186) with the cell, and
/// so does a 2.22 that logs in later. A 4.04r2's `PLI_REQUESTUPDATEBOARD`
/// (130) for the level gets the change again, in its form.
#[test]
fn gmap_players_get_cell_changes_and_an_update_request_is_answered() {
    use common::raw::RawClient;
    let w = world("board-gmap", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/world.gmap"), GRID).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), "GLEVNW01\n").unwrap();
    std::fs::write(w.join("levels/east.nw"), "GLEVNW01\n").unwrap();
    let (addr, _server) = start(&w);
    let in_level = |m: &common::raw::Seen| m.iter().any(|(id, _)| *id == 6);
    let mut ann = RawClient::join(&addr, ClientVersion::V205, "ann", in_level);
    let mut gia = RawClient::join(&addr, ClientVersion::V222, "gia", in_level);
    gia.settle();

    let cut = board_modify_message(39, 14, 2, 2, &[1834, 1835, 1850, 1851]);
    ann.sess.conn.send_messages(&[cut]).unwrap();
    let is_186 = |p: &[u8]| {
        seihai_core::protocol::layouts::parse_board_modify2(p).is_some_and(|b| {
            (b.map_x, b.map_y, b.x, b.y) == (0, 0, 39, 14) && b.tiles == [1834, 1835, 1850, 1851]
        })
    };
    gia.until("the cut as 186", |m| {
        m.iter().any(|(id, p)| *id == 186 && is_186(p))
    });

    let hal = RawClient::join(&addr, ClientVersion::V222, "hal", |m| {
        m.iter().any(|(id, p)| *id == 186 && is_186(p))
    });
    assert!(!hal.seen.iter().any(|(id, _)| *id == 7));

    // 130: name, mod time, x, y, width, height.
    let request = |level: &str| {
        let mut r = vec![level.len() as u8 + 32];
        r.extend(level.as_bytes());
        seihai_gserver::wire::gint5(0, &mut r);
        for v in [38u16, 13, 4, 4] {
            seihai_gserver::wire::gshort(v, &mut r);
        }
        r
    };
    // 4.04r2 is the first build that sends 130.
    let mut kai = RawClient::join(&addr, ClientVersion::V404R2, "kai", in_level);
    kai.settle();
    kai.send(130, &request("onlinestartlocal.nw"));
    kai.until("the update as 186", |m| {
        m.iter().any(|(id, p)| *id == 186 && is_186(p))
    });
    // A rectangle away from the change gets nothing.
    let mut away = request("onlinestartlocal.nw");
    let n = away.len();
    away[n - 8..n - 6].copy_from_slice(&[0x20, 0x20]);
    away[n - 6..n - 4].copy_from_slice(&[0x20, 0x20]);
    kai.send(130, &away);
    assert!(!kai.settle().iter().any(|(id, _)| *id == 186));
}

/// The board check (#107): with `anticheat.board = correct` and the
/// world's `config/boardrules.txt`, a cut bush that matches a rule is
/// passed on, and a block that matches none is kept from the others and
/// put back for the sender.
#[test]
fn the_board_check_passes_rules_and_puts_back_the_rest() {
    use common::raw::RawClient;
    let w = world("board-check", "*\n");
    std::fs::write(
        w.join("config/boardrules.txt"),
        "# the 2.05 lab's cut bush\nbush 2 2 0 0 0 0 -> 1834 1835 1850 1851\n",
    )
    .unwrap();
    let cfg = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("anticheat = on\nanticheat.board = correct\n");
    std::fs::write(&cfg, text).unwrap();
    let (addr, server) = start(&w);
    let log = server.anticheat.log_path().to_path_buf();
    let in_level = |m: &common::raw::Seen| m.iter().any(|(id, _)| *id == 6);
    let mut ann = RawClient::join(&addr, ClientVersion::V205, "ann", in_level);
    let mut ben = RawClient::join(&addr, ClientVersion::V222, "ben", in_level);
    ann.settle();

    let cut = board_modify_message(39, 14, 2, 2, &[1834, 1835, 1850, 1851]);
    ann.sess.conn.send_messages(&[cut]).unwrap();
    ben.until("the cut", |m| m.iter().any(|(id, _)| *id == 7));

    let paint = board_modify_message(10, 10, 2, 2, &[1, 1, 1, 1]);
    ann.sess.conn.send_messages(&[paint]).unwrap();
    let back = ann.until("the tiles put back", |m| m.iter().any(|(id, _)| *id == 7));
    let (_, p) = back.iter().find(|(id, _)| *id == 7).unwrap();
    let read = seihai_core::protocol::layouts::parse_board_modify(p).unwrap();
    assert_eq!((read.x, read.y), (10, 10));
    assert_eq!(read.tiles, [0, 0, 0, 0]);
    assert!(!ben.settle().iter().any(|(id, _)| *id == 7));
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(logged.contains("board"), "{logged}");
}
