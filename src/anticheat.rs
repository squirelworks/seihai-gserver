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
//!   pass. Moves the server makes (a warp's arrival, a login) start afresh.
//! - **Warp** (`world.rs`): `PLI_LEVELWARP`/`PLI_LEVELWARPMOD` only for a
//!   link in the player's level to that level, which the player stands on
//!   (with [`LINK_SLACK`] tiles for a position a frame old), and to the
//!   link's arrival; a gmap crossing only into a cell next to the player's.
//! - **Rate** (`server.rs`): more than `anticheat_rate` messages in one
//!   second on a connection, file checks (`PLI_WANTFILE`, `PLI_UPDATEFILE`)
//!   left out: a client checks its whole cache at login. One run of
//!   messages over the limit is one violation.
//!
//! Actions: `off`, `log` (a line in the log file), `correct` (also put it
//! back: the position is sent back to the player and not passed on, the
//! warp is not answered, the message is dropped), and `kick after N in S`
//! on top of either (N violations of that kind within S seconds). The
//! defaults never kick: speed is logged, the rest are logged and
//! corrected.
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
//! ```

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Tiles a second the speed check allows: twice a walk, for slack.
pub const MAX_SPEED: f32 = 20.0;
/// The most the speed budget holds, in tiles.
pub const BURST: f32 = 12.0;
/// A longer step in one update is a teleport, in tiles.
pub const TELEPORT: f32 = 16.0;
/// How far from a link a warp through it may start, in tiles.
pub const LINK_SLACK: f32 = 3.0;

/// What was broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Speed,
    Teleport,
    Warp,
    Rate,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Speed, Kind::Teleport, Kind::Warp, Kind::Rate];

    /// The name in the settings and the log.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Speed => "speed",
            Kind::Teleport => "teleport",
            Kind::Warp => "warp",
            Kind::Rate => "rate",
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
}

