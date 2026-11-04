//! Stage 2.2: the login burst carries the start level from the world's
//! `levels/` folder (board, links, signs, chests), read with Seihai's own
//! client code (`seihai_core::net`) on 127.0.0.1, for one build of each
//! era. A world without the file still gets the stage 1 board.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::Duration;

const LEVEL: &str = "GLEVNW01\n\
LINK next.nw 0 10 1 4 62 playery\n\
LINK house.nw 30 0 2 1 30.5 60\n\
SIGN 20 21\nHello #A!\nSIGNEND\n\
CHEST 30 36 greenrupee -1\n\
CHEST 10 12 bow 0\n";

fn first_level(
    addr: &str,
    v: ClientVersion,
    account: &str,
    chests: usize,
) -> Result<SessionState, String> {
    let mut sess = login_gserver(addr, v, random_key(), account, PASSWORD, false)
        .map_err(|e| e.to_string())?;
    let rx = sess.conn.spawn_reader().map_err(|e| e.to_string())?;
    let mut st = SessionState::for_version(v);
    let plo = v.profile().protocol.plo;
    let want_chests = if plo.contains(4) { chests } else { 0 };
    // The chests are the last level message in every build's burst.
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.board.is_some() && s.chests.len() >= want_chests
    })?;
    // Let the rest of the burst in, so nothing is cut short.
    let _ = read_until(&rx, &mut st, Duration::from_millis(300), |_| false);
    Ok(st)
}

#[test]
fn the_start_level_comes_from_the_folder() {
    let w = world("levels", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    // The settings name onlinestartlocal.nw; the file's case differs.
    std::fs::write(w.join("levels/OnlineStartLocal.nw"), LEVEL).unwrap();
    let expect = seihai_core::assets::Level::parse(LEVEL).unwrap().tile_ids();
    let (addr, _server) = start(&w);
    for (i, v) in [
        ClientVersion::V139,
        ClientVersion::V205,
        ClientVersion::V222,
        ClientVersion::V231,
    ]
    .into_iter()
    .enumerate()
    {
        let st =
            first_level(&addr, v, &format!("lvl{i}"), 2).unwrap_or_else(|e| panic!("{v:?}: {e}"));
        assert_eq!(
            st.current_level_name().as_deref(),
            Some("OnlineStartLocal.nw"),
            "{v:?}"
        );
        assert_eq!(st.board.as_deref(), Some(&expect[..]), "{v:?}");
        let plo = v.profile().protocol.plo;
        if plo.contains(1) {
            assert_eq!(
                st.links,
                ["next.nw 0 10 1 4 62 playery", "house.nw 30 0 2 1 30.5 60"],
                "{v:?}"
            );
        }
        if plo.contains(5) {
            let (x, y, text) = seihai_core::assets::decode_sign_record(&st.signs[0]).unwrap();
            assert_eq!((x, y, text.as_str()), (20, 21, "Hello #A!"), "{v:?}");
        }
        if plo.contains(4) {
            assert_eq!(st.chests[0], [0x20, 0x3e, 0x44, 0x20, 0xff], "{v:?}");
        }
        assert!(st.ignored.is_empty(), "{v:?} drops {:?}", st.ignored);
        assert!(st.disconnect_message.is_none(), "{v:?}");
    }
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn a_world_without_the_file_gets_the_blank_board() {
    let w = world("nolevels", "*\n");
    let (addr, server) = start(&w);
    let st = first_level(&addr, ClientVersion::V222, "blank", 0).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        st.current_level_name().as_deref(),
        Some("onlinestartlocal.nw")
    );
    let tile = server.cfg.board_tile;
    assert!(st.board.as_ref().unwrap().iter().all(|&t| t == tile));
    assert!(st.links.is_empty() && st.signs.is_empty() && st.chests.is_empty());
    let _ = std::fs::remove_dir_all(&w);
}
