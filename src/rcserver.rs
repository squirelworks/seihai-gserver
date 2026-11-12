//! Stage 4 slice 5, the rest (issue #52): server options, folder config
//! and server flags from an RC.
//!
//! The ids are wire facts. The layouts are the ones seihai-rc sends and
//! reads (its `docs/PROTOCOL.md`, "Server options, folder config, server
//! flags"). No real RC was captured using these yet, so they are marked
//! **check** there too. seihai-rc reads each value back after a set and
//! fails when it differs, so a wrong guess shows on the first lab run.
//!
//! | What | Get | Reply | Set | Payload |
//! |---|---|---|---|---|
//! | Server options | 51 | 76 | 52 | tokenized text, one item per line |
//! | Folder config | 53 | 77 | 54 | the same |
//! | Server flags | 68 | 61 | 69 | `GSHORT` count, then a `GSTRING` per flag (`name=value`) |
//!
//! Where they live here:
//!
//! - **Server options** are `config/server.txt`, line for line, comments
//!   and all. A set writes the file. The settings in it are read once at
//!   start, so a change applies when the server restarts; the RC is told
//!   so on RC chat, with a line for each setting the server would not
//!   take.
//! - **Folder config** is `config/foldersconfig.txt`. `seihai-gserver
//!   import-v2` reads it to pick what to copy. The server itself serves
//!   every file under the world folder (`files.rs`), so the file is kept
//!   for the RC and the importer and not acted on.
//! - **Server flags** are the world's (`world.rs`): the same ones the
//!   NPC-server sets, sent to players as `PLO_FLAGSET`. A set replaces
//!   the whole list; players online get a `PLO_FLAGDEL` for each flag gone
//!   and a `PLO_FLAGSET` for each new or changed one. The world keeps them
//!   in `config/serverflags.txt`, so they last over a restart.

use crate::wire::{detokenize, gshort, gstring, msg, text, tokenize};
use seihai_core::protocol::read_gshort;
use std::path::{Path, PathBuf};

pub const PLI_RC_SERVEROPTIONSGET: u8 = 51;
pub const PLI_RC_SERVEROPTIONSSET: u8 = 52;
pub const PLI_RC_FOLDERCONFIGGET: u8 = 53;
pub const PLI_RC_FOLDERCONFIGSET: u8 = 54;
pub const PLI_RC_SERVERFLAGSGET: u8 = 68;
pub const PLI_RC_SERVERFLAGSSET: u8 = 69;
pub const PLO_RC_SERVERFLAGSGET: u8 = 61;
pub const PLO_RC_SERVEROPTIONSGET: u8 = 76;
pub const PLO_RC_FOLDERCONFIGGET: u8 = 77;

/// The longest flag one `GSTRING` holds.
const FLAG_MAX: usize = 223;

/// A server options, folder config or server flags request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    OptionsGet,
    OptionsSet(Vec<Vec<u8>>),
    FoldersGet,
    FoldersSet(Vec<Vec<u8>>),
    FlagsGet,
    FlagsSet(Vec<String>),
}

/// Read one RC message (id, payload) if it is one of these.
pub fn parse(id: u8, rest: &[u8]) -> Option<Op> {
    Some(match id {
        PLI_RC_SERVEROPTIONSGET => Op::OptionsGet,
        PLI_RC_SERVEROPTIONSSET => Op::OptionsSet(detokenize(rest)),
        PLI_RC_FOLDERCONFIGGET => Op::FoldersGet,
        PLI_RC_FOLDERCONFIGSET => Op::FoldersSet(detokenize(rest)),
        PLI_RC_SERVERFLAGSGET => Op::FlagsGet,
        PLI_RC_SERVERFLAGSSET => Op::FlagsSet(flags(rest)?),
        _ => return None,
    })
}

/// `GSHORT` count, then that many `GSTRING`s. Empty flags are dropped.
fn flags(p: &[u8]) -> Option<Vec<String>> {
    let n = read_gshort(p)?;
    let mut i = 2;
    let mut out = Vec::new();
    for _ in 0..n {
        let len = usize::from(p.get(i)?.wrapping_sub(0x20));
        let f = p.get(i + 1..i + 1 + len)?;
        i += 1 + len;
        let f = text(f).trim().to_string();
        if !f.is_empty() {
            out.push(f);
        }
    }
    Some(out)
}

/// The text files the RC edits, in the world folder.
pub struct Files {
    options: PathBuf,
    folders: PathBuf,
    world: PathBuf,
}

/// What a request comes to: replies for the RC and a line for the log.
pub struct Done {
    pub replies: Vec<Vec<u8>>,
    pub log: String,
}

impl Files {
    pub fn new(world: &Path) -> Files {
        let config = world.join("config");
        Files {
            options: config.join("server.txt"),
            folders: config.join("foldersconfig.txt"),
            world: world.to_path_buf(),
        }
    }

