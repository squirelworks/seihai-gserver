//! Horses left in a level: `PLI_HORSEADD` (7) and `PLI_HORSEDEL` (8).
//!
//! A player who gets off a horse (or a level NPC's `puthorse`) sends
//! `PLI_HORSEADD`: `GCHAR x*2, GCHAR y*2, GCHAR dir`, then the image name.
//! Getting back on sends `PLI_HORSEDEL` with the spot. This is how both
//! November 1999 servers (GServer 1.28 beta 2 and 1.3) keep them, read from
//! the binaries (`docs/ORIGINAL-SERVERS.md`):
//!
//! - A horse added on a spot replaces the one there. It is not passed on
//!   to the others in the level at once.
//! - A player who enters the level gets each horse there as
//!   `PLO_HORSEADD` (17): the same four fields, the image without its
//!   extension.
//! - A horse lasts the horse lifetime (`horse_lifetime`, 10 s); then the
//!   level is told `PLO_HORSEDEL` (18) with the spot, and it is gone. A
//!   delete from a client is not passed on.
//!
//! The 1.3 lab run of 2026-10-30 saw this: the horses of a `puthorse`
//! went 9.5 to 9.8 s after the level was entered.

use crate::wire::{gchar, msg};
use std::time::Instant;

/// `PLI_HORSEADD`.
pub const PLI_HORSEADD: u8 = 7;
/// `PLI_HORSEDEL`.
pub const PLI_HORSEDEL: u8 = 8;
/// `PLO_HORSEADD`.
pub const PLO_HORSEADD: u8 = 17;
/// `PLO_HORSEDEL`.
pub const PLO_HORSEDEL: u8 = 18;

/// The most horses kept for one level; a horse past it is dropped.
pub const MAX_HORSES: usize = 64;

/// A horse lying in a level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Horse {
    /// Half tiles, as on the wire.
    pub x2: u8,
    pub y2: u8,
    /// The third byte as sent (the direction, and on later builds more
    /// bits); passed back unchanged.
    pub dir: u8,
    /// The image name without its extension.
    pub image: Vec<u8>,
    pub until: Instant,
}

impl Horse {
    /// A `PLI_HORSEADD` payload. `None` when it is cut short.
    pub fn parse(raw: &[u8], until: Instant) -> Option<Horse> {
        let [x, y, dir, image @ ..] = raw else {
            return None;
        };
        let g = |b: &u8| b.wrapping_sub(0x20);
        let image = match image.iter().rposition(|&c| c == b'.') {
            Some(dot) => &image[..dot],
            None => image,
        };
        Some(Horse {
            x2: g(x),
            y2: g(y),
            dir: g(dir),
            image: image.to_vec(),
            until,
        })
    }

    /// `PLO_HORSEADD` for an arriving player.
    pub fn add_message(&self) -> Vec<u8> {
        let mut p = vec![gchar(self.x2), gchar(self.y2), gchar(self.dir)];
        p.extend_from_slice(&self.image);
        msg(PLO_HORSEADD, &p)
    }

    /// `PLO_HORSEDEL` when its time is up.
    pub fn del_message(&self) -> Vec<u8> {
        msg(PLO_HORSEDEL, &[gchar(self.x2), gchar(self.y2)])
    }
}

/// The spot of a `PLI_HORSEDEL` payload, in half tiles.
pub fn spot(raw: &[u8]) -> Option<(u8, u8)> {
    match raw {
        [x, y, ..] => Some((x.wrapping_sub(0x20), y.wrapping_sub(0x20))),
        _ => None,
    }
}

/// The horses of one level.
#[derive(Debug, Clone, Default)]
pub struct Horses {
    list: Vec<Horse>,
}

impl Horses {
    /// Keep `h`, in place of any horse on its spot.
    pub fn add(&mut self, h: Horse) {
        self.remove(h.x2, h.y2);
        if self.list.len() < MAX_HORSES {
            self.list.push(h);
        }
    }

    /// Remove the horse on a spot.
    pub fn remove(&mut self, x2: u8, y2: u8) {
        self.list.retain(|h| (h.x2, h.y2) != (x2, y2));
    }

    /// `PLO_HORSEADD` for each horse, for an arriving player.
    pub fn messages(&self) -> Vec<Vec<u8>> {
        self.list.iter().map(Horse::add_message).collect()
    }

    /// Drop the horses whose time is up; returns their `PLO_HORSEDEL`s.
    pub fn expire(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        self.list.retain(|h| {
            if h.until <= now {
                out.push(h.del_message());
                false
            } else {
                true
            }
        });
        out
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_puthorse_from_the_1_3_capture() {
        // GServer13 lab capture: `puthorse ride.gif,53,28` sent as
        // 8a 58 23 "ride".
        let t0 = Instant::now();
        let h = Horse::parse(b"\x8aX#ride", t0).unwrap();
        assert_eq!(
            (h.x2, h.y2, h.dir, h.image.as_slice()),
            (106, 56, 3, &b"ride"[..])
        );
        let m = h.add_message();
        assert_eq!(m, b"1\x8aX#ride\n");
        assert_eq!(h.del_message(), b"2\x8aX\n");
        // An extension is dropped.
        let h = Horse::parse(b"\x8aX#ride.gif", t0).unwrap();
        assert_eq!(h.image, b"ride");
        assert!(Horse::parse(b"\x8aX", t0).is_none());
    }

    #[test]
    fn a_spot_holds_one_horse_and_horses_run_out() {
        let t0 = Instant::now();
        let life = Duration::from_secs(10);
        let mut hs = Horses::default();
        hs.add(Horse::parse(b"\x8aX#ride", t0 + life).unwrap());
        hs.add(Horse::parse(b"\x8aX#ride2", t0 + life + life).unwrap());
        hs.add(Horse::parse(b"\x8ad#ride2", t0 + life).unwrap());
        assert_eq!(hs.messages().len(), 2);
        assert!(hs.expire(t0 + life - Duration::from_millis(1)).is_empty());
        let gone = hs.expire(t0 + life);
        assert_eq!(gone, vec![b"2\x8ad\n".to_vec()]);
        hs.remove(106, 56);
        assert!(hs.is_empty());
    }
}
