//! Level NPCs (stage 3.1).
//!
//! The NPCs of a level file (`NPC image x y`, the script, `NPCEND`) are
//! the server's: the world gives each an id when the level is first read
//! and sends them as `PLO_NPCPROPS` (3) to every player who enters the
//! level, `GINT3 id` and then props. The clients run the GS1 scripts and
//! report what a script changed (`PLI_NPCPROPS`, 3, the same shape); the
//! world keeps the latest value of each prop and passes the change on to
//! the others in the level, so a block one player pushed is where they
//! see it, and where a later arrival sees it too.
//!
//! The world keeps prop values as the bytes on the wire. [`split`] cuts a
//! prop stream into them with the prop lengths of `PLO_NPCPROPS` (a fact
//! of the wire, the same table `seihai_core::protocol::npcprops` reads).
//! Each receiver gets the props its build reads: ids up to its
//! `npc_max`, and prop 12 only in its own shape (the bow before 2.1, the
//! gani name after).
//!
//! A client may not set an NPC's script (1), id (17) or level (52): a
//! script from one client would run on every other client in the level.

use crate::wire::ids::PLO_NPCPROPS;
use crate::wire::{gchar, gint3, gshort, latin1, msg};
use seihai_core::protocol::ClientVersion;
use std::collections::BTreeMap;

/// Prop ids.
pub const IMAGE: u8 = 0;
pub const SCRIPT: u8 = 1;
pub const X: u8 = 2;
pub const Y: u8 = 3;
/// The gani name from 2.1 on; before, the NPC's bow.
pub const GANI_OR_BOW: u8 = 12;
pub const VISFLAGS: u8 = 13;
/// VISFLAGS bit 1: drawn.
pub const VIS_SHOWN: u8 = 1;
pub const MESSAGE: u8 = 15;
pub const ID: u8 = 17;
pub const SPRITE: u8 = 18;
/// One byte, `ff` on every record GServer-v2 sends.
/// The cell of the NPC's level on a gmap (`GCHAR`), sent to a player who
/// plays that level as part of the gmap.
pub const GMAPLEVELX: u8 = 41;
pub const GMAPLEVELY: u8 = 42;
pub const UNKNOWN43: u8 = 43;
pub const CURLEVEL: u8 = 52;

/// Props a client's update may not change.
const SERVER_ONLY: [u8; 5] = [SCRIPT, GMAPLEVELX, GMAPLEVELY, ID, CURLEVEL];

/// The longest script a `GSHORT` length holds.
pub const MAX_SCRIPT: usize = (1 << 14) - 1;

/// One NPC as the world holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Npc {
    pub id: u32,
    /// Prop id to its value as on the wire.
    pub props: BTreeMap<u8, Vec<u8>>,
}

impl Npc {
    /// An NPC from the level file: image, script and position.
    pub fn from_level(id: u32, n: &seihai_core::assets::Npc) -> Self {
        let mut props = BTreeMap::new();
        let mut image = latin1(&n.image);
        image.truncate(223);
        let mut v = vec![gchar(image.len() as u8)];
        v.extend(image);
        props.insert(IMAGE, v);
        let script = script_bytes(&n.script);
        if !script.is_empty() && script.len() <= MAX_SCRIPT {
            let mut v = Vec::new();
            gshort(script.len() as u16, &mut v);
            v.extend(script);
            props.insert(SCRIPT, v);
        }
        props.insert(X, vec![gchar(half_tiles(n.x))]);
        props.insert(Y, vec![gchar(half_tiles(n.y))]);
        // The rest of the record a level NPC gets from GServer-v2 beta4
        // (lab capture, 2026-11-04): shown, no message, its own id, sprite
        // 2, and 43 as `ff`. (X2/Y2/Z2 are left out: client updates move
        // X/Y.) Real 2.05 and 2.22 draw it (Wine lab, 2026-11-05).
        props.insert(VISFLAGS, vec![gchar(VIS_SHOWN)]);
        props.insert(MESSAGE, vec![gchar(0)]);
        let mut idv = Vec::new();
        gint3(id, &mut idv);
        props.insert(ID, idv);
        props.insert(SPRITE, vec![gchar(2)]);
        props.insert(UNKNOWN43, vec![0xFF]);
        Npc { id, props }
    }

    /// Take a client's update (as its build writes it); returns the props
    /// that changed, to pass on. Props a client may not set are dropped.
    pub fn update(&mut self, raw: &[u8], sender: ClientVersion) -> Vec<(u8, Vec<u8>)> {
        let mut changed = Vec::new();
        for (id, v) in split(raw, bow12(sender)) {
            if SERVER_ONLY.contains(&id) || (id == GANI_OR_BOW && bow12(sender)) {
                continue;
            }
            if self.props.get(&id) != Some(&v) {
                self.props.insert(id, v.clone());
                changed.push((id, v));
            }
        }
        changed
    }

