//! Shared helpers for the loopback login tests (`login_matrix`,
//! `secure_login`): a temporary world, a server on an ephemeral
//! `127.0.0.1` port, and one build's full run with Seihai's own client.

#![allow(dead_code)]

use seihai_core::net::{
    login_gserver, login_gserver_secure, random_key, updatefile_message, wantfile_message,
    Connection, SecureLogin,
};
use seihai_core::protocol::{login::classic_login_props_message, ClientVersion, SessionState};
use seihai_gserver::server::{serve, Server};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const PASSWORD: &str = "labpw";

pub fn world(tag: &str, allowed: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("seihai-gserver-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(
        dir.join("config/server.txt"),
        "create_accounts = true\nheartbeat = 1\n",
    )
    .unwrap();
    std::fs::write(dir.join("config/allowedversions.txt"), allowed).unwrap();
    dir
}

/// Start a server on 127.0.0.1 and an ephemeral port in its own thread.
pub fn start(world: &std::path::Path) -> (String, Arc<Server>) {
    let (mut server, warnings) = seihai_gserver::load_world(world).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    seihai_gserver::load_secure_identity(&mut server).unwrap();
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
pub fn read_until(
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
pub fn run_one(
    addr: &str,
    server: &Server,
    v: ClientVersion,
    account: &str,
    stay: Duration,
    secure: Option<&SecureLogin>,
) -> Result<(), String> {
    let mut sess = login_gserver_secure(
        addr,
        v,
        random_key(),
        account,
        PASSWORD,
        true,
        true,
        wantfile_message,
        secure,
    )
    .map_err(|e| e.to_string())?;
    let rx = sess.conn.spawn_reader().map_err(|e| e.to_string())?;
    let mut st = SessionState::for_version(v);
    st.file_has_modtime = v.file_has_modtime();
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
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

/// Log in and return the disconnect message the server answers with.
pub fn refused(addr: &str, v: ClientVersion, account: &str, password: &str) -> Option<String> {
    let mut sess = login_gserver(addr, v, random_key(), account, password, false).ok()?;
    let rx = sess.conn.spawn_reader().ok()?;
    let mut st = SessionState::for_version(v);
    let _ = read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.disconnect_message.is_some()
    });
    st.disconnect_message
}

/// Run every profile `run_one`, at most `SEIHAI_TEST_PARALLEL` (default 6)
/// at once. Each new account hashes its password with PBKDF2 in a debug
/// build; fifty of those at once on a two-core CI runner (Windows, macOS)
/// starve the logins past their 10 s limit. Returns the labels that
/// failed, after printing each result.
pub fn run_all(
    addr: &str,
    server: &Arc<Server>,
    prefix: &str,
    stay: Duration,
    secure_key: Option<[u8; 32]>,
) -> Vec<&'static str> {
    let parallel = std::env::var("SEIHAI_TEST_PARALLEL")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(6);
    let work: Vec<(usize, ClientVersion)> = ClientVersion::all_versions()
        .iter()
        .copied()
        .enumerate()
        .collect();
    let queue = Arc::new(std::sync::Mutex::new(work.into_iter()));
    let (tx, results) = std::sync::mpsc::channel();
    let workers: Vec<_> = (0..parallel)
        .map(|_| {
            let (queue, tx) = (queue.clone(), tx.clone());
            let (addr, server, prefix) = (addr.to_string(), server.clone(), prefix.to_string());
            std::thread::spawn(move || loop {
                let Some((i, v)) = queue.lock().unwrap().next() else {
                    return;
                };
                let secure = secure_key.map(SecureLogin::pinned);
                let r = run_one(
                    &addr,
                    &server,
                    v,
                    &format!("{prefix}{i}"),
                    stay,
                    secure.as_ref(),
                );
                let _ = tx.send((v, r));
            })
        })
        .collect();
    drop(tx);
    let mut failed = Vec::new();
    for (v, r) in results {
        match r {
            Ok(()) => eprintln!("ok   {:<14} {} {:?}", v.label(), v.token(), v.encryption()),
            Err(e) => {
                eprintln!("FAIL {:<14} {e}", v.label());
                failed.push(v.label());
            }
        }
    }
    for w in workers {
        w.join().unwrap();
    }
    failed
}
