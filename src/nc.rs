//! NPC Control logins (stage 4 slice 6, issue #52).
//!
//! An NC logs in with type 3 and the token `NCL11012`, then speaks plain
//! zlib frames both ways (`login.rs`). It must be a staff account, from an
//! address its `IPRANGE` allows, as for an RC (`rc::admit`). The NPCs it
//! sees are the NPC-server's database NPCs: the NPC-server tells the
//! gserver about them over the link (`NC_NPC`, `PROTOCOL.md`), and the
//! gserver keeps that list for every NC (`npclink::Hub::nc_watch`).
//!
//! The layouts are seihai-rc's (its `docs/PROTOCOL.md`, "NPC Control"),
//! except for the texts. A real NPC Control ("GServer2 NPC Control
//! 2001/07/13", token `NCL15031`, lab 2026-11-12, under Wine on loopback)
//! reads and sends a script or the attributes as a tokenized line list,
//! the same form an RC's server options use (`wire::tokenize`). It does
//! not read raw blocks. Sent raw lines, it split the first line into one
//! word per line.
//!
//! | NC sends | Answer |
//! |---|---|
//! | (login) | 158 `NC_NPCADD` for every NPC: `GINT3` id, then `GCHAR 50` + `GSTRING` name, `GCHAR 51` + `GSTRING` type, `GCHAR 52` + `GSTRING` level |
//! | 103 `NC_NPCGET`: `GINT3` id | 157: the attributes, `name=value` lines, tokenized |
//! | 106 `NC_NPCSCRIPTGET`: `GINT3` id | 160: `GINT3` id, then the script's lines, tokenized |
//! | 109 `NC_NPCSCRIPTSET`: `GINT3` id, then the script's lines, tokenized | none; the NPC stays on the list |
//! | 104 `NC_NPCDELETE`: `GINT3` id | 159 `NC_NPCDELETE`: `GINT3` id |
//! | 105 `NC_NPCRESET`: `GINT3` id | 29: `GINT3` id, and a console line; the NPC-server gets `NC_NPC_RESET` |
//! | 111 `NC_NPCADD`: `name,id,type,scripter,level,x,y` tokenized | none here; the NPC-server gets `NC_NPC_ADD`, and the new NPC comes back as 158 |
//! | 107 `NC_NPCWARP`: `GINT3` id, `GCHAR` x × 2, y × 2, then the level | none; the NPC-server gets `NC_NPC_WARP` |
//! | (login) | 163 `NC_CLASSADD` for every class in `scripts/`: the name |
//! | 112 `NC_CLASSEDIT`: name | 162: `GSTRING` name, then the class's lines, tokenized (empty for a class the world lacks) |
//! | 113 `NC_CLASSADD`: `GSTRING` name, then the lines, tokenized | 163 with the name and a console line; the file `scripts/<name>.txt` is written |
//! | 119 `NC_CLASSDELETE`: name | 188 with the name, when the class existed; the file goes |
//! | 115 `NC_WEAPONLISTGET` | 167: every weapon in `weapons/`, a `GSTRING` each |
//! | 116 `NC_WEAPONGET`: name | 33: `GSTRING` name, `GCHAR 0` + `GSTRING` image, `GCHAR 1` + length + the script, line breaks as `0xA7` |
//! | 117 `NC_WEAPONADD`: `GSTRING` name, `GSTRING` image, script with `0xA7` breaks | a console line and 167; the weapon file is written |
//! | 118 `NC_WEAPONDELETE`: name | a console line and 167; the file goes |
//!
//! While the NC is on, 158 and 159 also go out as the NPC-server's list
//! changes. A `SEND_TO_NC` line goes out as [`console_line`] (`PLO_RC_CHAT`,
//! 74) to every logged-in NC. That is every such session, not a rights check.
//!
//! Classes and weapons are the game side's own (`classes.rs`,
//! `weapons.rs`): the NPC-server is not asked. Players already holding a
//! changed weapon get it on their next login. NPC flags,
//! the local-NPC dump and the level list are not answered yet. The class
//! and weapon layouts are seihai-rc's (`docs/PROTOCOL.md`, from 2004 RC
//! and RC3 captures).

