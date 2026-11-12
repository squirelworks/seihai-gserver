//! Server options, folder config and server flags from an RC (stage 4
//! slice 5, issue #52) on 127.0.0.1, with seihai-rc's own messages: each
//! is set, read back as sent, and the flags reach a player online and
//! last over a restart.

mod common;

use common::raw::RawClient;
use common::*;
use seihai_core::protocol::ClientVersion;
use seihai_gserver::accounts::{Account, Store};
use seihai_rc::rc::{Event, LoginParams, RcKind, Session};
use seihai_rc::server::{flags_get, flags_set, parse_flags, parse_text, plo, ServerText};
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(10);
const FLAGSET: u8 = 28;
const FLAGDEL: u8 = 31;

fn login(addr: &str) -> Session {
    Session::login(
        addr,
        &LoginParams {
            kind: RcKind::Gen5,
            token: RcKind::Gen5.default_token().into(),
            account: "boss".into(),
            password: PASSWORD.into(),
            key: 77,
            nickname: None,
            identity: None,
        },
        T,
    )
    .unwrap()
}

/// The next message with `id`; RC chat lines before it are collected.
fn reply<R>(s: &mut Session, id: u8, parse: impl Fn(&[u8]) -> Option<R>) -> (R, Vec<String>) {
    let deadline = Instant::now() + T;
    let mut chat = Vec::new();
    loop {
        match s.next_event(deadline).unwrap() {
            Some(Event::Other(got, p)) if got == id => return (parse(&p).unwrap(), chat),
            Some(Event::RcChat(line)) => chat.push(line),
            Some(_) => {}
            None => panic!("no id {id}; chat {chat:?}"),
        }
    }
}

fn text(s: &mut Session, which: ServerText) -> (Vec<String>, Vec<String>) {
    s.send_messages(&[which.get_message()]).unwrap();
    reply(s, which.ids().1, |p| Some(parse_text(p)))
}

fn flags(s: &mut Session) -> Vec<String> {
    s.send_messages(&[flags_get()]).unwrap();
    reply(s, plo::RC_SERVERFLAGSGET, parse_flags).0
}

#[test]
fn options_folders_and_flags() {
    let w = world("rcserver", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), "GLEVNW01\n").unwrap();
    let mut a = Account::new("boss", "onlinestartlocal.nw", 30.0, 30.0);
    a.set_password(PASSWORD.as_bytes());
    a.staff = true;
    Store::new(w.join("accounts")).save(&a).unwrap();
    let (addr, server) = start(&w);
    let mut s = login(&addr);

    // The options are config/server.txt, line for line.
    let (got, _) = text(&mut s, ServerText::Options);
    assert_eq!(got, ["create_accounts = true", "heartbeat = 1"]);
    let new = "create_accounts = true\nheartbeat = 1\n# Lab, with a comma\nname = Lab\nport = lots";
    s.send_messages(&[ServerText::Options.set_message(new)])
        .unwrap();
    let (got, chat) = text(&mut s, ServerText::Options);
    assert_eq!(got, new.lines().collect::<Vec<_>>());
    assert!(chat.iter().any(|c| c.contains("line 5")), "{chat:?}");
    assert!(chat.iter().any(|c| c.contains("restarts")), "{chat:?}");
    assert_eq!(
        std::fs::read_to_string(w.join("config/server.txt")).unwrap(),
        format!("{new}\n")
    );
    // Put back, so the restart below reads a good file.
    s.send_messages(&[ServerText::Options.set_message("create_accounts = true\nheartbeat = 1")])
        .unwrap();

    // The folder config: empty, then what was set.
    assert!(text(&mut s, ServerText::Folders).0.is_empty());
    let folders = "file levels/*.png\nlevel levels/*.nw";
    s.send_messages(&[ServerText::Folders.set_message(folders)])
        .unwrap();
    assert_eq!(
        text(&mut s, ServerText::Folders).0,
        ["file levels/*.png", "level levels/*.nw"]
    );

    // Flags: a player online sees each set and each one dropped.
    let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", |m| {
        m.iter().any(|(id, _)| *id == 6)
    });
    assert!(flags(&mut s).is_empty());
    let set = |l: &[&str]| flags_set(&l.iter().map(|f| f.to_string()).collect::<Vec<_>>()).unwrap();
    s.send_messages(&[set(&["server.a=1", "server.b"])])
        .unwrap();
    assert_eq!(flags(&mut s), ["server.a=1", "server.b"]);
    amy.until("both flags", |m| {
        [&b"server.a=1"[..], b"server.b"]
            .iter()
            .all(|f| m.iter().any(|(id, p)| *id == FLAGSET && p == f))
    });
    s.send_messages(&[set(&["server.a=2"])]).unwrap();
    assert_eq!(flags(&mut s), ["server.a=2"]);
    amy.until("a changed, b gone", |m| {
        m.iter().any(|(id, p)| *id == FLAGSET && p == b"server.a=2")
            && m.iter().any(|(id, p)| *id == FLAGDEL && p == b"server.b")
    });
    s.close();
    drop(amy);
    drop(server);

    // The flags last over a restart.
    assert_eq!(
        std::fs::read_to_string(w.join("config/serverflags.txt")).unwrap(),
        "server.a=2\n"
    );
    let (addr, _server) = start(&w);
    let mut s = login(&addr);
    assert_eq!(flags(&mut s), ["server.a=2"]);
    s.close();
    let _ = std::fs::remove_dir_all(&w);
}
