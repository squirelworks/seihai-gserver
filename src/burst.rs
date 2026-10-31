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
//! Stage 1 has no level files: the board is generated, all one tile.

use crate::accounts::Account;
use crate::config::Config;
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
    level_mod_time: u64,
    world_time: u32,
) -> Vec<Vec<u8>> {
    let plo = &version.profile().protocol.plo;
    let has = |id: u8| plo.contains(id);
    let mut out = Out {
        has: &has,
        body: Vec::new(),
    };
    let a = p.account;
    let level = if a.level.is_empty() {
        cfg.start_level.as_str()
    } else {
        a.level.as_str()
    };
    let props = own_props(version, p);
    let mut warp = vec![gchar((a.x * 2.0) as u8), gchar((a.y * 2.0) as u8)];
    warp.extend(latin1(level));
    let board = board_payload(cfg.board_tile);
    let mut mod_time = Vec::new();
    gint5(level_mod_time, &mut mod_time);

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
        out.put(PLO_LEVELMODTIME, &mod_time);
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
    out.put(PLO_DEFAULTWEAPON, &[gchar(8)]);
    out.put(PLO_DEFAULTWEAPON, &[gchar(7)]);
    out.put(PLO_PLAYERWARP, &warp);
    out.put(PLO_LEVELNAME, &latin1(level));
    out.body.extend(wire::raw_block(PLO_BOARDPACKET, &board));
    out.put(PLO_LEVELMODTIME, &mod_time);
    out.put(PLO_SETACTIVELEVEL, &latin1(level));
    out.put(PLO_ISLEADER, b"");
    let mut t = Vec::new();
    gint4(world_time, &mut t);
    out.put(PLO_NEWWORLDTIME, &t);
    out.put(PLO_LISTPROCESSES, b"");
    vec![out.body]
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

/// A 64 x 64 board of one tile, little-endian `u16`s.
pub fn board_payload(tile: u16) -> Vec<u8> {
    (0..64 * 64).flat_map(|_| tile.to_le_bytes()).collect()
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
            let frames = login_burst(&cfg, v, v.encryption(), &p, 1_700_000_000, 1234);
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

    #[test]
    fn gen2_order_puts_the_name_after_the_board() {
        let cfg = Config::defaults(Path::new("w"));
        let a = account();
        let frames = login_burst(
            &cfg,
            ClientVersion::V131,
            EncryptionGen::Gen2,
            &Player { id: 1, account: &a },
            0,
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
            0,
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
            0,
            0,
        );
        let m = seihai_core::protocol::split_messages(&f[0]);
        assert_eq!(m.last().unwrap().id, PLO_LISTPROCESSES);
    }
}
