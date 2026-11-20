//! Low-depth PNGs rewritten with 8 bits per pixel, for builds that read
//! only 8.
//!
//! The NewWorld betas draw a 1, 2 or 4-bit PNG garbled, on an unkeyed
//! black square: the stock `head0.png` (32 x 560, 4-bit palette with
//! `tRNS`) came out that way on beta 1 (Wine lab, 2026-11-20). Every PNG
//! the betas ship is 8-bit (palette or RGB), and the beta readme says it
//! does not read 4-bit PNGs. So a palette or grey PNG under 8 bits goes to
//! them with each pixel widened to one byte, the same palette and the same
//! transparency.
//!
//! The rewrite keeps the standard chunks it understands (`PLTE`, `tRNS`,
//! `gAMA`, `sRGB`, `pHYs`, `tIME` and the text chunks) and drops the rest,
//! as the PNG rules ask of an editor that changes the image data. An
//! interlaced or broken file, or any other kind, is left as it is.

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{Read, Write};

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
/// Chunks copied to the rewritten file. `tRNS` is changed for a grey image.
const KEEP: &[&[u8; 4]] = &[
    b"PLTE", b"tRNS", b"gAMA", b"sRGB", b"pHYs", b"tIME", b"tEXt", b"zTXt", b"iTXt",
];
/// The most image bytes a rewrite will unpack (the file cap is 16 MB).
const MAX_PIXELS: usize = 64 * 1024 * 1024;

struct Chunk<'a> {
    kind: [u8; 4],
    data: &'a [u8],
}

fn chunks(png: &[u8]) -> Option<Vec<Chunk<'_>>> {
    let mut rest = png.strip_prefix(SIGNATURE)?;
    let mut out = Vec::new();
    while !rest.is_empty() {
        let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        let kind: [u8; 4] = rest.get(4..8)?.try_into().ok()?;
        let data = rest.get(8..8 + len)?;
        rest = rest.get(12 + len..)?;
        out.push(Chunk { kind, data });
        if &kind == b"IEND" {
            break;
        }
    }
    Some(out)
}

fn put_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

