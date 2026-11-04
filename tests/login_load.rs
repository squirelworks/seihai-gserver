//! Login bursts (issue #17): password checks run at most `password_checks`
//! at once, and one address gets at most `login_rate` logins a minute.
//! Only `127.0.0.1` is used.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::Duration;

fn world_with(tag: &str, extra: &str) -> std::path::PathBuf {
    let w = world(tag, "*\n");
    let p = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&p).unwrap();
    text.push_str(extra);
    std::fs::write(&p, text).unwrap();
    w
}

#[test]
fn a_burst_of_new_accounts_queues_behind_two_checks() {
    let w = world_with("burst", "password_checks = 2\n");
    let (addr, server) = start(&w);
    let logins: Vec<_> = (0..10)
        .map(|i| {
            let addr = addr.clone();
            std::thread::spawn(move || -> Result<(), String> {
                let v = ClientVersion::V222;
                let mut sess = login_gserver(
                    &addr,
                    v,
                    random_key(),
                    &format!("burst{i}"),
                    PASSWORD,
                    false,
                )
                .map_err(|e| e.to_string())?;
                let rx = sess.conn.spawn_reader().map_err(|e| e.to_string())?;
                let mut st = SessionState::for_version(v);
                // Ten debug-build hashes, two at a time, on a slow runner.
                read_until(&rx, &mut st, Duration::from_secs(60), |s| s.board.is_some())
            })
        })
        .collect();
    for (i, t) in logins.into_iter().enumerate() {
        t.join()
            .unwrap()
            .unwrap_or_else(|e| panic!("burst{i}: {e}"));
    }
    assert_eq!(server.peak_password_checks(), 2);
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn one_address_is_held_to_its_login_rate() {
    let w = world_with("rate", "login_rate = 3\nlogin_rate_loopback = true\n");
    let (addr, _server) = start(&w);
    for _ in 0..3 {
        let m = refused(&addr, ClientVersion::V222, "nobody", "").unwrap();
        assert!(m.contains("password"), "{m}");
    }
    // Gen 2 and gen 5 both read the refusal.
    for v in [ClientVersion::V222, ClientVersion::V133] {
        let m = refused(&addr, v, "nobody", "").unwrap();
        assert!(m.contains("Too many logins"), "{v:?}: {m}");
    }
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn loopback_is_not_limited_by_default() {
    let w = world_with("rate-lo", "login_rate = 1\n");
    let (addr, _server) = start(&w);
    for _ in 0..4 {
        let m = refused(&addr, ClientVersion::V222, "nobody", "").unwrap();
        assert!(m.contains("password"), "{m}");
    }
    let _ = std::fs::remove_dir_all(&w);
}
