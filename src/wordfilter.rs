//! The word filter: `config/wordfilter.txt` in the world folder.
//!
//! One entry per line, `#` starts a comment. A line is a word or phrase
//! to hide, matched without regard to ASCII case. `word = replacement`
//! puts that text in its place; without `=` each matched character
//! becomes `*`. The file is read again when its modification time
//! changes, so an RC upload of a new list takes effect on the next line.
//! No file, or an empty one, filters nothing.
//!
//! What it covers: a player's chat (prop 12), `PLI_TOALL` and the text of
//! a player's private message. It is this server's own design: the file
//! name and the line format are Seihai's, not GServer-v2's.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// One entry: the lower-case pattern and its replacement (`None` for
/// asterisks).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    pattern: Vec<u8>,
    replacement: Option<Vec<u8>>,
}

/// The parsed list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    entries: Vec<Entry>,
}

impl Rules {
    /// Parses the file's text (Latin-1 bytes).
    pub fn parse(text: &[u8]) -> Self {
        let mut entries = Vec::new();
        for line in text.split(|&b| b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = trim(line);
            if line.is_empty() || line[0] == b'#' {
                continue;
            }
            let (pat, rep) = match line.iter().position(|&b| b == b'=') {
                Some(i) => (trim(&line[..i]), Some(trim(&line[i + 1..]).to_vec())),
                None => (line, None),
            };
            if pat.is_empty() {
                continue;
            }
            entries.push(Entry {
                pattern: pat.to_ascii_lowercase(),
                replacement: rep,
            });
        }
        // Longer patterns first, so "badword" wins over "bad".
        entries.sort_by_key(|e| std::cmp::Reverse(e.pattern.len()));
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `text` with every match replaced, or `None` when nothing matched.
    pub fn apply(&self, text: &[u8]) -> Option<Vec<u8>> {
        if self.entries.is_empty() || text.is_empty() {
            return None;
        }
        let lower = text.to_ascii_lowercase();
        let mut out = Vec::with_capacity(text.len());
        let mut i = 0;
        let mut hit = false;
        'outer: while i < text.len() {
            for e in &self.entries {
                if lower[i..].starts_with(&e.pattern) {
                    match &e.replacement {
                        Some(r) => out.extend_from_slice(r),
                        None => out.extend(std::iter::repeat_n(b'*', e.pattern.len())),
                    }
                    i += e.pattern.len();
                    hit = true;
                    continue 'outer;
                }
            }
            out.push(text[i]);
            i += 1;
        }
        hit.then_some(out)
    }
}

fn trim(b: &[u8]) -> &[u8] {
    let s = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let e = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(s, |i| i + 1);
    &b[s..e]
}

/// The world's filter file, read again when it changes.
pub struct WordFilter {
    path: PathBuf,
    cached: Mutex<(Option<SystemTime>, Rules)>,
}

impl WordFilter {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cached: Mutex::new((None, Rules::default())),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `text` filtered, or `None` when it is unchanged.
    pub fn apply(&self, text: &[u8]) -> Option<Vec<u8>> {
        let modified = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        let mut c = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if c.0 != modified {
            let rules = match modified {
                Some(_) => std::fs::read(&self.path)
                    .map(|t| Rules::parse(&t))
                    .unwrap_or_default(),
                None => Rules::default(),
            };
            *c = (modified, rules);
        }
        c.1.apply(text)
    }

    /// A player-props stream with each chat (prop 12) filtered, or `None`
    /// when there is no chat or nothing in it matched. Other props and
    /// any bytes the parser stopped at are kept as sent.
    pub fn filter_props(&self, payload: &[u8], bow_prop10: bool) -> Option<Vec<u8>> {
        let props = seihai_core::protocol::parse_player_props_with(payload, bow_prop10);
        if !props.iter().any(|p| p.id == CURCHAT) {
            return None;
        }
        let mut matched = false;
        let mut out = Vec::with_capacity(payload.len() + 8);
        let mut used = 0;
        for p in &props {
            used += 1 + p.raw.len();
            out.push(p.id + 0x20);
            if let Some(new) = (p.id == CURCHAT)
                .then(|| p.raw.get(1..).and_then(|text| self.apply(text)))
                .flatten()
            {
                matched = true;
                // A GCHAR length: at most 223 bytes.
                let n = new.len().min(223);
                out.push(n as u8 + 0x20);
                out.extend_from_slice(&new[..n]);
            } else {
                out.extend_from_slice(&p.raw);
            }
        }
        out.extend_from_slice(&payload[used.min(payload.len())..]);
        matched.then_some(out)
    }
}