    /// Carry out a text request (options or folders); flags go to the
    /// world and are answered with [`flags_reply`].
    pub fn run(&self, op: &Op) -> Option<Done> {
        Some(match op {
            Op::OptionsGet => Done {
                replies: vec![text_reply(PLO_RC_SERVEROPTIONSGET, &self.options)],
                log: "reads the server options".into(),
            },
            Op::FoldersGet => Done {
                replies: vec![text_reply(PLO_RC_FOLDERCONFIGGET, &self.folders)],
                log: "reads the folder config".into(),
            },
            Op::OptionsSet(lines) => {
                let body = joined(lines);
                let mut chat = Vec::new();
                match write(&self.options, &body) {
                    Ok(()) => {
                        let mut cfg = crate::config::Config::defaults(&self.world);
                        for w in cfg.apply(&String::from_utf8_lossy(&body)) {
                            chat.push(format!("Server options: {w}"));
                        }
                        chat.push(
                            "Server options saved. They apply when the server restarts.".into(),
                        );
                    }
                    Err(e) => chat.push(format!("Server options not saved: {e}")),
                }
                Done {
                    replies: chat.iter().map(|c| rc_chat(c)).collect(),
                    log: format!("sets the server options ({} lines)", lines.len()),
                }
            }
            Op::FoldersSet(lines) => {
                let line = match write(&self.folders, &joined(lines)) {
                    Ok(()) => "Folder config saved.".to_string(),
                    Err(e) => format!("Folder config not saved: {e}"),
                };
                Done {
                    replies: vec![rc_chat(&line)],
                    log: format!("sets the folder config ({} lines)", lines.len()),
                }
            }
            Op::FlagsGet | Op::FlagsSet(_) => return None,
        })
    }
}

/// Lines as a file: each ends in `\n`.
fn joined(lines: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for l in lines {
        out.extend_from_slice(l.strip_suffix(b"\r").unwrap_or(l));
        out.push(b'\n');
    }
    out
}

/// Write through a temporary file, so a failed write leaves the old one.
fn write(path: &Path, body: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("txt.new");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// A 76 or 77: the file's lines, tokenized. A missing file is empty.
fn text_reply(id: u8, path: &Path) -> Vec<u8> {
    let body = std::fs::read(path).unwrap_or_default();
    let mut lines: Vec<Vec<u8>> = body
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l).to_vec())
        .collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    msg(id, &tokenize(&lines))
}

/// The 61 reply: `GSHORT` count, then a `GSTRING` per flag. A flag too
/// long for one is left out.
pub fn flags_reply(flags: &[String]) -> Vec<u8> {
    let fit: Vec<Vec<u8>> = flags
        .iter()
        .map(|f| crate::wire::latin1(f))
        .filter(|f| f.len() <= FLAG_MAX)
        .collect();
    let mut p = Vec::new();
    gshort(fit.len() as u16, &mut p);
    for f in &fit {
        gstring(f, &mut p);
    }
    msg(PLO_RC_SERVERFLAGSGET, &p)
}

/// A line on RC chat for this RC.
fn rc_chat(line: &str) -> Vec<u8> {
    msg(crate::rc::PLO_RC_CHAT, &crate::wire::latin1(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message's payload: no id, no `\n`.
    fn body(m: &[u8]) -> &[u8] {
        &m[1..m.len() - 1]
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("seihai-rcserver-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("config")).unwrap();
        d
    }

    #[test]
    fn requests_read_as_seihai_rc_sends_them() {
        assert_eq!(parse(51, b""), Some(Op::OptionsGet));
        assert_eq!(
            parse(52, b"\"name=Lab\",\"staff=labrc,admin\""),
            Some(Op::OptionsSet(vec![
                b"name=Lab".to_vec(),
                b"staff=labrc,admin".to_vec()
            ]))
        );
        assert_eq!(parse(53, b""), Some(Op::FoldersGet));
        // Two flags, the second empty and dropped.
        let mut p = Vec::new();
        gshort(2, &mut p);
        gstring(b"server.a=1", &mut p);
        gstring(b"", &mut p);
        assert_eq!(parse(69, &p), Some(Op::FlagsSet(vec!["server.a=1".into()])));
        // A count longer than the flags is refused.
        assert_eq!(parse(69, &[0x20, 0x23, 0x21, b'x']), None);
        assert_eq!(parse(70, b""), None);
    }

    #[test]
    fn options_round_trip_and_warn() {
        let w = tmp("options");
        let f = Files::new(&w);
        let lines = vec![
            b"# a comment, with a comma".to_vec(),
            b"name = Lab".to_vec(),
            b"port = lots".to_vec(),
        ];
        let d = f.run(&Op::OptionsSet(lines.clone())).unwrap();
        let chat: Vec<String> = d.replies.iter().map(|r| text(body(r))).collect();
        assert_eq!(chat.len(), 2, "{chat:?}");
        assert!(chat[0].contains("line 3"), "{chat:?}");
        assert!(chat[1].contains("restarts"), "{chat:?}");
        let got = f.run(&Op::OptionsGet).unwrap().replies.remove(0);
        assert_eq!(got[0], PLO_RC_SERVEROPTIONSGET + 32);
        assert_eq!(detokenize(body(&got)), lines);
        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn folders_round_trip_and_a_missing_file_is_empty() {
        let w = tmp("folders");
        let f = Files::new(&w);
        let got = f.run(&Op::FoldersGet).unwrap().replies.remove(0);
        assert_eq!(got, [PLO_RC_FOLDERCONFIGGET + 32, b'\n']);
        let lines = vec![b"file levels/*.png".to_vec(), b"level levels/*.nw".to_vec()];
        f.run(&Op::FoldersSet(lines.clone())).unwrap();
        let got = f.run(&Op::FoldersGet).unwrap().replies.remove(0);
        assert_eq!(detokenize(body(&got)), lines);
        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn the_flag_list() {
        let m = flags_reply(&["server.a=1".into(), "x".repeat(300), "server.b".into()]);
        assert_eq!(m[0], PLO_RC_SERVERFLAGSGET + 32);
        assert_eq!(
            parse(PLI_RC_SERVERFLAGSSET, body(&m)),
            Some(Op::FlagsSet(vec!["server.a=1".into(), "server.b".into()]))
        );
    }
}
