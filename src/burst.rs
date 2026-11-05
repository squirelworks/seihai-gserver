//! The login burst: what the server sends once a login is accepted.
//!
//! Two orders, from the lab findings in the handoff notes and in Seihai's
//! `documentation/login-burst.md`:
//!
//! * **Gen 2 (1.21 to 1.39).** Three frames, in the order the real 1.31
//!   needed (it stalls when the level name comes before the board; 1.32 to
//!   1.39 take either): own props, the warp and the raw-data header; the
//!   board; then the level name, the level mod time and the leader flag.
//! * **Gen 3 and gen 5 (1.41 on).** One frame: signature, start message,
//!   status list, own props, the default weapons (8 then 7), the warp, the
//!   level name (before the board: 2.05 stays on its loading screen
//!   otherwise), the board, the level mod time, `SETACTIVELEVEL` (156, which
//!   2.05 waits for), the leader flag, the world time and, last,
//!   `LISTPROCESSES` (182), which the real 2.17 to 2.31 answer with their
//!   post-login frame.
//!
//! Each message goes out only if the client build handles its id (the
//! profile's `plo` set), and each own prop only if its prop parser takes
//! that id (`props.own`), since the first unknown prop ends the stream.
//!
//! The level comes from the world (`levels.rs`): its board, then its links
//! and signs, the mod time, then its chests (the lab and public 2.22
//! captures have the chests after `LEVELMODTIME`, and `SETACTIVELEVEL`
//! after the chests). A world without the level's file gets the stage 1
//! board, all one tile.

use crate::accounts::Account;
use crate::config::Config;
use crate::gmaps::Place;
use crate::levels::{chest_record, link_line, sign_record, Level};
use crate::wire::{self, gchar, gint3, gint4, gint5, gshort, gstring, ids::*, latin1, msg};
use seihai_core::protocol::{encode_pos_precise, ClientVersion, EncryptionGen, IdSet};

/// The build a client's burst is shaped for: the one its token names, or
/// for a token seihai-core does not know, a stand-in for its generation.
pub fn shape_version(version: Option<ClientVersion>, gen: EncryptionGen) -> ClientVersion {
    version.unwrap_or(match gen {
        EncryptionGen::Gen1 | EncryptionGen::Gen2 => ClientVersion::V139,
        EncryptionGen::Gen3 => ClientVersion::V217,
        EncryptionGen::Gen5 => ClientVersion::V231,
    })
}

/// Everything the burst needs to know about the player.
pub struct Player<'a> {
    pub id: u16,
    pub account: &'a Account,
}

