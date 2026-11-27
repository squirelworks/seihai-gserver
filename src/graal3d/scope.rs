//! Distance scoping of a 3D world's objects (protocols 21 and 24).
//!
//! A GameObject-era client (4.20r6, 4.20r8, 5.x) has a ghost table of
//! [`MAX_GHOSTS`] entries, and without scoping every world object is
//! ghosted at join. A real 5.12 (lab, 2026-11-27) took about 130 creates a
//! second at the default pacing (4000 objects: 31 s), and at 4094 objects
//! or more the table was full before its own object, Camera and
//! TerrainBlock, so it drew a black view all session. It never draws an
//! object more than about 1000 units from its camera anyway.
//!
//! With scoping a world `object` (a [`Ghost::GameObject`] of the world
//! file) is ghosted only while it is within the radius of the player's own
//! object (3D distance), and deleted once it is farther than the radius
//! plus the hysteresis, so an object near the edge is not created and
//! deleted on every step. The rest of the world (Sun, Sky, terrain, water,
//! ground cover, players) is never scoped. On the same 5.12, a world of
//! 20,164 objects with radius 1100 and hysteresis 200 joined in about 3 s,
//! peaked at about 1400 table entries while walking, and drew the same
//! objects as radius 1500.
//!
//! [`MAX_GHOSTS`]: super::conn::MAX_GHOSTS
//! [`Ghost::GameObject`]: super::objects::Ghost::GameObject

use std::collections::HashSet;

/// `graal3d_scope`: whether world objects are scoped by distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Every object is ghosted at join (the default).
    #[default]
    Off,
    /// Every world is scoped.
    On,
    /// A world is scoped only when it has more world objects than this.
    Auto(usize),
}

/// The threshold of `graal3d_scope = auto` when `graal3d_scopeauto` is not
/// given: half the client's table.
pub const DEFAULT_AUTO: usize = 2000;
/// `graal3d_scoperadius` when not given, in world units.
pub const DEFAULT_RADIUS: f32 = 1100.0;
/// `graal3d_scopehysteresis` when not given, in world units.
pub const DEFAULT_HYSTERESIS: f32 = 200.0;
/// How often each client's scope is looked at again, in milliseconds.
pub const PASS_MS: u64 = 200;
/// Table entries kept free of scoped objects for everything else the
/// client is sent (players, 2D players, the Sun, Sky, terrain, water and
/// ground cover), so a dense spot cannot fill the table.
pub const RESERVED: usize = 512;

/// The scoping options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    pub mode: Mode,
    pub radius: f32,
    pub hysteresis: f32,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: Mode::Off,
            radius: DEFAULT_RADIUS,
            hysteresis: DEFAULT_HYSTERESIS,
        }
    }
}

impl Options {
    /// Whether a world with `objects` world objects is scoped.
    pub fn applies(&self, objects: usize) -> bool {
        match self.mode {
            Mode::Off => false,
            Mode::On => true,
            Mode::Auto(n) => objects > n,
        }
    }
}

/// Parses `graal3d_scope`: `off`, `on` or `auto`. `auto` takes its
/// threshold from `graal3d_scopeauto` ([`DEFAULT_AUTO`] when not given).
pub fn parse_mode(v: &str) -> Option<Mode> {
    match v.trim().to_ascii_lowercase().as_str() {
        "off" | "false" | "" => Some(Mode::Off),
        "on" | "true" => Some(Mode::On),
        "auto" => Some(Mode::Auto(DEFAULT_AUTO)),
        _ => None,
    }
}

/// Distance between two points.
pub fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// What one pass changes: the objects to ghost (nearest first) and the
/// ones to delete.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub add: Vec<usize>,
    pub remove: Vec<usize>,
}

