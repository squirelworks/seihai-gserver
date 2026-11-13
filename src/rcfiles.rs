//! The RC's file browser (stage 4 slice 5, issue #52): list a folder, get
//! a file, put a file, in the world folder, within the account's folder
//! rights. Layouts as the gserver-v2 fork used them on seihai-rc's
//! loopback run of 2026-11-01 (seihai-rc `docs/PROTOCOL.md`, "File link").
//!
//! | RC sends | Server answers |
//! |---|---|
//! | 90 `RC_FILEBROWSER_CD`: the folder, the rest of the line | 67 `Folder changed to <folder>`, then 66: `GSTRING` folder, then per file a space, a `GCHAR` record length, `GSTRING` name, `GSTRING` rights (`r` or `rw`), `GINT5` size, `GINT5` mod time |
//! | 92 `RC_FILEBROWSER_DOWN`: the name, the rest of the line | raw block (100) of 102: `GINT5` mod time, `GCHAR` name length, the name, the bytes |
//! | 93 `RC_FILEBROWSER_UP` in a raw block (50): `GSTRING` name, the bytes | 67 `Uploaded file <name>` |
//! | 96 `RC_FILEBROWSER_MOVE`: `GSTRING` name, the target folder as the rest of the line | 67 `Moved file <name> to <folder>` |
//! | 97 `RC_FILEBROWSER_DELETE`: the name, the rest of the line | 67 `Deleted file <name>` |
//! | 98 `RC_FILEBROWSER_RENAME`: `GSTRING` old name, `GSTRING` new name | 67 `Renamed file <old> to <new>` |
//!
//! Move, delete and rename are laid out as seihai-rc's `files mv`, `rm`
//! and `rename` send them (seihai-rc `docs/PROTOCOL.md`, marked "check":
//! not yet captured from a real RC). The answer lines are our own;
//! seihai-rc lists the folder again rather than reading them. Each needs a
//! write right on the file where it is and, for a move or rename, where it
//! goes; a file already there is not replaced. Folder delete (160) is not
//! answered.
//!
//! Any other 67 is a refusal, and says why. A get or put names a file in
//! the folder of the last list, as with the fork.
//!
//! Rights are `FOLDER` lines in the account file (`accounts.rs`), one per
//! right: `r` or `rw`, a space, then a folder and a file mask, as
//! `rw levels/*` or `r config/*.txt`. A right covers the files of that one
//! folder (not its subfolders) whose names match the mask (`*` any run,
//! `?` one character). An account with no `FOLDER` line sees no files.
//! Only plain files are listed, read or written: not folders or links. A
//! file over [`MAX_FILE`] is refused both ways, as one frame cannot carry
//! more until the large-file ids (155/156) have a lab layout. A put is
//! written through a temporary file. Folder rights from an RC (83/84) come
//! with a later slice; until then an admin edits the account file.

use crate::wire::{gchar, gint5, gstring, latin1, msg, raw_block, text};
use std::path::{Path, PathBuf};

pub const PLI_RC_FILEBROWSER_CD: u8 = 90;
pub const PLI_RC_FILEBROWSER_DOWN: u8 = 92;
pub const PLI_RC_FILEBROWSER_UP: u8 = 93;
pub const PLI_RC_FILEBROWSER_MOVE: u8 = 96;
pub const PLI_RC_FILEBROWSER_DELETE: u8 = 97;
pub const PLI_RC_FILEBROWSER_RENAME: u8 = 98;
pub const PLO_RC_FILEBROWSER_DIR: u8 = 66;
pub const PLO_RC_FILEBROWSER_MESSAGE: u8 = 67;
pub const PLO_FILE: u8 = 102;
/// The largest file the browser moves, as seihai-rc caps it.
pub const MAX_FILE: usize = 60 * 1024;

/// A file browser request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Cd(String),
    Down(String),
    Up { name: String, data: Vec<u8> },
    Move { name: String, to: String },
    Delete(String),
    Rename { from: String, to: String },
}

/// A `GSTRING` and what follows it.
fn gstr(rest: &[u8]) -> Option<(String, &[u8])> {
    let n = usize::from(rest.first()?.wrapping_sub(0x20));
    let s = rest.get(1..1 + n)?;
    Some((text(s), &rest[1 + n..]))
}

