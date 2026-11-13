//! Baddies (stage 3.2).
//!
//! The baddies of a level file (`BADDY x y type`, up to three verse lines,
//! `BADDYEND`) are the server's. The world numbers them 1, 2, ... per
//! level when the level is first read and sends each as `PLO_BADDYPROPS`
//! (2) to every player who enters: id, then `prop, value` pairs.
//!
//! The clients run them. The world names one player in each level its
//! leader (`PLO_ISLEADER`, 10): the first one in, and when the leader
//! goes, the next one still there. The leader's client runs the AI and
//! reports each change (`PLI_BADDYPROPS`, 15, the same shape); the world
//! keeps the latest value of each prop and passes the change on to the
//! others in the level. A follower that hits a baddy sends
//! `PLI_BADDYHURT` (16); the world hands it to the leader as
//! `PLO_BADDYHURT` (27), and the leader applies the hit.
//!
//! The world itself only ends a death and brings a dead baddy back. Two
//! seconds after a baddy is set dying (mode 5) it is dead (mode 9) for
//! everyone in the level, and a minute later it is back at its spawn
//! point as the level file has it. These are the times GServer-v2 beta4
//! keeps in the lab (2026-11-05, its default `baddyrespawntime = 60`).
//! The leader's client may say mode 9 itself; that starts the minute too.
//! The 1999 servers have no two-second step: they count the minute from
//! the leader's mode 9 (`docs/ORIGINAL-SERVERS.md`). The minute is the
//! `baddy_respawn_time` setting.
//!
//! The wire (Seihai `documentation/baddies.md`; every value +32, a text is
//! a length byte and the text):
//!
//! | prop | value |
//! |---|---|
//! | 0 | id (the record's own id byte comes first and is not repeated) |
//! | 1, 2 | x * 2, y * 2 |
//! | 3 | type |
//! | 4 | power, then the image (text) |
//! | 5 | mode |
//! | 6 | animation counter |
//! | 7 | dir * 4 + head |
//! | 8, 9, 10 | verses: on sight, on hurt, on attack (text) |

use crate::wire::ids::{PLO_BADDYHURT, PLO_BADDYPROPS};
use crate::wire::{gchar, latin1, msg};
use seihai_core::protocol::ClientVersion;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const ID: u8 = 0;
pub const X: u8 = 1;
pub const Y: u8 = 2;
pub const TYPE: u8 = 3;
pub const POWER_IMAGE: u8 = 4;
pub const MODE: u8 = 5;
pub const ANI: u8 = 6;
pub const DIR: u8 = 7;
pub const VERSE_SIGHT: u8 = 8;
pub const VERSE_HURT: u8 = 9;
pub const VERSE_ATTACK: u8 = 10;
const LAST: u8 = VERSE_ATTACK;

/// Modes the world sets itself.
pub const MODE_WALK: u8 = 0;
pub const MODE_DIE: u8 = 5;
pub const MODE_DEAD: u8 = 9;

/// From mode 5 to mode 9.
pub const DYING: Duration = Duration::from_secs(2);
/// From mode 9 back to the spawn point.
pub const RESPAWN: Duration = Duration::from_secs(60);

/// Power and image of each type (0 to 9).
const KINDS: [(u8, &str); 10] = [
    (2, "baddygray"),
    (3, "baddyblue"),
    (4, "baddyred"),
    (3, "baddyblue"),
    (2, "baddygray"),
    (1, "baddyhare"),
    (1, "baddyoctopus"),
    (6, "baddygold"),
    (12, "baddylizardon"),
    (8, "baddydragon"),
];

/// A new baddy faces down with its head straight: `dir * 4 + head`, as the
/// beta4 lab capture sends it for every type.
const START_DIR: u8 = 10;

/// One baddy as the world keeps it: the props as their value bytes, the
/// image name without its extension.
#[derive(Debug, Clone)]
pub struct Baddy {
    pub id: u8,
    props: BTreeMap<u8, Vec<u8>>,
    /// The record it comes back with.
    spawn: BTreeMap<u8, Vec<u8>>,
    /// When the death ends (mode 9).
    pub dead_at: Option<Instant>,
    /// When it comes back.
    pub back_at: Option<Instant>,
}