/// One pass over the world's scoped objects. `objects` gives each object's
/// index and position, and whether this player is shown it; `scoped` is
/// what the client has now. An object not in scope comes in within
/// `radius`; one in scope goes beyond `radius + hysteresis`, or when it is
/// no longer shown. At most `budget` objects are in scope after the pass:
/// the nearest are taken first. When the scope is full, a kept object
/// farther than a waiting one by more than the hysteresis is swapped for
/// it, at most [`SWAPS_A_PASS`] a pass, so a player who walks into a spot
/// denser than the budget ends up holding the objects round it rather than
/// the ones it saw on the way in.
pub fn plan(
    scoped: &HashSet<usize>,
    at: [f32; 3],
    objects: impl IntoIterator<Item = (usize, [f32; 3], bool)>,
    opts: &Options,
    budget: usize,
) -> Plan {
    let mut out = Plan::default();
    let mut near: Vec<(f32, usize)> = Vec::new();
    let mut kept: Vec<(f32, usize)> = Vec::new();
    for (i, p, shown) in objects {
        let d = distance(at, p);
        if scoped.contains(&i) {
            if !shown || d > opts.radius + opts.hysteresis {
                out.remove.push(i);
            } else {
                kept.push((d, i));
            }
        } else if shown && d <= opts.radius {
            near.push((d, i));
        }
    }
    let order = |a: &(f32, usize), b: &(f32, usize)| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1));
    near.sort_by(order);
    let room = budget.saturating_sub(kept.len());
    let mut waiting = near.into_iter();
    out.add = waiting.by_ref().take(room).map(|(_, i)| i).collect();
    // Full: trade the farthest kept for the nearest still waiting.
    kept.sort_by(|a, b| order(b, a));
    for ((far, k), (close, w)) in kept.into_iter().zip(waiting).take(SWAPS_A_PASS) {
        if far <= close + opts.hysteresis {
            break;
        }
        out.remove.push(k);
        out.add.push(w);
    }
    out
}

