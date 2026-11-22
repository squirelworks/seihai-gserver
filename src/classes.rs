//! Classes: the world's class script files, and the answer to
//! `PLI_UPDATECLASS`.
//!
//! A world keeps its classes in `scripts/`, one `.txt` file each: the file
//! stem is the class name and the file body is the script text. That folder
//! layout is what GServer-v2 sample servers ship (`scripts/` next to
//! `weapons/` and `npcs/`); nothing from the fork's code is used here.
//!
//! A client that runs `this.join("name")` and does not already hold the
//! class asks with `PLI_UPDATECLASS` (161): a `GINT5` CRC of the body it
//! has stored (0 when none), then the class name. Builds that still read
//! `PLO_UNKNOWN133` (133) behind `PLO_RAWDATA` (3.153 to 5.12, including
//! 4.20r8) get the bytecode in 133: `GCHAR` length and the name, then the
//! compiled class script whole, the compiler's `\n` included. A CRC that
//! already matches is not answered. A name the world does not have gets
//! 133 with the name only (an empty class), so `join` can still finish.
//! Builds that dropped 133 (5.24 on) get nothing here; their 197 path is
//! not implemented yet.
//!
//! Behaviour and layout: Graal_analysis `protocol/class-bytecode-v4.md`
//! and `protocol/unknown-ids-2026-11-06.md` (binary reads). Not a port of
//! GServer-v2 patches.

use crate::npcs::{Script, LEVEL_ERA};
use crate::profile::ClientProfile;
use crate::wire::ids::PLO_UNKNOWN133;
use crate::wire::{gchar, latin1, raw_block};
use seihai_core::scripting::delivery::Delivery;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One class of the world.
#[derive(Debug, Clone)]
pub struct Class {
    pub name: String,
    pub source: String,
    /// The script, compiled once; `None` for an empty file.
    pub script: Option<Script>,
}

impl Class {
    pub fn new(name: &str, source: &str) -> Class {
        Class {
            name: name.to_string(),
            source: source.to_string(),
            script: (!source.trim().is_empty()).then(|| Script::gs1(source, LEVEL_ERA)),
        }
    }

    /// `PLO_UNKNOWN133` behind `PLO_RAWDATA` for `to`, when that build
    /// reads 133. `None` when it does not. The bytecode is the compiled
    /// file whole when there is one; otherwise the name alone (empty
    /// class).
    pub fn bytecode_message(&self, to: impl Into<ClientProfile>) -> Option<Vec<u8>> {
        let to = to.into();
        if !reads_class(&to) {
            return None;
        }
        let name = self.wire_name();
        let mut b = vec![gchar(name.len() as u8)];
        b.extend_from_slice(&name);
        if let Some(Delivery::Gs2Bytecode(code)) = self.script.as_ref().map(|s| s.delivery(to)) {
            b.extend_from_slice(code);
        }
        Some(raw_block(PLO_UNKNOWN133, &b))
    }

    /// Zlib CRC-32 of the bytecode that would go in 133, or 0 for an
    /// empty class / a script that has no GS2 form.
    pub fn checksum(&self, to: impl Into<ClientProfile>) -> u32 {
        let to = to.into();
        match self.script.as_ref().map(|s| s.delivery(to)) {
            Some(Delivery::Gs2Bytecode(code)) => crc32(code),
            _ => 0,
        }
    }

    fn wire_name(&self) -> Vec<u8> {
        let mut name = latin1(&self.name);
        name.truncate(223);
        name
    }
}

/// The build reads class bytecode as 133 behind `PLO_RAWDATA`.
fn reads_class(to: &ClientProfile) -> bool {
    to.reads_classic_class_bytecode()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(bytes);
    c.sum()
}

/// An empty-class 133 for a name the world does not have, when `to` reads
/// it.
pub fn empty_message(name: &str, to: impl Into<ClientProfile>) -> Option<Vec<u8>> {
    Class::new(name, "").bytecode_message(to)
}

/// A class name NPC Control may write: printable ASCII, no path
/// separators, not starting with a dot, at most 64 bytes.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name.bytes().all(|b| {
            b.is_ascii_graphic()
                && !matches!(
                    b,
                    b'/' | b'\\' | b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|'
                )
        })
}

/// The world's classes, by lower-case name.
#[derive(Debug, Default)]
pub struct Classes {
    /// `scripts/`, where NPC Control's changes are written.
    dir: Option<PathBuf>,
    by_name: BTreeMap<String, Class>,
    /// Preserve the actual loaded path, including the extension's case.
    files: BTreeMap<String, PathBuf>,
}