impl Rule {
    /// `off`, `log` or `correct`, then optionally `, kick after N in S`.
    pub fn parse(v: &str) -> Result<Rule, String> {
        let mut parts = v.split(',').map(str::trim);
        let action = match parts.next().unwrap_or("").to_ascii_lowercase().as_str() {
            "off" => Action::Off,
            "log" => Action::Log,
            "correct" => Action::Correct,
            other => return Err(format!("`{other}`: expected off, log or correct")),
        };
        let mut kick_after = None;
        if let Some(k) = parts.next() {
            let w: Vec<&str> = k.split_whitespace().collect();
            match w.as_slice() {
                ["kick", "after", n, "in", s] => {
                    let n: u32 = n.parse().map_err(|_| format!("`{n}`: not a count"))?;
                    let s: u64 = s.parse().map_err(|_| format!("`{s}`: not seconds"))?;
                    if n == 0 || s == 0 {
                        return Err("kick after needs a count and seconds above 0".into());
                    }
                    kick_after = Some((n, Duration::from_secs(s)));
                }
                _ => return Err(format!("`{k}`: expected `kick after N in S`")),
            }
        }
        if parts.next().is_some() {
            return Err("too many parts".into());
        }
        if action == Action::Off && kick_after.is_some() {
            return Err("`off` cannot kick".into());
        }
        Ok(Rule { action, kick_after })
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
    rules: [Rule; 4],
}

impl Default for Policy {
    fn default() -> Self {
        let correct = Rule {
            action: Action::Correct,
            kick_after: None,
        };
        Policy {
            enabled: true,
            log: PathBuf::from("logs/anticheat.log"),
            rate: 200,
            rules: [
                Rule {
                    action: Action::Log,
                    kick_after: None,
                },
                correct,
                correct,
                correct,
            ],
        }
    }
}

impl Policy {
    pub fn rule(&self, kind: Kind) -> Rule {
        if !self.enabled {
            return Rule {
                action: Action::Off,
                kick_after: None,
            };
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
}

/// One player's recent violations, by kind.
#[derive(Debug, Default)]
pub struct Score {
    recent: [VecDeque<Instant>; 4],
}

impl Score {
    /// A violation of `kind` at `now`: what the policy says to do.
    pub fn report(&mut self, policy: &Policy, kind: Kind, now: Instant) -> Verdict {
        let rule = policy.rule(kind);
        if rule.action == Action::Off {
            return Verdict::default();
        }
        let mut kick = false;
        if let Some((n, within)) = rule.kick_after {
            let q = &mut self.recent[kind.index()];
            q.push_back(now);
            while q
                .front()
                .is_some_and(|&t| now.saturating_duration_since(t) > within)
            {
                q.pop_front();
            }
            kick = q.len() >= n as usize;
        }
        Verdict {
            log: true,
            correct: rule.action == Action::Correct,
            kick,
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

    /// Take the position after all (a violation that is only logged), with
    /// the budget spent.
    pub fn accept(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
        self.budget = 0.0;
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

/// The policy and the log file, shared by the world and the connections.
#[derive(Debug)]
pub struct AntiCheat {
    pub policy: Policy,
    path: PathBuf,
    file: Mutex<Option<std::fs::File>>,
}

impl AntiCheat {
    pub fn new(policy: Policy, world: &Path) -> AntiCheat {
        let path = world.join(&policy.log);
        AntiCheat {
            policy,
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
                kick_after: Some((5, Duration::from_secs(60)))
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
        assert_eq!(p.set("name", "x"), None);
        assert_eq!(p.set("anticheat.warp", "log"), Some(Ok(())));
        assert_eq!(p.rule(Kind::Warp).action, Action::Log);
        assert!(p.set("anticheat.fly", "log").unwrap().is_err());
        assert_eq!(p.set("anticheat", "off"), Some(Ok(())));
        assert_eq!(p.rule(Kind::Teleport).action, Action::Off);
    }

    #[test]
    fn the_defaults_never_kick() {
        let p = Policy::default();
        let mut s = Score::default();
        let now = Instant::now();
        for kind in Kind::ALL {
            for _ in 0..100 {
                assert!(!s.report(&p, kind, now).kick);
            }
        }
        assert!(!s.report(&p, Kind::Speed, now).correct, "speed is logged");
        assert!(s.report(&p, Kind::Teleport, now).correct);
    }

    #[test]
    fn kicks_count_within_the_window() {
        let mut p = Policy::default();
        p.set("anticheat.speed", "log, kick after 3 in 10");
        let mut s = Score::default();
        let t = Instant::now();
        let at = |secs| t + Duration::from_secs(secs);
        assert!(!s.report(&p, Kind::Speed, at(0)).kick);
        assert!(!s.report(&p, Kind::Speed, at(5)).kick);
        // The first has aged out.
        assert!(!s.report(&p, Kind::Speed, at(12)).kick);
        assert!(s.report(&p, Kind::Speed, at(13)).kick);
    }

    #[test]
    fn walking_passes_and_jumps_do_not() {
        let t = Instant::now();
        let ms = |n| t + Duration::from_millis(n);
        let mut w = Walk::at(30.0, 30.0, t);
        // A real walk: half a tile every 100 ms, for 10 s.
        for i in 1..=100u64 {
            let x = 30.0 + i as f32 * 0.5;
            assert_eq!(w.step(x.min(63.0), 30.0, ms(i * 100)), None, "step {i}");
        }
        // Steps batched after a 1 s stall: 5 tiles at once.
        let (x, y) = w.pos();
        assert_eq!(w.step(x - 5.0, y, ms(11_100)), None);
        // Too far for the time: speed; the position stays.
        let (x, y) = w.pos();
        assert_eq!(
            w.step(x - 12.0, y + 4.0, ms(11_150)).map(|v| v.0),
            Some(Kind::Speed)
        );
        assert_eq!(w.pos(), (x, y));
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