/// The burst as frame bodies, in order.
pub fn login_burst(
    cfg: &Config,
    version: ClientVersion,
    gen: EncryptionGen,
    p: &Player,
    level: &Level,
    gmap: Option<&Place>,
    world_time: u32,
) -> Vec<Vec<u8>> {
    let plo = &version.profile().protocol.plo;
    let has = |id: u8| plo.contains(id);
    let mut out = Out {
        has: &has,
        body: Vec::new(),
    };
    let a = p.account;
    let lvl = level;
    let level = lvl.name.as_str();
    let props = own_props(version, p);
    let mut warp = vec![gchar((a.x * 2.0) as u8), gchar((a.y * 2.0) as u8)];
    warp.extend(latin1(level));
    let board: Vec<u8> = lvl.board.iter().flat_map(|t| t.to_le_bytes()).collect();
    let mut mod_time = Vec::new();
    gint5(lvl.mod_time, &mut mod_time);
    let links_and_signs = |out: &mut Out| {
        for l in &lvl.links {
            out.put(PLO_LEVELLINK, &link_line(l));
        }
        for s in &lvl.signs {
            out.put(PLO_LEVELSIGN, &sign_record(s));
        }
    };
    let chests = |out: &mut Out| {
        for c in lvl.chests.iter().filter_map(chest_record) {
            out.put(PLO_LEVELCHEST, &c);
        }
    };
    // The account's player flags, one `PLO_FLAGSET` each.
    let flags = |out: &mut Out| {
        for f in &a.flags {
            out.put(PLO_FLAGSET, &latin1(f));
        }
    };

    if gen == EncryptionGen::Gen2 || gen == EncryptionGen::Gen1 {
        out.put(PLO_PLAYERPROPS, &props);
        out.put(PLO_PLAYERWARP, &warp);
        // The raw-data header ends the first frame, the board is the second.
        let block = wire::raw_block(PLO_BOARDPACKET, &board);
        let split = block
            .iter()
            .position(|&b| b == b'\n')
            .expect("raw header ends in a newline")
            + 1;
        out.body.extend_from_slice(&block[..split]);
        let first = std::mem::take(&mut out.body);
        let second = block[split..].to_vec();
        out.put(PLO_LEVELNAME, &latin1(level));
        links_and_signs(&mut out);
        out.put(PLO_LEVELMODTIME, &mod_time);
        chests(&mut out);
        flags(&mut out);
        out.put(PLO_ISLEADER, b"");
        return vec![first, second, out.body];
    }

    out.put(PLO_SIGNATURE, &[gchar(73)]);
    if !cfg.start_message.is_empty() {
        out.put(PLO_STARTMESSAGE, &latin1(&cfg.start_message));
    }
    if !cfg.status_list.is_empty() {
        out.put(PLO_STATUSLIST, &latin1(&cfg.status_list));
    }
    out.put(PLO_PLAYERPROPS, &props);
    flags(&mut out);
    out.put(PLO_DEFAULTWEAPON, &[gchar(8)]);
    out.put(PLO_DEFAULTWEAPON, &[gchar(7)]);
    match gmap {
        Some(g) => out.put(PLO_PLAYERWARP2, &warp2(a.x, a.y, g)),
        None => out.put(PLO_PLAYERWARP, &warp),
    }
    out.put(PLO_LEVELNAME, &latin1(level));
    out.body.extend(wire::raw_block(PLO_BOARDPACKET, &board));
    links_and_signs(&mut out);
    out.put(PLO_LEVELMODTIME, &mod_time);
    chests(&mut out);
    if let Some(g) = gmap {
        out.put(PLO_LEVELNAME, &latin1(&g.gmap));
    }
    out.put(PLO_SETACTIVELEVEL, &latin1(level));
    out.put(PLO_ISLEADER, b"");
    let mut t = Vec::new();
    gint4(world_time, &mut t);
    out.put(PLO_NEWWORLDTIME, &t);
    out.put(PLO_LISTPROCESSES, b"");
    vec![out.body]
}

/// The server's answer to a link request into `level` (`PLI_LEVELWARP` or
/// `PLI_LEVELWARPMOD`), as frame bodies. It has no `PLO_PLAYERWARP`: the
/// client puts the player at the link's arrival itself (Seihai's lab notes
/// on the beta4 server, "Warps"). With `cached` (the client's copy
/// has the level's mod time) there is no board, mod time, links or signs,
/// as in the lab (Seihai `protocol.md`, "Level changes").
///
/// The level name comes first in every generation: a client drops the old
/// level's board, links and signs when it is told a new name. Gen 2 keeps
/// the burst's frames otherwise (the raw-data header ends a frame, the board
/// is a frame of its own); whether a real gen-2 client needs that on a warp
/// is not in any capture yet.
pub fn level_answer(
    version: ClientVersion,
    gen: EncryptionGen,
    level: &Level,
    cached: bool,
) -> Vec<Vec<u8>> {
    let plo = &version.profile().protocol.plo;
    let has = |id: u8| plo.contains(id);
    let mut out = Out {
        has: &has,
        body: Vec::new(),
    };
    let name = latin1(&level.name);
    let gen2 = matches!(gen, EncryptionGen::Gen1 | EncryptionGen::Gen2);
    let mut frames = Vec::new();
    out.put(PLO_LEVELNAME, &name);
    if !cached {
        let board: Vec<u8> = level.board.iter().flat_map(|t| t.to_le_bytes()).collect();
        let block = wire::raw_block(PLO_BOARDPACKET, &board);
        if gen2 {
            let split = block
                .iter()
                .position(|&b| b == b'\n')
                .expect("raw header ends in a newline")
                + 1;
            out.body.extend_from_slice(&block[..split]);
            frames.push(std::mem::take(&mut out.body));
            frames.push(block[split..].to_vec());
        } else {
            out.body.extend(block);
        }
        for l in &level.links {
            out.put(PLO_LEVELLINK, &link_line(l));
        }
        for s in &level.signs {
            out.put(PLO_LEVELSIGN, &sign_record(s));
        }
        let mut mod_time = Vec::new();
        gint5(level.mod_time, &mut mod_time);
        out.put(PLO_LEVELMODTIME, &mod_time);
    }
    for c in level.chests.iter().filter_map(chest_record) {
        out.put(PLO_LEVELCHEST, &c);
    }
    out.put(PLO_SETACTIVELEVEL, &name);
    frames.push(out.body);
    frames
}

