//! Stage 3.1: level NPCs, with Seihai's own client code on 127.0.0.1. The
//! NPCs of the level file reach every build that enters, with image,
//! script and position. A push a 2.05 reports (`PLI_NPCPROPS`) reaches
//! the others in the level and a later arrival, and a script a client
//! tries to set is not passed on.

mod common;

use common::*;
use seihai_core::net::{login_gserver, random_key, GserverSession};
use seihai_core::protocol::npcprops::{
    npc_props_message, PropValue, NPCPROP_BLOCKFLAGS, NPCPROP_MESSAGE, NPCPROP_X,
};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::sync::mpsc::Receiver;
use std::time::Duration;

const START: &str = "GLEVNW01\n\
NPC block.png 30 31\nif (playerenters) {\n  canbepushed;\n}\nNPCEND\n\
NPC sign.png 10 12.5\n// a sign\nNPCEND\n";

type Session = (
    GserverSession,
    Receiver<std::io::Result<Vec<u8>>>,
    SessionState,
);

fn join(addr: &str, v: ClientVersion, account: &str) -> Session {
    let mut sess = login_gserver(addr, v, random_key(), account, PASSWORD, false)
        .unwrap_or_else(|e| panic!("{v:?}: {e}"));
    let rx = sess.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.board.is_some() && s.server_npcs.len() == 2
    })
    .unwrap_or_else(|e| panic!("{v:?}: {e}"));
    (sess, rx, st)
}

/// The NPC with `image`: its id and x.
fn npc(st: &SessionState, image: &str) -> (u32, Option<f32>) {
    st.server_npcs
        .iter()
        .find(|(_, (_, p))| p.image.as_deref() == Some(image))
        .map(|(id, (_, p))| (*id, p.x))
        .unwrap_or_else(|| panic!("no {image}: {:?}", st.server_npcs))
}

#[test]
fn level_npcs_are_shared() {
    let w = world("npcs", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), START).unwrap();
    let (addr, _server) = start(&w);

    let (mut a, ra, mut sa) = join(&addr, ClientVersion::V205, "ann");
    let (_b, rb, mut sb) = join(&addr, ClientVersion::V222, "ben");
    for st in [&sa, &sb] {
        let (_, p) = st
            .server_npcs
            .values()
            .find(|(_, p)| p.image.as_deref() == Some("block.png"))
            .unwrap();
        assert_eq!(
            p.script.as_deref(),
            Some("if (playerenters) {\ncanbepushed;\n}")
        );
        assert_eq!((p.x, p.y), (Some(30.0), Some(31.0)));
        assert_eq!(npc(st, "sign.png").1, Some(10.0));
    }
    let (block, _) = npc(&sa, "block.png");
    assert_eq!(npc(&sb, "block.png").0, block);

    // Ann's 2.05 runs the script and pushes the block one tile right.
    a.conn
        .send_messages(&[npc_props_message(
            block,
            &[
                (NPCPROP_BLOCKFLAGS, PropValue::Byte(8)),
                (NPCPROP_X, PropValue::Byte(62)),
            ],
        )])
        .unwrap();
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| {
        npc(s, "block.png").1 == Some(31.0)
    })
    .unwrap();
    assert_eq!(sb.server_npcs[&block].1.block_flags, Some(8));

    // A script from a client is dropped; the message beside it is kept.
    let mut m = npc_props_message(block, &[(NPCPROP_MESSAGE, PropValue::Text("hi".into()))]);
    let mut script = vec![1 + 0x20, 0x20, 5 + 0x20];
    script.extend(b"hello");
    m.splice(m.len() - 1..m.len() - 1, script);
    a.conn.send_messages(&[m]).unwrap();
    read_until(&rb, &mut sb, Duration::from_secs(10), |s| {
        s.server_npcs[&block].1.message.as_deref() == Some("hi")
    })
    .unwrap();

    // Cal arrives later, on 1.39, and finds the block where Ann put it.
    let (_c, _rc, sc) = join(&addr, ClientVersion::V139, "cal");
    assert_eq!(npc(&sc, "block.png").1, Some(31.0));
    let (_, p) = &sc.server_npcs[&block];
    assert_eq!(
        p.script.as_deref(),
        Some("if (playerenters) {\ncanbepushed;\n}")
    );

    // The pusher is not told its own push.
    let _ = read_until(&ra, &mut sa, Duration::from_millis(300), |_| false);
    assert_eq!(npc(&sa, "block.png").1, Some(30.0));
    for st in [&sa, &sb, &sc] {
        assert!(st.ignored.is_empty(), "{:?}", st.ignored);
    }
    let _ = std::fs::remove_dir_all(&w);
}
