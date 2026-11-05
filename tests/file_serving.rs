//! Stage 2.7: files from the world folder, with Seihai's own client code
//! (`seihai_core::net`) on 127.0.0.1, for a build of each file layout:
//! 1.39 (no mod time, no large files), 2.05 (no mod time), 2.22 (mod time,
//! large files) and 2.31 (mod time, large files with their size).

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key, updatefile_message, wantfile_message};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::Duration;

#[test]
fn files_come_from_the_world_folder() {
    let w = world("files", "*\n");
    std::fs::create_dir_all(w.join("files/heads")).unwrap();
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("files/heads/Head0.png"), b"\x89PNG small").unwrap();
    let big: Vec<u8> = (0..150_000u32).map(|i| (i * 13 % 256) as u8).collect();
    std::fs::write(w.join("files/big.png"), &big).unwrap();
    let grid = "GRMAP001\nWIDTH 1\nHEIGHT 1\nLEVELNAMES\n\"onlinestartlocal.nw\"\nLEVELNAMESEND\n";
    std::fs::write(w.join("levels/world.gmap"), grid).unwrap();
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
        let mut sess = login_gserver(&addr, v, random_key(), &format!("f{i}"), PASSWORD, false)
            .unwrap_or_else(|e| panic!("{v:?}: {e}"));
        let rx = sess.conn.spawn_reader().unwrap();
        let mut st = SessionState::for_version(v);
        st.file_has_modtime = seihai_gserver::files::has_modtime(v);
        read_until(&rx, &mut st, Duration::from_secs(10), |s| s.board.is_some())
            .unwrap_or_else(|e| panic!("{v:?}: {e}"));
        let checks = sess.conn.can_send(34);
        let mut msgs = vec![
            wantfile_message("head0.png"),
            wantfile_message("missing.png"),
            wantfile_message("world.gmap"),
            wantfile_message("big.png"),
            wantfile_message("../config/server.txt"),
        ];
        if checks {
            // An old copy gets the file; a copy from the future is current.
            msgs.push(updatefile_message(1_000_000, "HEAD0.png"));
            msgs.push(updatefile_message(4_000_000_000, "big.png"));
        }
        sess.conn.send_messages(&msgs).unwrap();
        let mut want = vec![
            "FILE:head0.png",
            "FAIL:missing.png",
            "FILE:world.gmap",
            "FILE:big.png",
            "FAIL:../config/server.txt",
        ];
        if checks {
            // A large file is one name per chunk; the check comes last.
            want.push("FILE:HEAD0.png");
            want.push(if v.profile().protocol.plo.contains(45) {
                "UPTODATE:big.png"
            } else {
                "FAIL:big.png"
            });
        }
        read_until(&rx, &mut st, Duration::from_secs(10), |s| {
            want.iter().all(|w| s.file_requests.iter().any(|f| f == w))
        })
        .unwrap_or_else(|e| panic!("{v:?}: {e} ({:?})", st.file_requests));
        let got = |n: &str| {
            st.incoming_files
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, d)| d.clone())
        };
        assert_eq!(
            got("head0.png").as_deref(),
            Some(&b"\x89PNG small"[..]),
            "{v:?}"
        );
        assert_eq!(got("world.gmap").as_deref(), Some(grid.as_bytes()), "{v:?}");
        assert!(
            got("big.png").is_some_and(|d| d == big),
            "{v:?}: big.png differs"
        );
        assert!(st.ignored.is_empty(), "{v:?} drops {:?}", st.ignored);
        assert!(st.disconnect_message.is_none(), "{v:?}");
    }
    let _ = std::fs::remove_dir_all(&w);
}
