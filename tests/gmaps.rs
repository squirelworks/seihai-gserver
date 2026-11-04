//! Stage 2.5: a 2x1 gmap, with Seihai's own client code (`seihai_core`) on
//! 127.0.0.1. A 2.22 logs into its west cell and is told the gmap
//! (`PLO_PLAYERWARP2`), asks for the east neighbour (`PLI_ADJACENTLEVEL`),
//! walks across the seam (props 43 and X2) and back. A 2.05, which does not
//! play gmaps, gets the same level as a plain level and sees the 2.22 leave
//! and come back.

mod common;

use common::*;
use seihai_core::gmap::{
    adjacent_level_message, crossing_props_message, Crossing, Gmap, GmapRules,
};
use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{ClientVersion, SessionState};
use std::time::Duration;

const GRID: &str = "GRMAP001\nWIDTH 2\nHEIGHT 1\nLEVELNAMES\n\
\"onlinestartlocal.nw\",\"east.nw\",\nLEVELNAMESEND\n";
const WEST: &str = "GLEVNW01\nBOARD 0 0 2 0 ABAB\n";
const EAST: &str = "GLEVNW01\nBOARD 0 0 2 0 ACAC\nLINK house.nw 30 0 2 1 30 60\n";

#[test]
fn a_gmap_is_one_world() {
    let w = world("gmaps", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/world.gmap"), GRID).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), WEST).unwrap();
    std::fs::write(w.join("levels/east.nw"), EAST).unwrap();
    let tiles = |t: &str| seihai_core::assets::Level::parse(t).unwrap().tile_ids();
    let (west, east) = (tiles(WEST), tiles(EAST));
    let (addr, _server) = start(&w);

    // Amy (2.05) waits in the west level, a plain level for her.
    let v205 = ClientVersion::V205;
    let mut a = login_gserver(&addr, v205, random_key(), "amy", PASSWORD, false).unwrap();
    let ra = a.conn.spawn_reader().unwrap();
    let mut sa = SessionState::for_version(v205);
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| s.board.is_some()).unwrap();
    assert!(sa.gmap.is_none());
    assert_eq!(sa.board.as_deref(), Some(&west[..]));

    // Gia (2.22) logs into the same cell and is told the gmap.
    let v = ClientVersion::V222;
    let mut g = login_gserver(&addr, v, random_key(), "gia", PASSWORD, false).unwrap();
    let rg = g.conn.spawn_reader().unwrap();
    let mut sg = SessionState::for_version(v);
    read_until(&rg, &mut sg, Duration::from_secs(10), |s| {
        s.board.is_some() && s.others.iter().any(|o| o.account.as_deref() == Some("amy"))
    })
    .unwrap();
    let gm = sg.gmap.as_ref().expect("on the gmap");
    assert_eq!((gm.name.as_str(), gm.cell), ("world.gmap", (0, 0)));
    assert_eq!(
        sg.current_level_name().as_deref(),
        Some("onlinestartlocal.nw")
    );
    assert_eq!(sg.board.as_deref(), Some(&west[..]));
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.iter().any(|o| o.nick.as_deref() == Some("gia"))
    })
    .unwrap();

    // The client has the .gmap (file serving comes later): the grid is set
    // as the client would read it from its cache.
    sg.set_gmap_grid(Gmap::parse(GRID).unwrap());

    // The east neighbour, asked for with no copy.
    g.conn
        .send_messages(&[adjacent_level_message(0, "east.nw")])
        .unwrap();
    read_until(&rg, &mut sg, Duration::from_secs(10), |s| {
        s.board_of("east.nw").is_some()
    })
    .unwrap();
    assert_eq!(sg.board_of("east.nw").map(|b| &b[..]), Some(&east[..]));
    // The player's own level is still the west one.
    assert_eq!(sg.board.as_deref(), Some(&west[..]));
    assert!(sg.links.is_empty(), "the neighbour's link stays with it");

    // Across the seam, east.
    let rules = GmapRules::for_version(v);
    let across = |from, to, x| Crossing {
        from,
        to,
        local_x: x,
        local_y: 30.0,
    };
    sg.set_gmap_cell((1, 0));
    g.conn
        .send_messages(&[crossing_props_message(&rules, &across((0, 0), (1, 0), 0.0))])
        .unwrap();
    read_until(&rg, &mut sg, Duration::from_secs(10), |s| {
        s.level_name.as_deref() == Some("east.nw") && !s.links.is_empty()
    })
    .unwrap();
    assert_eq!(sg.board.as_deref(), Some(&east[..]));
    // The first entry sends the level in full, as the lab's server did,
    // though the 35 answer had sent it already; Seihai's client adds the
    // link a second time to the copy it parked.
    assert!(sg.links.iter().all(|l| l == "house.nw 30 0 2 1 30 60"));
    // Amy saw her go; Gia no longer sees Amy.
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.is_empty()
    })
    .unwrap();
    assert!(sg.others.is_empty());

    // And back west: the client holds the level, the answer names it.
    sg.set_gmap_cell((0, 0));
    g.conn
        .send_messages(&[crossing_props_message(
            &rules,
            &across((1, 0), (0, 0), 63.5),
        )])
        .unwrap();
    read_until(&rg, &mut sg, Duration::from_secs(10), |s| {
        s.level_name.as_deref() == Some("onlinestartlocal.nw")
            && s.others.iter().any(|o| o.account.as_deref() == Some("amy"))
    })
    .unwrap();
    assert_eq!(sg.board.as_deref(), Some(&west[..]));
    read_until(&ra, &mut sa, Duration::from_secs(10), |s| {
        s.others.iter().any(|o| o.nick.as_deref() == Some("gia"))
    })
    .unwrap();
    assert!(sg.ignored.is_empty(), "{:?}", sg.ignored);
    assert!(sa.ignored.is_empty(), "{:?}", sa.ignored);
    let _ = std::fs::remove_dir_all(&w);
}