/// Read a request: `id` and its payload (a put's from inside its raw
/// block).
pub fn parse(id: u8, rest: &[u8]) -> Option<Op> {
    match id {
        PLI_RC_FILEBROWSER_CD => Some(Op::Cd(text(rest).trim().to_string())),
        PLI_RC_FILEBROWSER_DOWN => Some(Op::Down(text(rest).trim().to_string())),
        PLI_RC_FILEBROWSER_UP => {
            let n = usize::from(rest.first()?.wrapping_sub(0x20));
            let name = rest.get(1..1 + n)?;
            Some(Op::Up {
                name: text(name),
                data: rest[1 + n..].to_vec(),
            })
        }
        PLI_RC_FILEBROWSER_MOVE => {
            let (name, to) = gstr(rest)?;
            Some(Op::Move {
                name,
                to: text(to).trim().to_string(),
            })
        }
        PLI_RC_FILEBROWSER_DELETE => Some(Op::Delete(text(rest).trim().to_string())),
        PLI_RC_FILEBROWSER_RENAME => {
            let (from, rest) = gstr(rest)?;
            let (to, _) = gstr(rest)?;
            Some(Op::Rename { from, to })
        }
        _ => None,
    }
}

/// One folder right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Right {
    pub write: bool,
    /// Ends in `/`, or is empty for the world folder itself.
    pub folder: String,
    pub mask: String,
}

impl Right {
    /// `rw levels/*`, `r config/*.txt`. `None` for a line that is not one,
    /// or whose folder walks out of the world.
    pub fn parse(line: &str) -> Option<Right> {
        let (mode, path) = line.trim().split_once(' ')?;
        let write = match mode {
            "r" => false,
            "rw" => true,
            _ => return None,
        };
        let path = path.trim();
        let (folder, mask) = match path.rfind('/') {
            Some(i) => (&path[..=i], &path[i + 1..]),
            None => ("", path),
        };
        (safe_folder(folder) && !mask.is_empty()).then(|| Right {
            write,
            folder: folder.to_string(),
            mask: mask.to_string(),
        })
    }

    fn covers(&self, folder: &str, name: &str) -> bool {
        self.folder.eq_ignore_ascii_case(folder) && glob(&self.mask, name)
    }
}

/// A folder as rights and requests give it: relative, `/`-separated, no
/// `..`, backslash or colon, ending in `/` (or empty).
fn safe_folder(f: &str) -> bool {
    f.is_empty()
        || (f.ends_with('/')
            && f[..f.len() - 1]
                .split('/')
                .all(|s| !s.is_empty() && s != ".." && s != "." && !s.contains(['\\', ':'])))
}

/// One file name in a folder.
fn safe_name(n: &str) -> bool {
    !n.is_empty()
        && n != "."
        && n != ".."
        && !n.contains(['/', '\\', ':', '\0', '\n'])
        && n.len() <= 223
}

/// `*` any run, `?` one character, without case.
pub fn glob(mask: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some((b'*', rest)) => (0..=n.len()).any(|k| go(rest, &n[k..])),
            Some((b'?', rest)) => !n.is_empty() && go(rest, &n[1..]),
            Some((c, rest)) => n
                .split_first()
                .is_some_and(|(d, nr)| c.eq_ignore_ascii_case(d) && go(rest, nr)),
        }
    }
    go(mask.as_bytes(), name.as_bytes())
}

/// One RC's file browser.
#[derive(Debug)]
pub struct Browser {
    world: PathBuf,
    rights: Vec<Right>,
    /// The folder of the last list.
    folder: Option<String>,
}

fn refusal(why: &str) -> Vec<u8> {
    msg(PLO_RC_FILEBROWSER_MESSAGE, &latin1(why))
}

fn mod_time(m: &std::fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// A plain file's metadata (no links, no folders).
fn plain(path: &Path) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(path).ok().filter(|m| m.is_file())
}

impl Browser {
    /// `rights` are the account's `FOLDER` lines; lines that do not read
    /// are left out.
    pub fn new(world: &Path, rights: &[String]) -> Browser {
        Browser {
            world: world.to_path_buf(),
            rights: rights.iter().filter_map(|r| Right::parse(r)).collect(),
            folder: None,
        }
    }

