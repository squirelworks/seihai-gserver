//! The items check, bans and the dead-player relogin, on 127.0.0.1 with
//! Seihai's own client code (issues #54 and #51).

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use seihai_gserver::accounts::{Account, Store};
use seihai_gserver::bans::{Ban, Bans};
use std::time::Duration;

/// `PLI_PLAYERPROPS` with one prop.
fn prop(id: u8, value: &[u8]) -> Vec<u8> {
    let mut m = vec![2 + 0x20, id + 0x20];
    m.extend_from_slice(value);
    m.push(b'\n');
    m
}

fn gint3(v: u32) -> Vec<u8> {
    let mut b = Vec::new();
    seihai_gserver::wire::gint3(v, &mut b);
    b
}

#[test]
fn a_gralat_raise_from_nowhere_is_put_back() {
    let w = world("items", "*\n");
    let cfg = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("anticheat.items = correct\n");
    std::fs::write(&cfg, text).unwrap();
    let (addr, server) = start(&w);
    let log = server.anticheat.log_path().to_path_buf();

    let v = ClientVersion::V222;
    let mut c = login_gserver(&addr, v, random_key(), "cash", PASSWORD, false).unwrap();
    let rx = c.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.board.is_some() && s.own_rupees.is_some() && s.own_cur_power.is_some()
    })
    .unwrap();
    assert_eq!(st.own_rupees, Some(0));
    let max = st.own_max_power.unwrap();

    // Hearts down to nothing, then back to full: the client's respawn.
    c.conn
        .send_messages(&[prop(2, &[0x20]), prop(2, &[0x20 + max * 2])])
        .unwrap();
    // 500 gralats out of thin air: the server's 0 comes back.
    st.own_rupees = None;
    c.conn.send_messages(&[prop(3, &gint3(500))]).unwrap();
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.own_rupees == Some(0)
    })
    .unwrap();
    // A heart container nobody gave.
    c.conn.send_messages(&[prop(1, &[0x20 + max + 1])]).unwrap();
    st.own_max_power = None;
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.own_max_power == Some(max)
    })
    .unwrap();

    let text = std::fs::read_to_string(&log).unwrap();
    let items: Vec<&str> = text.lines().filter(|l| l.contains("\titems\t")).collect();
    assert_eq!(items.len(), 2, "{text}");
    assert!(items[0].contains("gralats 0 to 500"), "{text}");
    assert!(items[1].contains("max hearts"), "{text}");
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn bans_and_dead_players() {
    let w = world("bans", "*\n");
    let store = Store::new(w.join("accounts"));
    // Saved dead: 3 max hearts, 0 left.
    let mut dead = Account::new("ghost", "onlinestartlocal.nw", 30.0, 30.0);
    dead.set_password(PASSWORD.as_bytes());
    dead.max_hp = 3;
    dead.hp = 0.0;
    store.save(&dead).unwrap();
    let (addr, _server) = start(&w);

    // Every generation the burst serves gets full hearts.
    for v in [
        ClientVersion::V139,
        ClientVersion::V205,
        ClientVersion::V222,
        ClientVersion::V231,
    ] {
        let mut c = login_gserver(&addr, v, random_key(), "ghost", PASSWORD, false).unwrap();
        let rx = c.conn.spawn_reader().unwrap();
        let mut st = SessionState::for_version(v);
        read_until(&rx, &mut st, Duration::from_secs(10), |s| {
            s.own_cur_power.is_some()
        })
        .unwrap();
        assert_eq!(st.own_cur_power, Some(6), "{v:?}");
        drop(c);
        // Wait for the logout before the next build logs in.
        std::thread::sleep(Duration::from_millis(300));
    }

    // A ban holds at login, with its reason; lifted, the login goes in.
    let bans = Bans::new(&w);
    bans.add(Ban {
        account: "Ghost".into(),
        until: None,
        admin: "boss".into(),
        reason: "duping".into(),
    })
    .unwrap();
    let m = refused(&addr, ClientVersion::V222, "ghost", PASSWORD);
    assert_eq!(
        m.as_deref(),
        Some("You are banned from this server: duping.")
    );
    assert!(bans.remove("ghost").unwrap());
    assert_eq!(refused(&addr, ClientVersion::V222, "ghost", PASSWORD), None);
    let _ = std::fs::remove_dir_all(&w);
}