/// The most kept objects a pass swaps for nearer ones when the scope is
/// full: about 1.2 KB of creates and deletes a pass at 5 passes a second.
pub const SWAPS_A_PASS: usize = 24;

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Options {
        Options {
            mode: Mode::On,
            radius: 100.0,
            hysteresis: 20.0,
        }
    }

    fn line(xs: &[f32]) -> Vec<(usize, [f32; 3], bool)> {
        xs.iter()
            .enumerate()
            .map(|(i, x)| (i, [*x, 0.0, 0.0], true))
            .collect()
    }

    #[test]
    fn objects_come_in_within_the_radius_nearest_first() {
        let p = plan(
            &HashSet::new(),
            [0.0; 3],
            line(&[50.0, 100.0, 101.0, -99.0]),
            &opts(),
            99,
        );
        assert_eq!(p.add, vec![0, 3, 1]);
        assert!(p.remove.is_empty());
    }

    #[test]
    fn distance_is_three_dimensional() {
        let objs = vec![(0, [60.0, 60.0, 60.0], true), (1, [0.0, 0.0, 99.0], true)];
        let p = plan(&HashSet::new(), [0.0; 3], objs, &opts(), 99);
        assert_eq!(p.add, vec![1]);
    }

    #[test]
    fn an_object_in_scope_stays_until_past_the_hysteresis() {
        let scoped: HashSet<usize> = [0, 1, 2].into_iter().collect();
        // 0 is in the band (radius < d <= radius + hysteresis): it stays.
        // 1 is right on the outer edge: it stays. 2 is past it: it goes.
        let p = plan(&scoped, [0.0; 3], line(&[110.0, 120.0, 120.5]), &opts(), 99);
        assert!(p.add.is_empty());
        assert_eq!(p.remove, vec![2]);
        // And an object in the band that was not in scope does not come in.
        let p = plan(&HashSet::new(), [0.0; 3], line(&[110.0]), &opts(), 99);
        assert!(p.add.is_empty());
    }

    #[test]
    fn walking_out_and_back_crosses_each_edge_once() {
        let o = opts();
        let mut scoped = HashSet::new();
        let mut events = Vec::new();
        // The player walks from x = 0 to x = 300 and back; the object is
        // at x = 0.
        let walk: Vec<f32> = (0..=30)
            .chain((0..30).rev())
            .map(|s| s as f32 * 10.0)
            .collect();
        for x in walk {
            let p = plan(&scoped, [x, 0.0, 0.0], vec![(7, [0.0; 3], true)], &o, 99);
            for i in p.add {
                scoped.insert(i);
                events.push(("add", x));
            }
            for i in p.remove {
                scoped.remove(&i);
                events.push(("remove", x));
            }
        }
        assert_eq!(
            events,
            vec![("add", 0.0), ("remove", 130.0), ("add", 100.0)]
        );
    }

    #[test]
    fn an_object_no_longer_shown_goes() {
        let scoped: HashSet<usize> = [0].into_iter().collect();
        let p = plan(
            &scoped,
            [0.0; 3],
            vec![(0, [1.0, 0.0, 0.0], false)],
            &opts(),
            99,
        );
        assert_eq!(p.remove, vec![0]);
        let p = plan(
            &HashSet::new(),
            [0.0; 3],
            vec![(0, [1.0, 0.0, 0.0], false)],
            &opts(),
            99,
        );
        assert!(p.add.is_empty());
    }

    #[test]
    fn the_budget_takes_the_nearest_and_trades_far_ones() {
        // Two already in scope (far), four new candidates; room for 4.
        let scoped: HashSet<usize> = [0, 1].into_iter().collect();
        let p = plan(
            &scoped,
            [0.0; 3],
            line(&[90.0, 95.0, 40.0, 10.0, 30.0, 20.0]),
            &opts(),
            4,
        );
        // Room for two (10 and 20); then 95 and 90 are traded for 30 and 40.
        assert_eq!(p.add, vec![3, 5, 4, 2]);
        assert_eq!(p.remove, vec![1, 0]);
        // A full scope trades its farthest object for a much nearer one.
        let p = plan(&scoped, [0.0; 3], line(&[90.0, 95.0, 1.0]), &opts(), 2);
        assert_eq!((p.add, p.remove), (vec![2], vec![1]));
        // But not for one about as near: no churn at the edge.
        let p = plan(&scoped, [0.0; 3], line(&[90.0, 95.0, 80.0]), &opts(), 2);
        assert!(p.add.is_empty() && p.remove.is_empty());
    }

    #[test]
    fn a_full_scope_converges_on_the_nearest_within_a_few_passes() {
        // 100 seen on the way in, far; 100 dense ones round the player.
        let o = opts();
        let mut objs: Vec<(usize, [f32; 3], bool)> = (0..100)
            .map(|i| (i, [99.0, i as f32 * 0.01, 0.0], true))
            .collect();
        objs.extend((100..200).map(|i| (i, [1.0, i as f32 * 0.01, 0.0], true)));
        let mut scoped: HashSet<usize> = (0..100).collect();
        let mut passes = 0;
        while scoped.iter().any(|i| *i < 100) {
            let p = plan(&scoped, [0.0; 3], objs.clone(), &o, 100);
            assert!(p.add.len() <= SWAPS_A_PASS && p.add.len() == p.remove.len());
            for i in p.remove {
                scoped.remove(&i);
            }
            scoped.extend(p.add);
            assert_eq!(scoped.len(), 100);
            passes += 1;
            assert!(passes < 10);
        }
        assert_eq!(passes, 100usize.div_ceil(SWAPS_A_PASS));
    }

    #[test]
    fn auto_turns_on_only_past_its_threshold() {
        let mut o = opts();
        o.mode = Mode::Auto(2000);
        assert!(!o.applies(2000));
        assert!(o.applies(2001));
        o.mode = Mode::Off;
        assert!(!o.applies(1_000_000));
        o.mode = Mode::On;
        assert!(o.applies(0));
    }

    #[test]
    fn modes_parse() {
        assert_eq!(parse_mode("off"), Some(Mode::Off));
        assert_eq!(parse_mode(" On "), Some(Mode::On));
        assert_eq!(parse_mode("auto"), Some(Mode::Auto(DEFAULT_AUTO)));
        assert_eq!(parse_mode("sometimes"), None);
    }
}