    /// The whole NPC for `to`'s build.
    pub fn message(&self, to: ClientVersion) -> Option<Vec<u8>> {
        self.message_on(to, None)
    }

    /// The NPC for `to`, which plays its level as `cell` of a gmap when
    /// one is given. A gmap client keeps the NPCs of every cell under the
    /// gmap's name and places each by its cell (props 41 and 42) and its
    /// X/Y in that cell; without them the NPC is not drawn.
    pub fn message_on(&self, to: ClientVersion, cell: Option<(u8, u8)>) -> Option<Vec<u8>> {
        let mut all: Vec<_> = self.props.iter().map(|(k, v)| (*k, v.clone())).collect();
        if let Some((cx, cy)) = cell {
            all.push((GMAPLEVELX, vec![gchar(cx)]));
            all.push((GMAPLEVELY, vec![gchar(cy)]));
            all.sort_by_key(|(k, _)| *k);
        }
        message(self.id, &all, to)
    }
}

/// `PLO_NPCPROPS` for `to`, with the props its build reads. `None` when
/// none of them is left.
pub fn message(id: u32, props: &[(u8, Vec<u8>)], to: ClientVersion) -> Option<Vec<u8>> {
    let max = to.profile().props.npc_max;
    let mut p = Vec::new();
    gint3(id, &mut p);
    let head = p.len();
    for (pid, v) in props {
        if *pid > max || (*pid == GANI_OR_BOW && bow12(to)) {
            continue;
        }
        p.push(gchar(*pid));
        p.extend_from_slice(v);
    }
    (p.len() > head).then(|| msg(PLO_NPCPROPS, &p))
}

/// Prop 12 is the bow (pre-2.1 shape) for this build.
fn bow12(v: ClientVersion) -> bool {
    v.profile().props.bow_prop10
}

/// A script as the wire carries it: Latin-1, each line without its
/// leading blanks (as GServer-v2 sends it), lines joined by `§`.
fn script_bytes(s: &str) -> Vec<u8> {
    let s: Vec<&str> = s.lines().map(str::trim_start).collect();
    let s = s.join("\n");
    let s = s.trim_end_matches('\n');
    latin1(s)
        .into_iter()
        .map(|b| if b == b'\n' { 0xA7 } else { b })
        .collect()
}

/// A position in tiles as `GCHAR` half tiles.
fn half_tiles(v: f32) -> u8 {
    (v * 2.0).round().clamp(0.0, 223.0) as u8
}

