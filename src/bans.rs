//! Bans (`docs/PLAN.md`, "Milestone: server-side anti-cheat").
//!
//! `config/bans.txt` in the world folder, one ban per line, tab-separated:
//!
//! ```text
//! key <TAB> until <TAB> admin <TAB> reason [<TAB> with]
//! ```
//!
//! The key is what is banned:
//!
//! - an account name (`Cheater`);
//! - `ip:` and an address (`ip:10.0.0.7`, `ip:2001:db8::7`);
//! - `machine:` and one of the machine ids a 5.00 or later client sends
//!   at login (`machine:3f2a...`, see [`crate::login::machine_ids`]).
//!
//! Account names cannot hold a `:`, so the three never mix. `until` is a
//! Unix time in seconds, or `-` for a ban with no end. `with` names the
//! account an address or machine ban came with (the anti-cheat writes
//! one), so lifting that account's ban lifts them too. Lines starting with
//! `#` and blank lines are skipped. Keys are compared without case. The
//! file is read at each login, so an admin can edit it while the server
//! runs; `seihai-gserver ban` and `unban` write it. A ban that has run out
//! is ignored (and dropped the next time the file is written).
//!
//! Address and machine bans do not keep out a staff account (`STAFF 1`),
//! so an admin who shares a machine or an address with a banned player,
//! or bans `ip:127.0.0.1` in a lab, can still log in. An account ban
//! holds for staff too.

use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One ban.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ban {
    /// What is banned: an account name, or an [`ip_key`] or
    /// [`machine_key`].
    pub account: String,
    /// Unix seconds; `None` for no end.
    pub until: Option<u64>,
    pub admin: String,
    pub reason: String,
    /// For an address or machine ban, the account it came with; empty
    /// otherwise.
    pub with: String,
}

/// The ban key for an address. An IPv4 address seen through an IPv6
/// socket (`::ffff:10.0.0.7`) is keyed as the IPv4 address.
pub fn ip_key(ip: IpAddr) -> String {
    format!("ip:{}", ip.to_canonical())
}

/// The ban key for one machine id.
pub fn machine_key(id: &str) -> String {
    format!("machine:{}", id.to_ascii_lowercase())
}

/// A key as an admin typed it (`seihai-gserver ban`): an account name,
/// `ip:ADDR` or `machine:ID`, put in the form the file keeps. `None` when
/// it is none of these.
pub fn parse_key(key: &str) -> Option<String> {
    if let Some(ip) = key.strip_prefix("ip:") {
        return ip.parse::<IpAddr>().ok().map(ip_key);
    }
    if let Some(id) = key.strip_prefix("machine:") {
        return crate::login::valid_machine_id(id).then(|| machine_key(id));
    }
    crate::login::valid_account_name(key).then(|| key.to_string())
}

/// Who is logging in, for [`Bans::find_login`].
#[derive(Debug, Clone, Default)]
pub struct Who<'a> {
    pub account: &'a str,
    pub ip: Option<IpAddr>,
    /// The machine ids from the login line ([`crate::login::machine_ids`]).
    pub machines: &'a [String],
    /// A staff account: address and machine bans do not hold for it.
    pub staff: bool,
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
        let mut l = format!(
            "{}\t{}\t{}\t{}",
            self.account,
            self.until.map_or("-".to_string(), |u| u.to_string()),
            clean(&self.admin),
            clean(&self.reason)
        );
        if !self.with.is_empty() {
            l.push('\t');
            l.push_str(&clean(&self.with));
        }
        l
    }
}

/// Unix seconds now.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A time span as people say it: `3 days`, `5 hours`, `10 minutes`.
pub fn span(secs: u64) -> String {
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

    /// The ban that keeps this login out now, if any: one on the account,
    /// or, for an account that is not staff, one on its address or on any
    /// of its machine ids.
    pub fn find_login(&self, who: &Who) -> io::Result<Option<Ban>> {
        let now = now();
        let mut keys = vec![who.account.to_string()];
        if !who.staff {
            keys.extend(who.ip.map(ip_key));
            keys.extend(who.machines.iter().map(|m| machine_key(m)));
        }
        let live: Vec<Ban> = self.read()?.into_iter().filter(|b| b.holds(now)).collect();
        // The account's own ban first, so its reason is the one shown.
        Ok(keys.iter().find_map(|k| {
            live.iter()
                .find(|b| b.account.eq_ignore_ascii_case(k))
                .cloned()
        }))
    }

    /// Add a ban, replacing one on the same key.
    pub fn add(&self, ban: Ban) -> io::Result<()> {
        self.add_all(vec![ban])
    }

    /// Add several bans in one write, each replacing one on the same key.
    pub fn add_all(&self, bans: Vec<Ban>) -> io::Result<()> {
        let mut all = self.live()?;
        all.retain(|b| {
            !bans
                .iter()
                .any(|n| n.account.eq_ignore_ascii_case(&b.account))
        });
        all.extend(bans);
        self.write(&all)
    }

    /// Lift the ban on `key`, and the address and machine bans that came
    /// with it when it is an account; whether there was any.
    pub fn remove(&self, key: &str) -> io::Result<bool> {
        let mut all = self.live()?;
        let before = all.len();
        all.retain(|b| !b.account.eq_ignore_ascii_case(key) && !b.with.eq_ignore_ascii_case(key));
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
            String::from("# account, ip:ADDR or machine:ID\tuntil (unix seconds, - for no end)\tadmin\treason\t[account it came with]\n");
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
    let mut f = line.splitn(5, '\t');
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
        with: f.next().unwrap_or("").trim().to_string(),
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
            with: String::new(),
        })
        .unwrap();
        bans.add(Ban {
            account: "old".into(),
            until: Some(1),
            admin: "boss".into(),
            reason: String::new(),
            with: String::new(),
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
    fn keys() {
        assert_eq!(parse_key("Cheater").as_deref(), Some("Cheater"));
        assert_eq!(parse_key("ip:10.0.0.7").as_deref(), Some("ip:10.0.0.7"));
        assert_eq!(
            parse_key("ip:::ffff:10.0.0.7").as_deref(),
            Some("ip:10.0.0.7")
        );
        assert_eq!(
            parse_key("ip:2001:DB8::7").as_deref(),
            Some("ip:2001:db8::7")
        );
        assert_eq!(
            parse_key("machine:ABCD1234").as_deref(),
            Some("machine:abcd1234")
        );
        for bad in ["ip:nope", "machine:", "machine:a b", "../x", "x:y"] {
            assert_eq!(parse_key(bad), None, "{bad}");
        }
        // The account a machine ban came with goes in the fifth column.
        let w = dir("keys");
        let bans = Bans::new(&w);
        bans.add(Ban {
            account: machine_key("ABCD1234"),
            until: None,
            admin: "anti-cheat".into(),
            reason: "anti-cheat: speed".into(),
            with: "Cheater".into(),
        })
        .unwrap();
        let text = std::fs::read_to_string(bans.path()).unwrap();
        assert!(text.ends_with("machine:abcd1234\t-\tanti-cheat\tanti-cheat: speed\tCheater\n"));
        assert_eq!(bans.read().unwrap()[0].with, "Cheater");
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
