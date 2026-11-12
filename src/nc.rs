//! NPC Control logins (stage 4 slice 6, issue #52).
//!
//! An NC logs in with type 3 and the token `NCL11012`, then speaks plain
//! zlib frames both ways (`login.rs`). It must be a staff account, from an
//! address its `IPRANGE` allows, as for an RC (`rc::admit`). The NPCs it
//! sees are the NPC-server's database NPCs: the NPC-server tells the
//! gserver about them over the link (`NC_NPC`, `PROTOCOL.md`), and the
//! gserver keeps that list for every NC (`npclink::Hub::nc_watch`).
//!
//! Layouts are seihai-rc's (its `docs/PROTOCOL.md`, "NPC Control"):
//!
//! | NC sends | Answer |
//! |---|---|
//! | (login) | 158 `NC_NPCADD` for every NPC: `GINT3` id, then `GCHAR 50` + `GSTRING` name, `GCHAR 51` + `GSTRING` type, `GCHAR 52` + `GSTRING` level |
//! | 103 `NC_NPCGET`: `GINT3` id | 157: the attributes, `name=value` lines |
//! | 106 `NC_NPCSCRIPTGET`: `GINT3` id | 160: `GINT3` id, then the script |
//! | 109 `NC_NPCSCRIPTSET`: `GINT3` id, then the script | none; the NPC restarts, so 159 and 158 follow |
//! | 104 `NC_NPCDELETE`: `GINT3` id | 159 `NC_NPCDELETE`: `GINT3` id |
//! | 115 `NC_WEAPONLISTGET` | 167 with no names: the NPC-server keeps no weapons yet |
//!
//! While the NC is on, 158 and 159 also go out as the NPC-server's list
//! changes. A text with line feeds in it (a script, the attributes) goes
//! out in a raw block (`PLO_RAWDATA` 100), so the line feeds stay inside
//! the one message; a script set may come in one too (`rcfiles::split`).
//!
//! Classes and the rest of the weapon commands are not answered: the
//! NPC-server has neither yet.

use crate::npclink::NcNpc;
use crate::wire::{gchar, gint3, gstring, msg, raw_block};
use seihai_core::protocol::read_gint3;

pub const NC_LOGIN_TYPE: u8 = 3;
pub const NC_TOKEN: &str = "NCL11012";

pub const PLO_NC_NPCATTRIBUTES: u8 = 157;
pub const PLO_NC_NPCADD: u8 = 158;
pub const PLO_NC_NPCDELETE: u8 = 159;
pub const PLO_NC_NPCSCRIPT: u8 = 160;
pub const PLO_NC_WEAPONLISTGET: u8 = 167;

pub const PLI_NC_NPCGET: u8 = 103;
pub const PLI_NC_NPCDELETE: u8 = 104;
pub const PLI_NC_NPCSCRIPTGET: u8 = 106;
pub const PLI_NC_NPCSCRIPTSET: u8 = 109;
pub const PLI_NC_WEAPONLISTGET: u8 = 115;
/// `PLI_PONG` (162): sent empty as a keepalive, as by an RC.
const PLI_PONG: u8 = 162;

/// One NC request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Attributes(u32),
    Script(u32),
    SetScript(u32, Vec<u8>),
    Delete(u32),
    WeaponList,
}

/// Read one NC message (with or without its `\n`). `Err(id)` for a
/// message not served; `Ok(None)` for one taken quietly.
pub fn parse(m: &[u8]) -> Result<Option<Op>, u8> {
    let m = m.strip_suffix(b"\n").unwrap_or(m);
    let Some((&id, rest)) = m.split_first() else {
        return Ok(None);
    };
    let id = id.wrapping_sub(0x20);
    let npc = || read_gint3(rest).map(|n| n as u32).ok_or(id);
    Ok(Some(match id {
        PLI_PONG => return Ok(None),
        PLI_NC_NPCGET => Op::Attributes(npc()?),
        PLI_NC_NPCSCRIPTGET => Op::Script(npc()?),
        PLI_NC_NPCSCRIPTSET => Op::SetScript(npc()?, rest.get(3..).unwrap_or_default().to_vec()),
        PLI_NC_NPCDELETE => Op::Delete(npc()?),
        PLI_NC_WEAPONLISTGET => Op::WeaponList,
        other => return Err(other),
    }))
}

/// 158: one NPC of the list.
pub fn npc_add(id: u32, n: &NcNpc) -> Vec<u8> {
    let mut p = Vec::new();
    gint3(id, &mut p);
    for (prop, v) in [(50, &n.name), (51, &n.kind), (52, &n.level)] {
        p.push(gchar(prop));
        gstring(v, &mut p);
    }
    msg(PLO_NC_NPCADD, &p)
}

/// 159: an NPC gone.
pub fn npc_delete(id: u32) -> Vec<u8> {
    let mut p = Vec::new();
    gint3(id, &mut p);
    msg(PLO_NC_NPCDELETE, &p)
}

/// 157: the attributes.
pub fn attributes(text: &[u8]) -> Vec<u8> {
    text_message(PLO_NC_NPCATTRIBUTES, text)
}

/// 160: an NPC's script.
pub fn script(id: u32, script: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    gint3(id, &mut p);
    p.extend_from_slice(script);
    text_message(PLO_NC_NPCSCRIPT, &p)
}

/// 167 with no names.
pub fn no_weapons() -> Vec<u8> {
    msg(PLO_NC_WEAPONLISTGET, b"")
}

/// A message whose payload may hold line feeds: in a raw block then.
fn text_message(id: u8, payload: &[u8]) -> Vec<u8> {
    if payload.contains(&b'\n') {
        raw_block(id, payload)
    } else {
        msg(id, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g3(n: u32) -> Vec<u8> {
        let mut p = Vec::new();
        gint3(n, &mut p);
        p
    }

    #[test]
    fn requests_read_as_seihai_rc_sends_them() {
        let with = |id: u8, rest: &[u8]| {
            let mut m = vec![gchar(id)];
            m.extend_from_slice(rest);
            m.push(b'\n');
            parse(&m)
        };
        assert_eq!(with(103, &g3(7)), Ok(Some(Op::Attributes(7))));
        assert_eq!(with(106, &g3(300)), Ok(Some(Op::Script(300))));
        let mut set = g3(7);
        set.extend_from_slice(b"message hi;");
        assert_eq!(
            with(109, &set),
            Ok(Some(Op::SetScript(7, b"message hi;".to_vec())))
        );
        assert_eq!(with(104, &g3(7)), Ok(Some(Op::Delete(7))));
        assert_eq!(with(115, b""), Ok(Some(Op::WeaponList)));
        assert_eq!(with(162, b""), Ok(None));
        assert_eq!(with(103, b"!"), Err(103), "no id");
        assert_eq!(with(112, b"Class"), Err(112));
    }

    #[test]
    fn replies() {
        let n = NcNpc {
            name: b"bell".to_vec(),
            kind: b"".to_vec(),
            level: b"start.nw".to_vec(),
        };
        let mut want = vec![gchar(158)];
        want.extend(g3(2));
        want.extend(b"R$bellS ");
        want.extend([gchar(52), gchar(8)]);
        want.extend(b"start.nw\n");
        assert_eq!(npc_add(2, &n), want);
        // One line: a plain message. Two: a raw block, the line feed kept.
        assert_eq!(script(2, b"x;")[0], gchar(160));
        let raw = script(2, b"a;\nb;");
        assert_eq!(raw[0], gchar(100));
        assert!(raw.ends_with(b"a;\nb;\n"), "{raw:?}");
    }
}
