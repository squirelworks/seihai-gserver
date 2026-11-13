//! Board changes: a client's `PLI_BOARDMODIFY` (1), kept per level and
//! passed on as `PLO_BOARDMODIFY` (7).
//!
//! A client changes its level's tiles itself (a bush lifted or cut, a
//! stone block pushed) and reports the block it wrote: `GCHAR x, y, w, h`
//! in whole tiles, then the `w*h` tiles row by row as `GSHORT`s (core's
//! `board_modify_message`, from a 2.05 lab capture). The server keeps the
//! change for the level, sends it to the others there in the same shape
//! as 7, and to players who arrive later. A 2x2 change (a bush, a pot, a
//! sign) grows back after the world's regrowth time (`rules.rs`): the old
//! tiles go out to the level and the change is dropped. Larger changes
//! stay while the server runs. Only a 2x2 grows back on GServer-v2 and on
//! both 1999 servers alike; the time is 15 s on GServer-v2 ([`RESPAWN`])
//! and a random 7.5 to 12.5 s on the 1999 servers.
//!
//! A level keeps at most [`MAX_CHANGES`]. The tiles a client writes are
//! not checked against what may be cut or lifted there yet.

use crate::wire::{gchar, gshort, msg};
use std::time::{Duration, Instant};

/// `PLO_BOARDMODIFY`.
pub const PLO_BOARDMODIFY: u8 = 7;
/// `PLI_BOARDMODIFY`.
pub const PLI_BOARDMODIFY: u8 = 1;
/// How long a 2x2 change lasts under the `v2` rules.
pub const RESPAWN: Duration = Duration::from_secs(15);
/// The most changes kept for one level. A change to a new rectangle past
/// this is refused: it is neither kept nor passed on, so one client
/// cannot grow the list without end. A change to a rectangle already kept
/// still replaces it.
pub const MAX_CHANGES: usize = 256;

const SIDE: usize = 64;

/// One block a client wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub x: u8,
    pub y: u8,
    pub w: u8,
    pub h: u8,
    pub tiles: Vec<u16>,
}

impl Block {
    /// A `PLI_BOARDMODIFY` payload. `None` when it is cut short or the
    /// block does not lie inside the 64 x 64 board. Bytes after the tiles
    /// are ignored.
    pub fn parse(raw: &[u8]) -> Option<Block> {
        let [x, y, w, h, rest @ ..] = raw else {
            return None;
        };
        let g = |b: &u8| b.wrapping_sub(0x20);
        let (x, y, w, h) = (g(x), g(y), g(w), g(h));
        if w == 0
            || h == 0
            || usize::from(x) + usize::from(w) > SIDE
            || usize::from(y) + usize::from(h) > SIDE
        {
            return None;
        }
        let n = usize::from(w) * usize::from(h);
        let body = rest.get(..2 * n)?;
        let tiles = body
            .chunks(2)
            .map(|c| (u16::from(c[0].wrapping_sub(0x20)) << 7) | u16::from(c[1].wrapping_sub(0x20)))
            .collect();
        Some(Block { x, y, w, h, tiles })
    }

    /// `PLO_BOARDMODIFY` for this block.
    pub fn message(&self) -> Vec<u8> {
        let mut p = vec![gchar(self.x), gchar(self.y), gchar(self.w), gchar(self.h)];
        for &t in &self.tiles {
            gshort(t, &mut p);
        }
        msg(PLO_BOARDMODIFY, &p)
    }

    fn same_place(&self, o: &Block) -> bool {
        (self.x, self.y, self.w, self.h) == (o.x, o.y, o.w, o.h)
    }

    /// The same rectangle's tiles in `board` (64 wide, row by row).
    fn read_from(&self, board: &[u16]) -> Block {
        let mut tiles = Vec::with_capacity(self.tiles.len());
        for row in 0..usize::from(self.h) {
            for col in 0..usize::from(self.w) {
                let i = (usize::from(self.y) + row) * SIDE + usize::from(self.x) + col;
                tiles.push(board.get(i).copied().unwrap_or(0));
            }
        }
        Block {
            tiles,
            ..self.clone()
        }
    }

    fn write_to(&self, board: &mut [u16]) {
        for row in 0..usize::from(self.h) {
            for col in 0..usize::from(self.w) {
                let i = (usize::from(self.y) + row) * SIDE + usize::from(self.x) + col;
                if let Some(t) = board.get_mut(i) {
                    *t = self.tiles[row * usize::from(self.w) + col];
                }
            }
        }
    }
}

/// A change kept for a level.
#[derive(Debug, Clone)]
struct Change {
    now: Block,
    /// The tiles before it, to grow back.
    old: Block,
    /// When it grows back; `None` for one that stays.
    until: Option<Instant>,
}

/// The changes to one level's board.
#[derive(Debug, Clone, Default)]
pub struct Changes {
    list: Vec<Change>,
}

impl Changes {
    /// Keep `b` for a level whose file board is `file`. A 2x2 grows back
    /// after `grow`. A change to the same rectangle replaces the one
    /// before and keeps its old tiles. `false` when the level already has
    /// [`MAX_CHANGES`] and `b` is in a new place: it is not kept and should
    /// not be passed on.
    pub fn add(&mut self, b: Block, file: &[u16], now: Instant, grow: Duration) -> bool {
        let until = (b.w == 2 && b.h == 2).then_some(now + grow);
        let old = match self.list.iter().position(|c| c.now.same_place(&b)) {
            Some(i) => self.list.remove(i).old,
            None if self.list.len() >= MAX_CHANGES => return false,
            None => b.read_from(&self.board(file)),
        };
        self.list.push(Change { now: b, old, until });
        true
    }