impl Classes {
    /// Every `.txt` in `dir` (a missing folder is an empty list). The file
    /// stem is the class name; the body is the script.
    pub fn load(dir: &Path) -> Classes {
        let mut c = Classes {
            dir: Some(dir.to_path_buf()),
            ..Classes::default()
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return c;
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
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem.is_empty() {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let text: String = bytes.iter().map(|&b| b as char).collect();
            let class = Class::new(stem, &text);
            if let Some(e) = class
                .script
                .as_ref()
                .and_then(|s| s.prepared().compile_errors())
            {
                eprintln!("class {}: no GS2 bytecode: {e}", class.name);
            }
            let key = class.name.to_ascii_lowercase();
            c.files.insert(key.clone(), path);
            c.by_name.insert(key, class);
        }
        c
    }

    pub fn get(&self, name: &str) -> Option<&Class> {
        self.by_name.get(&name.to_ascii_lowercase())
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.values().map(|c| c.name.as_str())
    }

    /// NPC Control's class save: the class gets `source` and its file is
    /// written (`scripts/<name>.txt`). An existing class keeps its file
    /// name's case. `Err` for a name that is not a plain file name, or a
    /// file that cannot be written.
    pub fn set(&mut self, name: &str, source: &str) -> std::io::Result<()> {
        let name = self
            .get(name)
            .map_or_else(|| name.to_string(), |c| c.name.clone());
        let Some(dir) = self.dir.as_ref().filter(|_| valid_name(&name)) else {
            return Err(std::io::Error::other("not a class name"));
        };
        std::fs::create_dir_all(dir)?;
        let key = name.to_ascii_lowercase();
        let path = self
            .files
            .get(&key)
            .cloned()
            .unwrap_or_else(|| dir.join(format!("{name}.txt")));
        std::fs::write(&path, latin1(source))?;
        self.files.insert(key.clone(), path);
        self.by_name.insert(key, Class::new(&name, source));
        Ok(())
    }

    /// NPC Control's class delete: the class and its file go. False when
    /// the world had no such class or its backing file could not be
    /// removed. An already absent file counts as deleted.
    pub fn remove(&mut self, name: &str) -> bool {
        let key = name.to_ascii_lowercase();
        let Some(class) = self.by_name.get(&key) else {
            return false;
        };
        if let Some(dir) = &self.dir {
            let path = self
                .files
                .get(&key)
                .cloned()
                .unwrap_or_else(|| dir.join(format!("{}.txt", class.name)));
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "class {}: could not remove {}: {e}",
                        class.name,
                        path.display()
                    );
                    return false;
                }
            }
        }
        self.by_name.remove(&key);
        self.files.remove(&key);
        true
    }

    /// Answer `PLI_UPDATECLASS`: `crc` is the client's `GINT5`, `name` the
    /// rest. `None` when the build reads no 133, or when its CRC already
    /// matches.
    pub fn answer(&self, name: &str, crc: u32, to: impl Into<ClientProfile>) -> Option<Vec<u8>> {
        let to = to.into();
        if !reads_class(&to) {
            return None;
        }
        match self.get(name) {
            Some(class) => {
                if class.checksum(to) == crc {
                    None
                } else {
                    class.bytecode_message(to)
                }
            }
            None => empty_message(name, to),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::gint5;
    use seihai_core::protocol::ClientVersion;

    const SOURCE: &str = "if (created) {\n  setstring client.classok,1;\n}\n";

    #[test]
    fn save_and_delete_use_the_loaded_extension_case() {
        let dir =
            std::env::temp_dir().join(format!("seihai-class-path-case-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Fixture.TXT");
        std::fs::write(&path, SOURCE).unwrap();
        let mut classes = Classes::load(&dir);
        let replacement = "if (created) { this.on = 1; }";
        classes.set("fixture", replacement).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), replacement);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(
            Classes::load(&dir).get("FIXTURE").unwrap().source,
            replacement
        );
        assert!(classes.remove("fIxTuRe"));
        assert!(!path.exists());
        assert!(Classes::load(&dir).get("fixture").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_gs2_build_gets_133_with_the_bytecode() {
        let class = Class::new("fixture", SOURCE);
        let code = class.script.as_ref().unwrap().bytecode().unwrap();
        assert!(code.ends_with(b"\n"));
        for v in [
            ClientVersion::V404R2,
            ClientVersion::V420R6,
            ClientVersion::V420R8,
        ] {
            let b = class.bytecode_message(v).unwrap();
            let mut body = vec![gchar(7)];
            body.extend_from_slice(b"fixture");
            body.extend_from_slice(code);
            assert_eq!(b, raw_block(PLO_UNKNOWN133, &body), "{v:?}");
            assert_eq!(class.checksum(v), crc32(code));
        }
        assert!(class.bytecode_message(ClientVersion::V222).is_none());
        assert!(class.bytecode_message(ClientVersion::V524R4).is_none());
    }

    #[test]
    fn a_matching_crc_is_not_answered() {
        let mut classes = Classes::default();
        let class = Class::new("fixture", SOURCE);
        let crc = class.checksum(ClientVersion::V420R8);
        classes.by_name.insert("fixture".into(), class);
        assert!(classes
            .answer("fixture", crc, ClientVersion::V420R8)
            .is_none());
        let m = classes.answer("fixture", 0, ClientVersion::V420R8).unwrap();
        assert_eq!(
            m,
            classes
                .get("fixture")
                .unwrap()
                .bytecode_message(ClientVersion::V420R8)
                .unwrap()
        );
    }

    #[test]
    fn a_missing_class_gets_an_empty_133() {
        let classes = Classes::default();
        let m = classes.answer("missing", 0, ClientVersion::V420R8).unwrap();
        let mut body = vec![gchar(7)];
        body.extend_from_slice(b"missing");
        assert_eq!(m, raw_block(PLO_UNKNOWN133, &body));
        assert!(classes
            .answer("missing", 0, ClientVersion::V524R4)
            .is_none());
    }

    #[test]
    fn load_reads_scripts_by_stem() {
        let dir =
            std::env::temp_dir().join(format!("seihai-gserver-classes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fixture.txt"), SOURCE).unwrap();
        let classes = Classes::load(&dir);
        assert!(classes.get("fixture").is_some());
        assert!(classes.get("FIXTURE").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_ask_crc_is_a_gint5() {
        let mut h = Vec::new();
        gint5(0, &mut h);
        assert_eq!(h, [0x20, 0x20, 0x20, 0x20, 0x20]);
    }
}