/// Undo the PNG row filters in place. Every row is `1 + stride` bytes;
/// `bpp` is the filter's byte step (1 under 8 bits per pixel).
fn unfilter(raw: &mut [u8], stride: usize, bpp: usize) -> Option<()> {
    let row = stride + 1;
    for y in 0..raw.len() / row {
        let (done, cur) = raw.split_at_mut(y * row);
        let prev = if y == 0 {
            None
        } else {
            Some(&done[(y - 1) * row + 1..])
        };
        let (kind, cur) = cur[..row].split_first_mut()?;
        for x in 0..stride {
            let a = if x >= bpp { cur[x - bpp] } else { 0 };
            let b = prev.map_or(0, |p| p[x]);
            let c = if x >= bpp {
                prev.map_or(0, |p| p[x - bpp])
            } else {
                0
            };
            let add = match *kind {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                4 => {
                    let p = i16::from(a) + i16::from(b) - i16::from(c);
                    let (pa, pb, pc) = (
                        (p - i16::from(a)).abs(),
                        (p - i16::from(b)).abs(),
                        (p - i16::from(c)).abs(),
                    );
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => return None,
            };
            cur[x] = cur[x].wrapping_add(add);
        }
    }
    Some(())
}

/// `png` with 8 bits per pixel, when it is a non-interlaced palette (3) or
/// grey (0) image with 1, 2 or 4; `None` otherwise (send it as it is).
pub fn widen_to_8_bit(png: &[u8]) -> Option<Vec<u8>> {
    let list = chunks(png)?;
    let ihdr = list.first().filter(|c| &c.kind == b"IHDR")?.data;
    if ihdr.len() != 13 {
        return None;
    }
    let w = u32::from_be_bytes(ihdr[0..4].try_into().ok()?) as usize;
    let h = u32::from_be_bytes(ihdr[4..8].try_into().ok()?) as usize;
    let (depth, color, interlace) = (ihdr[8], ihdr[9], ihdr[12]);
    if !matches!(depth, 1 | 2 | 4) || !matches!(color, 0 | 3) || interlace != 0 {
        return None;
    }
    if ihdr[10] != 0 || ihdr[11] != 0 || w == 0 || h == 0 || w.checked_mul(h)? > MAX_PIXELS {
        return None;
    }
    let mut zipped = Vec::new();
    for c in list.iter().filter(|c| &c.kind == b"IDAT") {
        zipped.extend_from_slice(c.data);
    }
    let stride = (w * usize::from(depth)).div_ceil(8);
    let mut raw = Vec::with_capacity((stride + 1) * h);
    ZlibDecoder::new(&zipped[..])
        .take(((stride + 1) * h) as u64)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() != (stride + 1) * h {
        return None;
    }
    unfilter(&mut raw, stride, 1)?;
    // Grey samples scale to the full byte (a 4-bit 15 is 255); palette
    // indexes stay as they are.
    let max = (1u16 << depth) - 1;
    let scale = |v: u16| {
        if color == 0 {
            (v * 255 / max) as u8
        } else {
            v as u8
        }
    };
    let mut wide = Vec::with_capacity((w + 1) * h);
    for row in raw.chunks(stride + 1) {
        wide.push(0);
        let bits = &row[1..];
        for x in 0..w {
            let bit = x * usize::from(depth);
            let byte = bits[bit / 8];
            let shift = 8 - usize::from(depth) - bit % 8;
            wide.push(scale((u16::from(byte) >> shift) & max));
        }
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(&wide).ok()?;
    let idat = z.finish().ok()?;

    let mut out = SIGNATURE.to_vec();
    let mut head = ihdr.to_vec();
    head[8] = 8;
    put_chunk(&mut out, b"IHDR", &head);
    let mut wrote_idat = false;
    for c in &list[1..] {
        if &c.kind == b"IDAT" {
            if !wrote_idat {
                put_chunk(&mut out, b"IDAT", &idat);
                wrote_idat = true;
            }
        } else if &c.kind == b"tRNS" && color == 0 {
            // The grey key is one 16-bit sample in the old depth.
            let key = u16::from_be_bytes(c.data.get(..2)?.try_into().ok()?) & max;
            put_chunk(&mut out, b"tRNS", &u16::from(scale(key)).to_be_bytes());
        } else if KEEP.contains(&&c.kind) {
            put_chunk(&mut out, &c.kind, c.data);
        }
    }
    if !wrote_idat {
        return None;
    }
    put_chunk(&mut out, b"IEND", &[]);
    Some(out)
}

#[cfg(test)]
/// A PNG from rows of `depth`-bit pixels, each row filtered with
/// `filter` (0 or 1, Sub), plus `extra` chunks before the image data.
pub(crate) fn sample(
    w: u32,
    depth: u8,
    color: u8,
    rows: &[Vec<u8>],
    filter: u8,
    extra: &[(&[u8; 4], &[u8])],
) -> Vec<u8> {
    let mut out = SIGNATURE.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&(rows.len() as u32).to_be_bytes());
    ihdr.extend_from_slice(&[depth, color, 0, 0, 0]);
    put_chunk(&mut out, b"IHDR", &ihdr);
    for (k, d) in extra {
        put_chunk(&mut out, k, d);
    }
    let mut raw = Vec::new();
    for r in rows {
        let stride = (w as usize * usize::from(depth)).div_ceil(8);
        let mut packed = vec![0u8; stride];
        for (x, &v) in r.iter().enumerate() {
            let bit = x * usize::from(depth);
            packed[bit / 8] |= v << (8 - usize::from(depth) - bit % 8);
        }
        raw.push(filter);
        if filter == 1 {
            let mut prev = 0u8;
            for b in packed {
                raw.push(b.wrapping_sub(prev));
                prev = b;
            }
        } else {
            raw.extend(packed);
        }
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(&raw).unwrap();
    put_chunk(&mut out, b"IDAT", &z.finish().unwrap());
    put_chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::sample as png;
    use super::*;

    /// The 8-bit rows and the kept chunks of a rewritten file.
    type Chunks = Vec<([u8; 4], Vec<u8>)>;

    fn read(png: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>, Chunks) {
        let list = chunks(png).unwrap();
        let ihdr = list[0].data.to_vec();
        let mut z = Vec::new();
        for c in list.iter().filter(|c| &c.kind == b"IDAT") {
            z.extend_from_slice(c.data);
        }
        let mut raw = Vec::new();
        ZlibDecoder::new(&z[..]).read_to_end(&mut raw).unwrap();
        let w = u32::from_be_bytes(ihdr[0..4].try_into().unwrap()) as usize;
        let rows = raw.chunks(w + 1).map(|r| {
            assert_eq!(r[0], 0);
            r[1..].to_vec()
        });
        let others = list
            .iter()
            .filter(|c| !matches!(&c.kind, b"IHDR" | b"IDAT" | b"IEND"))
            .map(|c| (c.kind, c.data.to_vec()))
            .collect();
        (ihdr, rows.collect(), others)
    }

    #[test]
    fn a_4_bit_palette_image_keeps_its_indexes_palette_and_keys() {
        let rows = vec![vec![0, 1, 2, 15, 7], vec![3, 3, 0, 14, 1]];
        let plte = [0u8; 48];
        let trns = [0u8, 255, 255];
        for filter in [0, 1] {
            let src = png(
                5,
                4,
                3,
                &rows,
                filter,
                &[(b"PLTE", &plte), (b"caBX", b"junk"), (b"tRNS", &trns)],
            );
            let out = widen_to_8_bit(&src).unwrap();
            let (ihdr, got, others) = read(&out);
            assert_eq!(ihdr[8], 8, "8 bits per pixel");
            assert_eq!(ihdr[9], 3, "still a palette image");
            assert_eq!(got, rows, "filter {filter}");
            // The private chunk goes; the palette and the keys stay.
            assert_eq!(
                others,
                vec![(*b"PLTE", plte.to_vec()), (*b"tRNS", trns.to_vec())]
            );
        }
    }

    #[test]
    fn one_and_two_bit_images_and_odd_widths() {
        let rows = vec![vec![1, 0, 1, 1, 0, 0, 1, 0, 1, 1, 1]; 3];
        let (_, got, _) = read(&widen_to_8_bit(&png(11, 1, 3, &rows, 0, &[])).unwrap());
        assert_eq!(got, rows);
        let rows = vec![vec![3, 2, 1, 0, 3, 1, 2]; 2];
        let (_, got, _) = read(&widen_to_8_bit(&png(7, 2, 3, &rows, 1, &[])).unwrap());
        assert_eq!(got, rows);
    }

    #[test]
    fn grey_scales_to_the_full_byte_with_its_key() {
        let rows = vec![vec![0, 5, 15]];
        let src = png(3, 4, 0, &rows, 0, &[(b"tRNS", &[0, 15])]);
        let (_, got, others) = read(&widen_to_8_bit(&src).unwrap());
        assert_eq!(got, vec![vec![0, 85, 255]]);
        assert_eq!(others, vec![(*b"tRNS", vec![0, 255])]);
    }

    #[test]
    fn other_images_are_left_alone() {
        let rows = vec![vec![1, 2, 3]];
        assert!(
            widen_to_8_bit(&png(3, 8, 3, &rows, 0, &[])).is_none(),
            "already 8-bit"
        );
        assert!(widen_to_8_bit(b"GIF89a").is_none());
        let mut cut = png(3, 4, 3, &rows, 0, &[]);
        cut.truncate(cut.len() - 20);
        assert!(widen_to_8_bit(&cut).is_none(), "broken file");
    }

    /// The stock head the betas drew garbled: 32 x 560, 4-bit, palette and
    /// `tRNS` (built from the same shape, not the art).
    #[test]
    fn a_head_sized_image_widens() {
        let rows: Vec<Vec<u8>> = (0..560)
            .map(|y| (0..32).map(|x| ((x + y) % 16) as u8).collect())
            .collect();
        let src = png(
            32,
            4,
            3,
            &rows,
            0,
            &[(b"PLTE", &[9u8; 48]), (b"tRNS", &[0])],
        );
        let (ihdr, got, _) = read(&widen_to_8_bit(&src).unwrap());
        assert_eq!((ihdr[8], got.len(), got[0].len()), (8, 560, 32));
        assert_eq!(got, rows);
    }
}
