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
//!
//! A level NPC's script is written per receiver (issue #45, the scripts
//! row of `docs/CLIENT-PROFILES.md`). It is compiled once, when the level
//! is read, with `seihai_core::scripting::delivery`, and each build gets
//! the form its profile says it runs: the GS1 text in prop 1, the GS2
//! bytecode as `PLO_NPCBYTECODE` (131) right after the NPC's props, or
//! no script at all (a stub), which is logged once per NPC and build.

use crate::profile::ClientProfile;
use crate::wire::ids::{PLO_NPCBYTECODE, PLO_NPCPROPS};
use crate::wire::{gchar, gint3, gshort, latin1, msg, raw_block};
use seihai_core::protocol::ClientVersion;
use seihai_core::scripting::delivery::{Delivery, Prepared};
use seihai_core::scripting::engine::Era;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

/// The GS1 era level NPC scripts are written in: the world's levels are
/// 2.1-era files, as GServer-v2 serves them.
pub const LEVEL_ERA: Era = Era::V217;

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
    /// Prop id to its value as on the wire. Prop 1 holds the GS1 text; it
    /// goes only to the receivers [`Npc::script`] sends text to.
    pub props: BTreeMap<u8, Vec<u8>>,
    /// A level NPC's script, compiled once. `None` for an NPC without a
    /// script and for server NPCs, whose script stays on the NPC-server.
    pub script: Option<Script>,
}

/// A level NPC's script: its GS1 source and GS2 bytecode, and the builds
/// already logged as getting no script.
#[derive(Debug, Clone)]
pub struct Script(Arc<ScriptInner>);

#[derive(Debug)]
struct ScriptInner {
    prepared: Prepared,
    stub_logged: Mutex<HashSet<ClientVersion>>,
}

impl PartialEq for Script {
    fn eq(&self, other: &Self) -> bool {
        self.0.prepared.source() == other.0.prepared.source()
    }
}

impl Script {
    /// Compile `source` (GS1 of `era`) once.
    pub fn gs1(source: &str, era: Era) -> Script {
        Script(Arc::new(ScriptInner {
            prepared: Prepared::gs1(source, era),
            stub_logged: Mutex::new(HashSet::new()),
        }))
    }

    pub fn prepared(&self) -> &Prepared {
        &self.0.prepared
    }

    /// What `to` is sent, without logging.
    pub fn delivery(&self, to: impl Into<ClientProfile>) -> Delivery<'_> {
        let to = to.into();
        self.0.prepared.for_client(to.version).delivery
    }

    /// What `to` is sent. A stub is logged the first time each build gets
    /// it, so a server owner can see which builds go without the script.
    fn for_client(&self, npc: u32, to: impl Into<ClientProfile>) -> Delivery<'_> {
        let to = to.into();
        let d = self.0.prepared.for_client(to.version).delivery;
        if let Delivery::Stub(why) = d {
            let mut logged = self.0.stub_logged.lock().unwrap_or_else(|e| e.into_inner());
            if logged.insert(to.version) {
                eprintln!("npc {npc}: no script for {:?} ({why:?})", to.version);
            }
        }
        d
    }
}

impl Npc {
    /// An NPC from the level file: image, script and position.
    pub fn from_level(id: u32, n: &seihai_core::assets::Npc) -> Self {
        Npc::new(id, &n.image, &n.script, half_tiles(n.x), half_tiles(n.y))
    }

    /// An NPC a client put (`PLI_PUTNPC`, 21): the same record as a level
    /// NPC, at `x2`, `y2` half tiles.
    pub fn put(id: u32, image: &str, script: &str, x2: u8, y2: u8) -> Self {
        Npc::new(id, image, script, x2, y2)
    }

    fn new(id: u32, image: &str, source: &str, x2: u8, y2: u8) -> Self {
        let mut props = BTreeMap::new();
        let mut image = latin1(image);
        image.truncate(223);
        let mut v = vec![gchar(image.len() as u8)];
        v.extend(image);
        props.insert(IMAGE, v);
        let script = script_bytes(source);
        if !script.is_empty() && script.len() <= MAX_SCRIPT {
            let mut v = Vec::new();
            gshort(script.len() as u16, &mut v);
            v.extend(script);
            props.insert(SCRIPT, v);
        }
        props.insert(X, vec![gchar(x2)]);
        props.insert(Y, vec![gchar(y2)]);
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
        let script = (!source.trim().is_empty()).then(|| Script::gs1(source, LEVEL_ERA));
        Npc { id, props, script }
    }

