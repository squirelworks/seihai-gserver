//! What each 3D client build can take: its row of seihai-core's
//! `VersionProfile::three_d` (Graal_analysis `version-profiles/three-d.toml`).
//!
//! The service picks the row when a 3D link opens, by the TCP login's token
//! and build and the Torque protocol of the connect request: the token's own
//! row (the Linux 4.20 `G3D22126` has one but no version profile), else the
//! build's profile, else the protocol's default build (19: 4.04r2, 21:
//! 4.20r6, 24: 5.12). It logs the row and which values are assumed rather
//! than read, and sends the 5.x `TerrainBlock`, `WaterBlock` and ground
//! cover only to builds whose row takes them.

use super::objects::Ghost;
use seihai_core::protocol::three_d::{self, Field, GameLayer, ThreeD};
use seihai_core::protocol::ClientVersion;

/// One client's row.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub label: &'static str,
    pub row: ThreeD,
    /// The row is the protocol's default, not the client's own.
    pub fallback: bool,
}

fn default_label(protocol: u32) -> Option<&'static str> {
    match protocol {
        19 => Some("4.04r2"),
        21 => Some("4.20r6"),
        24 => Some("5.12"),
        _ => None,
    }
}

/// The row for a client that logged in with `token` as the build labelled
/// `label` (gserver's own name for it) and connects with `protocol`.
pub fn for_client(token: &str, label: &str, protocol: u32) -> Option<Caps> {
    let fits = |r: &ThreeD| u32::from(r.protocol) == protocol;
    if let Some((l, _, row)) = three_d::TOKEN_ROWS.iter().find(|(_, t, _)| *t == token) {
        if fits(row) {
            return Some(Caps {
                label: l,
                row: *row,
                fallback: false,
            });
        }
    }
    if let Some(v) = ClientVersion::from_label(label) {
        if let Some(row) = v.profile().three_d.filter(fits) {
            let label = three_d::ROWS
                .iter()
                .find(|(_, r)| *r == row)
                .map_or(v.label(), |(l, _)| l);
            return Some(Caps {
                label,
                row,
                fallback: false,
            });
        }
    }
    let want = default_label(protocol)?;
    three_d::ROWS
        .iter()
        .find(|(l, _)| *l == want)
        .map(|(l, row)| Caps {
            label: l,
            row: *row,
            fallback: true,
        })
}

impl Caps {
    /// The 5.x `TerrainBlock` and chunk layout (protocol 19's TerrainBlock is
    /// stock TGE's).
    pub fn takes_terrain(&self) -> bool {
        self.row.offers_terrain_chunks() && self.row.game_layer == GameLayer::GameObject
    }

    /// The 5.x `WaterBlock` layout.
    pub fn takes_water(&self) -> bool {
        self.row.water && self.row.game_layer == GameLayer::GameObject
    }

    /// Whether this build is sent a world object: the `terrain`, `water`
    /// and `replicator` lines' objects go only to builds that take them
    /// (ground cover grows on terrain). `all`: `graal3d_terrainall`.
    pub fn offers(&self, o: &Ghost, all: bool) -> bool {
        let era = self.row.game_layer == GameLayer::GameObject;
        match o {
            Ghost::Terrain(_) | Ghost::Replicator(_) => self.takes_terrain() || (all && era),
            Ghost::Water(_) => self.takes_water() || (all && era),
            _ => true,
        }
    }

    /// The log line: the build, and what is assumed.
    pub fn describe(&self) -> String {
        let r = &self.row;
        let mut parts = vec![
            format!("protocol {}", r.protocol),
            format!(
                "terrain {}",
                if self.takes_terrain() {
                    if r.terrain_textures {
                        "streamed"
                    } else {
                        "streamed, untextured"
                    }
                } else {
                    "not sent"
                }
            ),
            format!(
                "water {}",
                if self.takes_water() {
                    "sent"
                } else {
                    "not sent"
                }
            ),
            format!("DTS {} to {}", r.formats.dts_min, r.formats.dts_max),
        ];
        if r.requires_sky {
            parts.push("needs a Sky".into());
        }
        if r.sky_gif {
            parts.push("asks for sky textures as .gif".into());
        }
        if !r.sees_2d_players {
            parts.push("draws no 2D players (the server ghosts them)".into());
        }
        let assumed = if r.all_assumed_fields() {
            "every value assumed".to_string()
        } else {
            let n = Field::ALL.iter().filter(|f| !r.confirmed(**f)).count();
            format!("{n} values assumed")
        };
        parts.push(assumed);
        if self.fallback {
            parts.push("its build has no row; the protocol's default is used".into());
        }
        format!("a {} client: {}", self.label, parts.join(", "))
    }
}

trait AllAssumed {
    fn all_assumed_fields(&self) -> bool;
}

impl AllAssumed for ThreeD {
    fn all_assumed_fields(&self) -> bool {
        Field::ALL.iter().all(|f| !self.confirmed(*f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graal3d::ground::{Terrain, Water};

    #[test]
    fn rows_are_picked_by_token_build_and_protocol() {
        let c = for_client("G3D22126", "4.20r6", 24).unwrap();
        assert_eq!(c.label, "4.20 Linux");
        let c = for_client("G3D14097", "5.12", 24).unwrap();
        assert_eq!(c.label, "5.12");
        assert!(!c.fallback);
        assert!(c.takes_terrain() && c.takes_water());
        let c = for_client("XXXXXXXX", "nonsense", 21).unwrap();
        assert_eq!(c.label, "4.20r6");
        assert!(c.fallback);
        assert!(for_client("XXXXXXXX", "nonsense", 9).is_none());
    }

    #[test]
    fn ground_goes_only_to_builds_that_take_it() {
        let terrain = Ghost::Terrain(Box::new(Terrain::new("t", 8, false)));
        let water = Ghost::Water(Water {
            position: [0.0; 3],
            scale: [1.0; 3],
            surface_texture: Vec::new(),
            liquid_type: 0,
        });
        let p19 = for_client("", "", 19).unwrap();
        assert!(!p19.offers(&terrain, false) && !p19.offers(&water, false));
        assert!(
            !p19.offers(&terrain, true),
            "terrainall is for the GameObject era"
        );
        let r8 = for_client("", "4.20r8", 24).unwrap();
        assert!(r8.describe().contains("4.20r8"), "{}", r8.describe());
    }
}
