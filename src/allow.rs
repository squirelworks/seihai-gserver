//! The version allow-list, `config/allowedversions.txt`.
//!
//! One entry per line; `#` starts a comment:
//!
//! ```text
//! *                    every build seihai-core has a profile for
//! 2.05                 a build by its seihai-core label
//! 1.33..1.39           labels from .. to, in seihai-core's build order
//! GNW03081             a version token
//! GNW24011:GNW28015    tokens from : to, in the same order
//! ```
//!
//! A line of the form `name = entry` counts as `entry` and a `[section]`
//! line is skipped, so a GServer-v2-style file with ranges by generation
//! reads as the union of its ranges. A token no profile has can still be
//! listed on its own; such a client gets the closest known build's login
//! burst for its generation.

use seihai_core::protocol::ClientVersion;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default)]
pub struct AllowList {
    tokens: BTreeSet<String>,
}

impl AllowList {
    /// Every build seihai-core knows, and the builds served as a near one.
    pub fn all_known() -> Self {
        AllowList {
            tokens: ClientVersion::all_versions()
                .iter()
                .map(|v| v.token().to_string())
                // The builds served as a near build (`login::STAND_INS`):
                // every catalogued game token with no profile of its own.
                // Without them `*` turned a real 1.27 away (lab round 3,
                // 2026-11-14) and 3.151 too (2026-11-19).
                .chain(
                    crate::login::STAND_INS
                        .iter()
                        .map(|(t, _, _)| t.to_string()),
                )
                .collect(),
        }
    }

    /// Read the file's text. Returns the list and a warning per line not taken.
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut list = AllowList::default();
        let mut warnings = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let mut line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            if let Some((_, v)) = line.split_once('=') {
                line = v.trim();
            }
            for entry in line.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                if let Err(e) = list.add(entry) {
                    warnings.push(format!("allowedversions.txt line {}: {e}", n + 1));
                }
            }
        }
        (list, warnings)
    }

    fn add(&mut self, entry: &str) -> Result<(), String> {
        if entry == "*" {
            self.tokens.extend(Self::all_known().tokens);
            return Ok(());
        }
        let range = entry.split_once("..").or_else(|| entry.split_once(':'));
        if let Some((a, b)) = range {
            let (a, b) = (index_of(a.trim())?, index_of(b.trim())?);
            let (lo, hi) = (a.min(b), a.max(b));
            let all = ClientVersion::all_versions();
            for v in &all[lo..=hi] {
                self.tokens.insert(v.token().to_string());
            }
            // The builds served as a near one whose stand-in is in the
            // range, the ends' own tokens among them.
            for (t, _, v) in crate::login::STAND_INS {
                if all
                    .iter()
                    .position(|x| x == v)
                    .is_some_and(|i| (lo..=hi).contains(&i))
                {
                    self.tokens.insert(t.to_string());
                }
            }
            return Ok(());
        }
        if let Some(v) = ClientVersion::from_label(entry) {
            self.tokens.insert(v.token().to_string());
            return Ok(());
        }
        if is_token(entry) {
            self.tokens.insert(entry.to_string());
            return Ok(());
        }
        Err(format!(
            "`{entry}` is not a build label, a version token or a range"
        ))
    }

    pub fn allows(&self, token: &str) -> bool {
        self.tokens.contains(token)
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Listed tokens that no seihai-core profile has and that are not
    /// served as a near build (`login::STAND_INS`).
    pub fn unknown_tokens(&self) -> Vec<String> {
        let known: BTreeSet<&str> = ClientVersion::all_versions()
            .iter()
            .map(|v| v.token())
            .chain(crate::login::STAND_INS.iter().map(|(t, _, _)| *t))
            .collect();
        self.tokens
            .iter()
            .filter(|t| !known.contains(t.as_str()))
            .cloned()
            .collect()
    }
}

fn is_token(s: &str) -> bool {
    s.len() == 8 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Position of a label or token in seihai-core's build order. A token
/// served as a near build (`login::STAND_INS`) takes its stand-in's
/// place: GServer-v2's sample range `G3D11125:G3D0511C` starts at the
/// 4.03r1 Linux token, which has no profile, and the whole range was
/// refused, locking 6.03r7 out of an imported world (lab 2026-11-20).
fn index_of(s: &str) -> Result<usize, String> {
    let all = ClientVersion::all_versions();
    let by_label = ClientVersion::from_label(s).and_then(|v| all.iter().position(|&x| x == v));
    let stand_in = || {
        crate::login::STAND_INS
            .iter()
            .find(|(t, _, _)| *t == s)
            .and_then(|(_, _, v)| all.iter().position(|x| x == v))
    };
    by_label
        .or_else(|| all.iter().position(|v| v.token() == s))
        .or_else(stand_in)
        .ok_or_else(|| format!("`{s}` is not a build label or token seihai-core knows"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries() {
        let (l, w) =
            AllowList::parse("# lab\n2.05\n1.33..1.39\nGNW03014 # 2.22\nZZZ12345\nbogus entry\n");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(l.allows("GNW03081"));
        assert!(l.allows("GNW20020") && l.allows("GNW07080")); // 1.33, 1.39
        assert!(!l.allows("GNW13110")); // 1.41
        assert!(l.allows("GNW03014"));
        assert!(l.allows("ZZZ12345"));
        assert_eq!(l.unknown_tokens(), vec!["ZZZ12345".to_string()]);
    }

    #[test]
    fn a_range_from_a_stand_in_token() {
        // GServer-v2's own sample range for the modern clients.
        let (l, w) = AllowList::parse("[versions]\nmodern = G3D11125:G3D0511C\n");
        assert!(w.is_empty(), "{w:?}");
        assert!(l.allows("G3D0511C"), "6.03r7 Linux");
        assert!(l.allows("G3D11125"), "the 4.03r1 Linux end itself");
        assert!(l.allows("G3D26090"), "5.31r4 in between");
        assert!(!l.allows("GNW03081"), "2.05 is before the range");
    }

    #[test]
    fn gserver_style_ranges() {
        let (l, w) = AllowList::parse(
            "[generation-range]\nclassic = GNW13110\nnewmain = GNW24011:GNW28015\n",
        );
        assert!(w.is_empty(), "{w:?}");
        assert!(l.allows("GNW13110"));
        assert!(l.allows("GNW24011") && l.allows("GNW03081") && l.allows("GNW28015"));
        assert!(!l.allows("GNW07080") && !l.allows("G3D03014"));
    }

    #[test]
    fn star_is_every_profile() {
        let (l, _) = AllowList::parse("*\n");
        for v in ClientVersion::all_versions() {
            assert!(l.allows(v.token()), "{v:?}");
        }
        // And the stand-ins: 1.25, 1.27, 1.40 rev 1, 2.19 to 2.21.
        for t in [
            "GSERV009", "GSERV011", "GNW23090", "GNW15053", "GNW28063", "GNW01113",
        ] {
            assert!(l.allows(t), "{t}");
        }
    }
}
