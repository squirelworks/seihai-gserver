//! The RC player list and RC chat (stage 4 slice 2, issue #52) on
//! loopback, with Seihai's own RC client (`seihai-rc`) and game client
//! code. Only `127.0.0.1` is used.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use seihai_gserver::accounts::{Account, Store};
use seihai_rc::rc::{Event, LoginParams, RcKind, Session};
use std::path::Path;
use std::time::{Duration, Instant};

fn staff(w: &Path, name: &str) {
    let mut a = Account::new(name, "onlinestartlocal.nw", 30.0, 30.0);
    a.set_password(PASSWORD.as_bytes());
    a.staff = true;
    Store::new(w.join("accounts")).save(&a).unwrap();
}

fn rc(addr: &str, kind: RcKind, account: &str, nick: Option<&str>) -> Session {
    Session::login(
        addr,
        &LoginParams {
            kind,
            token: kind.default_token().into(),
            account: account.into(),
            password: PASSWORD.into(),
            key: 0x2A,
            nickname: nick.map(String::from),
        },
        Duration::from_secs(10),
    )
    .unwrap()
}

/// Events until `done` holds for one of them (10 s at most); all of them.
fn until(s: &mut Session, done: impl Fn(&Event) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while let Some(ev) = s.next_event(deadline).unwrap() {
        let hit = done(&ev);
        seen.push(ev);
        if hit {
            return seen;
        }
    }
    panic!("not seen; got {seen:?}");
}

/// `PLI_PLAYERPROPS` (2) with prop 0, the nickname.
fn nick_prop(nick: &str) -> Vec<u8> {
    let mut m = vec![2 + 0x20, 0x20, 0x20 + nick.len() as u8];
    m.extend(nick.as_bytes());
    m.push(b'\n');
    m
}

#[test]
fn rcs_list_players_and_each_other_and_chat() {
    let w = world("rcplayers", "*\n");
    staff(&w, "boss");
    staff(&w, "deputy");
    let (addr, _server) = start(&w);

    // A player is on.
    let v = ClientVersion::V222;
    let mut c = login_gserver(&addr, v, random_key(), "cash", PASSWORD, false).unwrap();
    let rx = c.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| s.board.is_some()).unwrap();

    // The first RC gets the player, then hears of itself on RC chat.
    let mut boss = rc(&addr, RcKind::Gen5, "boss", Some("Boss"));
    until(
        &mut boss,
        |e| matches!(e, Event::RcChat(t) if t == "New RC: boss"),
    );
    let cash = boss.players.players.values().find(|e| e.account == "cash");
    let cash = cash.expect("the player is listed").clone();
    assert!(!cash.is_rc());
    assert_eq!(cash.level.to_ascii_lowercase(), "onlinestartlocal.nw");
    assert_eq!(cash.community, "cash");
    assert!(
        boss.players.players.values().all(|e| e.account != "boss"),
        "an RC's own entry is not sent to it"
    );

    // The second RC (the old framing, no nickname) gets both; the first
    // gets it, listed as an RC with `*account`.
    let mut deputy = rc(&addr, RcKind::Classic, "deputy", None);
    until(
        &mut deputy,
        |e| matches!(e, Event::RcChat(t) if t == "New RC: deputy"),
    );
    let listed: Vec<_> = deputy.players.players.values().cloned().collect();
    assert_eq!(listed.len(), 2, "{listed:?}");
    let b = listed.iter().find(|e| e.account == "boss").unwrap();
    assert!(b.is_rc());
    assert_eq!(b.nickname, "Boss");
    until(
        &mut boss,
        |e| matches!(e, Event::AddPlayer(p) if p.account == "deputy" && p.is_rc() && p.nickname == "*deputy"),
    );
    until(
        &mut boss,
        |e| matches!(e, Event::RcChat(t) if t == "New RC: deputy"),
    );

    // RC chat goes to every RC, under the sender's name.
    deputy.send_rc_chat("hello all").unwrap();
    until(
        &mut boss,
        |e| matches!(e, Event::RcChat(t) if t == "*deputy: hello all"),
    );
    until(
        &mut deputy,
        |e| matches!(e, Event::RcChat(t) if t == "*deputy: hello all"),
    );

    // A nickname change is a list update.
    c.conn.send_messages(&[nick_prop("Cashy")]).unwrap();
    until(
        &mut boss,
        |e| matches!(e, Event::UpdatePlayer(p) if p.id == cash.id && p.nickname == "Cashy"),
    );

    // The player leaves, then the second RC.
    drop(c);
    until(&mut boss, |e| *e == Event::DelPlayer(cash.id));
    until(&mut deputy, |e| *e == Event::DelPlayer(cash.id));
    let deputy_id = b_id(&boss, "deputy");
    drop(deputy);
    until(&mut boss, |e| *e == Event::DelPlayer(deputy_id));
    let _ = std::fs::remove_dir_all(&w);
}

fn b_id(s: &Session, account: &str) -> u16 {
    s.players
        .players
        .values()
        .find(|e| e.account == account)
        .unwrap()
        .id
}

