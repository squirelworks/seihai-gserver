//! The NPC-server link end to end on loopback: this gserver, the real
//! `seihai-npcserver` session code, and real client logins through
//! `seihai_core::net`. Only `127.0.0.1` is used.

use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{login::classic_login_props_message, ClientVersion, SessionState};
use seihai_gserver::npclink::serve_link;
use seihai_gserver::server::{serve, Server};
use seihai_npcserver::session::{self, End, Timing};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PASSWORD: &str = "labpw";
const TOKEN: &str = "lab-token-0123456789";

fn world(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("seihai-gserver-npc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("levels/gmaps")).unwrap();
    std::fs::write(dir.join("levels/onlinestartlocal.nw"), "GLEVNW01\n").unwrap();
    std::fs::write(dir.join("levels/gmaps/house1.nw"), "GLEVNW01\n").unwrap();
    std::fs::write(
        dir.join("config/server.txt"),
        format!("name = Link Lab\ncreate_accounts = true\nheartbeat = 1\nnpcserver = external\nnpcserver_token = {TOKEN}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("config/allowedversions.txt"), "*\n").unwrap();
    dir
}

/// Start the gserver and its link on ephemeral loopback ports.
fn start(world: &std::path::Path) -> (String, String, Arc<Server>) {
    let (server, warnings) = seihai_gserver::load_world(world).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let server = Arc::new(server);
    let (tx, rx) = std::sync::mpsc::channel();
    let s = server.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let game = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let link = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send((game.local_addr().unwrap(), link.local_addr().unwrap()))
                .unwrap();
            tokio::spawn(serve_link(s.npc.clone(), link));
            let _ = serve(s, game).await;
        });
    });
    let (g, l) = rx.recv().unwrap();
    (g.to_string(), l.to_string(), server)
}

fn quick() -> Timing {
    Timing {
        tick: Duration::from_millis(20),
        ..Timing::default()
    }
}

/// Run an NPC-server session in a thread; its log lines land in the vec.
fn npcserver(link: &str, token: &str) -> (Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<End>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let (l, link, token) = (lines.clone(), link.to_string(), token.to_string());
    let h = std::thread::spawn(move || {
        let (end, _) = session::run(&link, token.as_bytes(), quick(), &mut |s| {
            l.lock().unwrap().push(s)
        });
        end
    });
    (lines, h)
}

fn wait_for(lines: &Mutex<Vec<String>>, want: &str) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if lines.lock().unwrap().iter().any(|l| l.contains(want)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no {want:?} in:\n{}", lines.lock().unwrap().join("\n"));
}

/// Log in and wait for the level, so the server has the player.
fn client(
    addr: &str,
    v: ClientVersion,
    account: &str,
) -> (
    seihai_core::net::GserverSession,
    SessionState,
    std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
) {
    let mut sess = login_gserver(addr, v, random_key(), account, PASSWORD, true).unwrap();
    let rx = sess.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    st.file_has_modtime = v.file_has_modtime();
    let end = Instant::now() + Duration::from_secs(10);
    while st.current_level_name().is_none() {
        let body = rx
            .recv_timeout(end.saturating_duration_since(Instant::now()))
            .unwrap()
            .unwrap();
        st.apply_body(&body);
    }
    (sess, st, rx)
}

#[test]
fn npcserver_sees_levels_and_players() {
    let w = world("feed");
    let (game, link, server) = start(&w);

    // One player is on before the NPC-server connects: it comes in the snapshot.
    let (_a, _, _) = client(&game, ClientVersion::V222, "linka");
    let (lines, _h) = npcserver(&link, TOKEN);
    wait_for(&lines, "authenticated: server \"Link Lab\"");
    wait_for(&lines, "level list: 2 levels");
    wait_for(&lines, "level gmaps/house1.nw");
    wait_for(&lines, "account \"linka\" in \"onlinestartlocal.nw\"");
    assert!(server.npc.connected());

    // A second player joins live, sends props, and leaves.
    let (mut b, st, _rx) = client(&game, ClientVersion::V205, "linkb");
    wait_for(&lines, "account \"linkb\"");
    let props = classic_login_props_message(
        ClientVersion::V205,
        &st.player_props,
        "linkb",
        st.own_prop_ids.as_ref(),
    );
    b.conn.send_messages(&[props]).unwrap();
    wait_for(&lines, "player 2 props: 0=%linkb");
    drop(b);
    wait_for(&lines, "left: \"linkb\"");

    // Only one NPC-server at a time.
    let (_, second) = npcserver(&link, TOKEN);
    assert_eq!(
        second.join().unwrap(),
        End::Rejected("already connected".into())
    );
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn wrong_token_is_rejected() {
    let w = world("token");
    let (_game, link, server) = start(&w);
    let (_, h) = npcserver(&link, "not-the-token-at-all");
    assert_eq!(h.join().unwrap(), End::Rejected("bad token".into()));
    assert!(!server.npc.connected());
    let _ = std::fs::remove_dir_all(&w);
}
