//! Level files (stage 2, skeleton).
//!
//! To do: read `.nw` (`GLEVNW01`: `BOARD`, `LINK`, `SIGN`, `CHEST`, `NPC`,
//! `BADDY` lines), `.graal` and `.zelda` (binary, Z3-V1.03/1.04 read by
//! magic, not extension) from `<world>/levels/`, and gmaps (`GRMAP001`).
//! Seihai's `seihai_core::assets` already reads these for the client; reuse
//! it rather than writing a second reader. Then send:
//! `PLO_LEVELNAME`, `PLO_RAWDATA` + `PLO_BOARDPACKET`, `PLO_LEVELLINK` (1),
//! `PLO_LEVELSIGN` (5), `PLO_LEVELCHEST` (4), `PLO_LEVELMODTIME`,
//! `PLO_SETACTIVELEVEL`, in the order `burst.rs` uses.
//!
//! Level names come from clients: check them like account names (no path
//! separators, no `..`) before they name a file.

/// A level as the world holds it.
#[derive(Debug, Clone)]
pub struct Level {
    pub name: String,
    /// 64 x 64 tiles, row by row.
    pub board: Vec<u16>,
    /// Unix time of the file, for `PLO_LEVELMODTIME` and cached-level checks.
    pub mod_time: u64,
}

impl Level {
    /// The stage 1 level: one tile everywhere.
    pub fn blank(name: &str, tile: u16, mod_time: u64) -> Self {
        Level {
            name: name.to_string(),
            board: vec![tile; 64 * 64],
            mod_time,
        }
    }
}