/// Cut a prop stream into `(prop id, value bytes)`. Reading stops at a
/// prop whose length is not known, or at a value cut short.
pub fn split(b: &[u8], bow12: bool) -> Vec<(u8, Vec<u8>)> {
    let gc = |i: usize| b.get(i).map(|c| c.wrapping_sub(0x20) as usize);
    let gs = |i: usize| -> Option<usize> { Some(gc(i)? << 7 | gc(i + 1)?) };
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(prop) = gc(i) {
        let start = i + 1;
        let len = match prop {
            // GCHAR length and the text.
            0 | 15 | 20 | 21 | 35..=40 | 44..=47 | 49..=73 => gc(start).map(|n| 1 + n),
            12 if !bow12 => gc(start).map(|n| 1 + n),
            12 => gc(start).map(|n| 1 + n.saturating_sub(10)),
            // GSHORT length and the text.
            1 | 74 => gs(start).map(|n| 2 + n),
            2 | 3 | 4 | 6..=9 | 13 | 14 | 18 | 23..=33 | 41..=43 => Some(1),
            5 | 17 => Some(3),
            16 | 75..=77 => Some(2),
            19 => Some(5),
            34 => Some(6),
            10 => gc(start).map(|p| {
                if p > 4 {
                    1 + 1 + gc(start + 1).unwrap_or(0)
                } else {
                    1
                }
            }),
            11 => gc(start).map(|p| {
                if p > 3 {
                    1 + 1 + gc(start + 1).unwrap_or(0)
                } else {
                    1
                }
            }),
            22 => gc(start).map(|n| if n < 100 { 1 } else { 1 + n - 100 }),
            _ => None,
        };
        let Some(len) = len else { break };
        let Some(v) = b.get(start..start + len) else {
            break;
        };
        out.push((prop as u8, v.to_vec()));
        i = start + len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::npcprops::{
        npc_props_message, parse_npc_props, PropValue, NPCPROP_BLOCKFLAGS, NPCPROP_X,
    };

    fn level_npc() -> Npc {
        let n = seihai_core::assets::Npc::new(
            "block.png".into(),
            30.0,
            31.5,
            "if (playerenters) {\n  canbepushed;\n}\n".into(),
        );
        Npc::from_level(7, &n)
    }

    /// The payload of a message from [`message`] (no id byte, no newline).
    fn payload(m: &[u8]) -> &[u8] {
        &m[1..m.len() - 1]
    }

    #[test]
    fn a_level_npc_reads_back_in_every_build() {
        let n = level_npc();
        for v in [
            ClientVersion::V139,
            ClientVersion::V205,
            ClientVersion::V222,
            ClientVersion::V231,
        ] {
            let m = n.message(v).unwrap();
            let (id, p) = parse_npc_props(payload(&m), bow12(v)).unwrap();
            assert_eq!(id, 7);
            assert_eq!(p.image.as_deref(), Some("block.png"));
            assert_eq!(
                p.script.as_deref(),
                Some("if (playerenters) {\ncanbepushed;\n}")
            );
            assert_eq!((p.x, p.y), (Some(30.0), Some(31.5)), "{v:?}");
        }
    }

    #[test]
    fn a_client_update_is_kept_and_passed_on() {
        let mut n = level_npc();
        // 2.05 after `canbepushed` and a push: BLOCKFLAGS 8, then X.
        let m = npc_props_message(
            7,
            &[
                (NPCPROP_BLOCKFLAGS, PropValue::Byte(8)),
                (NPCPROP_X, PropValue::Byte(62)),
            ],
        );
        let changed = n.update(&m[4..m.len() - 1], ClientVersion::V205);
        assert_eq!(changed.len(), 2);
        let out = message(7, &changed, ClientVersion::V222).unwrap();
        let (_, p) = parse_npc_props(payload(&out), false).unwrap();
        assert_eq!((p.block_flags, p.x), (Some(8), Some(31.0)));
        // The same again changes nothing.
        assert!(n.update(&m[4..m.len() - 1], ClientVersion::V205).is_empty());
        // A late arrival gets the pushed position.
        let (_, p) =
            parse_npc_props(payload(&n.message(ClientVersion::V139).unwrap()), true).unwrap();
        assert_eq!(p.x, Some(31.0));
    }

    #[test]
    fn a_client_may_not_set_a_script() {
        let mut n = level_npc();
        let mut raw = vec![gchar(SCRIPT)];
        gshort(5, &mut raw);
        raw.extend(b"hello");
        raw.push(gchar(X));
        raw.push(gchar(10));
        let changed = n.update(&raw, ClientVersion::V222);
        assert_eq!(changed, [(X, vec![gchar(10)])]);
        let (_, p) =
            parse_npc_props(payload(&n.message(ClientVersion::V222).unwrap()), false).unwrap();
        assert_eq!(
            p.script.as_deref(),
            Some("if (playerenters) {\ncanbepushed;\n}")
        );
    }

    #[test]
    fn a_receiver_gets_only_the_props_it_reads() {
        // GATTRIB6 (44) is past 2.05's NPC props; 1.21 stops at 17.
        let props = vec![(X, vec![gchar(4)]), (44, vec![gchar(3), b'a', b'b', b'c'])];
        let m = message(1, &props, ClientVersion::V205).unwrap();
        assert_eq!(
            payload(&m),
            [gchar(0), gchar(0), gchar(1), gchar(X), gchar(4)]
        );
        assert!(message(1, &props[1..], ClientVersion::V205).is_none());
        assert!(message(1, &props, ClientVersion::V222)
            .unwrap()
            .ends_with(b"abc\n"));
    }

    #[test]
    fn a_gmap_receiver_gets_the_cell() {
        let n = Npc {
            id: 1,
            props: [(X, vec![gchar(60)]), (UNKNOWN43, vec![0xFF])].into(),
        };
        let m = n.message_on(ClientVersion::V222, Some((1, 2))).unwrap();
        assert_eq!(
            payload(&m),
            [
                gchar(0),
                gchar(0),
                gchar(1),
                gchar(X),
                gchar(60),
                gchar(GMAPLEVELX),
                gchar(1),
                gchar(GMAPLEVELY),
                gchar(2),
                gchar(UNKNOWN43),
                0xFF
            ]
        );
        // A client may not move an NPC to another cell.
        let mut n = n;
        assert!(n
            .update(&[gchar(GMAPLEVELX), gchar(3)], ClientVersion::V222)
            .is_empty());
    }

    #[test]
    fn split_stops_at_an_unknown_prop() {
        let raw = [gchar(X), gchar(4), gchar(48), gchar(1), gchar(Y), gchar(5)];
        assert_eq!(split(&raw, false), [(X, vec![gchar(4)])]);
        // A value cut short is not taken.
        assert!(split(&[gchar(IMAGE), gchar(9), b'a'], false).is_empty());
    }
}
