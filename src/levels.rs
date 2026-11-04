//! Level files (stage 2).
//!
//! A level is read from `<world>/levels/` by `seihai_core::assets::Level`,
//! the reader the client uses, so there is no second reader here. It reads
//! by magic, not extension: `GLEVNW01` (`.nw`), `GR-V1` (`.graal`) and
//! `Z3-V1` (`.zelda`, and the many `.graal` files that hold one).
//!
//! The world sends a level as `PLO_LEVELNAME`, `PLO_RAWDATA` +
//! `PLO_BOARDPACKET`, `PLO_LEVELLINK` (1), `PLO_LEVELSIGN` (5),
//! `PLO_LEVELMODTIME`, `PLO_LEVELCHEST` (4) and `PLO_SETACTIVELEVEL`, in the
//! order `burst.rs` uses. The writers for links, signs and chests are here,
//! each checked against the client's reader.
//!
//! Level names come from clients and settings: [`valid_level_name`] checks
//! them like account names (no path separators, no `..`) before they name a
//! file, and the file is looked up without regard to case.
//!
//! To do: gmaps (`GRMAP001`) and levels in subfolders (stage 2.5), and
//! reloading a level whose file changed (`LEVEL_RELOADED`, stage 5).

use crate::wire::{gchar, latin1};
use seihai_core::assets::{self, CHEST_ITEMS};
use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Larger level files are refused. Real `.nw` files with many NPCs stay
/// well under 1 MB.
pub const MAX_LEVEL_FILE: u64 = 4 << 20;

/// A level as the world holds it.
#[derive(Debug, Clone)]
pub struct Level {
    /// The name the client is told: the file's own name when it was read.
    pub name: String,
    /// 64 x 64 tiles, row by row.
    pub board: Vec<u16>,
    pub links: Vec<assets::Link>,
    pub signs: Vec<assets::Sign>,
    pub chests: Vec<assets::Chest>,
    /// The NPCs of the file (`npcs.rs` gives them ids and sends them).
    pub npcs: Vec<assets::Npc>,
    /// Unix time of the file, for `PLO_LEVELMODTIME` and cached-level checks.
    pub mod_time: u64,
}

impl Level {
    /// The stage 1 level, for a world without the level's file: one tile
    /// everywhere, nothing else.
    pub fn blank(name: &str, tile: u16, mod_time: u64) -> Self {
        Level {
            name: name.to_string(),
            board: vec![tile; 64 * 64],
            links: Vec::new(),
            signs: Vec::new(),
            chests: Vec::new(),
            npcs: Vec::new(),
            mod_time,
        }
    }

    /// Read `name` from `dir`. The name must pass [`valid_level_name`]; the
    /// file is found without regard to case.
    pub fn load(dir: &Path, name: &str) -> io::Result<Self> {
        if !valid_level_name(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("bad level name {name:?}"),
            ));
        }
        let file = find_file(dir, name)?;
        let path = dir.join(&file);
        let meta = std::fs::metadata(&path)?;
        if !meta.is_file() || meta.len() > MAX_LEVEL_FILE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{file}: not a level file"),
            ));
        }
        let data = std::fs::read(&path)?;
        let level = assets::Level::from_bytes(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{file}: {e}")))?;
        let mod_time = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        Ok(Level {
            name: file,
            board: level.tile_ids(),
            links: level.links,
            signs: level.signs,
            chests: level.chests,
            npcs: level.npcs,
            mod_time,
        })
    }
}

/// A level name a client may use: a plain file name in the levels folder.
/// No path separators, no `..`, no leading dot, no control characters, and
/// at most 120 bytes.
pub fn valid_level_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && !name.starts_with('.')
        && !name.contains("..")
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
}

/// The file in `dir` named `name`, compared without case, under its name
/// as the folder lists it. An exact match wins. The folder is always
/// listed, so the name is the same on case-blind file systems (macOS,
/// Windows) as on Linux.
fn find_file(dir: &Path, name: &str) -> io::Result<String> {
    let mut found = None;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(n) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !n.eq_ignore_ascii_case(name) || !entry.path().is_file() {
            continue;
        }
        if n == name {
            return Ok(n);
        }
        found.get_or_insert(n);
    }
    found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no level {name}")))
}

/// `PLO_LEVELLINK` (1): `dest x y w h newx newy`. A link that keeps the
/// player's coordinate says `playerx` or `playery`, with its offset if the
/// file had one.
pub fn link_line(l: &assets::Link) -> Vec<u8> {
    let arrival = |v: Option<f32>, word: &str, offset: f32| match v {
        Some(v) => num(v),
        None if offset != 0.0 => format!(
            "{word}{}{}",
            if offset > 0.0 { "+" } else { "" },
            num(offset)
        ),
        None => word.to_string(),
    };
    latin1(&format!(
        "{} {} {} {} {} {} {}",
        l.dest,
        l.x,
        l.y,
        l.width,
        l.height,
        arrival(l.new_x, "playerx", l.x_offset),
        arrival(l.new_y, "playery", l.y_offset)
    ))
}

