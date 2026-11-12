//! Accounts from an RC (stage 4 slice 4, issue #52) on 127.0.0.1, driven
//! with seihai-rc's own account messages and parsers.

mod common;

use common::*;
use seihai_gserver::accounts::{Account as File, Store};
use seihai_rc::accounts::{
    account_add, account_delete, account_get, account_list, account_set, parse_account,
    parse_account_list, Account,
};
use seihai_rc::rc::{Event, LoginParams, RcKind, Session};
use std::time::{Duration, Instant};

fn reply(s: &mut Session, id: u8) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while let Some(ev) = s.next_event(deadline).unwrap() {
        if let Event::Other(got, p) = ev {
            if got == id {
                return p;
            }
        }
    }
    panic!("no reply {id}");
}

fn list(s: &mut Session, pattern: &str, conditions: &str) -> Vec<String> {
    s.send_messages(&[account_list(pattern, conditions).unwrap()])
        .unwrap();
    parse_account_list(&reply(s, 70)).unwrap()
}

fn get(s: &mut Session, account: &str) -> Account {
    s.send_messages(&[account_get(account).unwrap()]).unwrap();
    parse_account(&reply(s, 73)).unwrap()
}

#[test]
fn an_rc_lists_reads_writes_adds_and_deletes_accounts() {
    let w = world("rcaccounts", "*\n");
    let store = Store::new(w.join("accounts"));
    let mut boss = File::new("boss", "onlinestartlocal.nw", 30.0, 30.0);
    boss.set_password(PASSWORD.as_bytes());
    boss.staff = true;
    store.save(&boss).unwrap();
    store
        .save(&File::new("bob", "onlinestartlocal.nw", 30.0, 30.0))
        .unwrap();
    let (addr, _server) = start(&w);

    let mut s = Session::login(
        &addr,
        &LoginParams {
            kind: RcKind::Gen5,
            token: RcKind::Gen5.default_token().into(),
            account: "boss".into(),
            password: PASSWORD.into(),
            key: 9,
            nickname: None,
        },
        Duration::from_secs(10),
    )
    .unwrap();

    assert_eq!(list(&mut s, "%", ""), ["bob", "boss"]);
    assert_eq!(list(&mut s, "b%b", ""), ["bob"]);

    // Set Bob's fields and a password; read them back.
    let mut bob = get(&mut s, "bob");
    assert_eq!(bob.account, "bob");
    assert!(!bob.banned);
    bob.email = "bob@example.org".into();
    bob.admin_level = 2;
    bob.admin_worlds = "main".into();
    bob.load_only = true;
    s.send_messages(&[account_set(&bob, "bobpw").unwrap()])
        .unwrap();
    assert_eq!(get(&mut s, "bob"), bob);
    let file = store.load("bob").unwrap().unwrap();
    assert!(file.check_password(b"bobpw"));
    assert!(!file.staff, "an admin level is not RC use");
    assert_eq!(list(&mut s, "%", "adminlevel>1"), ["bob"]);

    // Add one, then delete it.
    let cat = Account {
        account: "cat".into(),
        email: "cat@example.org".into(),
        ..Default::default()
    };
    s.send_messages(&[account_add(&cat, "catpw").unwrap()])
        .unwrap();
    assert_eq!(get(&mut s, "cat").email, "cat@example.org");
    assert!(store.load("cat").unwrap().unwrap().check_password(b"catpw"));
    s.send_messages(&[account_delete("cat").unwrap()]).unwrap();
    assert_eq!(list(&mut s, "%", ""), ["bob", "boss"]);
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn an_rc_sets_rights_and_comments_and_the_rights_open_folders() {
    use seihai_rc::players::{
        comments_get, comments_set, parse_comments, parse_rights, rights_get, rights_set, Rights,
    };
    let w = world("rcrights", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/a.nw"), "GLEVNW01\n").unwrap();
    let store = Store::new(w.join("accounts"));
    for name in ["boss", "helper"] {
        let mut a = File::new(name, "onlinestartlocal.nw", 30.0, 30.0);
        a.set_password(PASSWORD.as_bytes());
        a.staff = true;
        store.save(&a).unwrap();
    }
    let (addr, _server) = start(&w);
    let login = |account: &str| {
        Session::login(
            &addr,
            &LoginParams {
                kind: RcKind::Gen5,
                token: RcKind::Gen5.default_token().into(),
                account: account.into(),
                password: PASSWORD.into(),
                key: 3,
                nickname: None,
            },
            Duration::from_secs(10),
        )
        .unwrap()
    };
    let mut s = login("boss");

    // The helper has no folder rights yet: no folder opens.
    let mut h = login("helper");
    assert!(h.list_dir("levels", Duration::from_secs(10)).is_err());
    drop(h);

    let want = Rights {
        account: "helper".into(),
        rights: 1 | 1 << 9,
        ip_range: "127.0.0.1".into(),
        folders: vec!["rw levels/*".into()],
    };
    s.send_messages(&[rights_set(&want).unwrap()]).unwrap();
    s.send_messages(&[rights_get("helper").unwrap()]).unwrap();
    assert_eq!(parse_rights(&reply(&mut s, 62)).unwrap(), want);

    s.send_messages(&[comments_set("helper", "new staff\nmentor: boss").unwrap()])
        .unwrap();
    s.send_messages(&[comments_get("helper").unwrap()]).unwrap();
    let c = parse_comments(&reply(&mut s, 63)).unwrap();
    assert_eq!(c.lines, ["new staff", "mentor: boss"]);

    // Logged in again, the helper opens the folder its rights name.
    let mut h = login("helper");
    assert_eq!(
        h.list_dir("levels", Duration::from_secs(10)).unwrap(),
        ["a.nw"]
    );
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn an_rc_reads_and_sets_player_props() {
    use seihai_core::net::{login_gserver, random_key};
    use seihai_core::protocol::{ClientVersion, SessionState};
    use seihai_rc::players::{
        parse_prop_edit, parse_props, props_get, props_get_account, props_set,
    };
    let w = world("rcprops", "*\n");
    let store = Store::new(w.join("accounts"));
    let mut boss = File::new("boss", "onlinestartlocal.nw", 30.0, 30.0);
    boss.set_password(PASSWORD.as_bytes());
    boss.staff = true;
    store.save(&boss).unwrap();
    let mut test1 = File::new("test1", "onlinestartlocal.nw", 30.0, 30.0);
    test1.flags = vec!["labflag=1".into()];
    test1.rupees = 100;
    test1.set_password(PASSWORD.as_bytes());
    store.save(&test1).unwrap();
    let (addr, _server) = start(&w);
    let mut s = Session::login(
        &addr,
        &LoginParams {
            kind: RcKind::Gen5,
            token: RcKind::Gen5.default_token().into(),
            account: "boss".into(),
            password: PASSWORD.into(),
            key: 5,
            nickname: None,
        },
        Duration::from_secs(10),
    )
    .unwrap();

    // Offline, by account: read, change, read back.
    s.send_messages(&[props_get_account("test1").unwrap()])
        .unwrap();
    let got = parse_props(&reply(&mut s, 72)).unwrap();
    assert_eq!((got.id, got.account.as_str()), (0, "test1"));
    assert!(got.props_complete);
    assert_eq!(got.flags, ["labflag=1"]);
    let edits: Vec<_> = ["gralats=5", "bombs=7", "hearts=2.5"]
        .iter()
        .map(|e| parse_prop_edit(e).unwrap())
        .collect();
    s.send_messages(&[props_set(&got, &edits).unwrap()])
        .unwrap();
    s.send_messages(&[props_get_account("test1").unwrap()])
        .unwrap();
    let again = parse_props(&reply(&mut s, 72)).unwrap();
    let file = store.load("test1").unwrap().unwrap();
    assert_eq!((file.rupees, file.bombs, file.hp), (5, 7, 2.5));
    assert_eq!(
        file.flags,
        ["labflag=1"],
        "the flags came back with the set"
    );
    assert_eq!(file.level, "onlinestartlocal.nw");
    assert_eq!(again.props.len(), got.props.len());

    // Online, by player id: read; a set is refused and the file kept.
    let v = ClientVersion::V222;
    let mut c = login_gserver(&addr, v, random_key(), "test1", PASSWORD, false).unwrap();
    let rx = c.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| s.board.is_some()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let id = loop {
        if let Some(e) = s.players.players.values().find(|e| e.account == "test1") {
            break e.id;
        }
        assert!(std::time::Instant::now() < deadline, "test1 not listed");
        s.next_event(std::time::Instant::now() + Duration::from_millis(200))
            .unwrap();
    };
    s.send_messages(&[props_get(id).unwrap()]).unwrap();
    let online = parse_props(&reply(&mut s, 72)).unwrap();
    assert_eq!((online.id, online.account.as_str()), (id, "test1"));
    let edits = [parse_prop_edit("gralats=999").unwrap()];
    s.send_messages(&[props_set(&online, &edits).unwrap()])
        .unwrap();
    s.send_messages(&[props_get_account("test1").unwrap()])
        .unwrap();
    reply(&mut s, 72);
    assert_eq!(store.load("test1").unwrap().unwrap().rupees, 5);
    let _ = std::fs::remove_dir_all(&w);
}
