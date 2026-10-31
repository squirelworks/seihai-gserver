//! Stage 1 lab test: every build seihai-core has a profile for logs in to
//! this server over loopback with Seihai's own client code
//! (`seihai_core::net`), gets its level and board, and stays connected.
//!
//! `SEIHAI_STAY_SECS` sets how long each client stays (default 3; the stage
//! acceptance run uses 60). Only `127.0.0.1` is used.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::Duration;

#[test]
fn every_profile_logs_in_and_stays() {
    let stay = Duration::from_secs(
        std::env::var("SEIHAI_STAY_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3),
    );
    let w = world("matrix", "*\n");
    let (addr, server) = start(&w);
    let handles: Vec<_> = ClientVersion::all_versions()
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let (addr, server) = (addr.clone(), server.clone());
            std::thread::spawn(move || {
                (
                    v,
                    run_one(&addr, &server, v, &format!("lab{i}"), stay, None),
                )
            })
        })
        .collect();
    let mut failed = Vec::new();
    for h in handles {
        let (v, r) = h.join().unwrap();
        match r {
            Ok(()) => eprintln!("ok   {:<14} {} {:?}", v.label(), v.token(), v.encryption()),
            Err(e) => {
                eprintln!("FAIL {:<14} {e}", v.label());
                failed.push(v.label());
            }
        }
    }
    let _ = std::fs::remove_dir_all(&w);
    assert!(failed.is_empty(), "failed: {failed:?}");
}

#[test]
fn wrong_password_version_and_double_login_are_refused() {
    let w = world("refuse", "2.05\n2.22\n1.39\n");
    let (addr, _server) = start(&w);
    // First login makes the account (create_accounts is on).
    let mut first = login_gserver(
        &addr,
        ClientVersion::V205,
        random_key(),
        "dup",
        PASSWORD,
        false,
    )
    .unwrap();
    let rx = first.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(ClientVersion::V205);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| s.board.is_some()).unwrap();

    let m = refused(&addr, ClientVersion::V222, "dup", PASSWORD).unwrap();
    assert!(m.contains("already in use"), "{m}");
    let m = refused(&addr, ClientVersion::V222, "dup", "wrong").unwrap();
    assert!(m.contains("password"), "{m}");
    let m = refused(&addr, ClientVersion::V231, "other", PASSWORD).unwrap();
    assert!(m.contains("not allowed"), "{m}");
    // Gen 2 gets its refusal too.
    let m = refused(&addr, ClientVersion::V133, "other", PASSWORD).unwrap();
    assert!(m.contains("not allowed"), "{m}");
    // The first session is still fine.
    drop(first);
    let _ = std::fs::remove_dir_all(&w);
}
