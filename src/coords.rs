//! Level-local vs gmap-global player positions.
//!
//! Graal levels are 64×64 tiles. On a gmap, a player's **local** position is
//! inside the current cell (typically 0..64); their **global** position is
//! `cell * 64 + local`, used for anti-cheat walking, hurt range, and the
//! client's internal overworld coordinates.
//!
//! Wire arrivals (`PLI_LEVELWARP`, RC/NPC `WARP_PLAYER`, login / account X/Y,
//! `PLO_PLAYERWARP` / `PLO_PLAYERWARP2` x/y) are always **local**. Cell
//! indices travel separately (`Place`, or PLAYERWARP2's gmap x/y bytes).
//! Mixing the two spaces is a warp bug: this module keeps them as distinct
//! types with no implicit conversion.

/// Tiles along one axis of a single level / gmap cell.
pub const TILES_PER_CELL: f32 = 64.0;

/// A position inside one level (or one gmap cell), in tiles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalPos {
    pub x: f32,
    pub y: f32,
}

impl LocalPos {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Lift into gmap-global space for `cell` (off a gmap, pass `(0, 0)`).
    pub fn to_global(self, cell: (u8, u8)) -> GlobalPos {
        GlobalPos {
            x: f32::from(cell.0) * TILES_PER_CELL + self.x,
            y: f32::from(cell.1) * TILES_PER_CELL + self.y,
        }
    }
}

/// A position on a gmap in tiles (`cell * 64 + local`), or the same as
/// [`LocalPos`] when the player is not on a gmap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobalPos {
    pub x: f32,
    pub y: f32,
}

impl GlobalPos {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Drop back to level-local space for `cell`.
    pub fn to_local(self, cell: (u8, u8)) -> LocalPos {
        LocalPos {
            x: self.x - f32::from(cell.0) * TILES_PER_CELL,
            y: self.y - f32::from(cell.1) * TILES_PER_CELL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_and_global_round_trip_on_a_nonzero_cell() {
        let local = LocalPos::new(12.5, 40.0);
        let global = local.to_global((2, 1));
        assert_eq!(global, GlobalPos::new(2.0 * 64.0 + 12.5, 64.0 + 40.0));
        assert_eq!(global.to_local((2, 1)), local);
    }

    #[test]
    fn off_a_gmap_global_equals_local() {
        let local = LocalPos::new(30.0, 30.5);
        assert_eq!(local.to_global((0, 0)), GlobalPos::new(30.0, 30.5));
    }
}