impl Baddy {
    /// A baddy of the level file, numbered `id`, starting in `start_mode`
    /// (`Rules::baddy_start_mode`). `None` for a type that is not 0 to 9.
    pub fn from_level(
        id: u8,
        b: &seihai_core::assets::Baddy,
        start_mode: impl Fn(u8) -> u8,
    ) -> Option<Self> {
        let kind: u8 = b.kind.trim().parse().ok()?;
        let (power, image) = *KINDS.get(usize::from(kind))?;
        let mut props = BTreeMap::new();
        props.insert(X, vec![gchar(half_tiles(b.x))]);
        props.insert(Y, vec![gchar(half_tiles(b.y))]);
        props.insert(TYPE, vec![gchar(kind)]);
        props.insert(POWER_IMAGE, power_image(power, image.as_bytes()));
        props.insert(MODE, vec![gchar(start_mode(kind))]);
        props.insert(ANI, vec![gchar(0)]);
        props.insert(DIR, vec![gchar(START_DIR)]);
        let mut verses = b.verses.split('\n');
        for p in [VERSE_SIGHT, VERSE_HURT, VERSE_ATTACK] {
            let v = latin1(verses.next().unwrap_or("").trim_end_matches('\r'));
            let v = &v[..v.len().min(223)];
            let mut t = vec![gchar(v.len() as u8)];
            t.extend_from_slice(v);
            props.insert(p, t);
        }
        Some(Baddy {
            id,
            spawn: props.clone(),
            props,
            dead_at: None,
            back_at: None,
        })
    }

    /// The whole baddy for `to`.
    pub fn message(&self, to: ClientVersion) -> Vec<u8> {
        let all: Vec<_> = self.props.iter().map(|(k, v)| (*k, v.clone())).collect();
        message(self.id, &all, to)
    }

    /// A client's change: kept, and returned as the props that changed, to
    /// pass on. Prop 0 (the id) is not the client's to change. A hurt
    /// carries the new power with an empty image: the power is kept and the
    /// image stays, so a later arrival still gets it. A mode 9 (dead) from
    /// the client brings the baddy back after `respawn`, unless it is
    /// already dead and counting.
    pub fn update(&mut self, raw: &[u8], now: Instant, respawn: Duration) -> Vec<(u8, Vec<u8>)> {
        let mut changed = Vec::new();
        for (id, v) in split(raw) {
            if id == ID {
                continue;
            }
            let keep = if id == POWER_IMAGE && v.get(1) == Some(&gchar(0)) {
                let mut k = v.clone();
                if let Some(old) = self.props.get(&POWER_IMAGE) {
                    k.truncate(1);
                    k.extend_from_slice(&old[1..]);
                }
                k
            } else {
                v.clone()
            };
            if id == MODE {
                let mode = v[0].wrapping_sub(0x20);
                self.dead_at = (mode == MODE_DIE).then(|| now + DYING);
                self.back_at = match (mode, self.back_at) {
                    (MODE_DEAD, Some(t)) => Some(t),
                    (MODE_DEAD, None) => Some(now + respawn),
                    _ => None,
                };
            }
            if self.props.get(&id) != Some(&keep) || id == POWER_IMAGE {
                self.props.insert(id, keep);
                changed.push((id, v));
            }
        }
        changed
    }

    /// Dead now: mode 9, and back after `respawn` ([`RESPAWN`] by
    /// default).
    pub fn die(&mut self, now: Instant, respawn: Duration) -> Vec<(u8, Vec<u8>)> {
        self.dead_at = None;
        self.back_at = Some(now + respawn);
        self.props.insert(MODE, vec![gchar(MODE_DEAD)]);
        vec![(MODE, vec![gchar(MODE_DEAD)])]
    }

    /// Back at its spawn point, as the level file has it.
    pub fn respawn(&mut self) {
        self.props = self.spawn.clone();
        self.dead_at = None;
        self.back_at = None;
    }

    pub fn mode(&self) -> Option<u8> {
        self.props.get(&MODE).map(|v| v[0].wrapping_sub(0x20))
    }
}

/// `PLO_BADDYPROPS` with `props` for `to`. A default image (`baddygray`
/// and the rest) goes with the build's own extension ([`image_ext`]).
pub fn message(id: u8, props: &[(u8, Vec<u8>)], to: ClientVersion) -> Vec<u8> {
    let mut p = vec![gchar(id)];
    for (pid, v) in props {
        p.push(gchar(*pid));
        if *pid == POWER_IMAGE {
            p.extend(with_ext(v, image_ext(to)));
        } else {
            p.extend_from_slice(v);
        }
    }
    msg(PLO_BADDYPROPS, &p)
}

