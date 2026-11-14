//! File serving (stage 2.7): `PLI_WANTFILE` (23) and `PLI_UPDATEFILE` (34)
//! answered from the world folder.
//!
//! Where files come from: everything under `<world>/files/` (any depth),
//! and the `.gmap` files in `<world>/levels/` (a client needs the grid to
//! play a gmap). Levels themselves are not served: real clients get levels
//! as board packets, never as files. Names are looked up without regard to
//! case; a name with a path separator, `..`, a drive colon or a control
//! character is refused.
//!
//! The reply, by the client's build (`profile.files`, Seihai
//! `documentation/file-cache.md`):
//!
//! - `PLO_FILE` (102) behind `PLO_RAWDATA` (100): from 2.1 on a GINT5 mod
//!   time (the file's, in Unix seconds), then `GCHAR` name length, the name,
//!   the data and a closing `\n`; before 2.1 no time and no `\n`.
//! - Over 64 KB, where the build has `files.large_files` (2.14 on):
//!   `PLO_LARGEFILESTART` (68) name, `PLO_LARGEFILESIZE` (84) GINT5 size
//!   where `files.large_file_size` is set (2.3 on), the data in `PLO_FILE`
//!   chunks, `PLO_LARGEFILEEND` (69) name.
//! - Without large files, one `PLO_FILE` up to the GINT3 limit of the raw
//!   data header; a bigger file gets `PLO_FILESENDFAILED` (30).
//!
//! A file's bytes go out in frames of at most [`CHUNK`] bytes so no frame
//! outgrows the `u16` length; a raw-data message may run across frames, as
//! the gen-2 board does after its header frame.
//!
//! `UPDATEFILE` carries the client copy's GINT5 time: a newer file on the
//! server is sent, otherwise `PLO_FILEUPTODATE` (45), or before 2.1, which
//! has no 45, `PLO_FILESENDFAILED`. A name the server does not have also
//! gets "up to date" (as in stage 1), so the client keeps its copy.
//!
//! A `.gpake` is sent only with its key from `config/gpakkeys.txt` first
//! (`gpak.rs`). A package the build does not load, or an encrypted package
//! with no key, is `PLO_FILESENDFAILED`. `gpakkeys.txt` is never served.

use crate::profile::ClientProfile;
use crate::wire::{gint3, gint5, ids::*, latin1, msg};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// The most file bytes in one frame body.
pub const CHUNK: usize = 32_000;
/// Files over this go as large files where the build has them.
pub const LARGE: usize = 64 * 1024;
/// The biggest raw-data message (`GINT3`).
const RAW_MAX: usize = (1 << 21) - 1;
/// The biggest file served; anything bigger counts as not there.
pub const MAX_FILE: u64 = 16 * 1024 * 1024;
/// How often a miss may rescan the folders.
const RESCAN: Duration = Duration::from_secs(2);

/// The files a world serves, by lower-case name.
pub struct FileStore {
    files_dir: PathBuf,
    levels_dir: PathBuf,
    /// `config/gpakkeys.txt`. Read when a `.gpake` is sent. Not indexed.
    keys_path: PathBuf,
    index: Mutex<Index>,
}

struct Index {
    by_name: HashMap<String, PathBuf>,
    scanned: Option<Instant>,
}

/// A file found for a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The name as the client asked for it.
    pub name: String,
    pub data: Vec<u8>,
    /// Last write, Unix seconds.
    pub mod_time: u64,
}

/// Whether a client's file name may be looked up.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 223
        && !name.starts_with('.')
        && !name.contains("..")
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
}

impl FileStore {
    pub fn new(world: &Path) -> Self {
        FileStore {
            files_dir: world.join("files"),
            levels_dir: world.join("levels"),
            keys_path: world.join("config").join("gpakkeys.txt"),
            index: Mutex::new(Index {
                by_name: HashMap::new(),
                scanned: None,
            }),
        }
    }

    /// The DES key for `name` from `config/gpakkeys.txt`, if that file has one.
    pub fn package_key(&self, name: &str) -> Option<[u8; 8]> {
        let text = std::fs::read_to_string(&self.keys_path).unwrap_or_default();
        crate::gpak::key_in(&text, name)
    }