/// `PLO_PLAYERWARP2` (49): `GCHAR x*2, y*2, z, gmap x, gmap y`, then the
/// gmap's name. z goes as 50, the value in the captured 49s Seihai's tests
/// keep; no client build reads it.
pub fn warp2(x: f32, y: f32, g: &Place) -> Vec<u8> {
    let mut w = vec![
        gchar((x * 2.0).clamp(0.0, 223.0) as u8),
        gchar((y * 2.0).clamp(0.0, 223.0) as u8),
        gchar(50),
        gchar(g.cell.0),
        gchar(g.cell.1),
    ];
    w.extend(latin1(&g.gmap));
    w
}

/// A gmap level for a build that plays gmaps, as one frame: the answer to
/// `PLI_ADJACENTLEVEL` (35) for a neighbour, and the level a player walks
/// into across a seam. With `full`: the name, `PLO_LEVELMODTIME`, the
/// board, links, signs and chests; without (the client holds the level):
/// the name alone. Both end with an empty `PLO_LEVELBOARD` (0) and a
/// `PLO_LEVELNAME` that names the gmap again, which tells the client the
/// player's own level is current again. The lab's 2.22 and 2.31 crossings
/// had this shape (Seihai `protocol.md`, "Gmaps"); the 35 answers are
/// taken to be the same.
pub fn gmap_level(version: ClientVersion, level: &Level, gmap: &str, full: bool) -> Vec<u8> {
    let plo = &version.profile().protocol.plo;
    let has = |id: u8| plo.contains(id);
    let mut out = Out {
        has: &has,
        body: Vec::new(),
    };
    out.put(PLO_LEVELNAME, &latin1(&level.name));
    if full {
        let mut mod_time = Vec::new();
        gint5(level.mod_time, &mut mod_time);
        out.put(PLO_LEVELMODTIME, &mod_time);
        let board: Vec<u8> = level.board.iter().flat_map(|t| t.to_le_bytes()).collect();
        out.body.extend(wire::raw_block(PLO_BOARDPACKET, &board));
        for l in &level.links {
            out.put(PLO_LEVELLINK, &link_line(l));
        }
        for s in &level.signs {
            out.put(PLO_LEVELSIGN, &sign_record(s));
        }
        for c in level.chests.iter().filter_map(chest_record) {
            out.put(PLO_LEVELCHEST, &c);
        }
    }
    out.put(PLO_LEVELBOARD, b"");
    out.put(PLO_LEVELNAME, &latin1(gmap));
    out.body
}

struct Out<'a> {
    has: &'a dyn Fn(u8) -> bool,
    body: Vec<u8>,
}

impl Out<'_> {
    fn put(&mut self, id: u8, payload: &[u8]) {
        if (self.has)(id) {
            self.body.extend(msg(id, payload));
        }
    }
}

/// The player's own props, as `PLO_PLAYERPROPS` carries them.
pub fn own_props(version: ClientVersion, p: &Player) -> Vec<u8> {
    let prof = version.profile();
    let own: &IdSet = &prof.props.own;
    let a = p.account;
    let mut b = Vec::new();
    let mut put = |id: u8, v: &[u8]| {
        if own.contains(id) {
            b.push(gchar(id));
            b.extend_from_slice(v);
        }
    };
    let s = |text: &str| {
        let mut v = Vec::new();
        gstring(&latin1(text), &mut v);
        v
    };
    put(0, &s(&a.nick));
    put(1, &[gchar(a.max_hp)]);
    put(2, &[gchar((a.hp * 2.0).clamp(0.0, 223.0) as u8)]);
    let mut r = Vec::new();
    gint3(a.rupees, &mut r);
    put(3, &r);
    put(4, &[gchar(a.arrows)]);
    put(5, &[gchar(a.bombs)]);
    put(6, &[gchar(a.glove)]);
    // Sword: c(power + 30) + image; shield: c(power + 10) + image. With no
    // image the power goes alone.
    put(8, &power_image(a.sword.0, 30, &a.sword.1));
    put(9, &power_image(a.shield.0, 10, &a.shield.1));
    // Before 2.1 prop 10 is the bow slot; it is left out there.
    if !prof.props.bow_prop10 {
        put(10, &s("idle"));
    }
    // Head: c(len + 100) + image.
    let head = latin1(&a.head);
    let head = &head[..head.len().min(123)];
    let mut h = vec![gchar(100 + head.len() as u8)];
    h.extend_from_slice(head);
    put(11, &h);
    put(13, &a.colors.map(gchar));
    let mut id = Vec::new();
    gshort(p.id, &mut id);
    put(14, &id);
    put(15, &[gchar((a.x * 2.0) as u8)]);
    put(16, &[gchar((a.y * 2.0) as u8)]);
    put(17, &[gchar(a.sprite)]);
    put(18, &[gchar(a.status & !0x02)]);
    put(34, &s(&a.name));
    put(35, &s(&a.body));
    if prof.props.precise_pos {
        for (pid, v) in [(78u8, a.x), (79, a.y)] {
            let mut w = Vec::new();
            gshort(encode_pos_precise(v), &mut w);
            put(pid, &w);
        }
    }
    b
}

