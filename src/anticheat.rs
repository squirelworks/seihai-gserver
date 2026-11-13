//! Server-side anti-cheat (stage 2.9, the first part of stage 7 in
//! `docs/PLAN.md`): the checks report a violation of a [`Kind`], and the
//! [`Policy`] the admin set decides what happens. Nothing runs on the
//! client, so every build works unchanged.
//!
//! Checks so far:
//! - **Speed** and **teleport** (`world.rs`): each player's position is
//!   followed in gmap coordinates, so walking across a seam is a step like
//!   any other. A step longer than [`TELEPORT`] tiles is a teleport; else
//!   the step is paid from a budget that fills at [`MAX_SPEED`] tiles a
//!   second up to [`BURST`] tiles, so steps a client batches after a stall
//!   pass. Moves the server makes (a warp's arrival, a login) start afresh,
//!   and the first position after a warp that still names the spot left is
//!   dropped (props the client queued before the warp).
//! - **Warp** (`world.rs`): `PLI_LEVELWARP`/`PLI_LEVELWARPMOD` only for a
//!   link in the player's level to that level, which the player stands on
//!   (with [`LINK_SLACK`] tiles for a position a frame old), and to the
//!   link's arrival; a gmap crossing only into a cell next to the player's.
//! - **Items** (`world.rs`): the server keeps the counted props (max
//!   hearts 1, hearts 2, gralats 3, arrows 4, bombs 5). A client's raise
//!   of one is a violation unless the server already gave it: a floor item
//!   the player took first (`PLI_ITEMTAKE`, or `PLI_ITEMDEL` from a build
//!   before 1.38, which gives itself the item), a chest, the NPC-server,
//!   or hearts back after a death (up to the max). Correcting sends the
//!   server's value back and passes the raise on to nobody. Logged only by
//!   default: classic level scripts run on the client and may grant
//!   gralats the server never hears of.
//! - **Hurt** (`world.rs`): a `PLI_HURTPLAYER` (26) from an attacker more
//!   than [`HURT_RANGE`] tiles from the victim. Correcting drops the hit.
//!   Real 2.05 and 2.22 clients do not send 26 for a sword: the victim's
//!   client hurts itself when it sees the attacker's `sword` gani (prop
//!   10), so this only covers hits a client claims for another player.
//! - **Rate** (`server.rs`): more than `anticheat_rate` messages in one
//!   second on a connection, file checks (`PLI_WANTFILE`, `PLI_UPDATEFILE`)
//!   left out: a client checks its whole cache at login. One run of
//!   messages over the limit is one violation.
//!
//! Actions: `off`, `log` (a line in the log file), `correct` (also put it
//! back: the position is sent back to the player and not passed on, the
//! warp is not answered, the message is dropped), and on top of either
//! `kick after N in S` (N violations of that kind within S seconds) and
//! `ban after N in S` or `ban SPAN after N in S` (`SPAN` as `30m`, `12h`,
//! `3d`, `2w`): the account goes in `config/bans.txt` with `anti-cheat` as
//! the admin and the kind as the reason, and is kicked. The machine ids
//! its client sent at login (5.00 and later) are banned with it, so a new
//! account on the same machine stays out too; lifting the account's ban
//! lifts them. The address is not banned, since players share addresses
//! and get new ones; an admin bans one by hand. `staff exempt`
//! leaves staff accounts (`STAFF 1`) out of that check.
//!
//! **Off by default.** A check can only guess, and a wrong guess hurts an
//! honest player more than it stops a cheat, so nothing runs until the
//! admin sets `anticheat = on`. Even then every check only logs until the
//! admin picks `correct` or a kick for it. Bans (`bans.rs`) are moderation
//! and work either way.
//!
//! Settings in `config/server.txt`:
//!
//! ```text
//! anticheat = on
//! anticheat_log = logs/anticheat.log
//! anticheat_rate = 200
//! anticheat.speed = correct, kick after 5 in 60
//! anticheat.teleport = correct
//! anticheat.warp = log
//! anticheat.rate = correct, kick after 3 in 10
//! anticheat.hurt = correct
//! anticheat.items = log, ban 1d after 10 in 600, staff exempt
//! ```

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Tiles a second the speed check allows. A walk is half a tile every
/// 50 ms tick, 10 tiles a second (14 on a diagonal), as read live in 2.05
/// rev 2 (Seihai's `WALK_STEP_MS`); this leaves room over the diagonal.
/// The lab run of 2026-11-12 (1.39, 2.05 rev 2, 2.17.1, 2.22, 2.31) saw at
/// most 15.6 tiles in any second of walking, and 1.58 tiles in one step.
pub const MAX_SPEED: f32 = 24.0;
/// The most the speed budget holds, in tiles: steps that the server reads
/// together after a stall on the server or the network (a frozen client
/// does not catch up). The lab run of 2026-11-12 read 31 tiles at once
/// after a 3 s stall and 38 after 5 s; 30 kicked a 2.05 for the first, so
/// this holds about 6 s of walking.
pub const BURST: f32 = 60.0;
/// A longer step in one update is a teleport, in tiles.
pub const TELEPORT: f32 = 16.0;
/// How far from a link a warp through it may start, in tiles.
pub const LINK_SLACK: f32 = 3.0;
/// How far from its victim a hit may come, in tiles: an arrow's flight
/// across the screen, with room. A first guess, like the speed figures.
pub const HURT_RANGE: f32 = 24.0;