/// A number as level files write it: no `.0` on a whole number.
fn num(v: f32) -> String {
    if v.fract() == 0.0 && v.abs() < 1e6 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// `PLO_LEVELCHEST` (4) for a closed chest: `c 0, c x, c y, c item, c sign`.
/// Sign index -1 (no sign) goes as `0xFF`, as the beta4 lab gserver sends it
/// to 2.00, 2.05 and 2.22 (Seihai `chest-drawing.md`). `None` for an item
/// name the clients have no index for. A chest the account has opened goes
/// as the short record: opened flag, x, y.
pub fn chest_record(c: &assets::Chest) -> Option<Vec<u8>> {
    let item = chest_item(c)?;
    let pos = |v: i32| gchar(v.clamp(0, 63) as u8);
    if c.opened {
        return Some(vec![gchar(1), pos(c.x), pos(c.y)]);
    }
    let sign = if c.sign_index < 0 {
        0xFF
    } else {
        gchar(c.sign_index.min(223) as u8)
    };
    Some(vec![gchar(0), pos(c.x), pos(c.y), gchar(item), sign])
}

/// A chest's item as its index in the clients' item table.
pub fn chest_item(c: &assets::Chest) -> Option<u8> {
    CHEST_ITEMS
        .iter()
        .position(|i| i.eq_ignore_ascii_case(c.item.trim()))
        .map(|i| i as u8)
}

/// `PLO_LEVELSIGN` (5): `c x, c y`, then the text in the sign alphabet.
pub fn sign_record(s: &assets::Sign) -> Vec<u8> {
    let pos = |v: i32| gchar(v.clamp(0, 63) as u8);
    let mut out = vec![pos(s.x), pos(s.y)];
    out.extend(sign_text(&s.text));
    out
}

/// The sign alphabet: a byte on the wire is `index + 32` into this table.
/// `#` slots are glyph codes (Seihai `formats/sign-text.md`, and
/// `seihai_core::assets::decode_sign_text`, which the tests check this
/// against).
const SIGN_ALPHABET: &[u8; 97] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!?-.,#>()#####\"####':/~&### <####;\n";

/// Glyph tags in sign text and their codes. Two-letter codes first.
const SIGN_GLYPHS: [(&str, &[u8]); 14] = [
    ("#h", &[74, 75]),
    ("#4", &[87, 88]),
    ("#A", &[91]),
    ("#B", &[92]),
    ("#X", &[93]),
    ("#Y", &[94]),
    ("#u", &[77]),
    ("#d", &[78]),
    ("#l", &[79]),
    ("#r", &[80]),
    ("#x", &[71]),
    ("#y", &[72]),
    ("#z", &[73]),
    ("#.", &[67]),
];

/// A `#` that is not a glyph: the slot the clients read as a plain `#`.
const PLAIN_HASH: u8 = 86;

/// Sign text in the sign alphabet. Glyph tags (`#A`, `#h`, ...) become their
/// codes; a character outside the alphabet goes as `#K(n)`; CR is dropped,
/// as the clients drop it; past Latin-1 it is `?`.
pub fn sign_text(text: &str) -> Vec<u8> {
    let code = |c: u8| -> Option<u8> {
        if c == b'#' {
            return Some(PLAIN_HASH);
        }
        SIGN_ALPHABET.iter().position(|&a| a == c).map(|i| i as u8)
    };
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some((tag, codes)) = SIGN_GLYPHS.iter().find(|(t, _)| rest.starts_with(t)) {
            out.extend(codes.iter().map(|&k| k + 32));
            rest = &rest[tag.len()..];
            continue;
        }
        rest = &rest[c.len_utf8()..];
        let n = c as u32;
        if c == '\r' {
            continue;
        }
        let byte = if n < 0x100 { n as u8 } else { b'?' };
        match code(byte) {
            Some(k) => out.push(k + 32),
            None => {
                for b in format!("#K({byte})").bytes() {
                    out.push(code(b).expect("escape characters are in the alphabet") + 32);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::assets::decode_sign_text;

    #[test]
    fn level_names_are_checked() {
        for ok in [
            "onlinestartlocal.nw",
            "a.graal",
            "Level 2.zelda",
            "x-y_z.nw",
        ] {
            assert!(valid_level_name(ok), "{ok}");
        }
        for bad in [
            "",
            "../x.nw",
            "a/b.nw",
            "a\\b.nw",
            "..",
            ".hidden.nw",
            "c:x.nw",
            "a..nw",
            "a\nb.nw",
        ] {
            assert!(!valid_level_name(bad), "{bad:?}");
        }
        assert!(!valid_level_name(&"a".repeat(121)));
    }

    /// The table agrees with the client's decoder on every code that is not
    /// a glyph.
    #[test]
    fn sign_alphabet_matches_the_client() {
        let glyph: Vec<u8> = SIGN_GLYPHS
            .iter()
            .flat_map(|(_, c)| c.iter().copied())
            .collect();
        for (i, &ch) in SIGN_ALPHABET.iter().enumerate() {
            let i = i as u8;
            if glyph.contains(&i) {
                continue;
            }
            assert_eq!(
                decode_sign_text(&[i + 32]),
                (ch as char).to_string(),
                "code {i}"
            );
        }
        assert_eq!(decode_sign_text(&[PLAIN_HASH + 32]), "#");
    }

    #[test]
    fn sign_text_round_trips_through_the_client() {
        for text in [
            "Welcome to the lab!",
            "Line one\nline two; (3/4) ~ & 'q' \"w\"",
            "Press #A to read, #B to close. #h #4 #u#d#l#r #x#y#z #.",
            "A plain # and 100% {braces} [x] é",
        ] {
            assert_eq!(decode_sign_text(&sign_text(text)), text, "{text:?}");
        }
        // CR goes; a character past Latin-1 is `?`.
        assert_eq!(decode_sign_text(&sign_text("a\r\nb")), "a\nb");
        assert_eq!(decode_sign_text(&sign_text("日")), "?");
    }

    #[test]
    fn links_signs_and_chests_read_back_in_the_client() {
        let lvl = assets::Level::parse(
            "GLEVNW01\n\
             LINK next.nw 0 10 1 4 62 playery\n\
             LINK house.nw 30 0 2 1 30.5 60\n\
             LINK side.nw 63 0 1 64 playerx-15 playery+10\n\
             SIGN 20 21\nHello #A!\nSIGNEND\n\
             CHEST 30 36 greenrupee -1\n\
             CHEST 10 12 bow 0\n\
             CHEST 1 2 notanitem 0\n",
        )
        .unwrap();
        let lines: Vec<String> = lvl
            .links
            .iter()
            .map(|l| String::from_utf8(link_line(l)).unwrap())
            .collect();
        assert_eq!(lines[0], "next.nw 0 10 1 4 62 playery");
        assert_eq!(lines[1], "house.nw 30 0 2 1 30.5 60");
        let mut back = assets::Level::from_board(&[0; 4096]);
        back.add_server_links(&lines);
        for (a, b) in lvl.links.iter().zip(&back.links) {
            assert_eq!(
                (&a.dest, a.x, a.y, a.width, a.height, a.new_x, a.new_y),
                (&b.dest, b.x, b.y, b.width, b.height, b.new_x, b.new_y)
            );
            assert_eq!((a.x_offset, a.y_offset), (b.x_offset, b.y_offset));
        }
        assert_eq!(back.links.len(), 3);

        back.add_server_signs(&[sign_record(&lvl.signs[0])]);
        assert_eq!(
            (
                back.signs[0].x,
                back.signs[0].y,
                back.signs[0].text.as_str()
            ),
            (20, 21, "Hello #A!")
        );

        // The beta4 lab bytes for `CHEST 30 36 greenrupee -1`.
        assert_eq!(
            chest_record(&lvl.chests[0]).unwrap(),
            [0x20, 0x3e, 0x44, 0x20, 0xff]
        );
        let records: Vec<Vec<u8>> = lvl.chests.iter().filter_map(chest_record).collect();
        assert_eq!(records.len(), 2, "the unknown item is left out");
        back.add_server_chests(&records);
        assert_eq!(back.chests[1].item, "bow");
        assert_eq!(
            (
                back.chests[1].x,
                back.chests[1].y,
                back.chests[1].sign_index
            ),
            (10, 12, 0)
        );
        assert!(!back.chests[1].opened);
    }

    #[test]
    fn load_finds_the_file_without_case_and_refuses_bad_names() {
        let dir =
            std::env::temp_dir().join(format!("seihai-gserver-levels-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let board = "H/".repeat(64);
        let text = format!("GLEVNW01\nBOARD 0 0 64 0 {board}\nLINK b.nw 0 0 1 1 30 30\n");
        std::fs::write(dir.join("Start.nw"), text).unwrap();
        std::fs::write(dir.join("junk.nw"), "not a level").unwrap();

        let l = Level::load(&dir, "start.nw").unwrap();
        assert_eq!(l.name, "Start.nw");
        let expect = assets::Level::parse(&std::fs::read_to_string(dir.join("Start.nw")).unwrap())
            .unwrap()
            .tile_ids();
        assert_eq!(l.board, expect);
        assert_ne!(l.board[0], l.board[64], "row 0 is set, row 1 is not");
        assert_eq!(l.links.len(), 1);
        assert!(l.mod_time > 0);
        assert_eq!(
            Level::load(&dir, "missing.nw").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            Level::load(&dir, "junk.nw").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            Level::load(&dir, "../Start.nw").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