    /// The board as the players see it now.
    pub fn board(&self, file: &[u16]) -> Vec<u16> {
        let mut board = file.to_vec();
        board.resize(SIDE * SIDE, 0);
        for c in &self.list {
            c.now.write_to(&mut board);
        }
        board
    }

    /// The messages that bring an arriving player up to date.
    pub fn messages(&self) -> Vec<Vec<u8>> {
        self.list.iter().map(|c| c.now.message()).collect()
    }

    /// Drop the changes whose time is up; returns the messages that grow
    /// them back.
    pub fn expire(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        self.list.retain(|c| match c.until {
            Some(t) if t <= now => {
                out.push(c.old.message());
                false
            }
            _ => true,
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

    fn bush_cut() -> Vec<u8> {
        // The 2.05 lab capture's cut bush: x 39, y 14, 2 x 2.
        let m = seihai_core::net::board_modify_message(39, 14, 2, 2, &[1834, 1835, 1850, 1851]);
        m[1..m.len() - 1].to_vec()
    }

    #[test]
    fn a_block_reads_and_goes_out_as_7() {
        let b = Block::parse(&bush_cut()).unwrap();
        assert_eq!((b.x, b.y, b.w, b.h), (39, 14, 2, 2));
        assert_eq!(b.tiles, [1834, 1835, 1850, 1851]);
        let m = b.message();
        assert_eq!(m[0], gchar(PLO_BOARDMODIFY));
        // Core's client reads it back.
        let read = seihai_core::protocol::layouts::parse_board_modify(&m[1..m.len() - 1]).unwrap();
        assert_eq!((read.x, read.y, read.width, read.height), (39, 14, 2, 2));
        assert_eq!(read.tiles, b.tiles);
    }

    #[test]
    fn blocks_off_the_board_or_cut_short_are_dropped() {
        let mut raw = bush_cut();
        raw[0] = gchar(63);
        assert!(Block::parse(&raw).is_none());
        let raw = bush_cut();
        assert!(Block::parse(&raw[..raw.len() - 1]).is_none());
        assert!(Block::parse(&[gchar(1), gchar(1), gchar(0), gchar(1)]).is_none());
    }

    #[test]
    fn a_bush_grows_back_and_a_pushed_block_stays() {
        let file = vec![5u16; SIDE * SIDE];
        let t0 = Instant::now();
        let mut c = Changes::default();
        c.add(Block::parse(&bush_cut()).unwrap(), &file, t0, RESPAWN);
        let push = Block {
            x: 10,
            y: 10,
            w: 4,
            h: 4,
            tiles: vec![9; 16],
        };
        c.add(push, &file, t0, RESPAWN);
        assert_eq!(c.board(&file)[14 * SIDE + 39], 1834);
        assert_eq!(c.board(&file)[10 * SIDE + 10], 9);
        assert_eq!(c.messages().len(), 2);
        assert!(c.expire(t0 + RESPAWN - Duration::from_millis(1)).is_empty());
        let back = c.expire(t0 + RESPAWN);
        assert_eq!(back.len(), 1);
        let read =
            seihai_core::protocol::layouts::parse_board_modify(&back[0][1..back[0].len() - 1])
                .unwrap();
        assert_eq!(read.tiles, [5, 5, 5, 5]);
        assert_eq!(c.board(&file)[14 * SIDE + 39], 5);
        assert_eq!(c.messages().len(), 1);
    }

    #[test]
    fn a_second_change_in_the_same_place_keeps_the_first_old_tiles() {
        let file = vec![5u16; SIDE * SIDE];
        let t0 = Instant::now();
        let mut c = Changes::default();
        let mut b = Block::parse(&bush_cut()).unwrap();
        c.add(b.clone(), &file, t0, RESPAWN);
        b.tiles = vec![7; 4];
        c.add(b, &file, t0, RESPAWN);
        assert_eq!(c.messages().len(), 1);
        let back = c.expire(t0 + RESPAWN);
        let read =
            seihai_core::protocol::layouts::parse_board_modify(&back[0][1..back[0].len() - 1])
                .unwrap();
        assert_eq!(read.tiles, [5, 5, 5, 5]);
    }

    #[test]
    fn a_level_keeps_at_most_max_changes() {
        let file = vec![5u16; SIDE * SIDE];
        let t0 = Instant::now();
        let mut c = Changes::default();
        let at = |i: usize| Block {
            x: (i % SIDE) as u8,
            y: (i / SIDE) as u8,
            w: 1,
            h: 1,
            tiles: vec![9],
        };
        for i in 0..MAX_CHANGES {
            assert!(c.add(at(i), &file, t0, RESPAWN));
        }
        // A new place is refused; the same place is still taken.
        assert!(!c.add(at(MAX_CHANGES), &file, t0, RESPAWN));
        assert!(c.add(at(0), &file, t0, RESPAWN));
        assert_eq!(c.messages().len(), MAX_CHANGES);
        assert_eq!(c.board(&file)[MAX_CHANGES], 5);
    }
}