    fn right(&self, folder: &str, name: &str) -> Option<&Right> {
        // A write right wins over a read one for the same file.
        let mut found = None;
        for r in self.rights.iter().filter(|r| r.covers(folder, name)) {
            if r.write || found.is_none() {
                found = Some(r);
            }
        }
        found
    }

    /// Carry out `op`: the bodies for the RC, and a line for the log.
    pub fn run(&mut self, op: Op) -> (Vec<Vec<u8>>, String) {
        match op {
            Op::Cd(f) => self.cd(f),
            Op::Down(name) => self.down(&name),
            Op::Up { name, data } => self.up(&name, &data),
            Op::Delete(name) => self.delete(&name),
            Op::Move { name, to } => {
                let mut f = to.replace('\\', "/");
                if !f.is_empty() && !f.ends_with('/') {
                    f.push('/');
                }
                self.relocate("move", &name, &f, &name)
            }
            Op::Rename { from, to } => match self.folder.clone() {
                Some(f) => self.relocate("rename", &from, &f, &to),
                None => (
                    vec![refusal("Open a folder first.")],
                    "rename: no folder".into(),
                ),
            },
        }
    }

    fn delete(&self, name: &str) -> (Vec<Vec<u8>>, String) {
        let (f, path) = match self.place(name, true) {
            Ok(p) => p,
            Err(why) => return (vec![refusal(&why)], format!("delete {name}: refused")),
        };
        if plain(&path).is_none() {
            return (
                vec![refusal(&format!("{f}{name} is not a file."))],
                format!("delete {f}{name}: not a plain file"),
            );
        }
        match std::fs::remove_file(&path) {
            Ok(()) => (
                vec![msg(
                    PLO_RC_FILEBROWSER_MESSAGE,
                    &latin1(&format!("Deleted file {name}")),
                )],
                format!("delete {f}{name}"),
            ),
            Err(e) => (
                vec![refusal(&format!("Could not delete {name}."))],
                format!("delete {f}{name}: {e}"),
            ),
        }
    }

    /// Move or rename `name` (in the listed folder) to `to` in `folder`.
    fn relocate(&self, what: &str, name: &str, folder: &str, to: &str) -> (Vec<Vec<u8>>, String) {
        let (f, from) = match self.place(name, true) {
            Ok(p) => p,
            Err(why) => return (vec![refusal(&why)], format!("{what} {name}: refused")),
        };
        let allowed =
            safe_folder(folder) && safe_name(to) && self.right(folder, to).is_some_and(|r| r.write);
        if !allowed {
            return (
                vec![refusal(&format!("No rights for {folder}{to}."))],
                format!("{what} {f}{name} to {folder}{to}: refused"),
            );
        }
        let dest = self.world.join(folder).join(to);
        if plain(&from).is_none() {
            return (
                vec![refusal(&format!("{f}{name} is not a file."))],
                format!("{what} {f}{name}: not a plain file"),
            );
        }
        if std::fs::symlink_metadata(&dest).is_ok() {
            return (
                vec![refusal(&format!("{folder}{to} is already there."))],
                format!("{what} {f}{name} to {folder}{to}: target exists"),
            );
        }
        let done = std::fs::create_dir_all(self.world.join(folder))
            .and_then(|()| std::fs::rename(&from, &dest));
        let line = if what == "move" {
            format!("Moved file {name} to {folder}")
        } else {
            format!("Renamed file {name} to {to}")
        };
        match done {
            Ok(()) => (
                vec![msg(PLO_RC_FILEBROWSER_MESSAGE, &latin1(&line))],
                format!("{what} {f}{name} to {folder}{to}"),
            ),
            Err(e) => (
                vec![refusal(&format!("Could not {what} {name}."))],
                format!("{what} {f}{name} to {folder}{to}: {e}"),
            ),
        }
    }

