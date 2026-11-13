//! Weapons: the world's weapon files, the default weapons, and the
//! messages that give a player a weapon.
//!
//! A world keeps its weapons in `weapons/`, one file each, in the format
//! GServer-v2 worlds use (`GRAWP001`, then `REALNAME`, `IMAGE` and the
//! script between `SCRIPT` and `SCRIPTEND`; written from the fork's sample
//! data file, as `docs/GSERVER-V2-WORLD.md` is). An account lists the
//! weapons it holds as repeated `WEAPON name` lines.
//!
//! A weapon goes to a client as:
//!
//! - `PLO_DEFAULTWEAPON` (43) with the item id, for the weapons built into
//!   the client (the bow, the bomb and the rest of [`DEFAULT`]);
//! - `PLO_NPCWEAPONADD` (33): `GCHAR` length and the name, then NPC props:
//!   the image (0) and the script (1), the layout core's client reader
//!   takes (`SessionState::weapons`). Only builds that
//!   run the script as GS1 text get it this way. A GS2-only build (5.x,
//!   6.x) is sent nothing until `PLO_NPCWEAPONSCRIPT` (140) has a capture,
//!   which is also what it was sent before weapons were served.
//!
//! `PLI_WEAPONADD` (33), from 1.38 on, is a client asking to keep a
//! weapon: type 0 and a default weapon's item id, or type 1 and the
//! `GINT3` id of the NPC whose `toweapons` the player ran. The server
//! takes an NPC weapon only when that NPC is in the player's level and its
//! script has `toweapons` with that name. A name the world has no file
//! for gets one from the NPC's image and script.
//!
//! `PLI_TOWEAPON` (25) is the same ask from 1.21 to 1.37, which send it
//! for every weapon they hold: the name, then an image and the client's
//! own copy of the script. Only the name is read. The player keeps the
//! weapon when the world has it, or when an NPC in the level gives it
//! with `toweapons` (the world's copy then comes from that NPC); the
//! uploaded script is never kept or passed on.

use crate::npcs::{Script, LEVEL_ERA};
use crate::wire::ids::{PLO_DEFAULTWEAPON, PLO_NPCWEAPONADD};
use crate::wire::{gchar, gshort, latin1, msg};
use seihai_core::protocol::{ClientVersion, Runs};
use seihai_core::scripting::delivery::Delivery;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Weapons built into every client, by name and item id (the chest item
/// indexes, `seihai_core::assets::CHEST_ITEMS`).
pub const DEFAULT: [(&str, u8); 8] = [
    ("bow", 7),
    ("bomb", 8),
    ("superbomb", 12),
    ("fireball", 20),
    ("fireblast", 21),
    ("nukeshot", 22),
    ("joltbomb", 23),
    ("spinattack", 24),
];

/// The item id of a default weapon's name (`bombs` is the bomb too).
pub fn default_id(name: &str) -> Option<u8> {
    let n = name.trim().to_ascii_lowercase();
    let n = if n == "bombs" { "bomb".to_string() } else { n };
    DEFAULT.iter().find(|(d, _)| *d == n).map(|(_, id)| *id)
}

/// The name of a default weapon's item id.
pub fn default_name(id: u8) -> Option<&'static str> {
    DEFAULT.iter().find(|(_, i)| *i == id).map(|(n, _)| *n)
}

/// One weapon of the world.
#[derive(Debug, Clone)]
pub struct Weapon {
    pub name: String,
    pub image: String,
    pub source: String,
    /// The script, compiled once; `None` for a weapon without one.
    pub script: Option<Script>,
}

impl Weapon {
    pub fn new(name: &str, image: &str, source: &str) -> Weapon {
        Weapon {
            name: name.to_string(),
            image: image.to_string(),
            source: source.to_string(),
            script: (!source.trim().is_empty()).then(|| Script::gs1(source, LEVEL_ERA)),
        }
    }