/// What was broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Speed,
    Teleport,
    Warp,
    Rate,
    /// A counted prop (hearts, max hearts, gralats, arrows, bombs) raised
    /// with no source the server knows.
    Items,
    /// A hit on another player from too far away.
    Hurt,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Speed,
        Kind::Teleport,
        Kind::Warp,
        Kind::Rate,
        Kind::Items,
        Kind::Hurt,
    ];

    /// The name in the settings and the log.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Speed => "speed",
            Kind::Teleport => "teleport",
            Kind::Warp => "warp",
            Kind::Rate => "rate",
            Kind::Items => "items",
            Kind::Hurt => "hurt",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// What a violation of a kind does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Off,
    Log,
    Correct,
}

/// The setting for one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub action: Action,
    /// Kick after this many violations within this long.
    pub kick_after: Option<(u32, Duration)>,
    /// Ban (and kick) after this many violations within this long, for
    /// this many seconds (`None`: no end).
    pub ban_after: Option<(u32, Duration, Option<u64>)>,
    /// Staff accounts are not checked for this kind.
    pub staff_exempt: bool,
}

impl Rule {
    const LOG: Rule = Rule {
        action: Action::Log,
        kick_after: None,
        ban_after: None,
        staff_exempt: false,
    };
    const OFF: Rule = Rule {
        action: Action::Off,
        ..Rule::LOG
    };

    /// `off`, `log` or `correct`, then any of `kick after N in S`,
    /// `ban after N in S` (no end), `ban SPAN after N in S` (`SPAN` as
    /// `30m`, `12h`, `3d`, `2w` or seconds) and `staff exempt`, each after
    /// a comma.
    pub fn parse(v: &str) -> Result<Rule, String> {
        let mut parts = v.split(',').map(str::trim);
        let action = match parts.next().unwrap_or("").to_ascii_lowercase().as_str() {
            "off" => Action::Off,
            "log" => Action::Log,
            "correct" => Action::Correct,
            other => return Err(format!("`{other}`: expected off, log or correct")),
        };
        let mut rule = Rule {
            action,
            ..Rule::LOG
        };
        let count = |n: &str, s: &str| -> Result<(u32, Duration), String> {
            let n: u32 = n.parse().map_err(|_| format!("`{n}`: not a count"))?;
            let s: u64 = s.parse().map_err(|_| format!("`{s}`: not seconds"))?;
            if n == 0 || s == 0 {
                return Err("after needs a count and seconds above 0".into());
            }
            Ok((n, Duration::from_secs(s)))
        };
        for k in parts {
            let lower = k.to_ascii_lowercase();
            let w: Vec<&str> = lower.split_whitespace().collect();
            match w.as_slice() {
                ["kick", "after", n, "in", s] if rule.kick_after.is_none() => {
                    rule.kick_after = Some(count(n, s)?);
                }
                ["ban", "after", n, "in", s] if rule.ban_after.is_none() => {
                    let (n, s) = count(n, s)?;
                    rule.ban_after = Some((n, s, None));
                }
                ["ban", span, "after", n, "in", s] if rule.ban_after.is_none() => {
                    let len = crate::bans::parse_span(span)
                        .ok_or_else(|| format!("`{span}`: not a time like 3d"))?;
                    let (n, s) = count(n, s)?;
                    rule.ban_after = Some((n, s, Some(len)));
                }
                ["staff", "exempt"] if !rule.staff_exempt => rule.staff_exempt = true,
                _ => {
                    return Err(format!(
                        "`{k}`: expected `kick after N in S`, `ban [SPAN] after N in S` \
                         or `staff exempt`, each once"
                    ))
                }
            }
        }
        if action == Action::Off && (rule.kick_after.is_some() || rule.ban_after.is_some()) {
            return Err("`off` cannot kick or ban".into());
        }
        Ok(rule)
    }
}

