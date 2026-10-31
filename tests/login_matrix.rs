//! Stage 1 lab test: every build seihai-core has a profile for logs in to
//! this server over loopback with Seihai's own client code
//! (`seihai_core::net`), gets its level and board, and stays connected.
//!
//! `SEIHAI_STAY_SECS` sets how long each client stays (default 3; the stage
//! acceptance run uses 60). Only `127.0.0.1` is used.

use seihai_core::net::{
    login_gserver, random_key, updatefile_message, wantfile_message, Connection,
};
use seihai_core::protocol::{login::classic_login_props_message, ClientVersion, SessionState};
use seihai_gserver::server::{serve, Server};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

const PASSWORD: &str = "labpw";
/// How long a login gets to answer. Generous: parallel tests share the CPU.
const LOGIN_WAIT: Duration = Duration::from_secs(30);

fn world(tag: &str, allowed: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("seihai-gserver-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(
        dir.join("config/server.txt"),
        // Few PBKDF2 rounds: the matrix makes 46 accounts at once, and at
        // the default count that starves a small CI runner's CPU.
        "create_accounts = true\nheartbeat = 1\npassword_rounds = 1000\n",
    )
    .unwrap();
    std::fs::write(dir.join("config/allowedversions.txt"), allowed).unwrap();
    dir
}

/// Start a server on 127.0.0.1 and an ephemeral port in its own thread.
fn start(world: &std::path::Path) -> (String, Arc<Server>) {
    let (server, warnings) = seihai_gserver::load_world(world).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let server = Arc::new(server);
    let (tx, rx) = std::sync::mpsc::channel();
    let s = server.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(l.local_addr().unwrap()).unwrap();
            let _ = serve(s, l).await;
        });
    });
    (rx.recv().unwrap().to_string(), server)
}

/// Read frames into `st` until `done` holds or `limit` passes.
fn read_until(
    rx: &Receiver<std::io::Result<Vec<u8>>>,
    st: &mut SessionState,
    limit: Duration,
    done: impl Fn(&SessionState) -> bool,
) -> Result<(), String> {
    let end = Instant::now() + limit;
    while !done(st) {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("timed out".into());
        }
        match rx.recv_timeout(left) {
            Ok(Ok(body)) => st.apply_body(&body),
            Ok(Err(e)) => return Err(format!("connection: {e} ({:?})", st.disconnect_message)),
            Err(_) => return Err("timed out".into()),
        }
        if let Some(m) = &st.disconnect_message {
            return Err(format!("disconnected: {m}"));
        }
    }
    Ok(())
}

/// One build's run. Returns what went wrong, if anything.
fn run_one(
    addr: &str,
    server: &Server,
    v: ClientVersion,
    account: &str,
    stay: Duration,
) -> Result<(), String> {
    let mut sess =
        login_gserver(addr, v, random_key(), account, PASSWORD, true).map_err(|e| e.to_string())?;
    let rx = sess.conn.spawn_reader().map_err(|e| e.to_string())?;
    let mut st = SessionState::for_version(v);
    st.file_has_modtime = v.file_has_modtime();
    read_until(&rx, &mut st, LOGIN_WAIT, |s| {
        s.board.is_some() && s.current_level_name().is_some()
    })?;
    if st.current_level_name().as_deref() != Some("onlinestartlocal.nw") {
        return Err(format!("level {:?}", st.current_level_name()));
    }
    if !st.ignored.is_empty() {
        return Err(format!("the build dropped ids {:?}", st.ignored));
    }

    // What clients send once the level is in: their props, a file request
    // and a file check. Each file message gets its answer.
    let conn: &mut Connection = &mut sess.conn;
    let mut msgs = Vec::new();
    if conn.can_send(2) {
        msgs.push(classic_login_props_message(
            v,
            &st.player_props,
            account,
            st.own_prop_ids.as_ref(),
        ));
    }
    // A build that reads neither answer (1.21) gets none.
    let answers = v.profile().protocol.plo.contains(30);
    let mut want = 0;
    if conn.can_send(23) {
        msgs.push(wantfile_message("head0.png"));
        want += usize::from(answers);
    }
    if conn.can_send(34) {
        msgs.push(updatefile_message(1_700_000_000, "body.png"));
        want += usize::from(answers || v.profile().protocol.plo.contains(45));
    }
    conn.send_messages(&msgs)
        .map_err(|e| format!("send: {e}"))?;
    let before = st.file_requests.len();
    read_until(&rx, &mut st, Duration::from_secs(5), |s| {
        s.file_requests.len() >= before + want
    })?;

    // Stay connected. Builds that read the world time get the heartbeat.
    let start_time = st.world_time;
    let end = Instant::now() + stay;
    let mut beats = 0;
    while Instant::now() < end {
        match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(Ok(body)) => {
                st.apply_body(&body);
                beats += 1;
            }
            Ok(Err(e)) => {
                return Err(format!(
                    "dropped while idle: {e} ({:?})",
                    st.disconnect_message
                ))
            }
            Err(_) => break,
        }
    }
    let reads_time = v.profile().protocol.plo.contains(42);
    if reads_time && stay >= Duration::from_secs(2) && (beats == 0 || st.world_time.is_none()) {
        return Err(format!("no heartbeat in {stay:?} (time {start_time:?})"));
    }
    if !server
        .online_accounts()
        .contains(&account.to_ascii_lowercase())
    {
        return Err("not online on the server".into());
    }
    Ok(())
}

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
            std::thread::spawn(move || (v, run_one(&addr, &server, v, &format!("lab{i}"), stay)))
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

/// Log in and return the disconnect message the server answers with.
fn refused(addr: &str, v: ClientVersion, account: &str, password: &str) -> Option<String> {
    let mut sess = login_gserver(addr, v, random_key(), account, password, false).ok()?;
    let rx = sess.conn.spawn_reader().ok()?;
    let mut st = SessionState::for_version(v);
    let _ = read_until(&rx, &mut st, LOGIN_WAIT, |s| s.disconnect_message.is_some());
    st.disconnect_message
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
    read_until(&rx, &mut st, LOGIN_WAIT, |s| s.board.is_some()).unwrap();

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