    /// A weapon file. `None` without a `REALNAME`.
    pub fn parse(text: &str) -> Option<Weapon> {
        let mut name = None;
        let mut image = String::new();
        let mut script = Vec::new();
        let mut in_script = false;
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if in_script {
                if line.trim() == "SCRIPTEND" {
                    in_script = false;
                } else {
                    script.push(line);
                }
                continue;
            }
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            match k {
                "REALNAME" => name = Some(v.trim().to_string()),
                "IMAGE" => image = v.trim().to_string(),
                "SCRIPT" => in_script = true,
                _ => {}
            }
        }
        let name = name.filter(|n| !n.is_empty())?;
        let mut source = script.join("\n");
        if !source.is_empty() {
            source.push('\n');
        }
        Some(Weapon::new(&name, &image, &source))
    }

    pub fn to_text(&self) -> String {
        let mut s = format!(
            "GRAWP001\nREALNAME {}\nIMAGE {}\nSCRIPT\n",
            self.name, self.image
        );
        for line in self.source.lines() {
            s.push_str(line);
            s.push('\n');
        }
        s.push_str("SCRIPTEND\n");
        s
    }

    /// `PLO_NPCWEAPONADD` for `to`, or `None` when the build does not run
    /// this weapon's script as GS1 text (or reads no 33 at all).
    pub fn message(&self, to: ClientVersion) -> Option<Vec<u8>> {
        if !to.profile().protocol.plo.contains(PLO_NPCWEAPONADD) {
            return None;
        }
        let text = match &self.script {
            None => None,
            Some(s) => match s.delivery(to) {
                Delivery::Gs1Text(_) => Some(script_wire(&self.source)),
                _ => return None,
            },
        };
        // A weapon without a script follows the same rule: only to builds
        // that would take GS1 text, so GS2 builds still get nothing.
        let scripts = to.profile().scripts;
        if self.script.is_none() && (scripts.gs1 == Runs::No || scripts.gs2 == Runs::Yes) {
            return None;
        }
        let name = latin1(&self.name);
        let mut b = vec![gchar(name.len().min(223) as u8)];
        b.extend_from_slice(&name[..name.len().min(223)]);
        let image = latin1(&self.image);
        b.push(gchar(0));
        b.push(gchar(image.len().min(223) as u8));
        b.extend_from_slice(&image[..image.len().min(223)]);
        if let Some(t) = text.filter(|t| t.len() <= crate::npcs::MAX_SCRIPT) {
            b.push(gchar(1));
            gshort(t.len() as u16, &mut b);
            b.extend(t);
        }
        Some(msg(PLO_NPCWEAPONADD, &b))
    }
}

/// A script as `PLO_NPCWEAPONADD` carries it: Latin-1, lines joined by
/// `§` (the same as an NPC's prop 1).
fn script_wire(s: &str) -> Vec<u8> {
    crate::npcs::script_bytes(s)
}

/// `PLO_DEFAULTWEAPON` for `id`, when `to` reads it.
pub fn default_message(id: u8, to: ClientVersion) -> Option<Vec<u8>> {
    to.profile()
        .protocol
        .plo
        .contains(PLO_DEFAULTWEAPON)
        .then(|| msg(PLO_DEFAULTWEAPON, &[gchar(id)]))
}

/// The weapon `name` from a `toweapons` in `script`, ignoring case.
/// GS1 writes `toweapons name;` with the name as plain text up to the `;`.
pub fn toweapons_name(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = script.to_ascii_lowercase();
    let mut at = 0;
    while let Some(i) = lower[at..].find("toweapons") {
        let start = at + i + "toweapons".len();
        let before = lower[..at + i].chars().next_back();
        at = start;
        if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
            continue;
        }
        if !script[start..].starts_with([' ', '\t']) {
            continue;
        }
        let rest = &script[start..];
        let end = rest.find([';', '\n', '}']).unwrap_or(rest.len());
        let name = rest[..end].trim();
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

/// The file a weapon is saved in: `weapon` and the name, with every byte
/// that is not a letter or digit written as `%` and three decimal digits,
/// as GServer-v2 worlds name them (`weapon%045gr%095movement.txt`).
pub fn file_name(name: &str) -> String {
    let mut s = String::from("weapon");
    for b in latin1(name) {
        if b.is_ascii_alphanumeric() {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:03}"));
        }
    }
    s.push_str(".txt");
    s
}

/// The world's weapons, by lower-case name.
#[derive(Debug, Default)]
pub struct Weapons {
    dir: Option<PathBuf>,
    by_name: BTreeMap<String, Weapon>,
}

impl Weapons {
    /// Every weapon file in `dir` (a missing folder is an empty list).
    pub fn load(dir: &Path) -> Weapons {
        let mut w = Weapons {
            dir: Some(dir.to_path_buf()),
            by_name: BTreeMap::new(),
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return w;
        };
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path
                .extension()
                .is_none_or(|e| !e.eq_ignore_ascii_case("txt"))
            {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let text: String = bytes.iter().map(|&b| b as char).collect();
            match Weapon::parse(&text) {
                Some(weapon) => {
                    if let Some(e) = weapon
                        .script
                        .as_ref()
                        .and_then(|s| s.prepared().compile_errors())
                    {
                        eprintln!("weapon {}: no GS2 bytecode: {e}", weapon.name);
                    }
                    w.by_name.insert(weapon.name.to_ascii_lowercase(), weapon);
                }
                None => eprintln!("{}: not a weapon file (no REALNAME)", path.display()),
            }
        }
        w
    }

    pub fn get(&self, name: &str) -> Option<&Weapon> {
        self.by_name.get(&name.to_ascii_lowercase())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.values().map(|w| w.name.as_str())
    }

