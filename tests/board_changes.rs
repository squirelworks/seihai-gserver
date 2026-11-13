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
