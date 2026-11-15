//! Gmaps (stage 2.5): grids of ordinary levels, read from the `.gmap`
//! files (`GRMAP001`) in `<world>/levels/` with Seihai's own reader
//! (`seihai_core::gmap::Gmap`).
//!
//! A player whose build plays gmaps (`GmapRules`: `levels.gmap`, message 49,
//! own props 43/44 and `PLI_ADJACENTLEVEL`) and whose level is a cell of a
//! gmap plays it as one: the login and links into it push the `.gmap` file
//! then say `PLO_PLAYERWARP2` (49) with the gmap's name and the cell, every
//! level sent ends with a `PLO_LEVELNAME` naming the gmap again, the client
//! asks for the neighbours with `PLI_ADJACENTLEVEL` (35), and walking across
//! a seam is a props message with the new cell (43 `GMAPLEVELX`, 44
//! `GMAPLEVELY`). Older builds get the same levels as plain levels, joined
//! by their links.
//!
//! See Seihai `documentation/protocol.md`, "Gmaps".
//!
//! Pre-3.0 builds parse the `.gmap` from disk when they handle
//! `PLO_PLAYERWARP2` (49). The world therefore keeps the file bytes so a
//! login or warp can push them before 49 (see
//! Graal_analysis `protocol/gmap-file-on-warp.md`).

use seihai_core::gmap::{Gmap, GmapRules};
use seihai_core::protocol::ClientVersion;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// Own props that name the gmap cell.
pub const GMAPLEVELX: u8 = 43;
pub const GMAPLEVELY: u8 = 44;

/// A player's place on a gmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// The gmap's file name, as in the folder (`world.gmap`).
    pub gmap: String,
    /// Column and row.
    pub cell: (u8, u8),
}

/// Whether `version` plays gmaps.
pub fn plays_gmaps(version: ClientVersion) -> bool {
    let r = GmapRules::for_version(version);
    r.enabled && r.adjacent
}

/// Whether a gmap warp must push the `.gmap` file before every 49.
///
/// Matches GServer-v2's `CLVER_3` (3.0) cut: older builds load `LEVELNAMES`
/// from disk when they handle 49.
pub fn needs_gmap_file_before_warp(version: ClientVersion) -> bool {
    plays_gmaps(version) && !clver3_or_later(version)
}

/// GServer-v2 `CLVER_3` (3.0) and every later profile this server knows.
fn clver3_or_later(version: ClientVersion) -> bool {
    matches!(
        version,
        ClientVersion::V304
            | ClientVersion::V3153
            | ClientVersion::V404R2
            | ClientVersion::V404R2Mac
            | ClientVersion::V420R6
            | ClientVersion::V420R8
            | ClientVersion::V500R7
            | ClientVersion::V512
            | ClientVersion::V512Mac
            | ClientVersion::V524R4
            | ClientVersion::V530R3
            | ClientVersion::V531R4
            | ClientVersion::V600R2
            | ClientVersion::V600R4
            | ClientVersion::V6015
            | ClientVersion::V6034
            | ClientVersion::V603R7
            | ClientVersion::V603R7Linux
    )
}

/// One loaded gmap: grid, raw file bytes and mod time for a file push.
#[derive(Debug)]
struct Entry {
    name: String,
    map: Gmap,
    data: Vec<u8>,
    mod_time: u64,
}

/// The world's gmaps.
#[derive(Debug, Default)]
pub struct Gmaps {
    maps: Vec<Entry>,
}

