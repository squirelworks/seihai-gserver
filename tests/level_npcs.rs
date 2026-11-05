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

/// The messages in frame bodies, as (id, payload); a `PLO_RAWDATA` block
/// is skipped whole.
fn messages(frames: &[Vec<u8>]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    for b in frames {
        let mut i = 0;
        while i < b.len() {
            let id = b[i].wrapping_sub(32);
            let j = b[i..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(b.len(), |p| i + p);
            if id == 100 {
                let n = b[i + 1..i + 4]
                    .iter()
                    .fold(0usize, |a, c| a << 7 | usize::from(c - 32));
                i = j + 1 + n;
                continue;
            }
            out.push((id, b[i + 1..j].to_vec()));
            i = j + 1;
        }
    }
    out
}

/// On a gmap, a 2.22 takes a cell's NPCs under the gmap's name, with the
/// cell (props 41 and 42), and later pushes under the gmap's name too; the
/// level is made active again after. A 2.05 on the same level, which does
/// not play gmaps, gets them as a plain level's.
#[test]
fn a_gmap_client_takes_npcs_under_the_gmap() {
    let w = world("npcs-gmap", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(
        w.join("levels/world.gmap"),
        "GRMAP001\nWIDTH 2\nHEIGHT 1\nLEVELNAMES\n\"west.nw\",\"onlinestartlocal.nw\",\nLEVELNAMESEND\n",
    )
    .unwrap();
    std::fs::write(w.join("levels/west.nw"), "GLEVNW01\n").unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), START).unwrap();
    let (addr, _server) = start(&w);

    let (mut a, _ra, sa) = join(&addr, ClientVersion::V205, "ann");
    let v = ClientVersion::V222;
    let mut g = login_gserver(&addr, v, random_key(), "gia", PASSWORD, false).unwrap();
    let rg = g.conn.spawn_reader().unwrap();
    let mut frames = Vec::new();
    let until = std::time::Instant::now() + Duration::from_secs(10);
    let npc_count = |f: &[Vec<u8>]| messages(f).iter().filter(|(id, _)| *id == 3).count();
    // Whatever follows the awaited messages within a moment.
    let settle = |frames: &mut Vec<Vec<u8>>| {
        while let Ok(Ok(b)) = rg.recv_timeout(Duration::from_millis(300)) {
            frames.push(b);
        }
    };
    while npc_count(&frames) < 2 && std::time::Instant::now() < until {
        if let Ok(Ok(b)) = rg.recv_timeout(Duration::from_millis(100)) {
            frames.push(b);
        }
    }
    settle(&mut frames);
    let (block, _) = npc(&sa, "block.png");
    let mut id3 = Vec::new();
    seihai_gserver::wire::gint3(block, &mut id3);
    let check = |m: &[(u8, Vec<u8>)]| {
        let at = m
            .iter()
            .position(|(id, p)| *id == 3 && p.starts_with(&id3))
            .expect("the block reaches the 2.22");
        let before = m[..at]
            .iter()
            .rev()
            .find(|(id, _)| *id == 156)
            .expect("an active level");
        assert_eq!(before.1, b"world.gmap");
        let after = m[at..]
            .iter()
            .find(|(id, _)| *id == 156)
            .expect("the level again");
        assert_eq!(after.1, b"onlinestartlocal.nw");
        m[at].1.clone()
    };
    let first = check(&messages(&frames));
    // Prop 41 is 1 (the east cell) and 42 is 0.
    assert!(
        first
            .windows(4)
            .any(|w| w == [41 + 32, 1 + 32, 42 + 32, 32]),
        "{first:?}"
    );
    assert_eq!(
        sa.server_npcs[&block].0.as_deref(),
        Some("onlinestartlocal.nw")
    );

    a.conn
        .send_messages(&[npc_props_message(
            block,
            &[(NPCPROP_X, PropValue::Byte(62))],
        )])
        .unwrap();
    let start_at = frames.len();
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while npc_count(&frames[start_at..]) < 1 && std::time::Instant::now() < until {
        if let Ok(Ok(b)) = rg.recv_timeout(Duration::from_millis(100)) {
            frames.push(b);
        }
    }
    settle(&mut frames);
    let push = check(&messages(&frames[start_at..]));
    assert_eq!(&push[3..], &[NPCPROP_X + 32, 62 + 32]);
    let _ = std::fs::remove_dir_all(&w);
}
