//! Resource packages (`.gpak`, `.gpake`).
//!
//! A `.gpak` is a ZIP the client saves and reads by file name. A `.gpake`
//! is the same with each entry encrypted; the client opens it only after
//! `PLO_FILEDECRYPTKEY` (104) for that name. The key file is
//! `config/gpakkeys.txt` (`name.gpake` and 16 hex digits, the line
//! `seihai-servertools gpak pack` writes). It is never served.
//!
//! Who loads what was lab-checked on real clients (2026-11-06): `.gpake`
//! from 4.20r8 through 6.03r7, plain `.gpak` from 5.x and 6.x. 4.04r2 and
//! older load neither, so they are told the send failed. The byte after
//! the name is 1, which 6.03r7 needs for a loose `.enc` file; packages do
//! not check it. Other values of that byte are not sent.

use crate::wire::{self, gchar, gint5, gstring, ids::PLO_FILEDECRYPTKEY, msg};
use seihai_core::protocol::ClientVersion;

/// Which kind of package a file name is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    /// Not a package.
    None,
    /// Plain `.gpak`.
    Plain,
    /// Encrypted `.gpake`.
    Encrypted,
}

/// `.gpak` or `.gpake`, by the name's ending, ignoring case.
pub fn package_kind(name: &str) -> PackageKind {
    let n = name.to_ascii_lowercase();
    if n.ends_with(".gpake") {
        PackageKind::Encrypted
    } else if n.ends_with(".gpak") {
        PackageKind::Plain
    } else {
        PackageKind::None
    }
}

/// Whether `version` loads that kind of package.
pub fn client_loads(version: ClientVersion, kind: PackageKind) -> bool {
    match kind {
        PackageKind::None => true,
        PackageKind::Encrypted => loads_gpake(version),
        PackageKind::Plain => loads_gpak(version),
    }
}

fn loads_gpake(version: ClientVersion) -> bool {
    matches!(
        version,
        ClientVersion::V420R8
            | ClientVersion::V500R7
            | ClientVersion::V512
            | ClientVersion::V524R4
            | ClientVersion::V530R3
            | ClientVersion::V531R4
            | ClientVersion::V600R2
            | ClientVersion::V6015
            | ClientVersion::V6034
            | ClientVersion::V603R7
            | ClientVersion::V603R7Linux
    )
}

fn loads_gpak(version: ClientVersion) -> bool {
    matches!(
        version,
        ClientVersion::V500R7
            | ClientVersion::V512
            | ClientVersion::V524R4
            | ClientVersion::V530R3
            | ClientVersion::V531R4
            | ClientVersion::V600R2
            | ClientVersion::V6015
            | ClientVersion::V6034
            | ClientVersion::V603R7
            | ClientVersion::V603R7Linux
    )
}

/// The key `config/gpakkeys.txt` gives `package`, if the line is 16 hex digits.
pub fn key_in(text: &str, package: &str) -> Option<[u8; 8]> {
    let want = package.rsplit(['/', '\\']).next().unwrap_or(package);
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let mut parts = line.split_whitespace();
        let (Some(name), Some(hex)) = (parts.next(), parts.next()) else {
            continue;
        };
        if name.eq_ignore_ascii_case(want) {
            return parse_key(hex);
        }
    }
    None
}

fn parse_key(hex: &str) -> Option<[u8; 8]> {
    if hex.len() != 16 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 8];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// `PLO_FILEDECRYPTKEY`: lower-cased file name, byte 1, then the key as two
/// little-endian GINT5 words. The message ends in a line feed.
pub fn file_key_packet(name: &str, key: &[u8; 8]) -> Vec<u8> {
    let bare = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let name = wire::latin1(&bare.to_ascii_lowercase());
    let mut payload = Vec::new();
    gstring(&name, &mut payload);
    payload.push(gchar(1));
    let lo = u32::from_le_bytes([key[0], key[1], key[2], key[3]]);
    let hi = u32::from_le_bytes([key[4], key[5], key[6], key[7]]);
    gint5(u64::from(lo), &mut payload);
    gint5(u64::from(hi), &mut payload);
    msg(PLO_FILEDECRYPTKEY, &payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key and the 104 body the 2026-11-06 lab sent for `sample.gpake`
    /// (byte 1, little-endian halves). The line feed is this server's
    /// message end.
    const LAB_KEY: [u8; 8] = [0x13, 0x34, 0x57, 0x79, 0x9B, 0xBC, 0xDF, 0xF1];
    const LAB_BODY: &[u8] = b"\x88\x2csample.gpake\x21\x27\x6a\x7c\x88\x33\x2f\x2e\x9e\x99\x3b";

    #[test]
    fn file_key_matches_the_lab_packet() {
        let pkt = file_key_packet("Sample.GPAKE", &LAB_KEY);
        assert_eq!(pkt, {
            let mut v = LAB_BODY.to_vec();
            v.push(b'\n');
            v
        });
        let from_folder = file_key_packet("gpaks/sample.gpake", &LAB_KEY);
        assert_eq!(from_folder, pkt);
    }

    #[test]
    fn key_file_lines() {
        let text = "\
# comment\n\
other.gpake 0000000000000000\n\
  Sample.GPAKE   133457799bbcdff1\n\
broken.gpake 12345\n";
        assert_eq!(key_in(text, "sample.gpake"), Some(LAB_KEY));
        assert_eq!(key_in(text, "other.gpake"), Some([0; 8]));
        assert_eq!(key_in(text, "broken.gpake"), None);
        assert_eq!(key_in(text, "missing.gpake"), None);
        assert_eq!(key_in(text, "name.gpake"), None);
    }

    #[test]
    fn who_loads_a_package() {
        assert!(client_loads(ClientVersion::V420R8, PackageKind::Encrypted));
        assert!(!client_loads(ClientVersion::V420R8, PackageKind::Plain));
        assert!(!client_loads(ClientVersion::V420R6, PackageKind::Encrypted));
        assert!(!client_loads(ClientVersion::V404R2, PackageKind::Encrypted));
        assert!(!client_loads(ClientVersion::V222, PackageKind::Plain));
        assert!(client_loads(ClientVersion::V500R7, PackageKind::Plain));
        assert!(client_loads(ClientVersion::V500R7, PackageKind::Encrypted));
        assert!(client_loads(ClientVersion::V603R7, PackageKind::Plain));
        assert!(client_loads(
            ClientVersion::V603R7Linux,
            PackageKind::Encrypted
        ));
        assert!(client_loads(ClientVersion::V139, PackageKind::None));
    }
}