impl Gmaps {
    /// Every `.gmap` file in `dir`, in name order. A file that does not
    /// read is reported in the returned warnings and left out; a missing
    /// folder is a world without gmaps.
    pub fn load(dir: &Path) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut files: Vec<_> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| n.to_ascii_lowercase().ends_with(".gmap"))
                .collect(),
            Err(_) => Vec::new(),
        };
        files.sort();
        let mut maps = Vec::new();
        for name in files {
            let path = dir.join(&name);
            let data = std::fs::read(&path).unwrap_or_default();
            let text = String::from_utf8_lossy(&data).into_owned();
            let mod_time = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            match Gmap::parse(&text) {
                Some(map) => maps.push(Entry {
                    name,
                    map,
                    data,
                    mod_time,
                }),
                None => warnings.push(format!("{name}: not a GRMAP001 gmap; left out")),
            }
        }
        (Gmaps { maps }, warnings)
    }

    /// The gmaps as given, for tests. File bytes are the empty string.
    pub fn from_maps(maps: Vec<(String, Gmap)>) -> Self {
        Gmaps {
            maps: maps
                .into_iter()
                .map(|(name, map)| Entry {
                    name,
                    map,
                    data: Vec::new(),
                    mod_time: 0,
                })
                .collect(),
        }
    }

    /// The gmap and cell `level` is in (the first gmap that names it).
    pub fn find(&self, level: &str) -> Option<Place> {
        self.maps.iter().find_map(|e| {
            let (x, y) = e.map.cell_of(level)?;
            Some(Place {
                gmap: e.name.clone(),
                cell: (u8::try_from(x).ok()?, u8::try_from(y).ok()?),
            })
        })
    }

    /// The level at `cell` of the gmap called `gmap`, if the cell holds one.
    pub fn level_at(&self, gmap: &str, cell: (u8, u8)) -> Option<&str> {
        let e = self
            .maps
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(gmap))?;
        e.map.level_at(i32::from(cell.0), i32::from(cell.1))
    }

    /// Frames that send the `.gmap` file named `gmap` to `version`, or
    /// none when the world has no bytes for it or the build cannot take it.
    pub fn file_frames(
        &self,
        version: impl Into<crate::profile::ClientProfile>,
        gmap: &str,
    ) -> Option<Vec<Vec<u8>>> {
        let e = self
            .maps
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(gmap))?;
        if e.data.is_empty() {
            return None;
        }
        crate::files::send_file(
            version,
            &crate::files::Found {
                name: e.name.clone(),
                data: e.data.clone(),
                mod_time: e.mod_time,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: &str = "GRMAP001\nWIDTH 2\nHEIGHT 2\nLEVELNAMES\n\
\"gm_nw.nw\",\"gm_ne.nw\",\n\"gm_sw.nw\",\"\",\nLEVELNAMESEND\n";

    #[test]
    fn cells_and_levels() {
        let dir = std::env::temp_dir().join(format!("seihai-gserver-gmaps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("World.gmap"), GRID).unwrap();
        std::fs::write(dir.join("bad.gmap"), "nothing").unwrap();
        std::fs::write(dir.join("gm_nw.nw"), "GLEVNW01\n").unwrap();
        let (g, warnings) = Gmaps::load(&dir);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let p = g.find("GM_NE.nw").unwrap();
        assert_eq!((p.gmap.as_str(), p.cell), ("World.gmap", (1, 0)));
        assert_eq!(g.level_at("world.gmap", (0, 1)), Some("gm_sw.nw"));
        assert_eq!(g.level_at("world.gmap", (1, 1)), None, "a hole");
        assert_eq!(g.level_at("world.gmap", (2, 0)), None, "off the grid");
        assert!(g.find("elsewhere.nw").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn which_builds_play_gmaps() {
        assert!(plays_gmaps(ClientVersion::V222));
        assert!(plays_gmaps(ClientVersion::V231));
        assert!(!plays_gmaps(ClientVersion::V205));
        assert!(!plays_gmaps(ClientVersion::V139));
    }

    #[test]
    fn pre_3_gmap_builds_need_the_file_before_49() {
        assert!(needs_gmap_file_before_warp(ClientVersion::V222));
        assert!(needs_gmap_file_before_warp(ClientVersion::V231));
        assert!(!needs_gmap_file_before_warp(ClientVersion::V205));
        assert!(!needs_gmap_file_before_warp(ClientVersion::V420R8));
    }

    #[test]
    fn file_frames_carry_the_gmap_bytes() {
        let missing = std::env::temp_dir().join(format!(
            "seihai-gserver-gmaps-missing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&missing);
        let (g, _) = Gmaps::load(&missing);
        assert!(g.file_frames(ClientVersion::V222, "world.gmap").is_none());

        let dir = std::env::temp_dir().join(format!(
            "seihai-gserver-gmaps-bytes-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("world.gmap"), GRID).unwrap();
        let (g, warnings) = Gmaps::load(&dir);
        assert!(warnings.is_empty(), "{warnings:?}");
        let frames = g.file_frames(ClientVersion::V222, "world.gmap").unwrap();
        let body = frames.concat();
        assert!(body.windows(GRID.len()).any(|w| w == GRID.as_bytes()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
