//! Gmaps (stage 2.5): grids of ordinary levels, read from the `.gmap`
//! files (`GRMAP001`) in `<world>/levels/` with Seihai's own reader
//! (`seihai_core::gmap::Gmap`).
//!
//! A player whose build plays gmaps (`GmapRules`: `levels.gmap`, message 49,
//! own props 43/44 and `PLI_ADJACENTLEVEL`) and whose level is a cell of a
//! gmap plays it as one: the login and links into it say `PLO_PLAYERWARP2`
//! (49) with the gmap's name and the cell, every level sent ends with a
//! `PLO_LEVELNAME` naming the gmap again, the client asks for the
//! neighbours with `PLI_ADJACENTLEVEL` (35), and walking across a seam is a
//! props message with the new cell (43 `GMAPLEVELX`, 44 `GMAPLEVELY`). Older
//! builds get the same levels as plain levels, joined by their links.
//!
//! See Seihai `documentation/protocol.md`, "Gmaps".

use seihai_core::gmap::{Gmap, GmapRules};
use seihai_core::protocol::ClientVersion;
use std::path::Path;

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

/// The world's gmaps.
#[derive(Debug, Default)]
pub struct Gmaps {
    maps: Vec<(String, Gmap)>,
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
            let text = std::fs::read(dir.join(&name))
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            match Gmap::parse(&text) {
                Some(g) => maps.push((name, g)),
                None => warnings.push(format!("{name}: not a GRMAP001 gmap; left out")),
            }
        }
        (Gmaps { maps }, warnings)
    }

    /// The gmaps as given, for tests.
    pub fn from_maps(maps: Vec<(String, Gmap)>) -> Self {
        Gmaps { maps }
    }

    /// The gmap and cell `level` is in (the first gmap that names it).
    pub fn find(&self, level: &str) -> Option<Place> {
        self.maps.iter().find_map(|(name, g)| {
            let (x, y) = g.cell_of(level)?;
            Some(Place {
                gmap: name.clone(),
                cell: (u8::try_from(x).ok()?, u8::try_from(y).ok()?),
            })
        })
    }

    /// The level at `cell` of the gmap called `gmap`, if the cell holds one.
    pub fn level_at(&self, gmap: &str, cell: (u8, u8)) -> Option<&str> {
        let (_, g) = self
            .maps
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(gmap))?;
        g.level_at(i32::from(cell.0), i32::from(cell.1))
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
}
