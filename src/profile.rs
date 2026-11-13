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

use crate::wire::ids::PLO_PING;
use seihai_core::protocol::{ClientVersion, VersionProfile};

/// Everything one connection's writes depend on, resolved once at login
/// (`docs/CLIENT-PROFILES.md`): the build, its shared `seihai-core` row
/// and this server's rules for it. Writers ask this, not the version.
#[derive(Debug, Clone, Copy)]
pub struct ClientProfile {
    pub version: ClientVersion,
    /// The shared table: packet ids, props, files, levels, scripts.
    pub core: &'static VersionProfile,
    /// Server-only rules and lab overrides.
    pub server: ServerProfile,
}

impl From<ClientVersion> for ClientProfile {
    fn from(v: ClientVersion) -> ClientProfile {
        ClientProfile::of(v)
    }
}

impl ClientProfile {
    pub fn of(version: ClientVersion) -> ClientProfile {
        ClientProfile {
            version,
            core: version.profile(),
            server: ServerProfile::of(version),
        }
    }

    /// Whether the build reads server-to-client message `id`.
    pub fn reads(&self, id: u8) -> bool {
        self.core.protocol.plo.contains(id)
    }
}

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
    /// Reads `PLO_PING` (108) and answers it with `PLI_PONG` (162), so the
    /// ping timer runs for it (`ping.rs`, issue #44).
    pub ping: bool,
    /// Another player's id, at the head of `PLO_OTHERPLPROPS` and in
    /// private messages, is one `GCHAR` (capped at 223), not a `GSHORT`
    /// (`others.rs`, [`ONE_BYTE_ID`]).
    pub one_byte_id: bool,
    /// The extension the build's own player art has: sword, shield, head
    /// and body names in player props go with it, and the stock ones as
    /// their preset number (`others::for_receiver`).
    pub image_ext: &'static str,
    /// Status (prop 18) value 4 means "swimming", not "male" as from 2.0
    /// on. Another player's male bit would make the build draw that
    /// player as a swimmer, with no body (`others::for_receiver`).
    pub status_4_swims: bool,
}

/// Builds that read and write another player's id as one `GCHAR`: up to
/// 1.35 (1.35 from exact-build static evidence, graal-client-repro#179;
/// 1.21 to 1.34 from the packet interface notes). 1.36 on use a `GSHORT`.
/// Goes when `seihai-core`'s profile carries the id width.
const ONE_BYTE_ID: &[ClientVersion] = &[
    ClientVersion::V121,
    ClientVersion::V128B2,
    ClientVersion::V13T,
    ClientVersion::V131,
    ClientVersion::V132,
    ClientVersion::V133,
    ClientVersion::V134,
    ClientVersion::V135,
];

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

/// Builds that take 108 before their main switch, as 4.20 does, so the
/// profile's PLO list (read from the switch alone) leaves it out: Linux
/// 6.01r1 and 6.03r7, and the Windows 6.03r7 (Graal_analysis
/// `protocol/unknown-ids-2026-11-06.md`, "PLI 162 and PLO 108", and
/// `protocol/v6x-clients.md`, both from the binaries). 6.015 and 6.034
/// have no binary and take their id sets from those two Linux builds, so
/// they follow them. 6.00r2 is not listed: its switch was read, not what
/// comes before it. Goes when `seihai-core`'s 6.x profiles list 108
/// (squirelworks/seihai-core#7).
const PING_BEFORE_SWITCH: &[ClientVersion] = &[
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
            ping: core.protocol.plo.contains(PLO_PING) || PING_BEFORE_SWITCH.contains(&version),
            one_byte_id: ONE_BYTE_ID.contains(&version),
            image_ext: core.files.image_ext,
            // A static read of 1.39 shows status value 4 setting its
            // swimming flag, and the draw skipping every body part while
            // it is set; a real 1.39 drew others as heads only until the
            // bit was cleared (GServer-v2 fork lab, 2026-11-13, #27 rerun).
            // The 1.x line shares the status prop's layout.
            status_4_swims: version.label().starts_with("1."),
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
            // The same for its player art: 2.05 rev 2 draws `head0.png`
            // and `body.png` players (Wine lab, 2026-11-12, hurt run).
            p.image_ext = ".png";
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
                assert_eq!(p.image_ext, ".png");
                assert!(!p.prop10_bow && core.props.bow_prop10);
            } else {
                assert_eq!(p.file_modtime, core.files.modtime, "{v:?}");
                assert_eq!(p.baddy_image_ext, core.files.image_ext, "{v:?}");
                assert_eq!(p.image_ext, core.files.image_ext, "{v:?}");
                assert_eq!(p.prop10_bow, core.props.bow_prop10, "{v:?}");
            }
            assert!(!p.loads_gpak || p.loads_gpake, "{v:?}");
            let core_ping = core.protocol.plo.contains(PLO_PING);
            if PING_BEFORE_SWITCH.contains(&v) {
                // Drop the override once seihai-core lists 108 for 6.x.
                assert!(p.ping && !core_ping, "{v:?}: override no longer needed");
            } else {
                assert_eq!(p.ping, core_ping, "{v:?}");
            }
        }
    }

    #[test]
    fn a_client_profile_is_the_core_row_and_the_server_rules() {
        for &v in ClientVersion::all_versions() {
            let c = ClientProfile::of(v);
            assert!(std::ptr::eq(c.core, v.profile()));
            assert_eq!(c.server, ServerProfile::of(v));
            for id in [7u8, 37, 108, 178, 186] {
                assert_eq!(
                    c.reads(id),
                    v.profile().protocol.plo.contains(id),
                    "{v:?} {id}"
                );
            }
        }
        assert!(ServerProfile::of(ClientVersion::V135).one_byte_id);
        assert!(!ServerProfile::of(ClientVersion::V136).one_byte_id);
        for v in [
            ClientVersion::V121,
            ClientVersion::V139,
            ClientVersion::V141,
        ] {
            assert!(ServerProfile::of(v).status_4_swims, "{v:?}");
        }
        for v in [
            ClientVersion::V200,
            ClientVersion::V205,
            ClientVersion::V222,
        ] {
            assert!(!ServerProfile::of(v).status_4_swims, "{v:?}");
        }
    }

    #[test]
    fn who_is_pinged() {
        use ClientVersion::*;
        for v in [V420R6, V420R8, V6015, V6034, V603R7, V603R7Linux] {
            assert!(ServerProfile::of(v).ping, "{v:?}");
        }
        // 5.x and 6.00r2 neither handle 108 nor (5.x) send 162; nothing
        // before 4.20 reads it.
        for v in [V404R2, V500R7, V512, V524R4, V530R3, V531R4, V600R2, V205] {
            assert!(!ServerProfile::of(v).ping, "{v:?}");
        }
    }
}
