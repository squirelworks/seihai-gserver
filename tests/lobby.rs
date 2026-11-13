//! The Login lobby (stage 6), with Seihai's own client code on 127.0.0.1.
//! A 5.24r4 or 6.03r7 logs in on the lobby port, gets the login burst and
//! then `PLO_SERVERWARP` (178) naming the game port, logs in again there
//! as the client does after a warp, and plays. A 2.22 on the lobby port
//! gets no warp and plays where it is.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use seihai_gserver::server::{serve, serve_lobby, LobbyWarp, Server};
use std::sync::Arc;
use std::time::Duration;

/// The world on two ephemeral ports: (lobby, game).
fn start_with_lobby(world: &std::path::Path) -> (String, String, Arc<Server>) {
    let (mut server, warnings) = seihai_gserver::load_world(world).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    seihai_gserver::load_secure_identity(&mut server).unwrap();
    let server = Arc::new(server);
    let (tx, rx) = std::sync::mpsc::channel();
    let s = server.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let game = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let lobby = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (g, l) = (game.local_addr().unwrap(), lobby.local_addr().unwrap());
            let warp = LobbyWarp {
                name: "Lab Game".into(),
                host: "127.0.0.1".into(),
                port: g.port(),
            };
            tokio::spawn(serve_lobby(s.clone(), lobby, warp));
            tx.send((l.to_string(), g.to_string())).unwrap();
            let _ = serve(s, game).await;
        });
    });
    let (l, g) = rx.recv().unwrap();
    (l, g, server)
}

#[test]
fn lobby_builds_are_sent_on_to_the_game_port() {
    let w = world("lobby", "*\n");
    let (lobby, game, _server) = start_with_lobby(&w);
    for (v, account) in [
        (ClientVersion::V524R4, "ann"),
        (ClientVersion::V603R7, "ben"),
    ] {
        let mut sess = login_gserver(&lobby, v, random_key(), account, PASSWORD, false)
            .unwrap_or_else(|e| panic!("{v:?}: {e}"));
        let rx = sess.conn.spawn_reader().unwrap();
        let mut st = SessionState::for_version(v);
        read_until(&rx, &mut st, Duration::from_secs(10), |s| {
            s.server_warp.is_some()
        })
        .unwrap_or_else(|e| panic!("{v:?}: {e}"));
        let warp = st.server_warp.take().unwrap();
        assert_eq!(warp.name, "Lab Game", "{v:?}");
        let target = warp.address().unwrap();
        assert_eq!(target, game, "{v:?}");
        // The burst came first: the client had its level.
        assert!(st.board.is_some(), "{v:?}");
        let _ = sess.conn.shutdown();
        drop(rx);
        drop(sess);

        // The login on the game port is not turned away as online.
        let mut sess = login_gserver(&target, v, random_key(), account, PASSWORD, false)
            .unwrap_or_else(|e| panic!("{v:?}: {e}"));
        let rx = sess.conn.spawn_reader().unwrap();
        let mut st = SessionState::for_version(v);
        read_until(&rx, &mut st, Duration::from_secs(10), |s| s.board.is_some())
            .unwrap_or_else(|e| panic!("{v:?} on the game port: {e}"));
        let _ = read_until(&rx, &mut st, Duration::from_millis(300), |_| false);
        assert!(st.server_warp.is_none(), "{v:?}: no second warp");
        assert!(st.disconnect_message.is_none(), "{v:?}");
        let _ = &mut sess;
    }
}

#[test]
fn older_builds_play_on_the_lobby_port() {
    let w = world("lobby-old", "*\n");
    let (lobby, _game, _server) = start_with_lobby(&w);
    let v = ClientVersion::V222;
    let mut sess = login_gserver(&lobby, v, random_key(), "cat", PASSWORD, false).unwrap();
    let rx = sess.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| s.board.is_some()).unwrap();
    let _ = read_until(&rx, &mut st, Duration::from_millis(500), |_| false);
    assert!(st.server_warp.is_none());
    assert!(st.disconnect_message.is_none());
    let _ = &mut sess;
}

#[test]
fn the_warp_text_has_no_commas_in_its_fields() {
    let w = LobbyWarp {
        name: "Lab, Game".into(),
        host: "127.0.0.1".into(),
        port: 14802,
    };
    assert_eq!(w.payload(), b"0,Lab  Game,127.0.0.1,14802");
}