    fn cd(&mut self, f: String) -> (Vec<Vec<u8>>, String) {
        let mut f = f.replace('\\', "/");
        if f.is_empty() {
            match self.rights.first() {
                Some(r) => f = r.folder.clone(),
                None => {
                    return (
                        vec![refusal("You have no folder rights.")],
                        "list: no rights".into(),
                    )
                }
            }
        }
        if !f.is_empty() && !f.ends_with('/') {
            f.push('/');
        }
        if !safe_folder(&f)
            || !self
                .rights
                .iter()
                .any(|r| r.folder.eq_ignore_ascii_case(&f))
        {
            return (
                vec![refusal(&format!("No rights for the folder {f}."))],
                format!("list {f}: refused"),
            );
        }
        let mut entries: Vec<(String, u64, u64, bool)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.world.join(&f)) {
            for e in rd.flatten() {
                let Ok(name) = e.file_name().into_string() else {
                    continue;
                };
                let (Some(m), Some(r)) = (plain(&e.path()), self.right(&f, &name)) else {
                    continue;
                };
                if safe_name(&name) {
                    entries.push((name, m.len(), mod_time(&m), r.write));
                }
            }
        }
        entries.sort();
        let mut p = Vec::new();
        gstring(&latin1(&f), &mut p);
        for (name, size, time, write) in &entries {
            let mut rec = Vec::new();
            gstring(&latin1(name), &mut rec);
            gstring(if *write { b"rw" } else { b"r" }, &mut rec);
            gint5(*size, &mut rec);
            gint5(*time, &mut rec);
            p.push(b' ');
            p.push(gchar(rec.len().min(0xDF) as u8));
            p.extend(rec);
        }
        let n = entries.len();
        self.folder = Some(f.clone());
        (
            vec![
                msg(
                    PLO_RC_FILEBROWSER_MESSAGE,
                    &latin1(&format!("Folder changed to {f}")),
                ),
                msg(PLO_RC_FILEBROWSER_DIR, &p),
            ],
            format!("list {f}: {n} files"),
        )
    }

    /// The folder and the file's path, if `name` is one the account may
    /// `write` (or read).
    fn place(&self, name: &str, write: bool) -> Result<(String, PathBuf), String> {
        let Some(f) = self.folder.clone() else {
            return Err("Open a folder first.".into());
        };
        if !safe_name(name) {
            return Err(format!("{name} is not a file name."));
        }
        match self.right(&f, name) {
            Some(r) if r.write || !write => Ok((f.clone(), self.world.join(&f).join(name))),
            _ => Err(format!("No rights for {f}{name}.")),
        }
    }

    fn down(&self, name: &str) -> (Vec<Vec<u8>>, String) {
        let (f, path) = match self.place(name, false) {
            Ok(p) => p,
            Err(why) => return (vec![refusal(&why)], format!("get {name}: refused")),
        };
        let data = plain(&path).and_then(|m| {
            (m.len() as usize <= MAX_FILE)
                .then(|| std::fs::read(&path).ok().map(|d| (d, mod_time(&m))))
                .flatten()
        });
        let Some((data, time)) = data else {
            return (
                vec![refusal(&format!("Could not send {f}{name}."))],
                format!("get {f}{name}: not there or too large"),
            );
        };
        let mut p = Vec::new();
        gint5(time, &mut p);
        gstring(&latin1(name), &mut p);
        p.extend_from_slice(&data);
        (
            vec![raw_block(PLO_FILE, &p)],
            format!("get {f}{name}: {} bytes", data.len()),
        )
    }

    fn up(&self, name: &str, data: &[u8]) -> (Vec<Vec<u8>>, String) {
        let (f, path) = match self.place(name, true) {
            Ok(p) => p,
            Err(why) => return (vec![refusal(&why)], format!("put {name}: refused")),
        };
        if data.len() > MAX_FILE {
            return (
                vec![refusal(&format!("{name} is over {MAX_FILE} bytes."))],
                format!("put {f}{name}: too large"),
            );
        }
        if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file()) {
            return (
                vec![refusal(&format!("{f}{name} is not a file."))],
                format!("put {f}{name}: not a plain file"),
            );
        }
        let tmp = path.with_extension("rcupload.tmp");
        let written = std::fs::create_dir_all(self.world.join(&f))
            .and_then(|()| std::fs::write(&tmp, data))
            .and_then(|()| std::fs::rename(&tmp, &path));
        match written {
            Ok(()) => (
                vec![msg(
                    PLO_RC_FILEBROWSER_MESSAGE,
                    &latin1(&format!("Uploaded file {name}")),
                )],
                format!("put {f}{name}: {} bytes", data.len()),
            ),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                (
                    vec![refusal(&format!("Could not store {name}."))],
                    format!("put {f}{name}: {e}"),
                )
            }
        }
    }
}