    /// The file called `name`, if the world has it. A name not in the
    /// index rescans the folders, at most every [`RESCAN`], so files put in
    /// while the server runs are found.
    pub fn find(&self, name: &str) -> Option<Found> {
        if !valid_name(name) {
            return None;
        }
        let key = name.to_ascii_lowercase();
        let path = {
            let mut ix = self.index.lock().unwrap();
            let stale = ix.scanned.is_none_or(|t| t.elapsed() >= RESCAN);
            if !ix.by_name.contains_key(&key) && stale {
                ix.by_name = self.scan();
                ix.scanned = Some(Instant::now());
            }
            ix.by_name.get(&key)?.clone()
        };
        let meta = std::fs::metadata(&path).ok()?;
        if meta.len() > MAX_FILE {
            return None;
        }
        let mod_time = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        let data = std::fs::read(&path).ok()?;
        Some(Found {
            name: name.to_string(),
            data,
            mod_time,
        })
    }

    fn scan(&self) -> HashMap<String, PathBuf> {
        let mut out = HashMap::new();
        // The gmaps first, so a file of the same name under files/ wins.
        if let Ok(entries) = std::fs::read_dir(&self.levels_dir) {
            for e in entries.flatten() {
                let Some(n) = e.file_name().to_str().map(str::to_ascii_lowercase) else {
                    continue;
                };
                if n.ends_with(".gmap") && e.path().is_file() {
                    out.insert(n, e.path());
                }
            }
        }
        let mut dirs = vec![self.files_dir.clone()];
        let mut seen = 0;
        while let Some(dir) = dirs.pop() {
            seen += 1;
            if seen > 10_000 {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            // Name order, so which of two same-named files wins is fixed:
            // the first one met.
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let Ok(kind) = e.file_type() else { continue };
                if kind.is_dir() {
                    dirs.push(e.path());
                } else if kind.is_file() {
                    if let Some(n) = e.file_name().to_str() {
                        // The key file is private even if a copy is dropped
                        // under files/.
                        if n.eq_ignore_ascii_case("gpakkeys.txt") {
                            continue;
                        }
                        out.entry(n.to_ascii_lowercase())
                            .or_insert_with(|| e.path());
                    }
                }
            }
        }
        out
    }
}

/// Whether `PLO_FILE` carries the GINT5 mod time (and the closing line
/// end) for `version` (`profile.rs`: 2.1 on, and 2.05).
pub fn has_modtime(version: impl Into<ClientProfile>) -> bool {
    let version = version.into();
    version.server.file_modtime
}

/// `RAWDATA` + `PLO_FILE` for one file or chunk, as `version` reads it.
fn file_message(
    version: impl Into<ClientProfile>,
    name: &str,
    mod_time: u64,
    data: &[u8],
) -> Vec<u8> {
    let version = version.into();
    let modtime = has_modtime(version);
    let mut body = vec![crate::wire::gchar(PLO_FILE)];
    if modtime {
        gint5(mod_time, &mut body);
    }
    let n = latin1(name);
    body.push(crate::wire::gchar(n.len() as u8));
    body.extend_from_slice(&n);
    body.extend_from_slice(data);
    if modtime {
        body.push(b'\n');
    }
    let mut head = Vec::new();
    gint3(body.len() as u32, &mut head);
    let mut out = msg(PLO_RAWDATA, &head);
    out.extend(body);
    out
}

/// Bytes cut into frame bodies of at most [`CHUNK`] (plus the message
/// header they start with).
fn frames_of(bytes: Vec<u8>) -> Vec<Vec<u8>> {
    if bytes.len() <= CHUNK + 64 {
        return vec![bytes];
    }
    bytes.chunks(CHUNK).map(<[u8]>::to_vec).collect()
}