use crate::npclink::NcNpc;
use crate::wire::{detokenize, gchar, gint3, gstring, msg, tokenize};
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
pub const PLI_NC_NPCRESET: u8 = 105;
pub const PLI_NC_NPCSCRIPTGET: u8 = 106;
pub const PLI_NC_NPCWARP: u8 = 107;
pub const PLI_NC_NPCADD: u8 = 111;
pub const PLI_NC_NPCSCRIPTSET: u8 = 109;
pub const PLI_NC_WEAPONLISTGET: u8 = 115;
pub const PLI_NC_CLASSEDIT: u8 = 112;
pub const PLI_NC_CLASSADD: u8 = 113;
pub const PLI_NC_WEAPONGET: u8 = 116;
pub const PLI_NC_WEAPONADD: u8 = 117;
pub const PLI_NC_WEAPONDELETE: u8 = 118;
pub const PLI_NC_CLASSDELETE: u8 = 119;

pub const PLO_NPCWEAPONADD: u8 = 33;
/// To an NC, 29 is "this NPC was reset": `GINT3` id (seihai-rc
/// `docs/PROTOCOL.md`, from the fork's answer).
pub const PLO_NC_NPCRESET: u8 = 29;
pub const PLO_NC_CLASSGET: u8 = 162;
pub const PLO_NC_CLASSADD: u8 = 163;
pub const PLO_NC_CLASSDELETE: u8 = 188;

/// The line break in an NC weapon script (Latin-1 `§`).
const WEAPON_BREAK: u8 = 0xA7;
/// `PLI_PONG` (162): sent empty as a keepalive, as by an RC.
const PLI_PONG: u8 = 162;

/// One NC request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Attributes(u32),
    Script(u32),
    SetScript(u32, Vec<u8>),
    Delete(u32),
    Reset(u32),
    /// An NPC to `level` at (x, y) in half tiles.
    Warp(u32, Vec<u8>, u16, u16),
    /// Add NPC: name, type, scripter, level and (x, y) in half tiles.
    Add {
        name: Vec<u8>,
        kind: Vec<u8>,
        scripter: Vec<u8>,
        level: Vec<u8>,
        x: u16,
        y: u16,
    },
    WeaponList,
    ClassGet(String),
    ClassSet(String, String),
    ClassDelete(String),
    WeaponGet(String),
    WeaponSet {
        name: String,
        image: String,
        script: String,
    },
    WeaponDelete(String),
}

/// A class or weapon request NC makes of the world's stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOp {
    ClassNames,
    ClassGet(String),
    ClassSet(String, String),
    ClassDelete(String),
    WeaponNames,
    WeaponGet(String),
    WeaponSet {
        name: String,
        image: String,
        script: String,
    },
    WeaponDelete(String),
}

/// The world's answer to a [`StoreOp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreAnswer {
    Names(Vec<String>),
    /// A class's script, `None` when there is no such class.
    Class(Option<String>),
    /// A weapon's image and script.
    Weapon(Option<(String, String)>),
    /// Whether a save or delete completed. Deleting a known name whose
    /// backing file is already absent succeeds; other file errors leave
    /// the live entry intact and return false.
    Done(bool),
}

