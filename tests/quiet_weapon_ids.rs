//! Ids 25 and 33 are accepted and not recorded as unhandled.
//!
//! Seihai's 1.33 and 1.39 profiles send them on loopback. No archived client
//! was run.

mod common;

use common::{read_until, start, world, PASSWORD};
use seihai_core::net::{login_gserver, wantfile_message};
use seihai_core::protocol::{ClientVersion, SessionState};
use seihai_gserver::wire::ids;
use std::time::{Duration, Instant};

fn line(id: u8) -> Vec<u8> {
    vec![id.wrapping_add(0x20), b'\n']
}

/// Log in, send `probe` (an id stage 1 does not handle) and `quiet` in one
/// frame with a file request, and wait until the probe is recorded. The
/// file request is the sync: the reply is written after that frame is
/// handled, so the quiet id has been seen too.
fn seen_probe(version: ClientVersion, account: &str, quiet: u8, probe: u8) {
    let w = world(account, "*\n");
    let (addr, server) = start(&w);
    let mut sess = login_gserver(&addr, version, 1, account, PASSWORD, false)
        .unwrap_or_else(|e| panic!("{account} login: {e}"));
    assert!(
        sess.conn.can_send(quiet),
        "{account} profile cannot send {quiet}"
    );
    assert!(
        sess.conn.can_send(probe),
        "{account} profile cannot send {probe}"
    );
    let rx = sess.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(version);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.board.is_some() && s.current_level_name().is_some()
    })
    .unwrap_or_else(|e| panic!("{account} burst: {e}"));

    let before = st.file_requests.len();
    sess.conn
        .send_messages(&[line(quiet), line(probe), wantfile_message("no-such.png")])
        .unwrap();
    read_until(&rx, &mut st, Duration::from_secs(5), |s| {
        s.file_requests.len() > before
    })
    .unwrap_or_else(|e| panic!("{account} file reply: {e}"));

    let end = Instant::now() + Duration::from_secs(2);
    loop {
        let unknown = server.unknown_ids();
        if unknown.contains(&probe) {
            assert!(
                !unknown.contains(&quiet),
                "{account}: {quiet} was logged as unhandled; unknown {unknown:?}"
            );
            return;
        }
        if Instant::now() >= end {
            panic!("{account}: probe {probe} was never recorded; unknown {unknown:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn upload_25_and_add_33_are_not_unhandled() {
    seen_probe(ClientVersion::V133, "w133", ids::PLI_WEAPONUPLOAD, 7);
    seen_probe(ClientVersion::V139, "w139", ids::PLI_WEAPONADD, 7);
}
