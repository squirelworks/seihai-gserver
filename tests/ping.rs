//! The ping (issue #44) on 127.0.0.1 with Seihai's own client code: a
//! 4.20r8 gets `PLO_PING` and is kept while it answers, dropped when it
//! stops; a 2.22 and a 5.00r7 are never sent one, a 6.03r7 is.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{read_gshort, split_messages, ClientVersion, SessionState};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const PLO_PING: u8 = 108;
const PLI_PONG: u8 = 162;

/// The values of the pings in the bodies that come within `limit`.
fn pings(
    rx: &Receiver<std::io::Result<Vec<u8>>>,
    st: &mut SessionState,
    limit: Duration,
) -> Vec<u16> {
    let end = Instant::now() + limit;
    let mut out = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Ok(body)) => {
                for m in split_messages(&body) {
                    if m.id == PLO_PING {
                        assert_eq!(m.payload[0], 0x20, "sub 0 only");
                        out.push(read_gshort(&m.payload[1..]).unwrap());
                    }
                }
                st.apply_body(&body);
            }
            _ => break,
        }
    }
    out
}

#[test]
fn a_420_is_pinged_and_dropped_when_it_stops_answering() {
    let w = world("ping", "*\n");
    let cfg = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("ping = 0.2\nping_misses = 3\n");
    std::fs::write(&cfg, text).unwrap();
    let (addr, _server) = start(&w);

    // A 2.22 has no 108 in its profile: none comes.
    let v = ClientVersion::V222;
    let mut old = login_gserver(&addr, v, random_key(), "old", PASSWORD, false).unwrap();
    let old_rx = old.conn.spawn_reader().unwrap();
    let mut old_st = SessionState::for_version(v);
    assert!(pings(&old_rx, &mut old_st, Duration::from_millis(900)).is_empty());

    // A 4.20r8 that answers each ping stays on.
    let v = ClientVersion::V420R8;
    let mut c = login_gserver(&addr, v, random_key(), "pinged", PASSWORD, false).unwrap();
    let rx = c.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    // Pings are every 0.2 s, but a busy CI machine can hold the timers
    // back, so this waits for four answers rather than counting what
    // comes in a fixed time.
    let mut answered = Vec::new();
    let end = Instant::now() + Duration::from_secs(10);
    while answered.len() < 4 && Instant::now() < end {
        for v in pings(&rx, &mut st, Duration::from_millis(100)) {
            let mut m = vec![PLI_PONG + 0x20, b'!'];
            seihai_gserver::wire::gshort(v, &mut m);
            m.push(b'\n');
            c.conn.send_messages(&[m]).unwrap();
            answered.push(v);
        }
    }
    assert!(answered.len() >= 4, "{answered:?}");
    assert!(
        answered.windows(2).all(|w| w[1] == w[0] + 1),
        "{answered:?}"
    );
    assert_eq!(st.disconnect_message, None);

    // Silent, it is dropped after three pings in a row go unanswered.
    let r = read_until(&rx, &mut st, Duration::from_secs(5), |s| {
        s.disconnect_message.is_some()
    });
    assert!(r.is_err());
    assert_eq!(
        st.disconnect_message.as_deref(),
        Some("Disconnected for not answering.")
    );
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn a_603r7_is_pinged_and_a_500r7_is_not() {
    let w = world("ping6", "*\n");
    let cfg = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("ping = 0.2\n");
    std::fs::write(&cfg, text).unwrap();
    let (addr, _server) = start(&w);

    // 5.00r7 to 5.31r4 neither handle 108 nor send 162.
    let v = ClientVersion::V500R7;
    let mut five = login_gserver(&addr, v, random_key(), "five", PASSWORD, false).unwrap();
    let five_rx = five.conn.spawn_reader().unwrap();
    let mut five_st = SessionState::for_version(v);
    assert!(pings(&five_rx, &mut five_st, Duration::from_millis(900)).is_empty());

    // 6.03r7 takes 108 before its main switch, as 4.20 does.
    let v = ClientVersion::V603R7;
    let mut six = login_gserver(&addr, v, random_key(), "six", PASSWORD, false).unwrap();
    let six_rx = six.conn.spawn_reader().unwrap();
    let mut six_st = SessionState::for_version(v);
    // The first ping is due 0.2 s in; a busy machine can make it late.
    let mut got = Vec::new();
    let end = Instant::now() + Duration::from_secs(10);
    while got.is_empty() && Instant::now() < end {
        got = pings(&six_rx, &mut six_st, Duration::from_millis(100));
    }
    assert!(!got.is_empty(), "no ping to 6.03r7");
    let _ = std::fs::remove_dir_all(&w);
}