    /// Add a weapon and write its file, unless the world has one by that
    /// name already. Returns the world's weapon.
    pub fn add(&mut self, weapon: Weapon) -> &Weapon {
        let key = weapon.name.to_ascii_lowercase();
        if !self.by_name.contains_key(&key) {
            if let Some(dir) = &self.dir {
                let path = dir.join(file_name(&weapon.name));
                let text: Vec<u8> = latin1(&weapon.to_text());
                let saved = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, text));
                if let Err(e) = saved {
                    eprintln!(
                        "weapon {}: could not write {}: {e}",
                        weapon.name,
                        path.display()
                    );
                }
            }
            self.by_name.insert(key.clone(), weapon);
        }
        &self.by_name[&key]
    }
}

/// What `to` is sent for a weapon an account holds: a default weapon by
/// id, or the world's weapon by name. `None` for a name the world does
/// not have, or a weapon the build cannot run.
pub fn give(weapons: &Weapons, name: &str, to: ClientVersion) -> Option<Vec<u8>> {
    match default_id(name) {
        Some(id) => default_message(id, to),
        None => weapons.get(name)?.message(to),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The weapons a client of `v` keeps from `m`, read with core's client.
    fn parse_weapon_add(
        v: ClientVersion,
        m: &[u8],
    ) -> Vec<(String, Option<String>, Option<String>)> {
        let mut s = seihai_core::protocol::SessionState::for_version(v);
        s.apply_body(m);
        s.weapons
            .into_iter()
            .map(|w| (w.name, w.image, w.script))
            .collect()
    }

    const FILE: &str = "GRAWP001\nREALNAME Lantern\nIMAGE lantern.png\nSCRIPT\nif (weaponfired) {\n  say2 light;\n}\nSCRIPTEND\n";

    #[test]
    fn a_weapon_file_reads_and_writes_back() {
        let w = Weapon::parse(FILE).unwrap();
        assert_eq!(
            (w.name.as_str(), w.image.as_str()),
            ("Lantern", "lantern.png")
        );
        assert_eq!(w.source, "if (weaponfired) {\n  say2 light;\n}\n");
        assert_eq!(w.to_text(), FILE);
        assert!(Weapon::parse("GRAWP001\nIMAGE a.png\n").is_none());
    }

    #[test]
    fn gs1_builds_get_name_image_and_script() {
        let w = Weapon::parse(FILE).unwrap();
        for v in [
            ClientVersion::V139,
            ClientVersion::V205,
            ClientVersion::V222,
            ClientVersion::V231,
        ] {
            let m = w.message(v).unwrap_or_else(|| panic!("{v:?}"));
            let got = parse_weapon_add(v, &m)
                .pop()
                .unwrap_or_else(|| panic!("{v:?}"));
            assert_eq!(got.0, "Lantern", "{v:?}");
            assert_eq!(got.1.as_deref(), Some("lantern.png"), "{v:?}");
            assert_eq!(
                got.2.as_deref(),
                Some("if (weaponfired) {\nsay2 light;\n}"),
                "{v:?}"
            );
        }
    }

    #[test]
    fn gs2_only_builds_get_nothing() {
        let w = Weapon::parse(FILE).unwrap();
        for v in [
            ClientVersion::V512,
            ClientVersion::V524R4,
            ClientVersion::V603R7,
        ] {
            assert!(w.message(v).is_none(), "{v:?}");
        }
        // A weapon without a script does not go to them either.
        assert!(Weapon::new("Rock", "rock.png", "")
            .message(ClientVersion::V512)
            .is_none());
    }

    #[test]
    fn default_weapons_go_by_id() {
        assert_eq!(default_id("Bow"), Some(7));
        assert_eq!(default_id("bombs"), Some(8));
        assert_eq!(default_name(24), Some("spinattack"));
        let w = Weapons::default();
        assert_eq!(
            give(&w, "bomb", ClientVersion::V222),
            Some(msg(PLO_DEFAULTWEAPON, &[gchar(8)]))
        );
        assert!(give(&w, "Lantern", ClientVersion::V222).is_none());
    }

    #[test]
    fn toweapons_names_are_found() {
        let s = "if (playertouchsme) {\n  toweapons Fire Rod;\n  hide;\n}\n// mytoweapons x;\n";
        assert_eq!(toweapons_name(s), ["Fire Rod"]);
        assert!(toweapons_name("toweaponsx;").is_empty());
    }

    #[test]
    fn file_names_escape_as_v2_worlds_do() {
        assert_eq!(file_name("-gr_movement"), "weapon%045gr%095movement.txt");
        assert_eq!(file_name("Bow2"), "weaponBow2.txt");
    }

    #[test]
    fn a_new_weapon_is_written_once() {
        let dir = std::env::temp_dir().join(format!("seihai-weapons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut w = Weapons::load(&dir);
        w.add(Weapon::new(
            "Fire Rod",
            "rod.png",
            "if (weaponfired) { shootball; }\n",
        ));
        w.add(Weapon::new("fire rod", "other.png", ""));
        assert_eq!(w.get("FIRE ROD").unwrap().image, "rod.png");
        let again = Weapons::load(&dir);
        assert_eq!(
            again.get("fire rod").unwrap().source,
            "if (weaponfired) { shootball; }\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
