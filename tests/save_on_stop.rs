//! A stop saves the players still online. With Seihai's own client code on
//! 127.0.0.1, a 2.22 opens a chest and stays online; the stop's save (what
//! Ctrl-C and SIGTERM run) writes the chest and the rupees to the account
//! file while the player is still connected.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::Duration;

#[test]
fn a_stop_saves_who_is_online() {
    let w = world("savestop", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(
        w.join("levels/onlinestartlocal.nw"),
        "GLEVNW01\nCHEST 10 12 redrupee -1\n",
    )
    .unwrap();
    let (addr, server) = start(&w);

    let v = ClientVersion::V222;
    let mut s = login_gserver(&addr, v, random_key(), "stan", PASSWORD, false).unwrap();
    let rx = s.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.board.is_some() && s.chests.len() == 1
    })
    .unwrap();
    s.conn.send_open_chest(10, 12).unwrap();
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.own_rupees == Some(30)
    })
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(rt.block_on(server.world.save_all()), 1);
    let text = std::fs::read_to_string(w.join("accounts/stan.txt")).unwrap();
    assert!(
        text.lines().any(|l| l == "CHEST onlinestartlocal.nw 10 12"),
        "{text}"
    );
    assert!(text.lines().any(|l| l == "RUPEES 30"), "{text}");
    // Still online: the save is not a logout.
    assert_eq!(server.world.online_accounts(), ["stan"]);
    drop((s, rx));
    let _ = std::fs::remove_dir_all(&w);
}

/// Exercise the executable's signal handler, not just World::save_all.
#[cfg(unix)]
#[test]
fn sigterm_saves_a_still_connected_player_before_exit() {
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    struct ProcessWorld {
        child: Option<Child>,
        dir: std::path::PathBuf,
    }
    impl Drop for ProcessWorld {
        fn drop(&mut self) {
            if let Some(child) = &mut self.child {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    let mut process = ProcessWorld {
        child: None,
        dir: world("sigterm", "*\n"),
    };
    let w = &process.dir;
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(
        w.join("levels/onlinestartlocal.nw"),
        "GLEVNW01\nCHEST 10 12 redrupee -1\n",
    )
    .unwrap();
    let log = w.join("server.stderr");
    process.child = Some(
        Command::new(env!("CARGO_BIN_EXE_seihai-gserver"))
            .arg("--world")
            .arg(w)
            .args(["--bind", "127.0.0.1", "--port", "0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap(),
    );
    let child = process.child.as_mut().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let addr = loop {
        let text = std::fs::read_to_string(&log).unwrap();
        if let Some(addr) = text.lines().find_map(|line| {
            line.split_once(" listening on ")
                .and_then(|(_, addr)| addr.parse::<std::net::SocketAddr>().ok())
        }) {
            assert!(addr.ip().is_loopback());
            break addr.to_string();
        }
        assert!(child.try_wait().unwrap().is_none(), "server exited: {text}");
        assert!(Instant::now() < deadline, "server did not listen: {text}");
        std::thread::sleep(Duration::from_millis(10));
    };

    let v = ClientVersion::V222;
    let mut s = login_gserver(&addr, v, random_key(), "stan", PASSWORD, false).unwrap();
    let rx = s.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.board.is_some() && s.chests.len() == 1
    })
    .unwrap();
    s.conn.send_open_chest(10, 12).unwrap();
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.own_rupees == Some(30)
    })
    .unwrap();

    // Prove this mutation still needs the stop save; do not disconnect.
    let account = w.join("accounts/stan.txt");
    let before = std::fs::read_to_string(&account).unwrap();
    assert!(!before.lines().any(|l| l == "RUPEES 30"));
    assert!(!before.lines().any(|l| l == "CHEST onlinestartlocal.nw 10 12"));
    assert!(child.try_wait().unwrap().is_none());
    assert!(Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                status.success(),
                "{status}: {}",
                std::fs::read_to_string(&log).unwrap()
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "server did not exit after SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    process.child = None; // try_wait has reaped the owned child.
    let text = std::fs::read_to_string(&account).unwrap();
    assert!(text.lines().any(|l| l == "RUPEES 30"), "{text}");
    assert!(
        text.lines().any(|l| l == "CHEST onlinestartlocal.nw 10 12"),
        "{text}"
    );
    drop((s, rx));
}