/// The frames that send `f` to a client of `version`, or `None` when the
/// build cannot take a file that big.
pub fn send_file(version: impl Into<ClientProfile>, f: &Found) -> Option<Vec<Vec<u8>>> {
    let version = version.into();
    let prof = version.core;
    let plo = &prof.protocol.plo;
    // The profiles' `plo` sets leave out ids that only come behind
    // `PLO_RAWDATA` (101, and 102 before 3.x). Every build that reads a
    // file answer at all (`PLO_FILESENDFAILED`; not 1.21) reads `PLO_FILE`.
    if !plo.contains(PLO_FILE) && !plo.contains(PLO_FILESENDFAILED) {
        return None;
    }
    let large = prof.files.large_files;
    if f.data.len() > LARGE && large {
        let name = latin1(&f.name);
        let mut frames = Vec::new();
        let mut first = msg(PLO_LARGEFILESTART, &name);
        if prof.files.large_file_size {
            let mut size = Vec::new();
            gint5(f.data.len() as u64, &mut size);
            first.extend(msg(PLO_LARGEFILESIZE, &size));
        }
        frames.push(first);
        for chunk in f.data.chunks(CHUNK) {
            frames.push(file_message(version, &f.name, f.mod_time, chunk));
        }
        frames.push(msg(PLO_LARGEFILEEND, &name));
        return Some(frames);
    }
    let m = file_message(version, &f.name, f.mod_time, &f.data);
    if m.len() > RAW_MAX {
        return None;
    }
    Some(frames_of(m))
}

/// The name a `PLI_WANTFILE` stands for. A build whose art is `.gif`
/// (before 2.1) asks for an NPC's `setimg` image with no extension when
/// the script gave none, and draws the `.gif`: a real 1.39 asked for
/// `door` and drew nothing until the server answered with `door.gif`, as
/// GServer-v2 does (door lab, 2026-11-14, #45). Any other name, and any
/// name from a later build, is unchanged; the answer carries the name used
/// here.
pub fn wanted_name<'a>(version: impl Into<ClientProfile>, raw: &'a [u8]) -> Cow<'a, [u8]> {
    let ext = version.into().core.files.image_ext;
    let base = raw
        .rsplit(|&b| b == b'/' || b == b'\\')
        .next()
        .unwrap_or(raw);
    if ext != ".gif" || raw.is_empty() || base.contains(&b'.') {
        return Cow::Borrowed(raw);
    }
    let mut n = raw.to_vec();
    n.extend_from_slice(ext.as_bytes());
    Cow::Owned(n)
}

/// The frames that answer one file request.
///
/// `asked` is the name bytes the client sent (used on "failed" and "up to
/// date", so the client can match them). `name` is that name as text.
/// `key` is the `.gpake` key, when the world has one. A non-package is
/// unchanged from [`send_file`] / [`up_to_date`] / [`failed`].
pub fn offer(
    version: impl Into<ClientProfile>,
    asked: &[u8],
    name: &str,
    found: Option<&Found>,
    since: Option<u64>,
    key: Option<[u8; 8]>,
) -> Vec<Vec<u8>> {
    let version = version.into();
    let kind = crate::gpak::package_kind(name);
    if !crate::gpak::client_loads(version.version, kind) {
        return one(failed(version, asked));
    }
    let key_msg = if kind == crate::gpak::PackageKind::Encrypted {
        let Some(key) = key else {
            return one(failed(version, asked));
        };
        if !version.core.protocol.plo.contains(PLO_FILEDECRYPTKEY) {
            return one(failed(version, asked));
        }
        Some(crate::gpak::file_key_packet(name, &key))
    } else {
        None
    };
    let newer = match (&found, since) {
        (Some(f), Some(t)) => f.mod_time > t,
        (Some(_), None) => true,
        (None, _) => false,
    };
    if let Some(f) = found.filter(|_| newer) {
        if let Some(mut frames) = send_file(version, f) {
            if let Some(key_msg) = key_msg {
                if let Some(first) = frames.first_mut() {
                    let mut body = key_msg;
                    body.extend_from_slice(first);
                    *first = body;
                }
            }
            return frames;
        }
        return one(failed(version, asked));
    }
    if since.is_some() {
        let mut body = key_msg.unwrap_or_default();
        body.extend(up_to_date(version, asked));
        return one(body);
    }
    one(failed(version, asked))
}

fn one(body: Vec<u8>) -> Vec<Vec<u8>> {
    if body.is_empty() {
        Vec::new()
    } else {
        vec![body]
    }
}

