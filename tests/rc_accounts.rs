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
