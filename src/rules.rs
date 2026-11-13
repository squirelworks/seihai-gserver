//! Game rules the server keeps itself: how long a cut bush and a horse
//! last, how long a dead baddy stays away, how alignment points (AP) rise
//! and what a kill costs.
//!
//! Two sets exist, picked with `rules` in `config/server.txt`:
//!
//! - `v2` (the default): the rules this server has always had, measured on
//!   GServer-v2 in the lab. A cut bush grows back after 15 s, and a kill
//!   costs the killer `2 * (1 + AP / 20)` points.
//! - `1999`: the rules of the two original servers in the archive,
//!   GServer 1.28 beta 2 and GServer 1.3 (both November 1999), read from
//!   the binaries (`docs/ORIGINAL-SERVERS.md`). A cut bush grows back after
//!   a random time between 0.75 and 1.25 times `respawn_time` (10 s), a
//!   kill only counts when the victim had 10 AP or more, and it costs the
//!   killer `(victim's AP + 10) / 20`, rounded half to even. A dead player
//!   stops running the level's baddies, only the leader's baddy changes
//!   are taken, and swamp soldiers and hares start in their own modes.
//!
//! The times are settings in both sets: `respawn_time`, `horse_lifetime`,
//! `baddy_respawn_time` and `alignment_time`.

use std::time::Duration;

/// Which server's rules the world plays by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Era {
    /// GServer-v2's, as measured in the lab.
    #[default]
    V2,
    /// The November 1999 servers'.
    Y1999,
}

/// The rules and their times.
#[derive(Debug, Clone, PartialEq)]
pub struct Rules {
    pub era: Era,
    /// The base time a cut bush (any 2x2 board change) stays cut. `None`
    /// takes the era's: 15 s for `v2`, 10 s for `1999`.
    pub respawn: Option<Duration>,
    /// How long a horse left in a level stays there (10 s on both 1999
    /// servers).
    pub horse_life: Duration,
    /// How long a dead baddy stays dead (60 s).
    pub baddy_respawn: Duration,
    /// Time online for each AP point gained, up to 100. Zero turns it off.
    /// 1200 s on both 1999 servers.
    pub alignment_time: Duration,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            era: Era::V2,
            respawn: None,
            horse_life: Duration::from_secs(10),
            baddy_respawn: Duration::from_secs(60),
            alignment_time: Duration::from_secs(1200),
        }
    }
}

/// What a kill claim does to the killer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kill {
    /// The killer's AP after the kill.
    pub ap: u8,
}

impl Rules {
    /// How long a 2x2 change stays, for a random `roll` in `0.0..1.0`.
    /// `v2` ignores the roll.
    pub fn bush_regrowth(&self, roll: f64) -> Duration {
        match self.era {
            Era::V2 => self.respawn.unwrap_or(Duration::from_secs(15)),
            Era::Y1999 => {
                // GServer 1.3 counts down whole seconds from
                // round(random * R / 2 + 3R / 4).
                let r = self
                    .respawn
                    .unwrap_or(Duration::from_secs(10))
                    .as_secs_f64();
                let secs = (roll.clamp(0.0, 1.0) * r / 2.0 + 3.0 * r / 4.0).round();
                Duration::from_secs_f64(secs.max(1.0))
            }
        }
    }

    /// A kill claimed against a killer with `killer_ap` by a victim with
    /// `victim_ap`. `None` when it does not count at all.
    pub fn kill(&self, killer_ap: u8, victim_ap: u8) -> Option<Kill> {
        let killer_ap = killer_ap.min(100);
        match self.era {
            Era::V2 => Some(Kill {
                ap: killer_ap.saturating_sub(2 * (1 + killer_ap / 20)),
            }),
            Era::Y1999 => {
                if victim_ap < 10 {
                    return None;
                }
                let cost = round_half_even(u32::from(victim_ap.min(100)) + 10, 20);
                Some(Kill {
                    ap: (u32::from(killer_ap).saturating_sub(cost)) as u8,
                })
            }
        }
    }

    /// Whether a leader who dies hands the level's baddies to the next
    /// player (1999).
    pub fn dead_leader_hands_off(&self) -> bool {
        self.era == Era::Y1999
    }