/// "Could not send": `PLO_FILESENDFAILED` with the name, where the build
/// reads it (1.21 reads neither file answer and gets none).
pub fn failed(version: impl Into<ClientProfile>, name: &[u8]) -> Vec<u8> {
    let version = version.into();
    if version.core.protocol.plo.contains(PLO_FILESENDFAILED) {
        msg(PLO_FILESENDFAILED, name)
    } else {
        Vec::new()
    }
}

/// "Your copy is current": `PLO_FILEUPTODATE`, or before 2.1 (no 45)
/// `PLO_FILESENDFAILED`.
pub fn up_to_date(version: impl Into<ClientProfile>, name: &[u8]) -> Vec<u8> {
    let version = version.into();
    let plo = &version.core.protocol.plo;
    [PLO_FILEUPTODATE, PLO_FILESENDFAILED]
        .into_iter()
        .find(|&id| plo.contains(id))
        .map_or_else(Vec::new, |id| msg(id, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_builds_get_gif_for_a_name_with_no_extension() {
        use ClientVersion::*;
        for v in [V139, V141, V205] {
            assert_eq!(&*wanted_name(v, b"door"), b"door.gif", "{v:?}");
            assert_eq!(
                &*wanted_name(v, b"images/door"),
                b"images/door.gif",
                "{v:?}"
            );
            assert_eq!(&*wanted_name(v, b"door.png"), b"door.png", "{v:?}");
            assert_eq!(&*wanted_name(v, b"a.b/door.gif"), b"a.b/door.gif", "{v:?}");
            assert_eq!(&*wanted_name(v, b""), b"", "{v:?}");
        }
        for v in [V222, V231, V524R4, V603R7] {
            assert_eq!(&*wanted_name(v, b"door"), b"door", "{v:?}");
        }
    }
    use seihai_core::protocol::{ClientVersion, SessionState};

    fn store(tag: &str) -> (PathBuf, FileStore) {
        let w =
            std::env::temp_dir().join(format!("seihai-gserver-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&w);
        std::fs::create_dir_all(w.join("files/heads")).unwrap();
        std::fs::create_dir_all(w.join("levels")).unwrap();
        let s = FileStore::new(&w);
        (w, s)
    }

    #[test]
    fn names_are_checked() {
        for ok in ["head0.png", "Body.PNG", "a b.gani", "world.gmap"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "", "../x.png", "a/b.png", "a\\b.png", "c:x.png", ".hidden", "a\nb",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn files_and_gmaps_are_found_by_name() {
        let (w, s) = store("find");
        std::fs::write(w.join("files/heads/Head0.png"), b"png").unwrap();
        std::fs::write(w.join("levels/world.gmap"), b"GRMAP001\n").unwrap();
        std::fs::write(w.join("levels/start.nw"), b"GLEVNW01\n").unwrap();
        let f = s.find("head0.PNG").unwrap();
        assert_eq!((f.name.as_str(), &f.data[..]), ("head0.PNG", &b"png"[..]));
        assert!(f.mod_time > 0);
        assert!(s.find("world.gmap").is_some());
        assert!(s.find("start.nw").is_none(), "levels are not files");
        assert!(s.find("../files/heads/head0.png").is_none());
        // A file put in later is found after the rescan pause.
        std::fs::write(w.join("files/new.wav"), b"wav").unwrap();
        s.index.lock().unwrap().scanned = None;
        assert!(s.find("new.wav").is_some());
        let _ = std::fs::remove_dir_all(&w);
    }

    /// 2.05 gets the mod time and the closing line end, as 2.1 does; 2.04
    /// and 1.39 do not.
    #[test]
    fn two_point_oh_five_gets_the_mod_time() {
        let f = Found {
            name: "a.png".into(),
            data: b"PNG".to_vec(),
            mod_time: 1_700_000_000,
        };
        for (v, len) in [
            (ClientVersion::V205, 1 + 5 + 1 + 5 + 3 + 1),
            (ClientVersion::V204, 1 + 1 + 5 + 3),
            (ClientVersion::V139, 1 + 1 + 5 + 3),
        ] {
            let m = send_file(v, &f).unwrap().concat();
            let body = &m[m.iter().position(|&b| b == b'\n').unwrap() + 1..];
            assert_eq!(body.len(), len, "{v:?}");
            assert_eq!(body.ends_with(b"PNG\n"), v == ClientVersion::V205, "{v:?}");
        }
    }

    /// Every build that reads `PLO_FILE` gets the file whole, small or
    /// large, in frames no bigger than [`CHUNK`] and a header.
    #[test]
    fn every_build_reads_the_file() {
        for size in [5usize, 40_000, 150_000] {
            let data: Vec<u8> = (0..size).map(|i| (i * 7 % 251) as u8).collect();
            let f = Found {
                name: "big.png".into(),
                data: data.clone(),
                mod_time: 1_700_000_000,
            };
            for &v in ClientVersion::all_versions() {
                let Some(frames) = send_file(v, &f) else {
                    assert!(
                        !v.profile().protocol.plo.contains(PLO_FILESENDFAILED),
                        "{v:?}"
                    );
                    continue;
                };
                assert!(frames.iter().all(|b| b.len() <= CHUNK + 256), "{v:?}");
                let mut st = SessionState::for_version(v);
                st.file_has_modtime = has_modtime(v);
                for b in &frames {
                    st.apply_body(b);
                }
                assert_eq!(st.incoming_files.len(), 1, "{v:?} {size}");
                assert_eq!(st.incoming_files[0].0, "big.png", "{v:?}");
                assert!(st.incoming_files[0].1 == data, "{v:?} {size}: data differs");
                assert!(st.ignored.is_empty(), "{v:?} drops {:?}", st.ignored);
            }
        }
    }

    #[test]
    fn the_key_file_is_not_served_and_a_gpake_starts_with_104() {
        let (w, s) = store("gpak");
        std::fs::create_dir_all(w.join("config")).unwrap();
        std::fs::write(
            w.join("config/gpakkeys.txt"),
            "sample.gpake 133457799bbcdff1\n",
        )
        .unwrap();
        std::fs::write(w.join("files/gpakkeys.txt"), b"secret-key-line").unwrap();
        std::fs::write(w.join("files/sample.gpake"), b"PK").unwrap();
        std::fs::write(w.join("files/sample.gpak"), b"PK").unwrap();
        assert!(s.find("gpakkeys.txt").is_none());
        let key = [0x13, 0x34, 0x57, 0x79, 0x9b, 0xbc, 0xdf, 0xf1];
        assert_eq!(s.package_key("Sample.gpake"), Some(key));

        let enc = s.find("sample.gpake").unwrap();
        let plain = s.find("sample.gpak").unwrap();
        let sent = offer(
            ClientVersion::V420R8,
            b"sample.gpake",
            "sample.gpake",
            Some(&enc),
            None,
            Some(key),
        );
        assert_eq!(sent[0][0], 104 + 32, "key first");
        assert!(
            sent[0].windows(2).any(|b| b == b"PK"),
            "the package follows"
        );

        let old = offer(
            ClientVersion::V404R2,
            b"sample.gpake",
            "sample.gpake",
            Some(&enc),
            None,
            Some(key),
        );
        assert_eq!(old[0][0], 30 + 32);

        let no_key = offer(
            ClientVersion::V603R7,
            b"sample.gpake",
            "sample.gpake",
            Some(&enc),
            None,
            None,
        );
        assert_eq!(no_key[0][0], 30 + 32);

        let plain_old = offer(
            ClientVersion::V420R8,
            b"sample.gpak",
            "sample.gpak",
            Some(&plain),
            None,
            None,
        );
        assert_eq!(plain_old[0][0], 30 + 32);

        let plain_new = offer(
            ClientVersion::V603R7,
            b"sample.gpak",
            "sample.gpak",
            Some(&plain),
            None,
            None,
        );
        assert_ne!(plain_new[0][0], 104 + 32);
        assert_ne!(plain_new[0][0], 30 + 32);

        let cached = offer(
            ClientVersion::V603R7,
            b"sample.gpake",
            "sample.gpake",
            Some(&enc),
            Some(u64::MAX),
            Some(key),
        );
        let nl = cached[0].iter().position(|b| *b == b'\n').unwrap();
        assert_eq!(cached[0][0], 104 + 32);
        assert_eq!(cached[0][nl + 1], 45 + 32, "up to date follows the key");
    }
}