    /// A server NPC from the NPC-server (`NPC_ADDED`): the record a level
    /// NPC gets, with `id` as this server's id for it, and then the link's
    /// props (the 2.22 encoding). The script stays on the NPC-server: a
    /// script sent to the clients would run there as well.
    pub fn from_link(id: u32, props: &[u8]) -> Self {
        let mut idv = Vec::new();
        gint3(id, &mut idv);
        let mut n = Npc {
            id,
            props: BTreeMap::from([
                (VISFLAGS, vec![gchar(VIS_SHOWN)]),
                (MESSAGE, vec![gchar(0)]),
                (ID, idv),
                (SPRITE, vec![gchar(2)]),
                (UNKNOWN43, vec![0xFF]),
            ]),
            script: None,
        };
        n.update(props, ClientVersion::V222);
        n
    }

    /// Take a client's update (as its build writes it); returns the props
    /// that changed, to pass on. Props a client may not set are dropped.
    pub fn update(&mut self, raw: &[u8], sender: impl Into<ClientProfile>) -> Vec<(u8, Vec<u8>)> {
        let sender = sender.into();
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
    pub fn message(&self, to: impl Into<ClientProfile>) -> Option<Vec<u8>> {
        let to = to.into();
        self.message_on(to, None)
    }

    /// The NPC for `to`, which plays its level as `cell` of a gmap when
    /// one is given. A gmap client keeps the NPCs of every cell under the
    /// gmap's name and places each by its cell (props 41 and 42) and its
    /// X/Y in that cell; without them the NPC is not drawn.
    ///
    /// The script goes in the form `to` runs (see [`Script`]): prop 1 for
    /// GS1 text; for GS2, `PLO_NPCBYTECODE` after the props, in the same
    /// bytes, since the client drops bytecode for an NPC it does not have.
    pub fn message_on(
        &self,
        to: impl Into<ClientProfile>,
        cell: Option<(u8, u8)>,
    ) -> Option<Vec<u8>> {
        let to = to.into();
        let delivery = self.script.as_ref().map(|s| s.for_client(self.id, to));
        let text = matches!(delivery, None | Some(Delivery::Gs1Text(_)));
        let mut all: Vec<_> = self
            .props
            .iter()
            .filter(|(k, _)| text || **k != SCRIPT)
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        if let Some((cx, cy)) = cell {
            all.push((GMAPLEVELX, vec![gchar(cx)]));
            all.push((GMAPLEVELY, vec![gchar(cy)]));
            all.sort_by_key(|(k, _)| *k);
        }
        let mut out = message(self.id, &all, to)?;
        if let Some(Delivery::Gs2Bytecode(code)) = delivery {
            out.extend(bytecode_message(self.id, code));
        }
        Some(out)
    }
}

/// `PLO_NPCPROPS` for `to`, with the props its build reads. `None` when
/// none of them is left.
pub fn message(id: u32, props: &[(u8, Vec<u8>)], to: impl Into<ClientProfile>) -> Option<Vec<u8>> {
    let to = to.into();
    let max = to.core.props.npc_max;
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

/// `PLO_RAWDATA` and `PLO_NPCBYTECODE`: `GINT3` NPC id, then the
/// bytecode file as the compiler writes it (ending in its own `\n`). The
/// raw count covers the id byte and everything after it
/// (seihai's `documentation/gs2-receive.md`).
pub fn bytecode_message(id: u32, code: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(code.len() + 3);
    gint3(id, &mut payload);
    payload.extend_from_slice(code.strip_suffix(b"\n").unwrap_or(code));
    raw_block(PLO_NPCBYTECODE, &payload)
}

/// Prop 12 is the bow (pre-2.1 shape) for this build. It follows prop 10
/// of players (`ServerProfile::prop10_bow`), so the 2.05 rev 2 override
/// reaches NPCs too: that build reads both as the gani.
fn bow12(v: impl Into<ClientProfile>) -> bool {
    let v = v.into();
    v.server.prop10_bow
}

/// A script as the wire carries it: Latin-1, each line without its
/// leading blanks (as GServer-v2 sends it), lines joined by `§`.
pub(crate) fn script_bytes(s: &str) -> Vec<u8> {
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
            script: None,
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
    fn a_server_npc_has_the_level_npc_record_and_no_script() {
        let mut raw = vec![gchar(IMAGE), gchar(8)];
        raw.extend(b"door.png");
        raw.push(gchar(SCRIPT));
        gshort(5, &mut raw);
        raw.extend(b"hello");
        raw.extend([gchar(X), gchar(60)]);
        let n = Npc::from_link(12, &raw);
        let (id, p) =
            parse_npc_props(payload(&n.message(ClientVersion::V205).unwrap()), true).unwrap();
        assert_eq!(id, 12);
        assert_eq!(p.image.as_deref(), Some("door.png"));
        assert_eq!(p.x, Some(30.0));
        assert!(p.script.is_none());
        assert!(n.props.contains_key(&VISFLAGS) && n.props.contains_key(&ID));
    }

    const DOOR: &str = "if (playerenters) {\n  setimg door.png;\n  this.open = 0;\n}\n\
if (playertouchsme && this.open == 0) {\n  this.open = 1;\n  setimg dooropen.png;\n  timeout = 1;\n}\n\
if (timeout) {\n  this.open = 0;\n  setimg door.png;\n}\n";

    fn door() -> Npc {
        let n = seihai_core::assets::Npc::new("door.png".into(), 30.0, 30.0, DOOR.into());
        Npc::from_level(9, &n)
    }

    /// What a client of `v` keeps of `bytes`, read with core's client.
    fn received(v: ClientVersion, bytes: &[u8]) -> seihai_core::protocol::SessionState {
        let mut s = seihai_core::protocol::SessionState::for_version(v);
        s.apply_body(bytes);
        s
    }

    #[test]
    fn gs1_builds_get_the_text_and_no_bytecode() {
        let n = door();
        for v in [ClientVersion::V222, ClientVersion::V231] {
            let m = n.message(v).unwrap();
            let (_, p) = parse_npc_props(payload(&m), bow12(v)).unwrap();
            assert!(
                p.script
                    .as_deref()
                    .unwrap()
                    .starts_with("if (playerenters)"),
                "{v:?}"
            );
            assert_eq!(
                m.iter().filter(|&&b| b == b'\n').count(),
                1,
                "{v:?}: one message"
            );
        }
    }

    #[test]
    fn gs2_builds_get_bytecode_after_the_props_and_no_text() {
        let n = door();
        let code = n
            .script
            .as_ref()
            .unwrap()
            .prepared()
            .bytecode()
            .unwrap()
            .to_vec();
        for v in [
            ClientVersion::V3153,
            ClientVersion::V524R4,
            ClientVersion::V603R7,
        ] {
            let m = n.message(v).unwrap();
            let s = received(v, &m);
            let (_, p) = s.server_npcs.get(&9).expect("props first");
            assert!(p.script.is_none(), "{v:?}: no GS1 text");
            assert_eq!(s.npc_bytecode.get(&9), Some(&code), "{v:?}");
            assert_eq!(s.npc_bytecode_dropped, 0, "{v:?}");
        }
    }

    #[test]
    fn a_script_a_build_cannot_run_goes_without_it() {
        // `with` is 2.1 GS1: 2.05 cannot read it and runs no GS2.
        let src = "if (created) { with (getplayer(a)) { hide; } }\n";
        let n = Npc::from_level(
            4,
            &seihai_core::assets::Npc::new("a.png".into(), 1.0, 1.0, src.into()),
        );
        let m = n.message(ClientVersion::V205).unwrap();
        let (_, p) = parse_npc_props(payload(&m), bow12(ClientVersion::V205)).unwrap();
        assert!(p.script.is_none());
        assert_eq!(p.image.as_deref(), Some("a.png"));
        assert!(!m.contains(&gchar(PLO_NPCBYTECODE)));
        // 2.22 reads it as text.
        let m = n.message(ClientVersion::V222).unwrap();
        assert!(parse_npc_props(payload(&m), false)
            .unwrap()
            .1
            .script
            .is_some());
    }

    #[test]
    fn npc_prop_12_follows_the_server_profile() {
        // 2.05 rev 2 reads prop 12 as the gani, as it reads player prop 10.
        assert!(!bow12(ClientVersion::V205));
        assert!(bow12(ClientVersion::V139));
        assert!(!bow12(ClientVersion::V222));
        let props = vec![(GANI_OR_BOW, vec![gchar(4), b'w', b'a', b'l', b'k'])];
        assert!(message(1, &props, ClientVersion::V205)
            .unwrap()
            .ends_with(b"walk\n"));
        assert!(message(1, &props, ClientVersion::V139).is_none());
    }

    #[test]
    fn split_stops_at_an_unknown_prop() {
        let raw = [gchar(X), gchar(4), gchar(48), gchar(1), gchar(Y), gchar(5)];
        assert_eq!(split(&raw, false), [(X, vec![gchar(4)])]);
        // A value cut short is not taken.
        assert!(split(&[gchar(IMAGE), gchar(9), b'a'], false).is_empty());
    }
}