    /// The mode a baddy of `kind` starts (and comes back) in. In 1999 a
    /// swamp soldier (4) starts in mode 6 and a hare (5) in mode 7; `v2`
    /// starts every type walking, as GServer-v2 beta4 sends them.
    pub fn baddy_start_mode(&self, kind: u8) -> u8 {
        match (self.era, kind) {
            (Era::Y1999, 4) => 6,
            (Era::Y1999, 5) => 7,
            _ => 0,
        }
    }
}

/// `n / d` rounded half to even, as Delphi's `Round` does.
fn round_half_even(n: u32, d: u32) -> u32 {
    let (q, r) = (n / d, n % d);
    match (2 * r).cmp(&d) {
        std::cmp::Ordering::Less => q,
        std::cmp::Ordering::Greater => q + 1,
        std::cmp::Ordering::Equal => q + (q & 1),
    }
}

/// A random number in `0.0..1.0`.
pub fn roll() -> f64 {
    let mut b = [0u8; 4];
    if getrandom::getrandom(&mut b).is_err() {
        return 0.5;
    }
    f64::from(u32::from_le_bytes(b)) / (f64::from(u32::MAX) + 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn y1999() -> Rules {
        Rules {
            era: Era::Y1999,
            ..Rules::default()
        }
    }

    #[test]
    fn v2_keeps_the_lab_measured_kill_cost() {
        let r = Rules::default();
        for (ap, cost) in [
            (100, 12),
            (88, 10),
            (78, 8),
            (62, 8),
            (54, 6),
            (42, 6),
            (37, 4),
            (21, 4),
            (17, 2),
            (3, 2),
        ] {
            assert_eq!(r.kill(ap, 0).unwrap().ap, ap.saturating_sub(cost), "{ap}");
        }
    }

    #[test]
    fn a_1999_kill_costs_by_the_victims_ap() {
        let r = y1999();
        // A victim under 10 AP: no kill, no cost.
        assert_eq!(r.kill(50, 9), None);
        // (victim + 10) / 20, half to even: 20 -> 1.5 -> 2, 40 -> 2.5 -> 2,
        // 60 -> 3.5 -> 4, 80 -> 4.5 -> 4, 100 -> 5.5 -> 6.
        for (victim, cost) in [
            (10, 1),
            (20, 2),
            (29, 2),
            (31, 2),
            (40, 2),
            (60, 4),
            (80, 4),
            (100, 6),
        ] {
            assert_eq!(r.kill(50, victim).unwrap().ap, 50 - cost, "{victim}");
        }
        // Never below 0.
        assert_eq!(r.kill(1, 100).unwrap().ap, 0);
        assert_eq!(r.kill(0, 100).unwrap().ap, 0);
    }

    #[test]
    fn bushes_grow_back_by_era() {
        assert_eq!(Rules::default().bush_regrowth(0.9), Duration::from_secs(15));
        let r = y1999();
        // 7.5 s to 12.5 s around the 10 s default, in whole seconds.
        assert_eq!(r.bush_regrowth(0.0), Duration::from_secs(8));
        assert_eq!(r.bush_regrowth(0.5), Duration::from_secs(10));
        assert_eq!(r.bush_regrowth(0.999), Duration::from_secs(12));
        let r = Rules {
            respawn: Some(Duration::from_secs(20)),
            ..y1999()
        };
        assert_eq!(r.bush_regrowth(0.0), Duration::from_secs(15));
        assert_eq!(r.bush_regrowth(1.0), Duration::from_secs(25));
    }

    #[test]
    fn rolls_stay_in_range() {
        for _ in 0..100 {
            let r = roll();
            assert!((0.0..1.0).contains(&r));
        }
    }

    #[test]
    fn start_modes() {
        let r = y1999();
        assert_eq!(
            (
                r.baddy_start_mode(4),
                r.baddy_start_mode(5),
                r.baddy_start_mode(0)
            ),
            (6, 7, 0)
        );
        assert_eq!(Rules::default().baddy_start_mode(4), 0);
    }
}
