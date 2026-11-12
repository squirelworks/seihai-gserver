//! What this server writes differently per client build, in one place
//! (issue #45).
//!
//! The world keeps one model of players, levels and NPCs; each connection
//! writes it out in the form its build reads. Most of the rules for that
//! are `seihai-core`'s version profiles (`ClientVersion::profile()`): the
//! packet ids a build knows, which player props it gets, half-tile or
//! pixel positions, `.gif` or `.png`, gmaps. The server reads those
//! directly.
//!
//! [`ServerProfile`] holds the rest: rules only the server needs, and the
//! places where a lab run with a real client showed that the server must
//! answer a build differently from what the shared profile says. Each
//! override names the run that found it. When `seihai-core`'s profile
//! takes a correction, the override here goes, and the field reads the
//! profile again. Code elsewhere asks this table, never `version == …`.

use seihai_core::protocol::ClientVersion;

/// The server's rules for one client build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerProfile {
    /// `PLO_FILE` carries the GINT5 mod time and the closing line end.
    pub file_modtime: bool,
    /// The extension of the default baddy images.
    pub baddy_image_ext: &'static str,
    /// Loads plain `.gpak` packages.
    pub loads_gpak: bool,
    /// Loads encrypted `.gpake` packages (after `PLO_FILEDECRYPTKEY`).
    pub loads_gpake: bool,
    /// Prop 10 is the bow slot, not the gani: it is not kept from these
    /// senders and not sent to these receivers (`others.rs`).
    pub prop10_bow: bool,
}

/// Builds that load `.gpake`: 4.20r8 through 6.03r7 (lab, 2026-11-06).
/// 4.04r2 and older load neither kind of package.
const GPAKE: &[ClientVersion] = &[
    ClientVersion::V420R8,
    ClientVersion::V500R7,
    ClientVersion::V512,
    ClientVersion::V524R4,
    ClientVersion::V530R3,
    ClientVersion::V531R4,
    ClientVersion::V600R2,
    ClientVersion::V6015,
    ClientVersion::V6034,
    ClientVersion::V603R7,
    ClientVersion::V603R7Linux,
];

impl ServerProfile {
    pub fn of(version: ClientVersion) -> ServerProfile {
        let core = version.profile();
        let mut p = ServerProfile {
            file_modtime: core.files.modtime,
            baddy_image_ext: core.files.image_ext,
            // Plain `.gpak` from 5.x and 6.x (lab, 2026-11-06).
            loads_gpak: GPAKE.contains(&version) && version != ClientVersion::V420R8,
            loads_gpake: GPAKE.contains(&version),
            prop10_bow: core.props.bow_prop10,
        };
        if version == ClientVersion::V205 {
            // Real 2.05 rev 2 takes a file only with the mod time; sent
            // without it, the file is dropped and an NPC with that image
            // is not drawn (Wine lab, 2026-11-05). Other builds before
            // 2.1 are not checked yet and keep the profile's answer.
            p.file_modtime = true;
            // 2.05 rev 2 ships the baddy images as `.png` only: sent
            // `baddygray.gif` it draws the shadow but no soldier, and asks
            // for the file (Wine lab, 2026-11-05). beta4 sends `.png` too.
            p.baddy_image_ext = ".png";
            // 2.05 rev 2 sends and reads prop 10 as the gani (`sword`,
            // `hurt`), as 2.1 on do; the lab GServer relays it to and from
            // a 2.22 unchanged, and that is how the two hurt each other
            // (Wine lab, 2026-11-12, `docs/lab/hurt-2026-11-12.md`). 2.00
            // to 2.04 were not tested and keep the bow slot.
            p.prop10_bow = false;
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_are_only_where_a_lab_found_them() {
        for &v in ClientVersion::all_versions() {
            let (p, core) = (ServerProfile::of(v), v.profile());
            if v == ClientVersion::V205 {
                assert!(p.file_modtime && !core.files.modtime);
                assert_eq!(p.baddy_image_ext, ".png");
                assert!(!p.prop10_bow && core.props.bow_prop10);
            } else {
                assert_eq!(p.file_modtime, core.files.modtime, "{v:?}");
                assert_eq!(p.baddy_image_ext, core.files.image_ext, "{v:?}");
                assert_eq!(p.prop10_bow, core.props.bow_prop10, "{v:?}");
            }
            assert!(!p.loads_gpak || p.loads_gpake, "{v:?}");
        }
    }
}