/// The admin's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    pub enabled: bool,
    /// The log file, relative to the world folder unless absolute.
    pub log: PathBuf,
    /// Messages a second a connection may send.
    pub rate: u32,
    rules: [Rule; 6],
}

impl Default for Policy {
    /// Off, and every check only logs once it is turned on.
    fn default() -> Self {
        Policy {
            enabled: false,
            log: PathBuf::from("logs/anticheat.log"),
            rate: 200,
            rules: [Rule::LOG; 6],
        }
    }
}

impl Policy {
    pub fn rule(&self, kind: Kind) -> Rule {
        if !self.enabled {
            return Rule::OFF;
        }
        self.rules[kind.index()]
    }

    /// One `config/server.txt` key, if it is an anti-cheat key: `None` for
    /// another key.
    pub fn set(&mut self, key: &str, v: &str) -> Option<Result<(), String>> {
        let r = match key {
            "anticheat" => match v.to_ascii_lowercase().as_str() {
                "on" | "yes" | "true" => {
                    self.enabled = true;
                    Ok(())
                }
                "off" | "no" | "false" => {
                    self.enabled = false;
                    Ok(())
                }
                _ => Err(format!("`{v}`: expected on or off")),
            },
            "anticheat_log" => {
                self.log = PathBuf::from(v);
                Ok(())
            }
            "anticheat_rate" => match v.parse() {
                Ok(n) if n > 0 => {
                    self.rate = n;
                    Ok(())
                }
                _ => Err(format!("`{v}`: expected messages a second above 0")),
            },
            _ => {
                let name = key.strip_prefix("anticheat.")?;
                let Some(kind) = Kind::ALL.into_iter().find(|k| k.name() == name) else {
                    return Some(Err(format!("no anti-cheat check called `{name}`")));
                };
                Rule::parse(v).map(|r| self.rules[kind.index()] = r)
            }
        };
        Some(r)
    }
}

/// What to do about one violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Verdict {
    pub log: bool,
    pub correct: bool,
    pub kick: bool,
    /// Ban the account (a ban also kicks): for this many seconds, or with
    /// no end for `Some(None)`.
    pub ban: Option<Option<u64>>,
}

impl Verdict {
    /// What goes after the violation in the log line.
    pub fn suffix(&self) -> String {
        match self.ban {
            Some(None) => "; banned".into(),
            Some(Some(s)) => format!("; banned for {}", crate::bans::span(s)),
            None if self.kick => "; kicked".into(),
            None => String::new(),
        }
    }
}

/// One player's recent violations, by kind.
#[derive(Debug, Default)]
pub struct Score {
    recent: [VecDeque<Instant>; 6],
}

impl Score {
    /// A violation of `kind` at `now` by a player (`staff` for a staff
    /// account): what the policy says to do.
    pub fn report(&mut self, policy: &Policy, kind: Kind, now: Instant, staff: bool) -> Verdict {
        let rule = policy.rule(kind);
        if rule.action == Action::Off || (staff && rule.staff_exempt) {
            return Verdict::default();
        }
        // Kept as long as the longer of the two windows needs.
        let keep = [rule.kick_after.map(|k| k.1), rule.ban_after.map(|b| b.1)]
            .into_iter()
            .flatten()
            .max();
        let q = &mut self.recent[kind.index()];
        let (mut kick, mut ban) = (false, None);
        if let Some(keep) = keep {
            q.push_back(now);
            while q
                .front()
                .is_some_and(|&t| now.saturating_duration_since(t) > keep)
            {
                q.pop_front();
            }
            let seen = |within: Duration| {
                q.iter()
                    .filter(|&&t| now.saturating_duration_since(t) <= within)
                    .count()
            };
            if let Some((n, within)) = rule.kick_after {
                kick = seen(within) >= n as usize;
            }
            if let Some((n, within, len)) = rule.ban_after {
                if seen(within) >= n as usize {
                    ban = Some(len);
                    kick = true;
                }
            }
        }
        Verdict {
            log: true,
            correct: rule.action == Action::Correct,
            kick,
            ban,
        }
    }
}

