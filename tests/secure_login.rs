//! The Seihai secure login (`SEI00001`) on loopback with Seihai's own
//! client: every build logs in as the inner token with the account and
//! password sealed, then plays as after its classic login. Its own test
//! binary, so its 50 logins do not share the CPU with `login_matrix`'s.

mod common;

use common::*;
use seihai_core::net::{login_gserver_secure, random_key, wantfile_message, SecureLogin};
use seihai_core::protocol::ClientVersion;
use std::time::Duration;

/// The Seihai secure login (`SEI00001`): every build, as its inner token,
/// logs in with the account and password sealed, and then plays exactly as
/// after its classic login.
#[test]
fn every_profile_logs_in_through_the_secure_login() {
    let w = world("secure", "*\n");
    let (addr, server) = start(&w);
    let key = server
        .identity
        .as_ref()
        .expect("key made at load")
        .public_key();
    let handles: Vec<_> = ClientVersion::all_versions()
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let (addr, server) = (addr.clone(), server.clone());
            std::thread::spawn(move || {
                let secure = SecureLogin::pinned(key);
                let r = run_one(
                    &addr,
                    &server,
                    v,
                    &format!("sec{i}"),
                    Duration::from_secs(1),
                    Some(&secure),
                );
                (v, r)
            })
        })
        .collect();
    let mut failed = Vec::new();
    for h in handles {
        let (v, r) = h.join().unwrap();
        if let Err(e) = r {
            eprintln!("FAIL {:<14} {e}", v.label());
            failed.push(v.label());
        }
    }
    // A wrong pinned key: the client stops before the password goes.
    let wrong = SecureLogin::pinned([7; 32]);
    let e = login_gserver_secure(
        &addr,
        ClientVersion::V231,
        random_key(),
        "sec",
        PASSWORD,
        false,
        true,
        wantfile_message,
        Some(&wrong),
    )
    .err()
    .expect("refused");
    assert!(e.to_string().contains("not the given key"), "{e}");
    let _ = std::fs::remove_dir_all(&w);
    assert!(failed.is_empty(), "failed: {failed:?}");
}

#[test]
fn require_secure_login_turns_classic_logins_away() {
    let w = world("reqsecure", "*\n");
    std::fs::write(
        w.join("config/server.txt"),
        "create_accounts = true\nrequire_secure_login = true\n",
    )
    .unwrap();
    let (addr, server) = start(&w);
    let m = refused(&addr, ClientVersion::V205, "plain", PASSWORD);
    assert!(
        m.as_deref().is_some_and(|m| m.contains("secure login")),
        "{m:?}"
    );
    let key = server.identity.as_ref().unwrap().public_key();
    run_one(
        &addr,
        &server,
        ClientVersion::V205,
        "sealed",
        Duration::ZERO,
        Some(&SecureLogin::pinned(key)),
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&w);
}
