//! The world task (stage 2, skeleton).
//!
//! One tokio task will own every level and player. Connection tasks send it
//! [`WorldIn`] events and get message bodies back through their [`Outbox`];
//! the world never sees a codec. See `docs/ARCHITECTURE.md`, "The world task".
//!
//! To do, in order:
//! 1. Move `server::Online` here: `Joined` takes the account and returns the
//!    player id, or the refusal text ("Account already in use.").
//! 2. Build the burst here with the level from `levels.rs` instead of the
//!    generated board (`burst::login_burst` takes the board as an argument).
//! 3. Other players: `PLO_OTHERPLPROPS` (8) on join, prop changes, leave.
//!    Only props in each receiver's `props.other` set.
//! 4. `PLI_LEVELWARP` (0) and `PLI_LEVELWARPMOD` (30): load the level, send
//!    it (see Seihai `documentation/protocol.md`, "Level changes"); a current
//!    mod time gets no board.
//! 5. Gmaps: `PLI_ADJACENTLEVEL` (35), cell props 43/44 (`gmap-order.md`,
//!    `bigmap-neighbours.md` in the seihai repo).
//! 6. Flags (`PLI_FLAGSET`), chests, items on the ground.

use std::time::Duration;
use tokio::sync::mpsc;

/// The world tick. Real 2.x clients send walk props every 100 ms and expect
/// replies within a frame.
pub const TICK: Duration = Duration::from_millis(50);

/// What a connection tells the world.
#[derive(Debug)]
pub enum WorldIn {
    /// A login passed the allow-list and the password check.
    Joined { account: String, outbox: Outbox },
    /// `PLI_PLAYERPROPS` (2), as sent.
    Props { player: u16, raw: Vec<u8> },
    /// `PLI_LEVELWARP` (0) or `PLI_LEVELWARPMOD` (30).
    LevelWarp {
        player: u16,
        level: String,
        x: f32,
        y: f32,
        mod_time: Option<u64>,
    },
    /// The connection closed.
    Left { player: u16 },
}

/// Message bodies for one player; the connection task frames and sends them.
pub type Outbox = mpsc::UnboundedSender<Vec<u8>>;
