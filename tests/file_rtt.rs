//! Lab measure, not a check: file answer round trips while a level full of
//! players walks. `cargo test --release --test file_rtt -- --ignored --nocapture`
//! (`SEIHAI_RTT_WALKERS`, default 60). Only `127.0.0.1` is used.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key, walk_message_for, wantfile_message};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
#[ignore]
fn file_round_trips_under_walking() {
    let walkers: usize = std::env::var("SEIHAI_RTT_WALKERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let w = world("rtt", "*\n");
    std::fs::create_dir_all(w.join("files")).unwrap();
    std::fs::write(w.join("files/probe.png"), vec![7u8; 2000]).unwrap();
    // `SEIHAI_RTT_MISS=1`: ask for names the world does not have, as the
    // suite stress rig's probes do (each miss may rescan the folders).
    let miss = std::env::var("SEIHAI_RTT_MISS").is_ok_and(|v| v == "1");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    for i in 0..500 {
        std::fs::write(w.join(format!("levels/l{i}.nw")), "GLEVNW01\n").unwrap();
    }
    let (addr, _server) = start(&w);
    let v = ClientVersion::V222;
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    for i in 0..walkers {
        let (addr, stop) = (addr.clone(), stop.clone());
        threads.push(std::thread::spawn(move || {
            let mut s =
                login_gserver(&addr, v, random_key(), &format!("w{i}"), PASSWORD, false).unwrap();
            let rx = s.conn.spawn_reader().unwrap();
            let mut st = SessionState::for_version(v);
            read_until(&rx, &mut st, Duration::from_secs(60), |s| s.board.is_some()).unwrap();
            let mut k = 0u32;
            while !stop.load(Ordering::Relaxed) {
                k += 1;
                let x = 20.0 + (k % 20) as f32 * 0.5;
                let _ = s.conn.send_messages(&[walk_message_for(v, x, 30.0, 2)]);
                while rx.try_recv().is_ok() {}
                std::thread::sleep(Duration::from_millis(100));
            }
        }));
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_secs(2));
    let mut p = login_gserver(&addr, v, random_key(), "probe", PASSWORD, false).unwrap();
    let rx = p.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(60), |s| s.board.is_some()).unwrap();
    let mut rtts = Vec::new();
    for n in 0..100 {
        let before = st.file_requests.len();
        let t = Instant::now();
        let name = if miss {
            format!("missing{n}.png")
        } else {
            "probe.png".to_string()
        };
        p.conn.send_messages(&[wantfile_message(&name)]).unwrap();
        read_until(&rx, &mut st, Duration::from_secs(10), |s| {
            s.file_requests.len() > before
        })
        .unwrap();
        rtts.push(t.elapsed());
        std::thread::sleep(Duration::from_millis(37));
    }
    stop.store(true, Ordering::Relaxed);
    rtts.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    eprintln!(
        "{walkers} walkers: file round trip p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms",
        ms(rtts[49]),
        ms(rtts[94]),
        ms(rtts[99])
    );
    for t in threads {
        let _ = t.join();
    }
    let _ = std::fs::remove_dir_all(&w);
}