/// The extension of a default baddy image for `to` (`profile.rs`:
/// `.gif` before 2.1, but `.png` for 2.05).
fn image_ext(to: ClientVersion) -> &'static str {
    crate::profile::ServerProfile::of(to).baddy_image_ext
}

/// `PLO_BADDYHURT` (27): a follower's hit, as it sent it, for the leader.
pub fn hurt_message(raw: &[u8]) -> Option<Vec<u8>> {
    (raw.len() == 4).then(|| msg(PLO_BADDYHURT, raw))
}

/// The baddy id a `PLI_BADDYPROPS` or `PLI_BADDYHURT` names, and the rest.
pub fn id_of(raw: &[u8]) -> Option<(u8, &[u8])> {
    let (&id, rest) = raw.split_first()?;
    Some((id.wrapping_sub(0x20), rest))
}

/// A prop stream cut into `(prop, value bytes)`. It stops at an unknown
/// prop or a value cut short.
pub fn split(b: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0;
    let gc = |i: usize| b.get(i).map(|c| usize::from(c.wrapping_sub(0x20)));
    while let Some(id) = gc(i) {
        let start = i + 1;
        let len = match id as u8 {
            ID | X | Y | TYPE | MODE | ANI | DIR => Some(1),
            POWER_IMAGE => gc(start + 1).map(|n| 2 + n),
            VERSE_SIGHT..=LAST => gc(start).map(|n| 1 + n),
            _ => None,
        };
        let Some(len) = len else { break };
        let Some(v) = b.get(start..start + len) else {
            break;
        };
        out.push((id as u8, v.to_vec()));
        i = start + len;
    }
    out
}

/// Power, then the image as a text.
fn power_image(power: u8, image: &[u8]) -> Vec<u8> {
    let mut v = vec![gchar(power), gchar(image.len() as u8)];
    v.extend_from_slice(image);
    v
}

/// Prop 4 with `ext` on a default image name: one of the type images, bare
/// or with either extension. Any other image goes as it is.
fn with_ext(v: &[u8], ext: &str) -> Vec<u8> {
    let Some((&power, rest)) = v.split_first() else {
        return v.to_vec();
    };
    let name = rest.get(1..).unwrap_or_default();
    let stem = name
        .strip_suffix(b".gif")
        .or_else(|| name.strip_suffix(b".png"))
        .unwrap_or(name);
    if KINDS.iter().any(|(_, k)| k.as_bytes() == stem) {
        let mut image = stem.to_vec();
        image.extend_from_slice(ext.as_bytes());
        let mut out = vec![power];
        out.extend(power_image(0, &image)[1..].iter().copied());
        out
    } else {
        v.to_vec()
    }
}

