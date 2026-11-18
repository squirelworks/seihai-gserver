//! Shots: `PLI_SHOOT` (40) and `PLI_SHOOT2` (48), passed on to the
//! others in the level as `PLO_SHOOT` (175) or `PLO_SHOOT2` (191).
//!
//! The layouts are from the static read of 5.00r7 to 6.03r7
//! (Graal_analysis `protocol/shoot-5007.md`): 5.00r7 sends only 40, 5.12
//! on send 48, 2.17 to 6.00r4 read 175 and 4.04r2 on read 191, so 6.01
//! on read only 191 and 2.17 to 3.153 only 175. A receiver gets the shot
//! in the form it was sent when it reads that form, else converted:
//!
//! - 40 to 191: half-tile x and y become pixels, whole-tile z (+50)
//!   becomes pixels, the gmap cell is the shooter's, and gravity is the
//!   clients' default 2.0 (a 40 has none).
//! - 48 to 175: pixels become half tiles (rounded, kept in 0..=223), the
//!   shooter id is 0, and gravity is dropped (a 175 shot uses the
//!   receiver's own `gravity`).
//!
//! The gani and shoot-param strings are copied as sent. Two-byte fields
//! use the player-prop X2 form: `(|v| << 1) | sign`, as a `GSHORT`.

/// Shortest `PLI_SHOOT` body read: shooter, six one-byte fields and the
/// gani length.
pub const MIN_SHOOT: usize = 10;
/// Shortest `PLI_SHOOT2` body: positions, cell, three one-byte fields,
/// gravity and the gani length (5.00r7 drops a shorter 191 body).
pub const MIN_SHOOT2: usize = 14;

/// The default gravity, 2.0, as 191 carries it (`2.0 × 256`, X2-signed).
const DEFAULT_GRAVITY: u16 = 1024;

fn gc(b: u8) -> i32 {
    b as i32 - 0x20
}

fn x2(v: i32) -> u16 {
    let m = (v.unsigned_abs() << 1).min(28_670) as u16;
    m | u16::from(v < 0)
}

fn read_x2(b: &[u8]) -> i32 {
    let v = ((gc(b[0]) << 7) + gc(b[1])) as u32;
    let m = (v >> 1) as i32;
    if v & 1 == 1 {
        -m
    } else {
        m
    }
}

fn gchar_signed(v: i32) -> u8 {
    (v.clamp(-32, 223) + 0x20) as u8
}

/// A `PLI_SHOOT` body as a `PLO_SHOOT2` body (without the shooter id).
/// `cell` is the shooter's gmap cell, `(0, 0)` off a gmap.
pub fn shoot_to_shoot2(raw: &[u8], cell: (i32, i32)) -> Option<Vec<u8>> {
    if raw.len() < MIN_SHOOT {
        return None;
    }
    let mut out = Vec::with_capacity(raw.len() + 6);
    let x = gc(raw[3]) * 8;
    let y = gc(raw[4]) * 8;
    let z = (gc(raw[5]) - 50) * 16;
    for v in [x, y, z] {
        crate::wire::gshort(x2(v), &mut out);
    }
    out.push(gchar_signed(cell.0));
    out.push(gchar_signed(cell.1));
    out.extend_from_slice(&raw[6..9]);
    crate::wire::gshort(DEFAULT_GRAVITY, &mut out);
    out.extend_from_slice(&raw[9..]);
    Some(out)
}

/// A `PLI_SHOOT2` body as a `PLO_SHOOT` body (without the player id).
pub fn shoot2_to_shoot(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < MIN_SHOOT2 {
        return None;
    }
    let half = |px: i32| ((px as f64 / 8.0).round() as i32).clamp(0, 223) as u8 + 0x20;
    let x = read_x2(&raw[0..2]);
    let y = read_x2(&raw[2..4]);
    let z = read_x2(&raw[4..6]);
    let mut out = Vec::with_capacity(raw.len());
    out.extend_from_slice(b"   ");
    out.push(half(x));
    out.push(half(y));
    out.push(((z as f64 / 16.0).floor() as i32 + 50).clamp(0, 223) as u8 + 0x20);
    out.extend_from_slice(&raw[8..11]);
    out.extend_from_slice(&raw[13..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 5.00r7's 40 for a shot at (30.5, 20) tiles, z 0, angle 55, zangle
    /// 110, power 44 (1.0), gani `arrow`, no params.
    fn shot40() -> Vec<u8> {
        let mut b = b"   ".to_vec();
        b.extend([
            61 + 0x20,
            40 + 0x20,
            50 + 0x20,
            55 + 0x20,
            110 + 0x20,
            44 + 0x20,
        ]);
        b.push(5 + 0x20);
        b.extend_from_slice(b"arrow");
        b.push(0x20);
        b
    }

    #[test]
    fn x2_round_trip() {
        for v in [0, 1, -1, 488, -488, 8000] {
            let mut b = Vec::new();
            crate::wire::gshort(x2(v), &mut b);
            assert_eq!(read_x2(&b), v, "{v}");
        }
        // The note's table: gravity 2.0 is `( `.
        let mut b = Vec::new();
        crate::wire::gshort(DEFAULT_GRAVITY, &mut b);
        assert_eq!(b, b"( ");
    }

    #[test]
    fn shoot_to_shoot2_and_back() {
        let s = shot40();
        let two = shoot_to_shoot2(&s, (1, 2)).unwrap();
        assert!(two.len() >= MIN_SHOOT2);
        assert_eq!(read_x2(&two[0..2]), 61 * 8);
        assert_eq!(read_x2(&two[2..4]), 40 * 8);
        assert_eq!(read_x2(&two[4..6]), 0);
        assert_eq!(&two[6..8], &[0x21, 0x22]);
        assert_eq!(&two[8..11], &s[6..9]);
        assert_eq!(&two[11..13], b"( ");
        assert_eq!(&two[13..], &s[9..]);
        // Back to 40: the same bytes, the shooter 0.
        assert_eq!(shoot2_to_shoot(&two).unwrap(), s);
    }

    #[test]
    fn negative_z_and_short_bodies() {
        let mut s = shot40();
        s[5] = 48 + 0x20; // z = -2 tiles
        let two = shoot_to_shoot2(&s, (0, 0)).unwrap();
        assert_eq!(read_x2(&two[4..6]), -32);
        assert_eq!(shoot2_to_shoot(&two).unwrap()[5], 48 + 0x20);
        assert_eq!(shoot_to_shoot2(&s[..5], (0, 0)), None);
        assert_eq!(shoot2_to_shoot(&two[..10]), None);
    }
}