fn power_image(power: u8, bias: u8, image: &str) -> Vec<u8> {
    if image.is_empty() {
        return vec![gchar(power)];
    }
    let mut v = vec![gchar(power.saturating_add(bias))];
    gstring(&latin1(image), &mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::{parse_player_props_in, SessionState};
    use std::path::Path;

    fn account() -> Account {
        let mut a = Account::new("tester", "", 30.0, 30.5);
        a.status = 20;
        a
    }

    /// A login into a gmap cell and a gmap level answer, read by every
    /// build that plays gmaps: the gmap and cell from 49, the level from
    /// the first name, and the player's level current again after each
    /// level sent.
    #[test]
    fn gmap_login_and_levels_for_every_gmap_build() {
        let cfg = Config::defaults(Path::new("w"));
        let a = account();
        let mut board = vec![0x1FF; 4096];
        board[3] = 0x44;
        let west = Level {
            board: board.clone(),
            ..Level::blank("gm_w.nw", 0x1FF, 1_700_000_000)
        };
        let east = Level::blank("gm_e.nw", 0x22, 1_700_000_001);
        let place = Place {
            gmap: "world.gmap".into(),
            cell: (0, 0),
        };
        let mut seen = 0;
        for &v in ClientVersion::all_versions() {
            if !crate::gmaps::plays_gmaps(v) {
                continue;
            }
            seen += 1;
            let p = Player { id: 1, account: &a };
            let mut st = SessionState::for_version(v);
            for f in login_burst(&cfg, v, v.encryption(), &p, &west, Some(&place), 0) {
                st.apply_body(&f);
            }
            let g = st
                .gmap
                .as_ref()
                .unwrap_or_else(|| panic!("{v:?} not on the gmap"));
            assert_eq!((g.name.as_str(), g.cell), ("world.gmap", (0, 0)), "{v:?}");
            assert_eq!(st.current_level_name().as_deref(), Some("gm_w.nw"), "{v:?}");
            assert_eq!(st.board.as_deref(), Some(&board[..]), "{v:?}");
            st.apply_body(&gmap_level(v, &east, "world.gmap", true));
            assert_eq!(st.level_name.as_deref(), Some("gm_w.nw"), "{v:?}");
            assert_eq!(st.board_of("gm_e.nw").map(|b| b[0]), Some(0x22), "{v:?}");
            assert!(st.ignored.is_empty(), "{v:?} drops {:?}", st.ignored);
            let short = gmap_level(v, &east, "world.gmap", false);
            let ids: Vec<u8> = seihai_core::protocol::split_messages(&short)
                .iter()
                .map(|m| m.id)
                .collect();
            assert_eq!(ids, [PLO_LEVELNAME, PLO_LEVELBOARD, PLO_LEVELNAME], "{v:?}");
        }
        assert!(seen >= 3, "only {seen} builds play gmaps");
    }

    /// The core's own-prop parser reads every prop we send, for every
    /// build: nothing is cut short by an id the build does not take.
    #[test]
    fn own_props_parse_whole_for_every_build() {
        let a = account();
        for &v in ClientVersion::all_versions() {
            let p = Player { id: 7, account: &a };
            let bytes = own_props(v, &p);
            let prof = v.profile();
            let parsed =
                parse_player_props_in(&bytes, prof.props.bow_prop10, Some(&prof.props.own));
            let len: usize = parsed.iter().map(|p| 1 + p.raw.len()).sum();
            assert_eq!(len, bytes.len(), "{v:?}: {parsed:?}");
            assert!(parsed.iter().any(|p| p.id == 0), "{v:?} gets its nickname");
        }
    }

    /// Fed to the client's session state for its build, the burst gives the
    /// level, the board and the props, and nothing the build would drop.
    #[test]
    fn burst_loads_the_level_for_every_build() {
        let cfg = Config::defaults(Path::new("w"));
        let a = account();
        for &v in ClientVersion::all_versions() {
            let p = Player { id: 3, account: &a };
            let level = Level::blank("onlinestartlocal.nw", cfg.board_tile, 1_700_000_000);
            let frames = login_burst(&cfg, v, v.encryption(), &p, &level, None, 1234);
            let mut st = SessionState::for_version(v);
            for f in &frames {
                st.apply_body(f);
            }
            assert_eq!(
                st.current_level_name().as_deref(),
                Some("onlinestartlocal.nw"),
                "{v:?}"
            );
            assert_eq!(st.board.as_ref().map(|b| b.len()), Some(4096), "{v:?}");
            assert_eq!(st.board.as_ref().unwrap()[0], cfg.board_tile, "{v:?}");
            assert_eq!(st.nickname.as_deref(), Some("tester"), "{v:?}");
            assert!(st.ignored.is_empty(), "{v:?} drops {:?}", st.ignored);
            assert!(st.disconnect_message.is_none());
        }
    }

    /// A level from a file: every build that reads links, signs and chests
    /// gets all of them, in the client's own reader, and nothing it drops.
    #[test]
    fn burst_carries_links_signs_and_chests() {
        let cfg = Config::defaults(Path::new("w"));
        let a = account();
        let file = seihai_core::assets::Level::parse(
            "GLEVNW01\n\
             LINK next.nw 0 10 1 4 62 playery\n\
             SIGN 20 21\nHello #A!\nSIGNEND\n\
             CHEST 30 36 greenrupee -1\n\
             CHEST 10 12 bow 0\n",
        )
        .unwrap();
        let mut board = vec![0x1FF; 4096];
        board[65] = 0x22;
        let level = Level {
            name: "lab.nw".into(),
            board: board.clone(),
            links: file.links,
            signs: file.signs,
            chests: file.chests,
            npcs: Vec::new(),
            mod_time: 1_700_000_123,
        };
        for &v in ClientVersion::all_versions() {
            let p = Player { id: 3, account: &a };
            let frames = login_burst(&cfg, v, v.encryption(), &p, &level, None, 1234);
            let mut st = SessionState::for_version(v);
            for f in &frames {
                st.apply_body(f);
            }
            assert_eq!(st.current_level_name().as_deref(), Some("lab.nw"), "{v:?}");
            assert_eq!(st.board.as_deref(), Some(&board[..]), "{v:?}");
            assert!(st.ignored.is_empty(), "{v:?} drops {:?}", st.ignored);
            let plo = &v.profile().protocol.plo;
            let n = |id: u8, k: usize| if plo.contains(id) { k } else { 0 };
            assert_eq!(st.links.len(), n(PLO_LEVELLINK, 1), "{v:?}");
            assert_eq!(st.signs.len(), n(PLO_LEVELSIGN, 1), "{v:?}");
            assert_eq!(st.chests.len(), n(PLO_LEVELCHEST, 2), "{v:?}");
        }
        // 2.22: name, board, link, sign, mod time, chests, active level.
        let f = login_burst(
            &cfg,
            ClientVersion::V222,
            EncryptionGen::Gen3,
            &Player { id: 1, account: &a },
            &level,
            None,
            0,
        );
        let ids: Vec<u8> = seihai_core::protocol::split_messages(&f[0])
            .iter()
            .map(|m| m.id)
            .filter(|id| [6, 101, 1, 5, 39, 4, 156].contains(id))
            .collect();
        assert_eq!(ids, [6, 101, 1, 5, 39, 4, 4, 156]);
    }

    /// A link's answer, fed to each build's own reader: the new level
    /// with its board and links, or for a current cached copy only the
    /// name, chests and active level.
    #[test]
    fn level_answer_loads_the_level_for_every_build() {
        let file = seihai_core::assets::Level::parse(
            "GLEVNW01\nLINK back.nw 0 10 1 4 62 playery\nCHEST 30 36 greenrupee -1\n",
        )
        .unwrap();
        let mut board = vec![0x1FF; 4096];
        board[7] = 0x33;
        let level = Level {
            name: "house.nw".into(),
            board: board.clone(),
            links: file.links,
            signs: Vec::new(),
            chests: file.chests,
            npcs: Vec::new(),
            mod_time: 1_700_000_456,
        };
        for &v in ClientVersion::all_versions() {
            let plo = v.profile().protocol.plo;
            let mut st = SessionState::for_version(v);
            for f in level_answer(v, v.encryption(), &level, false) {
                st.apply_body(&f);
            }
            assert_eq!(
                st.current_level_name().as_deref(),
                Some("house.nw"),
                "{v:?}"
            );
            assert_eq!(st.board.as_deref(), Some(&board[..]), "{v:?}");
            assert!(st.ignored.is_empty(), "{v:?} drops {:?}", st.ignored);
            if plo.contains(PLO_LEVELLINK) {
                assert_eq!(st.links, ["back.nw 0 10 1 4 62 playery"], "{v:?}");
            }
            let cached = level_answer(v, v.encryption(), &level, true);
            assert_eq!(cached.len(), 1, "{v:?}");
            let ids: Vec<u8> = seihai_core::protocol::split_messages(&cached[0])
                .iter()
                .map(|m| m.id)
                .collect();
            assert!(ids.contains(&PLO_LEVELNAME), "{v:?}");
            for id in [
                PLO_BOARDPACKET,
                PLO_RAWDATA,
                PLO_LEVELLINK,
                PLO_LEVELMODTIME,
            ] {
                assert!(!ids.contains(&id), "{v:?} cached answer has {id}");
            }
        }
        // Gen 2 keeps the board in a frame of its own, after the name.
        let f = level_answer(ClientVersion::V131, EncryptionGen::Gen2, &level, false);
        assert_eq!(f.len(), 3);
        assert_eq!(f[0][0], gchar(PLO_LEVELNAME));
        assert_eq!(f[1][0], gchar(PLO_BOARDPACKET));
    }

    #[test]
    fn gen2_order_puts_the_name_after_the_board() {
        let cfg = Config::defaults(Path::new("w"));
        let a = account();
        let frames = login_burst(
            &cfg,
            ClientVersion::V131,
            EncryptionGen::Gen2,
            &Player { id: 1, account: &a },
            &Level::blank("start.nw", 0, 0),
            None,
            0,
        );
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0][0], gchar(PLO_PLAYERPROPS));
        assert!(
            frames[0].ends_with(b"\n") && frames[0].windows(1).any(|w| w[0] == gchar(PLO_RAWDATA))
        );
        assert_eq!(frames[1][0], gchar(PLO_BOARDPACKET));
        assert_eq!(frames[2][0], gchar(PLO_LEVELNAME));
    }

    #[test]
    fn later_builds_end_with_listprocesses_and_have_156() {
        let cfg = Config::defaults(Path::new("w"));
        let a = account();
        let f = login_burst(
            &cfg,
            ClientVersion::V205,
            EncryptionGen::Gen3,
            &Player { id: 1, account: &a },
            &Level::blank("start.nw", 0, 0),
            None,
            0,
        );
        let m = seihai_core::protocol::split_messages(&f[0]);
        let ids: Vec<u8> = m.iter().map(|m| m.id).collect();
        let pos = |id| ids.iter().position(|&x| x == id).unwrap();
        assert!(pos(PLO_LEVELNAME) < pos(PLO_BOARDPACKET));
        assert!(ids.contains(&PLO_SETACTIVELEVEL));
        assert_eq!(ids.iter().filter(|&&x| x == PLO_DEFAULTWEAPON).count(), 2);
        let f = login_burst(
            &cfg,
            ClientVersion::V231,
            EncryptionGen::Gen5,
            &Player { id: 1, account: &a },
            &Level::blank("start.nw", 0, 0),
            None,
            0,
        );
        let m = seihai_core::protocol::split_messages(&f[0]);
        assert_eq!(m.last().unwrap().id, PLO_LISTPROCESSES);
    }
}