/// `PLPROP_CURCHAT`.
const CURCHAT: u8 = 12;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_and_replacements() {
        let r = Rules::parse(b"# comment\nheck\n\ndarn = drat\r\n  = nothing\n");
        assert_eq!(r.apply(b"Oh HECK, darn it").unwrap(), b"Oh ****, drat it");
        assert_eq!(r.apply(b"fine words"), None);
        assert!(Rules::parse(b"").is_empty());
    }

    #[test]
    fn longer_patterns_win() {
        let r = Rules::parse(b"bad\nbadword=x\n");
        assert_eq!(r.apply(b"a badword and bad").unwrap(), b"a x and ***");
    }

    #[test]
    fn reloads_when_the_file_changes() {
        let dir = std::env::temp_dir().join(format!("wf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wordfilter.txt");
        let f = WordFilter::new(path.clone());
        assert_eq!(f.apply(b"heck"), None);
        std::fs::write(&path, "heck\n").unwrap();
        assert_eq!(f.apply(b"heck").unwrap(), b"****");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chat_prop_is_rewritten() {
        let dir = std::env::temp_dir().join(format!("wfp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wordfilter.txt");
        std::fs::write(&path, "heck=h\n").unwrap();
        let f = WordFilter::new(path);
        // Prop 12 "oh heck", then prop 2 (a one-byte prop).
        let mut p = vec![12 + 0x20, 7 + 0x20];
        p.extend_from_slice(b"oh heck");
        p.extend_from_slice(&[2 + 0x20, 0x21]);
        let out = f.filter_props(&p, false).unwrap();
        let mut want = vec![12 + 0x20, 4 + 0x20];
        want.extend_from_slice(b"oh h");
        want.extend_from_slice(&[2 + 0x20, 0x21]);
        assert_eq!(out, want);
        // No chat: unchanged.
        assert_eq!(f.filter_props(&[2 + 0x20, 0x21], false), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn repeated_chat_filter_case(
        version: seihai_core::protocol::ClientVersion,
        chats: [&[u8]; 2],
        expected: [&[u8]; 2],
        should_match: bool,
    ) {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let stamp = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "seihai-repeated-chat-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let fixture = Fixture(dir);
        let path = fixture.0.join("wordfilter.txt");
        std::fs::write(&path, b"heck=h\ndarn=drat\n").unwrap();
        let filter = WordFilter::new(path);
        let bow = crate::others::bow_prop10(version);
        let stream = |chats: [&[u8]; 2]| {
            let mut out = vec![2 + 0x20, 6 + 0x20, CURCHAT + 0x20];
            crate::wire::gstring(chats[0], &mut out);
            out.push(10 + 0x20);
            if bow {
                out.push(1 + 0x20);
            } else {
                crate::wire::gstring(b"idle", &mut out);
            }
            out.push(CURCHAT + 0x20);
            crate::wire::gstring(chats[1], &mut out);
            out.extend_from_slice(&[17 + 0x20, 2 + 0x20]);
            // The parser stops here; the existing filter contract retains this tail.
            out.extend_from_slice(&[0xff, 0x41, 0x42]);
            out
        };
        let input = stream(chats);
        let before = seihai_core::protocol::parse_player_props_with(&input, bow);
        assert_eq!(
            before.iter().map(|p| p.id).collect::<Vec<_>>(),
            [2, 12, 10, 12, 17]
        );
        let filtered = filter.filter_props(&input, bow);
        let delivered = filtered.as_deref().unwrap_or(&input);
        // This is the production receiver used by WorldTask after server filtering.
        let mut model = crate::others::Props::default();
        model.apply(delivered, version);
        let mut last = Vec::new();
        crate::wire::gstring(expected[1], &mut last);
        assert_eq!(model.values().get(&CURCHAT), Some(&last), "{version:?}");
        assert_eq!(filtered.is_some(), should_match, "{version:?}");
        assert_eq!(delivered, stream(expected), "{version:?}");
        let after = seihai_core::protocol::parse_player_props_with(delivered, bow);
        for id in [2, 10, 17] {
            assert_eq!(
                before.iter().find(|p| p.id == id).unwrap().raw,
                after.iter().find(|p| p.id == id).unwrap().raw,
                "non-chat prop {id}, {version:?}"
            );
        }
        assert!(delivered.ends_with(&[0xff, 0x41, 0x42]));
    }

    #[test]
    fn a_later_repeated_chat_is_filtered_after_an_unmatched_first_chat() {
        use seihai_core::protocol::ClientVersion::{V139, V222};
        for version in [V139, V222] {
            repeated_chat_filter_case(version, [b"hello", b"fine"], [b"hello", b"fine"], false);
            repeated_chat_filter_case(version, [b"hello", b"HECK"], [b"hello", b"h"], true);
        }
    }

    #[test]
    fn repeated_chats_keep_their_own_filtered_or_unmatched_text() {
        use seihai_core::protocol::ClientVersion::{V139, V222};
        for version in [V139, V222] {
            repeated_chat_filter_case(version, [b"Heck", b"hello"], [b"h", b"hello"], true);
            repeated_chat_filter_case(version, [b"Heck", b"DARN"], [b"h", b"drat"], true);
        }
    }
}
