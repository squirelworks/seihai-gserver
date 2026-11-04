//! Stage 2.6: items on the floor, with Seihai's own client code on
//! 127.0.0.1. An item one player puts down (`PLI_ITEMADD`) shows for the
//! others in the level, a player who arrives later sees it, a take
//! (`PLI_ITEMTAKE`) gives it to the first taker only, and a delete
//! (`PLI_ITEMDEL`) clears it for everyone.

mod common;

use common::*;
use seihai_core::items::{item_add_message, item_del_message, item_take_message, GREENRUPEE};
use seihai_core::net::{login_gserver, random_key, GserverSession};
use seihai_core::protocol::{ClientVersion, ServerItem, SessionState};
use std::sync::mpsc::Receiver;
use std::time::Duration;

type Session = (
    GserverSession,
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

fn adds(s: &SessionState) -> Vec<(u8, f32, f32)> {
    s.items
        .iter()
        .filter_map(|i| match i {
            ServerItem::Add(it) => Some((it.item, it.x, it.y)),
            ServerItem::Del(..) => None,
        })
        .collect()
}

fn dels(s: &SessionState) -> Vec<(f32, f32)> {
    s.items
        .iter()
        .filter_map(|i| match i {
            ServerItem::Del(x, y) => Some((*x, *y)),
            ServerItem::Add(_) => None,
        })
        .collect()
}

fn quiet(rx: &Receiver<std::io::Result<Vec<u8>>>, st: &mut SessionState) {
    let _ = read_until(rx, st, Duration::from_millis(300), |_| false);
}

#[test]
fn items_on_the_floor_are_shared() {
    let w = world("floor", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), "GLEVNW01\n").unwrap();
    let (addr, _server) = start(&w);

    let (mut a, ra, mut sa) = join(&addr, ClientVersion::V222, "ann");
    let (mut b, rb, mut sb) = join(&addr, ClientVersion::V205, "ben");
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        !s.others.is_empty()
    })
    .unwrap();

    // Ann rolls a green rupee from a bush; a sword (not a floor item) is
    // not passed on.
    a.conn
        .send_messages(&[
            item_add_message(10, 5.0, 5.0),
            item_add_message(GREENRUPEE, 20.0, 30.5),
        ])
        .unwrap();
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| {
        !adds(s).is_empty()
    })
    .unwrap();
    quiet(&rb, &mut sb);
    assert_eq!(adds(&sb), [(GREENRUPEE, 20.0, 30.5)]);
    assert!(adds(&sa).is_empty(), "the one who put it down is not told");

    // Cal arrives and sees it lying there.
    let (_c, rc, mut sc) = join(&addr, ClientVersion::V222, "cal");
    read_until(&rc, &mut sc, Duration::from_secs(10), |s| {
        !adds(s).is_empty()
    })
    .unwrap();
    assert_eq!(adds(&sc), [(GREENRUPEE, 20.0, 30.5)]);

    // Ben takes it and gets the rupee; Ann and Cal see it go.
    let before = sb.own_rupees.unwrap_or(0);
    b.conn
        .send_messages(&[item_take_message(GREENRUPEE, 20.0, 30.5)])
        .unwrap();
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| {
        s.own_rupees == Some(before + 1)
    })
    .unwrap();
    for (rx, st) in [(&ra, &mut sa), (&rc, &mut sc)] {
        read_until(rx, st, Duration::from_secs(10), |s| !dels(s).is_empty()).unwrap();
        assert_eq!(dels(st), [(20.0, 30.5)]);
    }

    // Ann was a moment late: she gets nothing, and nobody hears of it.
    let ann = sa.own_rupees;
    a.conn
        .send_messages(&[item_take_message(GREENRUPEE, 20.0, 30.5)])
        .unwrap();
    quiet(&ra, &mut sa);
    quiet(&rb, &mut sb);
    assert_eq!(sa.own_rupees, ann);
    assert!(dels(&sb).is_empty());

    // A heart Ann lays, cleared by her own delete.
    a.conn
        .send_messages(&[item_add_message(5, 8.0, 9.0), item_del_message(8.0, 9.0)])
        .unwrap();
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| {
        !dels(s).is_empty()
    })
    .unwrap();
    assert_eq!(dels(&sb), [(8.0, 9.0)]);
    for st in [&sa, &sb, &sc] {
        assert!(st.ignored.is_empty(), "{:?}", st.ignored);
    }
    let _ = std::fs::remove_dir_all(&w);
}