/// A player's position as the speed and teleport checks follow it.
#[derive(Debug, Clone)]
pub struct Walk {
    x: f32,
    y: f32,
    at: Instant,
    budget: f32,
}

impl Walk {
    /// A position the server put the player at.
    pub fn at(x: f32, y: f32, now: Instant) -> Walk {
        Walk {
            x,
            y,
            at: now,
            budget: BURST,
        }
    }

    /// A step to `x`, `y` (gmap coordinates): `None` when it is allowed,
    /// which also makes it the new position; else the kind and the
    /// distance, and the position stays.
    pub fn step(&mut self, x: f32, y: f32, now: Instant) -> Option<(Kind, f32)> {
        let d = (x - self.x).hypot(y - self.y);
        let dt = now.saturating_duration_since(self.at).as_secs_f32();
        let budget = (self.budget + dt * MAX_SPEED).min(BURST);
        let broke = if d > TELEPORT {
            Some(Kind::Teleport)
        } else if d > budget {
            Some(Kind::Speed)
        } else {
            None
        };
        // The time passes either way, so a refused step does not make
        // the next honest one look faster.
        self.at = now;
        self.budget = budget;
        if broke.is_some() {
            return broke.map(|k| (k, d));
        }
        self.budget -= d;
        self.x = x;
        self.y = y;
        None
    }

    /// Take the position after all (a violation that is only logged). The
    /// budget is left as it was, so steps the server reads in a backlog
    /// after it are not all counted against an empty one.
    pub fn accept(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
    }

    pub fn pos(&self) -> (f32, f32) {
        (self.x, self.y)
    }
}

/// Messages a connection sent in the last second.
#[derive(Debug, Default)]
pub struct Rate {
    seen: VecDeque<Instant>,
}

impl Rate {
    /// One message at `now`: whether it is over `limit` a second.
    pub fn over(&mut self, limit: u32, now: Instant) -> bool {
        while self
            .seen
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= Duration::from_secs(1))
        {
            self.seen.pop_front();
        }
        if self.seen.len() >= limit as usize {
            return true;
        }
        self.seen.push_back(now);
        false
    }
}

/// The player a verdict is about, for [`AntiCheat::enforce`].
#[derive(Debug, Clone, Copy)]
pub struct Offender<'a> {
    pub account: &'a str,
    /// The machine ids from the login line (`login::machine_ids`).
    pub machines: &'a [String],
    pub staff: bool,
}

/// The policy and the log file, shared by the world and the connections.
#[derive(Debug)]
pub struct AntiCheat {
    pub policy: Policy,
    /// The world folder, for the ban file.
    world: PathBuf,
    path: PathBuf,
    file: Mutex<Option<std::fs::File>>,
}

impl AntiCheat {
    pub fn new(policy: Policy, world: &Path) -> AntiCheat {
        let path = world.join(&policy.log);
        AntiCheat {
            policy,
            world: world.to_path_buf(),
            path,
            file: Mutex::new(None),
        }
    }

