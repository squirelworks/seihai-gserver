//! Stage 2.9: the anti-cheat against a cheating client built on Seihai's
//! own client code, on 127.0.0.1. A teleport is put back and the others
//! never see it, a step too fast for the time is logged, a warp with no
//! link under the player gets no answer, and a flood of messages is
//! kicked where the admin says so. Every violation is a line in the log.

mod common;

use common::*;
use seihai_core::net::{levelwarp_message, login_gserver, random_key, walk_message_for};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::Duration;

const START: &str = "GLEVNW01\nLINK house.nw 30 0 2 1 30 60\n";
const HOUSE: &str = "GLEVNW01\n";

#[test]
fn cheats_are_caught() {
    let w = world("anticheat", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), START).unwrap();
    std::fs::write(w.join("levels/house.nw"), HOUSE).unwrap();
    let cfg = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str(
        "anticheat = on\nanticheat.teleport = correct\nanticheat.warp = correct\n\
         anticheat_rate = 50\nanticheat.rate = correct, kick after 1 in 10\n",
    );
    std::fs::write(&cfg, text).unwrap();
    let (addr, server) = start(&w);
    let log = server.anticheat.log_path().to_path_buf();

    // Amy (2.05) watches; Dee (2.22) cheats.
    let v205 = ClientVersion::V205;
    let mut a = login_gserver(&addr, v205, random_key(), "amy", PASSWORD, false).unwrap();
    let ra = a.conn.spawn_reader().unwrap();
    let mut sa = SessionState::for_version(v205);
    let v = ClientVersion::V222;
    let mut d = login_gserver(&addr, v, random_key(), "dee", PASSWORD, false).unwrap();
    let rd = d.conn.spawn_reader().unwrap();
    let mut sd = SessionState::for_version(v);
    read_until(&rd, &mut sd, Duration::from_secs(10), |s| {
        s.board.is_some() && !s.others.is_empty()
    })
    .unwrap();
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.iter().any(|o| o.nick.as_deref() == Some("dee"))
    })
    .unwrap();
    let dee = sa.others[0].id;
    sd.player_x = None;

    // A teleport across the level: Dee is put back where she was, and Amy
    // sees only the honest step after it.
    d.conn
        .send_messages(&[
            walk_message_for(v, 2.0, 2.0, 2),
            walk_message_for(v, 31.0, 30.0, 2),
        ])
        .unwrap();
    read_until(&rd, &mut sd, Duration::from_secs(10), |s| {
        s.player_x == Some(30.0)
    })
    .unwrap();
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.iter().any(|o| o.id == dee && o.x == 31.0)
    })
    .unwrap();

    // Long steps at once: the third is too fast, and logged (the default
    // for speed), so Amy still sees it.
    d.conn
        .send_messages(&[
            walk_message_for(v, 45.0, 30.0, 2),
            walk_message_for(v, 59.0, 30.0, 2),
            walk_message_for(v, 45.0, 30.0, 2),
        ])
        .unwrap();
    let _ = read_until(&ra, &mut sa, Duration::from_millis(500), |_| false);
    assert!(sa.others.iter().any(|o| o.id == dee && o.x == 45.0));

    // A warp into the house from the middle of the level: no answer.
    d.conn
        .send_messages(&[levelwarp_message(30.0, 60.0, "house.nw")])
        .unwrap();
    let _ = read_until(&rd, &mut sd, Duration::from_millis(500), |_| false);
    assert_eq!(
        sd.current_level_name().as_deref(),
        Some("onlinestartlocal.nw")
    );

    // A flood (standing still, so only the rate is broken): kicked.
    let flood: Vec<Vec<u8>> = (0..80)
        .map(|_| walk_message_for(v, 45.0, 30.0, 2))
        .collect();
    d.conn.send_messages(&flood).unwrap();
    let e = read_until(&rd, &mut sd, Duration::from_secs(10), |_| false).unwrap_err();
    assert!(e.contains("Disconnected by the server"), "{e}");
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.is_empty()
    })
    .unwrap();
    assert!(sa.ignored.is_empty(), "{:?}", sa.ignored);

    let text = std::fs::read_to_string(&log).unwrap();
    let kinds: Vec<&str> = text
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f[1], "dee", "{l}");
            f[4]
        })
        .collect();
    assert_eq!(kinds, ["teleport", "speed", "warp", "rate"], "{text}");
    assert!(text.lines().last().unwrap().ends_with("; kicked"), "{text}");
    let _ = std::fs::remove_dir_all(&w);
}

/// `ban after N in S` bans the account through the world's ban file, so
/// the next login is refused, and `staff exempt` leaves staff alone.
#[test]
fn a_policy_ban_holds_and_staff_are_exempt() {
    use seihai_gserver::accounts::{Account, Store};
    let w = world("anticheat-ban", "*\n");
    let mut boss = Account::new("boss", "onlinestartlocal.nw", 30.0, 30.0);
    boss.set_password(PASSWORD.as_bytes());
    boss.staff = true;
    Store::new(w.join("accounts")).save(&boss).unwrap();
    let cfg = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str(
        "anticheat = on\nanticheat.teleport = correct, ban 2d after 1 in 60, staff exempt\n",
    );
    std::fs::write(&cfg, text).unwrap();
    let (addr, server) = start(&w);
    let log = server.anticheat.log_path().to_path_buf();
    let v = ClientVersion::V222;

    // The staff account jumps across the level and stays where it went.
    let mut b = login_gserver(&addr, v, random_key(), "boss", PASSWORD, false).unwrap();
    let rb = b.conn.spawn_reader().unwrap();
    let mut sb = SessionState::for_version(v);
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| s.board.is_some()).unwrap();
    sb.player_x = None;
    b.conn
        .send_messages(&[walk_message_for(v, 2.0, 2.0, 2)])
        .unwrap();
    let _ = read_until(&rb, &mut sb, Duration::from_millis(500), |_| false);
    assert_eq!(sb.player_x, None, "not put back");

    // Anyone else is banned for it on the first jump, and kicked.
    let mut d = login_gserver(&addr, v, random_key(), "dee", PASSWORD, false).unwrap();
    let rd = d.conn.spawn_reader().unwrap();
    let mut sd = SessionState::for_version(v);
    read_until(&rd, &mut sd, Duration::from_secs(10), |s| s.board.is_some()).unwrap();
    d.conn
        .send_messages(&[walk_message_for(v, 2.0, 2.0, 2)])
        .unwrap();
    let e = read_until(&rd, &mut sd, Duration::from_secs(10), |_| false).unwrap_err();
    assert!(e.contains("Disconnected by the server"), "{e}");
    std::thread::sleep(Duration::from_millis(300));
    let m = refused(&addr, v, "dee", PASSWORD).unwrap();
    assert!(m.contains("anti-cheat: teleport"), "{m}");
    // Two days, less the moment since the ban.
    assert!(m.contains(" 2 days") || m.contains(" 47 hours"), "{m}");

    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.contains("\tdee\t") && text.ends_with("; banned for 2 days\n"),
        "{text}"
    );
    drop(b);
    let _ = std::fs::remove_dir_all(&w);
}