#[test]
fn an_rc_messages_and_disconnects_players() {
    use seihai_core::protocol::ChatKind;
    use seihai_rc::players::{
        admin_message, disconnect_player, private_admin_message, private_message,
    };
    let w = world("rcactions", "*\n");
    staff(&w, "boss");
    let (addr, _server) = start(&w);

    let v = ClientVersion::V222;
    let login = |name: &str| {
        let mut c = login_gserver(&addr, v, random_key(), name, PASSWORD, false).unwrap();
        let rx = c.conn.spawn_reader().unwrap();
        let mut st = SessionState::for_version(v);
        read_until(&rx, &mut st, Duration::from_secs(10), |s| s.board.is_some()).unwrap();
        (c, rx, st)
    };
    let (_a, a_rx, mut a) = login("alice");
    let (_b, b_rx, mut b) = login("bob");

    let mut boss = rc(&addr, RcKind::Gen5, "boss", Some("Boss"));
    until(
        &mut boss,
        |e| matches!(e, Event::RcChat(t) if t == "New RC: boss"),
    );
    let alice = b_id(&boss, "alice");
    let bob = b_id(&boss, "bob");

    // The RC is in each player's list, nickname and account, before any
    // PM, as beta4 sends it (lab run 2026-11-12): a real 2.22 queues an
    // RC's PM under that entry.
    let listed = |s: &SessionState| {
        s.others
            .iter()
            .any(|o| o.account.as_deref() == Some("boss") && o.nick.as_deref() == Some("Boss"))
    };
    read_until(&a_rx, &mut a, Duration::from_secs(10), listed).unwrap();
    // A player who logs in after the RC gets it too.
    let (_c, c_rx, mut c) = login("carol");
    read_until(&c_rx, &mut c, Duration::from_secs(10), listed).unwrap();

    // A private message, named by the RC's nickname.
    boss.send_messages(&[private_message(&[alice], "hi \"you\"").unwrap()])
        .unwrap();
    read_until(&a_rx, &mut a, Duration::from_secs(10), |s| {
        s.chat_log.iter().any(|l| l.kind == ChatKind::Private)
    })
    .unwrap();
    let pm = a
        .chat_log
        .iter()
        .find(|l| l.kind == ChatKind::Private)
        .unwrap();
    assert_eq!((pm.from.as_str(), pm.text.as_str()), ("Boss", "hi \"you\""));

    // An admin message to everyone, then one to Bob only.
    boss.send_messages(&[admin_message("restart soon").unwrap()])
        .unwrap();
    boss.send_messages(&[private_admin_message(bob, "just you").unwrap()])
        .unwrap();
    read_until(&b_rx, &mut b, Duration::from_secs(10), |s| {
        s.chat_log
            .iter()
            .filter(|l| l.kind == ChatKind::Admin)
            .count()
            == 2
    })
    .unwrap();
    let admin: Vec<_> = b
        .chat_log
        .iter()
        .filter(|l| l.kind == ChatKind::Admin)
        .map(|l| (l.from.as_str(), l.text.as_str()))
        .collect();
    assert_eq!(
        admin,
        [("Admin boss", "restart soon"), ("Admin boss", "just you")]
    );
    read_until(&a_rx, &mut a, Duration::from_secs(10), |s| {
        s.chat_log.iter().any(|l| l.kind == ChatKind::Admin)
    })
    .unwrap();

    // A disconnect: Bob is told and dropped, the RC sees him leave.
    boss.send_messages(&[disconnect_player(bob).unwrap()])
        .unwrap();
    until(&mut boss, |e| *e == Event::DelPlayer(bob));
    let _ = read_until(&b_rx, &mut b, Duration::from_secs(10), |s| {
        s.disconnect_message.is_some()
    });
    assert_eq!(
        b.disconnect_message.as_deref(),
        Some("Disconnected by the server.")
    );
    assert!(
        !a.chat_log.iter().any(|l| l.text == "just you"),
        "the private admin message went to Bob only"
    );

    // The RC leaves: off the players' lists.
    drop(boss);
    read_until(&c_rx, &mut c, Duration::from_secs(10), |s| !listed(s)).unwrap();
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn an_rc_warps_a_player() {
    use seihai_rc::players::warp_player;
    let w = world("rcwarp", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), "GLEVNW01\n").unwrap();
    std::fs::write(w.join("levels/house.nw"), "GLEVNW01\n").unwrap();
    staff(&w, "boss");
    let (addr, _server) = start(&w);

    let v = ClientVersion::V222;
    let mut c = login_gserver(&addr, v, random_key(), "cash", PASSWORD, false).unwrap();
    let rx = c.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| s.board.is_some()).unwrap();

    let mut boss = rc(&addr, RcKind::Gen5, "boss", None);
    until(
        &mut boss,
        |e| matches!(e, Event::RcChat(t) if t == "New RC: boss"),
    );
    let cash = b_id(&boss, "cash");

    // Inside the level: the player is told where to stand.
    boss.send_messages(&[warp_player(cash, "onlinestartlocal.nw", 12.5, 40.0).unwrap()])
        .unwrap();
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.player_x == Some(12.5) && s.player_y == Some(40.0)
    })
    .unwrap();

    // A level that does not exist: nothing happens, the next warp still
    // goes. Into another level: the player gets it, the RC the new entry.
    boss.send_messages(&[
        warp_player(cash, "nowhere.nw", 1.0, 1.0).unwrap(),
        warp_player(cash, "house.nw", 20.0, 24.0).unwrap(),
    ])
    .unwrap();
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.current_level_name().as_deref() == Some("house.nw")
            && s.player_x == Some(20.0)
            && s.player_y == Some(24.0)
    })
    .unwrap();
    until(
        &mut boss,
        |e| matches!(e, Event::UpdatePlayer(p) if p.id == cash && p.level == "house.nw"),
    );
    let _ = std::fs::remove_dir_all(&w);
}