fn text(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

/// A leading `GSTRING`, and the rest.
fn split_gstring(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let (&n, rest) = b.split_first()?;
    rest.split_at_checked(n.checked_sub(0x20)? as usize)
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
        PLI_NC_NPCSCRIPTSET => Op::SetScript(
            npc()?,
            detokenize(rest.get(3..).unwrap_or_default()).join(&b'\n'),
        ),
        PLI_NC_NPCDELETE => Op::Delete(npc()?),
        PLI_NC_NPCRESET => Op::Reset(npc()?),
        // `name,id,type,scripter,level,x,y`, tokenized (seihai-rc
        // `docs/PROTOCOL.md`). The id is not used: the NPC-server numbers
        // its NPCs.
        PLI_NC_NPCADD => {
            let f = detokenize(rest);
            let field = |i: usize| f.get(i).cloned().unwrap_or_default();
            let half = |i: usize| -> u16 {
                let t: f32 = String::from_utf8_lossy(&field(i))
                    .trim()
                    .parse()
                    .unwrap_or(0.0);
                (t * 2.0).round().clamp(0.0, 65535.0) as u16
            };
            if field(0).iter().all(u8::is_ascii_whitespace) {
                return Err(id);
            }
            Op::Add {
                name: field(0),
                kind: field(2),
                scripter: field(3),
                level: field(4),
                x: half(5),
                y: half(6),
            }
        }
        PLI_NC_NPCWARP => match rest.get(3..5) {
            Some(&[x, y]) => Op::Warp(
                npc()?,
                rest[5..].to_vec(),
                u16::from(x.wrapping_sub(0x20)),
                u16::from(y.wrapping_sub(0x20)),
            ),
            _ => return Err(id),
        },
        PLI_NC_WEAPONLISTGET => Op::WeaponList,
        PLI_NC_CLASSEDIT => Op::ClassGet(text(rest)),
        PLI_NC_CLASSDELETE => Op::ClassDelete(text(rest)),
        PLI_NC_WEAPONGET => Op::WeaponGet(text(rest)),
        PLI_NC_WEAPONDELETE => Op::WeaponDelete(text(rest)),
        PLI_NC_CLASSADD => {
            let (name, script) = split_gstring(rest).ok_or(id)?;
            Op::ClassSet(text(name), text(&detokenize(script).join(&b'\n')))
        }
        PLI_NC_WEAPONADD => {
            let (name, rest) = split_gstring(rest).ok_or(id)?;
            let (image, script) = split_gstring(rest).ok_or(id)?;
            let script: Vec<u8> = script
                .iter()
                .map(|&c| if c == WEAPON_BREAK { b'\n' } else { c })
                .collect();
            Op::WeaponSet {
                name: text(name),
                image: text(image),
                script: text(&script),
            }
        }
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

/// 29 to an NC: the NPC was reset.
pub fn npc_reset(id: u32) -> Vec<u8> {
    let mut p = Vec::new();
    gint3(id, &mut p);
    msg(PLO_NC_NPCRESET, &p)
}

/// 159: an NPC gone.
pub fn npc_delete(id: u32) -> Vec<u8> {
    let mut p = Vec::new();
    gint3(id, &mut p);
    msg(PLO_NC_NPCDELETE, &p)
}

/// 157: the attributes.
pub fn attributes(text: &[u8]) -> Vec<u8> {
    msg(PLO_NC_NPCATTRIBUTES, &tokenize(&lines(text)))
}

/// 160: an NPC's script.
pub fn script(id: u32, script: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    gint3(id, &mut p);
    p.extend(tokenize(&lines(script)));
    msg(PLO_NC_NPCSCRIPT, &p)
}

/// A text's lines, without their line ends.
fn lines(text: &[u8]) -> Vec<Vec<u8>> {
    let text = text.strip_suffix(b"\n").unwrap_or(text);
    text.split(|&c| c == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l).to_vec())
        .collect()
}

/// 167: every weapon name, a `GSTRING` each.
pub fn weapon_list<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut p = Vec::new();
    for n in names {
        gstring(&crate::wire::latin1(n), &mut p);
    }
    msg(PLO_NC_WEAPONLISTGET, &p)
}

/// 33 to `NCL11012`: `GSTRING` name, `GCHAR 0` and `GSTRING` image,
/// `GCHAR 1`, a two-byte length and the script with its line breaks as
/// `0xA7`. The length's high byte is not masked to 7 bits, as the 2004
/// RC reads it (seihai-rc `docs/PROTOCOL.md`), so a script is cut at
/// the longest length that byte can hold.
pub fn weapon(name: &str, image: &str, script: &str) -> Vec<u8> {
    const MAX: usize = (0xFF - 0x20) * 128 + 127;
    let mut body: Vec<u8> = crate::wire::latin1(script)
        .into_iter()
        .map(|c| if c == b'\n' { WEAPON_BREAK } else { c })
        .filter(|&c| c != b'\r')
        .collect();
    body.truncate(MAX);
    let mut p = Vec::with_capacity(body.len() + 64);
    gstring(&crate::wire::latin1(name), &mut p);
    p.push(gchar(0));
    gstring(&crate::wire::latin1(image), &mut p);
    p.push(gchar(1));
    p.push(((body.len() >> 7) + 0x20) as u8);
    p.push(((body.len() & 0x7F) + 0x20) as u8);
    p.extend(body);
    msg(PLO_NPCWEAPONADD, &p)
}

/// 162: `GSTRING` name, then the class script tokenized.
pub fn class_get(name: &str, script: &str) -> Vec<u8> {
    let mut p = Vec::new();
    gstring(&crate::wire::latin1(name), &mut p);
    p.extend(tokenize(&lines(&crate::wire::latin1(script))));
    msg(PLO_NC_CLASSGET, &p)
}

/// 163: one class name, the rest of the line.
pub fn class_add(name: &str) -> Vec<u8> {
    msg(PLO_NC_CLASSADD, &crate::wire::latin1(name))
}

/// 188: a class gone, the rest of the line.
pub fn class_delete(name: &str) -> Vec<u8> {
    msg(PLO_NC_CLASSDELETE, &crate::wire::latin1(name))
}