    /// One line in the log: Unix time, account, build, level, kind and
    /// what was seen, tab separated.
    pub fn log(&self, account: &str, build: &str, level: &str, kind: Kind, what: &str) {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let line = format!(
            "{t}\t{account}\t{build}\t{level}\t{}\t{what}\n",
            kind.name()
        );
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if f.is_none() {
            if let Some(dir) = self.path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            *f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .map_err(|e| eprintln!("anti-cheat log {}: {e}", self.path.display()))
                .ok();
        }
        if let Some(file) = f.as_mut() {
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// What a verdict says beyond the log: a ban goes in the world's
    /// `config/bans.txt`, with `anti-cheat` as the admin and the kind as
    /// the reason, on the account and on each of its `machines` (none for
    /// a staff account, whom machine bans do not hold). Returns whether to
    /// close the connection.
    pub fn enforce(&self, who: &Offender, kind: Kind, v: &Verdict) -> bool {
        if let Some(len) = v.ban {
            let account = who.account;
            let until = len.map(|s| crate::bans::now().saturating_add(s));
            let ban = |key: String, with: &str| crate::bans::Ban {
                account: key,
                until,
                admin: "anti-cheat".into(),
                reason: format!("anti-cheat: {}", kind.name()),
                with: with.to_string(),
            };
            let mut all = vec![ban(account.to_string(), "")];
            if !who.staff {
                all.extend(
                    who.machines
                        .iter()
                        .map(|m| ban(crate::bans::machine_key(m), account)),
                );
            }
            if let Err(e) = crate::bans::Bans::new(&self.world).add_all(all) {
                eprintln!("anti-cheat ban of {account}: {e}");
            }
        }
        v.kick
    }

    /// Where the log goes.
    pub fn log_path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_read_and_refuse() {
        assert_eq!(
            Rule::parse("correct, kick after 5 in 60"),
            Ok(Rule {
                action: Action::Correct,
                kick_after: Some((5, Duration::from_secs(60))),
                ban_after: None,
                staff_exempt: false,
            })
        );
        assert_eq!(Rule::parse("LOG").map(|r| r.action), Ok(Action::Log));
        for bad in [
            "kick",
            "off, kick after 1 in 1",
            "log, kick after 0 in 5",
            "log, ban",
        ] {
            assert!(Rule::parse(bad).is_err(), "{bad}");
        }
        let mut p = Policy::default();
        assert_eq!(p.rule(Kind::Warp).action, Action::Off, "off by default");
        assert_eq!(p.set("anticheat", "on"), Some(Ok(())));
        assert_eq!(p.set("name", "x"), None);
        assert_eq!(p.set("anticheat.warp", "log"), Some(Ok(())));
        assert_eq!(p.rule(Kind::Warp).action, Action::Log);
        assert!(p.set("anticheat.fly", "log").unwrap().is_err());
        assert_eq!(p.set("anticheat", "off"), Some(Ok(())));
        assert_eq!(p.rule(Kind::Teleport).action, Action::Off);
    }

    #[test]
    fn the_defaults_do_nothing_and_then_only_log() {
        let mut p = Policy::default();
        let mut s = Score::default();
        let now = Instant::now();
        for kind in Kind::ALL {
            assert_eq!(s.report(&p, kind, now, false), Verdict::default());
        }
        p.set("anticheat", "on");
        for kind in Kind::ALL {
            for _ in 0..100 {
                let v = s.report(&p, kind, now, false);
                assert!(v.log && !v.correct && !v.kick, "{kind:?}");
            }
        }
    }

    #[test]
    fn kicks_count_within_the_window() {
        let mut p = Policy::default();
        p.set("anticheat", "on");
        p.set("anticheat.speed", "log, kick after 3 in 10");
        let mut s = Score::default();
        let t = Instant::now();
        let at = |secs| t + Duration::from_secs(secs);
        assert!(!s.report(&p, Kind::Speed, at(0), false).kick);
        assert!(!s.report(&p, Kind::Speed, at(5), false).kick);
        // The first has aged out.
        assert!(!s.report(&p, Kind::Speed, at(12), false).kick);
        assert!(s.report(&p, Kind::Speed, at(13), false).kick);
    }

    #[test]
    fn bans_and_staff() {
        let r = Rule::parse("correct, ban 3d after 2 in 60, Kick after 1 in 10, staff exempt");
        assert_eq!(
            r,
            Ok(Rule {
                action: Action::Correct,
                kick_after: Some((1, Duration::from_secs(10))),
                ban_after: Some((2, Duration::from_secs(60), Some(3 * 86_400))),
                staff_exempt: true,
            })
        );
        for bad in [
            "log, ban after 1 in 10, ban after 2 in 10",
            "log, ban soon after 1 in 10",
            "off, ban after 1 in 10",
            "log, staff exempt, staff exempt",
        ] {
            assert!(Rule::parse(bad).is_err(), "{bad}");
        }

        let mut p = Policy::default();
        p.set("anticheat", "on");
        p.set(
            "anticheat.teleport",
            "log, kick after 1 in 5, ban after 3 in 60",
        );
        p.set(
            "anticheat.warp",
            "correct, ban 1h after 1 in 1, staff exempt",
        );
        let mut s = Score::default();
        let t = Instant::now();
        let at = |secs| t + Duration::from_secs(secs);
        // The kick window is short, the ban window long: the third within
        // a minute bans, with no end.
        let v = s.report(&p, Kind::Teleport, at(0), false);
        assert!(v.kick && v.ban.is_none());
        assert!(s.report(&p, Kind::Teleport, at(20), false).ban.is_none());
        let v = s.report(&p, Kind::Teleport, at(40), false);
        assert_eq!((v.kick, v.ban), (true, Some(None)));
        assert_eq!(v.suffix(), "; banned");
        // Staff are not checked where the admin says so, others are.
        assert_eq!(s.report(&p, Kind::Warp, at(0), true), Verdict::default());
        let v = s.report(&p, Kind::Warp, at(0), false);
        assert_eq!((v.correct, v.kick, v.ban), (true, true, Some(Some(3600))));
        assert_eq!(v.suffix(), "; banned for 60 minutes");
        // Exempt only for that kind.
        assert!(s.report(&p, Kind::Teleport, at(41), true).log);

        // The ban goes in the world's ban file.
        let w = std::env::temp_dir().join(format!("seihai-ac-ban-{}", std::process::id()));
        let ac = AntiCheat::new(p.clone(), &w);
        let machines = vec!["aaaa1111".to_string(), "bbbb2222".to_string()];
        let dee = Offender {
            account: "Dee",
            machines: &machines,
            staff: false,
        };
        assert!(ac.enforce(&dee, Kind::Warp, &v));
        let bans = crate::bans::Bans::new(&w);
        let ban = bans.find("dee").unwrap().unwrap();
        assert_eq!(ban.admin, "anti-cheat");
        assert_eq!(ban.reason, "anti-cheat: warp");
        assert!(ban.until.is_some_and(|u| u > crate::bans::now() + 3500));
        // Her machines went in with her: a new account on one is kept out.
        fn fresh(machines: &[String], staff: bool) -> crate::bans::Who<'_> {
            crate::bans::Who {
                account: "NewAlt",
                ip: None,
                machines,
                staff,
            }
        }
        let other = ["bbbb2222".to_string()];
        let b = bans.find_login(&fresh(&other, false)).unwrap().unwrap();
        assert_eq!(
            (b.account.as_str(), b.with.as_str()),
            ("machine:bbbb2222", "Dee")
        );
        assert_eq!(b.until, ban.until);
        // Not a staff account, nor another machine.
        assert!(bans.find_login(&fresh(&other, true)).unwrap().is_none());
        assert!(bans
            .find_login(&fresh(&["cccc3333".into()], false))
            .unwrap()
            .is_none());
        // Lifting her ban lifts the machines.
        assert!(bans.remove("DEE").unwrap());
        assert!(bans.read().unwrap().is_empty());
        // A staff account that a non-exempt check bans: the account only.
        let boss = Offender {
            account: "Boss",
            machines: &machines,
            staff: true,
        };
        assert!(ac.enforce(&boss, Kind::Warp, &v));
        let keys: Vec<String> = bans
            .read()
            .unwrap()
            .into_iter()
            .map(|b| b.account)
            .collect();
        assert_eq!(keys, ["Boss"]);
        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn walking_passes_and_jumps_do_not() {
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        let mut w = Walk::at(2.0, 2.0, t);
        // A real diagonal walk, half a tile on each axis every 50 ms tick,
        // sent every 100 ms, for 6 s.
        for i in 1..=60u64 {
            let v = 2.0 + i as f32;
            assert_eq!(w.step(v, v, ms(i * 100)), None, "step {i}");
        }
        // Five seconds and more of steps the server reads at once after a
        // stall.
        let (x, y) = w.pos();
        for i in 1..=55 {
            assert_eq!(w.step(x - i as f32, y, ms(9_000)), None, "backlog {i}");
        }
        // Steps too long for the time: speed, and the position stays.
        let (x, y) = w.pos();
        assert_eq!(
            w.step(x + 14.0, y, ms(9_050)).map(|v| v.0),
            Some(Kind::Speed)
        );
        assert_eq!(w.pos(), (x, y));
        // Logged only: taken, with the budget left as it was.
        w.accept(x + 14.0, y);
        assert_eq!(w.step(x + 14.5, y, ms(9_100)), None);
        // Across the map in one update: a teleport.
        assert_eq!(
            w.step(2.0, 2.0, ms(30_000)).map(|v| v.0),
            Some(Kind::Teleport)
        );
    }

    #[test]
    fn the_rate_is_per_second() {
        let t = Instant::now();
        let mut r = Rate::default();
        for i in 0..10 {
            assert!(!r.over(10, t + Duration::from_millis(i)));
        }
        assert!(r.over(10, t + Duration::from_millis(500)));
        assert!(!r.over(10, t + Duration::from_millis(1_000)));
    }
}
