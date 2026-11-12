//! Account bans (`docs/PLAN.md`, "Milestone: server-side anti-cheat").
//!
//! `config/bans.txt` in the world folder, one ban per line, tab-separated:
//!
//! ```text
//! account <TAB> until <TAB> admin <TAB> reason
//! ```
//!
//! `until` is a Unix time in seconds, or `-` for a ban with no end. Lines
//! starting with `#` and blank lines are skipped. The account name is
//! compared without case. The file is read at each login, so an admin can
//! edit it while the server runs; `seihai-gserver ban` and `unban` write
//! it. A ban that has run out is ignored (and dropped the next time the
//! file is written). Bans by machine field (5.00 and later) come later.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One ban.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ban {
    pub account: String,
    /// Unix seconds; `None` for no end.
    pub until: Option<u64>,
    pub admin: String,
    pub reason: String,
}

impl Ban {
    /// Whether it still holds at `now` (Unix seconds).
    pub fn holds(&self, now: u64) -> bool {
        self.until.is_none_or(|u| now < u)
    }

    /// The text a banned player is sent.
    pub fn client_text(&self) -> String {
        let mut t = String::from("You are banned from this server");
        if let Some(u) = self.until {
            let left = u.saturating_sub(now());
            t.push_str(&format!(" for {}", span(left)));
        }
        if !self.reason.is_empty() {
            t.push_str(&format!(": {}", self.reason));
        }
        t.push('.');
        t
    }

    fn line(&self) -> String {
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
        format!(
            "{}\t{}\t{}\t{}",
            self.account,
            self.until.map_or("-".to_string(), |u| u.to_string()),
            clean(&self.admin),
            clean(&self.reason)
        )
    }
}

/// Unix seconds now.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A time span as people say it: `3 days`, `5 hours`, `10 minutes`.
fn span(secs: u64) -> String {
    let (n, unit) = match secs {
        s if s >= 2 * 86_400 => (s / 86_400, "day"),
        s if s >= 2 * 3600 => (s / 3600, "hour"),
        s => ((s / 60).max(1), "minute"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// `30m`, `12h`, `3d`, `2w` or plain seconds.
pub fn parse_span(s: &str) -> Option<u64> {
    let s = s.trim();
    let (n, mul) = match s.char_indices().last()? {
        (i, 'm') => (&s[..i], 60),
        (i, 'h') => (&s[..i], 3600),
        (i, 'd') => (&s[..i], 86_400),
        (i, 'w') => (&s[..i], 7 * 86_400),
        _ => (s, 1),
    };
    n.parse::<u64>().ok()?.checked_mul(mul).filter(|&v| v > 0)
}

/// The world's ban file.
#[derive(Debug, Clone)]
pub struct Bans {
    path: PathBuf,
}

impl Bans {
    pub fn new(world: &Path) -> Bans {
        Bans {
            path: world.join("config").join("bans.txt"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every ban in the file, run out or not. No file: none.
    pub fn read(&self) -> io::Result<Vec<Ban>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(text.lines().filter_map(parse_line).collect())
    }

    /// The ban on `account` that holds now, if any.
    pub fn find(&self, account: &str) -> io::Result<Option<Ban>> {
        let now = now();
        Ok(self
            .read()?
            .into_iter()
            .find(|b| b.account.eq_ignore_ascii_case(account) && b.holds(now)))
    }

    /// Add a ban, replacing one on the same account.
    pub fn add(&self, ban: Ban) -> io::Result<()> {
        let mut all = self.live()?;
        all.retain(|b| !b.account.eq_ignore_ascii_case(&ban.account));
        all.push(ban);
        self.write(&all)
    }

    /// Lift the ban on `account`; whether there was one.
    pub fn remove(&self, account: &str) -> io::Result<bool> {
        let mut all = self.live()?;
        let before = all.len();
        all.retain(|b| !b.account.eq_ignore_ascii_case(account));
        let found = all.len() != before;
        if found {
            self.write(&all)?;
        }
        Ok(found)
    }

    fn live(&self) -> io::Result<Vec<Ban>> {
        let now = now();
        Ok(self.read()?.into_iter().filter(|b| b.holds(now)).collect())
    }

    fn write(&self, bans: &[Ban]) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut text =
            String::from("# account\tuntil (unix seconds, - for no end)\tadmin\treason\n");
        for b in bans {
            text.push_str(&b.line());
            text.push('\n');
        }
        let tmp = self.path.with_extension("txt.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)
    }
}

fn parse_line(line: &str) -> Option<Ban> {
    let line = line.trim_end_matches('\r');
    if line.trim().is_empty() || line.starts_with('#') {
        return None;
    }
    let mut f = line.splitn(4, '\t');
    let account = f.next()?.trim().to_string();
    if account.is_empty() {
        return None;
    }
    let until = match f.next().map(str::trim) {
        None | Some("-") | Some("") => None,
        Some(u) => Some(u.parse().ok()?),
    };
    Some(Ban {
        account,
        until,
        admin: f.next().unwrap_or("").trim().to_string(),
        reason: f.next().unwrap_or("").trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("seihai-gserver-bans-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn add_find_and_lift() {
        let w = dir("rw");
        let bans = Bans::new(&w);
        assert_eq!(bans.find("Cheater").unwrap(), None);
        bans.add(Ban {
            account: "Cheater".into(),
            until: None,
            admin: "boss".into(),
            reason: "speed\thack".into(),
        })
        .unwrap();
        bans.add(Ban {
            account: "old".into(),
            until: Some(1),
            admin: "boss".into(),
            reason: String::new(),
        })
        .unwrap();
        let b = bans.find("cheater").unwrap().unwrap();
        assert_eq!(
            (b.admin.as_str(), b.reason.as_str()),
            ("boss", "speed hack")
        );
        assert_eq!(
            b.client_text(),
            "You are banned from this server: speed hack."
        );
        assert_eq!(bans.find("old").unwrap(), None, "run out");
        // A hand-written line with a time span left.
        let mut text = std::fs::read_to_string(bans.path()).unwrap();
        text.push_str(&format!(
            "timed\t{}\tboss\n# a note\n\n",
            now() + 3 * 86_400 + 60
        ));
        std::fs::write(bans.path(), text).unwrap();
        assert_eq!(
            bans.find("TIMED").unwrap().unwrap().client_text(),
            "You are banned from this server for 3 days."
        );
        assert!(bans.remove("CHEATER").unwrap());
        assert!(!bans.remove("cheater").unwrap());
        assert_eq!(bans.find("cheater").unwrap(), None);
        // The run-out one went when the file was written.
        assert!(bans.read().unwrap().iter().all(|b| b.account != "old"));
        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn spans() {
        assert_eq!(parse_span("30m"), Some(1800));
        assert_eq!(parse_span("12h"), Some(43_200));
        assert_eq!(parse_span("3d"), Some(259_200));
        assert_eq!(parse_span("2w"), Some(1_209_600));
        assert_eq!(parse_span("90"), Some(90));
        assert_eq!(parse_span("0d"), None);
        assert_eq!(parse_span("x"), None);
        assert_eq!(span(59), "1 minute");
        assert_eq!(span(5 * 3600), "5 hours");
    }
}