/// Split an RC's frame body into messages, each with its `\n`. A raw
/// block (`50`, `GINT3 n`, `\n`, then `n` bytes) is one message, line
/// feeds and all. Real RCs end the block with a line feed that `n` counts
/// (the 2004 RC and RC3: 32015 for a 32014-byte id 93, seihai-rc #45), and
/// that line feed ends the message, as the gserver-v2 fork reads it; a
/// block without one is given a `\n` of its own.
pub fn split(body: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        if let [0x52, a, b, c, b'\n', ..] = rest {
            // 0x52 is GCHAR 50.
            let n = [a, b, c].iter().fold(0usize, |v, &&d| {
                (v << 7) | usize::from(d.wrapping_sub(0x20) & 0x7F)
            });
            if let Some(inner) = rest.get(5..5 + n) {
                let mut m = inner.to_vec();
                if m.last() != Some(&b'\n') {
                    m.push(b'\n');
                }
                out.push(m);
                i += 5 + n;
                continue;
            }
        }
        match rest.iter().position(|&b| b == b'\n') {
            Some(e) => {
                out.push(rest[..=e].to_vec());
                i += e + 1;
            }
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::split_messages;

    #[test]
    fn rights_and_masks() {
        let r = Right::parse("rw levels/*.nw").unwrap();
        assert!(r.write && r.folder == "levels/" && r.mask == "*.nw");
        assert!(r.covers("Levels/", "start.NW"));
        assert!(!r.covers("levels/", "start.txt"));
        assert!(!r.covers("levels/sub/", "a.nw"));
        assert_eq!(Right::parse("r ../*"), None);
        assert_eq!(Right::parse("x levels/*"), None);
        assert_eq!(Right::parse("r /etc/*"), None);
        assert!(Right::parse("r *.txt").is_some_and(|r| r.folder.is_empty()));
        assert!(glob("a?c", "abc") && !glob("a?c", "ac"));
    }

    #[test]
    fn raw_blocks_split() {
        let mut body = b"\xc2hi\n".to_vec();
        // The closing line feed is the block's own, counted in its length.
        let inner = b"\x7d\x23a.txtline1\nline2\n\n";
        body.extend([0x52, 0x20, 0x20, 0x20 + inner.len() as u8, b'\n']);
        body.extend(inner);
        body.extend(b"\xc2\n");
        let m = split(&body);
        assert_eq!(m.len(), 3);
        assert_eq!(m[1][..inner.len()], inner[..]);
        let Some(Op::Up { name, data }) = parse(93, &m[1][1..m[1].len() - 1]) else {
            panic!()
        };
        assert_eq!(
            (name.as_str(), data.as_slice()),
            ("a.t", &b"xtline1\nline2\n"[..])
        );
    }

    #[test]
    fn list_get_put() {
        let w = std::env::temp_dir().join(format!("seihai-rcfiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&w);
        std::fs::create_dir_all(w.join("levels/sub")).unwrap();
        std::fs::write(w.join("levels/a.nw"), "GLEVNW01\n").unwrap();
        std::fs::write(w.join("levels/b.txt"), "x").unwrap();
        std::fs::create_dir_all(w.join("config")).unwrap();
        std::fs::write(w.join("config/server.txt"), "x").unwrap();
        let mut b = Browser::new(&w, &["rw levels/*.nw".into(), "r levels/*".into()]);
        let (out, _) = b.run(Op::Cd("levels".into()));
        assert_eq!(
            split_messages(&out[0])[0].payload,
            b"Folder changed to levels/"
        );
        let dir = &split_messages(&out[1])[0];
        assert_eq!(dir.id, PLO_RC_FILEBROWSER_DIR);
        let s = text(&dir.payload);
        assert!(s.contains("a.nw") && s.contains("rw") && s.contains("b.txt"));
        assert!(!s.contains("sub"), "folders are not listed");
        // Get: the raw block of 102.
        let (out, _) = b.run(Op::Down("a.nw".into()));
        let m = &split_messages(&out[0])[0];
        assert_eq!(m.id, PLO_FILE);
        assert!(m.payload.ends_with(b"\x24a.nwGLEVNW01\n\n"));
        // Put: rw on .nw only.
        let (out, _) = b.run(Op::Up {
            name: "new.nw".into(),
            data: b"GLEVNW01\nBOARD\n".to_vec(),
        });
        assert_eq!(split_messages(&out[0])[0].payload, b"Uploaded file new.nw");
        assert_eq!(
            std::fs::read(w.join("levels/new.nw")).unwrap(),
            b"GLEVNW01\nBOARD\n"
        );
        let (out, _) = b.run(Op::Up {
            name: "b.txt".into(),
            data: b"y".to_vec(),
        });
        assert!(text(&split_messages(&out[0])[0].payload).starts_with("No rights"));
        for bad in ["../config/server.txt", "sub", ".."] {
            let (out, _) = b.run(Op::Down(bad.into()));
            assert_eq!(
                split_messages(&out[0])[0].id,
                PLO_RC_FILEBROWSER_MESSAGE,
                "{bad}"
            );
        }
        let (out, _) = b.run(Op::Cd("config".into()));
        assert!(text(&split_messages(&out[0])[0].payload).starts_with("No rights"));
        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn move_delete_rename() {
        let w = std::env::temp_dir().join(format!("seihai-rcfiles-mv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&w);
        std::fs::create_dir_all(w.join("levels")).unwrap();
        std::fs::write(w.join("levels/a.nw"), "A").unwrap();
        std::fs::write(w.join("levels/b.nw"), "B").unwrap();
        std::fs::write(w.join("levels/ro.txt"), "R").unwrap();
        let mut b = Browser::new(
            &w,
            &[
                "rw levels/*.nw".into(),
                "r levels/*".into(),
                "rw old/*".into(),
            ],
        );
        let line = |out: &[Vec<u8>]| text(&split_messages(&out[0])[0].payload);
        // Before a list there is no folder.
        assert_eq!(
            line(&b.run(parse(97, b"a.nw").unwrap()).0),
            "Open a folder first."
        );
        b.run(Op::Cd("levels".into()));
        // Rename, as seihai-rc lays it out: two GSTRINGs.
        let op = parse(98, b"$a.nw$c.nw").unwrap();
        assert_eq!(line(&b.run(op).0), "Renamed file a.nw to c.nw");
        assert!(w.join("levels/c.nw").exists() && !w.join("levels/a.nw").exists());
        // Not over a file that is there, nor to a name without a write right.
        let op = parse(98, b"$c.nw$b.nw").unwrap();
        assert!(line(&b.run(op).0).ends_with("already there."));
        let op = parse(98, b"$c.nw%c.txt").unwrap();
        assert!(line(&b.run(op).0).starts_with("No rights"));
        // Move: a GSTRING, then the folder.
        let op = parse(96, b"$c.nwold").unwrap();
        assert_eq!(line(&b.run(op).0), "Moved file c.nw to old/");
        assert_eq!(std::fs::read(w.join("old/c.nw")).unwrap(), b"A");
        let op = parse(96, b"$b.nw../x").unwrap();
        assert!(line(&b.run(op).0).starts_with("No rights"));
        // Delete: the name, the rest of the line; a read right is not enough.
        assert_eq!(
            line(&b.run(parse(97, b"b.nw").unwrap()).0),
            "Deleted file b.nw"
        );
        assert!(!w.join("levels/b.nw").exists());
        assert!(line(&b.run(parse(97, b"ro.txt").unwrap()).0).starts_with("No rights"));
        assert!(line(&b.run(parse(97, b"gone.nw").unwrap()).0).ends_with("not a file."));
        assert!(w.join("levels/ro.txt").exists());
        let (out, _) = Browser::new(&w, &[]).run(Op::Cd(String::new()));
        assert!(text(&split_messages(&out[0])[0].payload).contains("no folder rights"));
        let _ = std::fs::remove_dir_all(&w);
    }
}