/// A position in tiles as the byte the wire carries: half tiles.
fn half_tiles(t: f32) -> u8 {
    (t * 2.0).round().clamp(0.0, 223.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(_: u8) -> u8 {
        MODE_WALK
    }

    fn level_baddy(kind: &str, verses: &str) -> seihai_core::assets::Baddy {
        seihai_core::assets::Baddy {
            x: 40.0,
            y: 30.0,
            kind: kind.into(),
            verses: verses.into(),
        }
    }

    /// The record GServer-v2 beta4 sends 2.22 for `BADDY 40 30 0` with the
    /// verses `Halt!`, `Ouch!` and `Take that!` (lab capture, 2026-11-05),
    /// and 1.39 the same with `.gif`.
    #[test]
    fn a_level_baddy_matches_the_capture() {
        let b = Baddy::from_level(1, &level_baddy("0", "Halt!\nOuch!\nTake that!"), walk).unwrap();
        let mut want = vec![
            0x22, 0x21, 0x21, 0x70, 0x22, 0x5c, 0x23, 0x20, 0x24, 0x22, 0x2d,
        ];
        want.extend(b"baddygray.png");
        want.extend([0x25, 0x20, 0x26, 0x20, 0x27, 0x2a, 0x28, 0x25]);
        want.extend(b"Halt!");
        want.extend([0x29, 0x25]);
        want.extend(b"Ouch!");
        want.extend([0x2a, 0x2a]);
        want.extend(b"Take that!");
        want.push(b'\n');
        assert_eq!(b.message(ClientVersion::V222), want);
        let classic = b.message(ClientVersion::V139);
        assert!(classic.windows(13).any(|w| w == b"baddygray.gif"));
    }

    #[test]
    fn a_gold_soldier_without_verses() {
        let b = Baddy::from_level(2, &level_baddy("7", "\n\n"), walk).unwrap();
        let m = b.message(ClientVersion::V222);
        assert!(m.ends_with(&[0x28, 0x20, 0x29, 0x20, 0x2a, 0x20, b'\n']));
        assert!(m.windows(2).any(|w| w == [0x24, 0x26]), "power 6");
        assert!(Baddy::from_level(3, &level_baddy("11", ""), walk).is_none());
        assert!(Baddy::from_level(3, &level_baddy("soldier", ""), walk).is_none());
    }

    #[test]
    fn a_leader_update_is_kept_and_a_hurt_keeps_the_image() {
        let now = Instant::now();
        let mut b = Baddy::from_level(1, &level_baddy("0", ""), walk).unwrap();
        // x 41, and the id, which is not the client's to change.
        assert_eq!(
            b.update(&[0x21, 0x72, 0x20, 0x29], now, RESPAWN),
            [(X, vec![0x72])]
        );
        // A follower's hurt: power 1, empty image, mode 3.
        let changed = b.update(&[0x24, 0x21, 0x20, 0x25, 0x23], now, RESPAWN);
        assert_eq!(changed[0], (POWER_IMAGE, vec![0x21, 0x20]));
        let m = b.message(ClientVersion::V222);
        assert!(m.windows(16).any(|w| w == b"\x24\x21\x2dbaddygray.png"));
        assert_eq!(b.mode(), Some(3));
    }

    #[test]
    fn dying_then_dead_then_back() {
        let now = Instant::now();
        let mut b = Baddy::from_level(1, &level_baddy("0", ""), walk).unwrap();
        b.update(&[0x21, 0x72, 0x25, 0x25], now, RESPAWN);
        assert_eq!(b.dead_at, Some(now + DYING));
        assert_eq!(b.die(now + DYING, RESPAWN), [(MODE, vec![0x29])]);
        assert_eq!(b.back_at, Some(now + DYING + RESPAWN));
        b.respawn();
        assert_eq!(b.mode(), Some(MODE_WALK));
        assert!(b
            .message(ClientVersion::V222)
            .starts_with(&[0x22, 0x21, 0x21, 0x70]));
        // A new mode before the death ends calls it off.
        b.update(&[0x25, 0x25], now, RESPAWN);
        b.update(&[0x25, 0x23], now, RESPAWN);
        assert_eq!(b.dead_at, None);
    }

    /// A leader that says mode 9 itself: the baddy still comes back, from
    /// its own 9 (as on the 1999 servers) or from the server's.
    #[test]
    fn a_clients_own_mode_9_brings_it_back() {
        let now = Instant::now();
        let mut b = Baddy::from_level(1, &level_baddy("0", ""), walk).unwrap();
        b.update(&[0x25, 0x25], now, RESPAWN);
        b.update(&[0x25, 0x29], now + Duration::from_secs(1), RESPAWN);
        assert_eq!(b.dead_at, None);
        assert_eq!(b.back_at, Some(now + Duration::from_secs(1) + RESPAWN));
        // After the server's 9, the client's own 9 keeps the time.
        let mut b = Baddy::from_level(1, &level_baddy("0", ""), walk).unwrap();
        b.update(&[0x25, 0x25], now, RESPAWN);
        b.die(now + DYING, RESPAWN);
        b.update(&[0x25, 0x29], now + Duration::from_secs(5), RESPAWN);
        assert_eq!(b.back_at, Some(now + DYING + RESPAWN));
    }

    /// The 1999 servers start swamp soldiers (4) in mode 6 and hares (5)
    /// in mode 7, and bring them back so.
    #[test]
    fn start_modes_come_from_the_rules() {
        let rules = crate::rules::Rules {
            era: crate::rules::Era::Y1999,
            ..Default::default()
        };
        let mode = |k| rules.baddy_start_mode(k);
        let mut b = Baddy::from_level(1, &level_baddy("4", ""), mode).unwrap();
        assert_eq!(b.mode(), Some(6));
        b.update(&[0x25, 0x29], Instant::now(), RESPAWN);
        b.respawn();
        assert_eq!(b.mode(), Some(6));
        let b = Baddy::from_level(1, &level_baddy("5", ""), mode).unwrap();
        assert_eq!(b.mode(), Some(7));
    }

    #[test]
    fn split_stops_at_an_unknown_prop() {
        assert_eq!(
            split(&[0x21, 0x70, 0x2b, 0x21, 0x22, 0x5c]),
            [(X, vec![0x70])]
        );
        assert!(split(&[0x28, 0x25, b'a']).is_empty());
    }
}