/// The first line an NC gets after its login: `Server: seihai-gserver
/// VERSION`. seihai-rc takes its text profile from it (seihai-rc #56).
pub const SERVER_LINE: &str = concat!("Server: seihai-gserver ", env!("CARGO_PKG_VERSION"));

/// A console line for a logged-in NPC Control session.
///
/// An NC client shows this as `PLO_RC_CHAT` (74): the same packet Remote
/// Control uses for a console line. Ids that are not NPC-list replies are
/// read on that console, and 74 is the line itself. The text is the rest
/// of the message. An empty text is still a line.
pub fn console_line(text: &[u8]) -> Vec<u8> {
    msg(crate::rc::PLO_RC_CHAT, text)
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
        // As the real 2001 NC sent it (lab, 2026-11-12): tokenized lines,
        // quoted where they hold a space or a comma.
        let mut set = g3(1);
        set.extend_from_slice(b"\"// two words\",\"message hello, world\",oneword");
        assert_eq!(
            with(109, &set),
            Ok(Some(Op::SetScript(
                1,
                b"// two words\nmessage hello, world\noneword".to_vec()
            )))
        );
        assert_eq!(with(104, &g3(7)), Ok(Some(Op::Delete(7))));
        assert_eq!(with(105, &g3(7)), Ok(Some(Op::Reset(7))));
        let mut warp = g3(7);
        warp.extend_from_slice(b"\x5d\x48house.nw");
        assert_eq!(
            with(107, &warp),
            Ok(Some(Op::Warp(7, b"house.nw".to_vec(), 61, 40)))
        );
        assert_eq!(with(107, &g3(7)), Err(107), "no position");
        assert_eq!(
            with(111, b"Guard,0,OBJECT,boss,start.nw,30.5,20"),
            Ok(Some(Op::Add {
                name: b"Guard".to_vec(),
                kind: b"OBJECT".to_vec(),
                scripter: b"boss".to_vec(),
                level: b"start.nw".to_vec(),
                x: 61,
                y: 40
            }))
        );
        assert_eq!(with(111, b""), Err(111), "no name");
        assert_eq!(with(115, b""), Ok(Some(Op::WeaponList)));
        assert_eq!(with(162, b""), Ok(None));
        assert_eq!(with(103, b"!"), Err(103), "no id");
        assert_eq!(with(112, b"Class"), Ok(Some(Op::ClassGet("Class".into()))));
        let mut add = b"%Class".to_vec();
        add.extend_from_slice(b"\"a b\",c");
        assert_eq!(
            with(113, &add),
            Ok(Some(Op::ClassSet("Class".into(), "a b\nc".into())))
        );
        assert_eq!(with(113, b"%Cl"), Err(113), "short name");
        let mut w = b"#Bow".to_vec();
        w.extend_from_slice(b"'bow.png");
        w.extend_from_slice(b"a;\xa7b;\xa7");
        assert_eq!(
            with(117, &w),
            Ok(Some(Op::WeaponSet {
                name: "Bow".into(),
                image: "bow.png".into(),
                script: "a;\nb;\n".into()
            }))
        );
        assert_eq!(with(118, b"Bow"), Ok(Some(Op::WeaponDelete("Bow".into()))));
        assert_eq!(
            with(119, b"Class"),
            Ok(Some(Op::ClassDelete("Class".into())))
        );
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
        // Lines tokenized, never a raw block.
        let mut want = vec![gchar(160)];
        want.extend(g3(2));
        want.extend(b"\"a, b;\",\"say \"\"hi\"\";\"\n");
        assert_eq!(script(2, b"a, b;\r\nsay \"hi\";\n"), want);
        // A weapon: name, 0 + image, 1 + two-byte length, `§` breaks.
        assert_eq!(
            weapon("Bow", "b.png", "a;\nb;\n"),
            b"\x41#Bow\x20%b.png\x21\x20\x26a;\xa7b;\xa7\n"
        );
        let mut list = vec![gchar(167)];
        list.extend(b"#Bow$Lamp\n");
        assert_eq!(weapon_list(["Bow", "Lamp"]), list);
        assert_eq!(class_get("C", "x\ny\n"), b"\xc2!C\"x\",\"y\"\n");
        assert_eq!(class_delete("C"), b"\xdcC\n");
        assert_eq!(
            attributes(b"name=bell\nx=5"),
            b"\xbd\"name=bell\",\"x=5\"\n"
        );
    }
}
