//! Lab: the `rawobject` and `rawevent` lines of `graal3dworld.txt`. A
//! raw object is a ghost of any class whose update is a list of fields
//! written in order; a raw event is one guaranteed event built the same
//! way. They are for classes whose layout is not read yet: the client
//! skips what it does not read, since each update and event carries its
//! length.
//!
//! Fields:
//!
//! - `f0`, `f1`: a flag
//! - `i<N>:<v>`: an N-bit int (1 to 32), decimal, `0x` hex or negative;
//!   `u8:`, `u16:`, `u32:` are `i8:`, `i16:`, `i32:`
//! - `f32:<v>`: a 32-bit float
//! - `str:<text>`: a string; `%20` for a space, `%25` for `%`, `-` for the
//!   empty string
//! - `hex:<bytes>`, `file:<path>`, `zfile:<path>` (zlib-compressed, with
//!   the header): bytes, with an optional length prefix, `file@10:<path>`
//!   writing the byte count as an int10 first. Paths are relative to the
//!   server's world folder.

use super::bits::BitWriter;
use std::io::Write;

#[derive(Debug, Clone, PartialEq)]
pub enum RawField {
    Flag(bool),
    Int {
        bits: u32,
        value: u32,
    },
    F32(f32),
    Str(Vec<u8>),
    /// `prefix`: the width of the byte count written first (0: none).
    Bytes {
        prefix: u32,
        data: Vec<u8>,
        /// Read by [`load_files`]; `None` for `hex:`.
        file: Option<String>,
        compress: bool,
    },
}

/// A `rawobject` line: a ghost of `class`.
#[derive(Debug, Clone, PartialEq)]
pub struct RawObject {
    pub class: String,
    pub fields: Vec<RawField>,
}

/// A `rawevent` line. `late`: once the client has its control object
/// (after the mission start), else when the world is ghosted.
#[derive(Debug, Clone, PartialEq)]
pub struct RawEvent {
    pub class: String,
    pub fields: Vec<RawField>,
    pub late: bool,
}

/// An int that fits `bits`: unsigned, or negative in two's complement.
fn parse_int(text: &str, bits: u32) -> Option<u32> {
    let (neg, digits) = match text.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, text),
    };
    let v: u64 = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(h) => u64::from_str_radix(h, 16).ok()?,
        None => digits.parse().ok()?,
    };
    let span = 1u64 << bits;
    if neg {
        if v > span / 2 {
            return None;
        }
        Some(((span - v) & (span - 1)) as u32)
    } else if v < span {
        Some(v as u32)
    } else {
        None
    }
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    if text == "-" {
        return Some(Vec::new());
    }
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Reads the field words of a line; on a bad one says why.
pub fn parse_fields(words: &[&str]) -> Result<Vec<RawField>, String> {
    let mut out = Vec::new();
    for &token in words {
        if token == "f0" || token == "f1" {
            out.push(RawField::Flag(token == "f1"));
            continue;
        }
        let Some((mut kind, value)) = token.split_once(':') else {
            return Err(format!("no ':' in raw field {token}"));
        };
        let bad = || format!("bad raw field {token}");
        let mut prefix = 0;
        if let Some((k, w)) = kind.split_once('@') {
            prefix = w
                .parse::<u32>()
                .ok()
                .filter(|w| (1..=32).contains(w))
                .ok_or_else(|| format!("bad length width in raw field {token}"))?;
            kind = k;
        }
        let field = match kind {
            "hex" => RawField::Bytes {
                prefix,
                data: hex_decode(value).ok_or_else(bad)?,
                file: None,
                compress: false,
            },
            "file" | "zfile" if !value.is_empty() => RawField::Bytes {
                prefix,
                data: Vec::new(),
                file: Some(value.to_string()),
                compress: kind == "zfile",
            },
            _ if prefix != 0 => return Err(bad()),
            "f32" => RawField::F32(value.parse().map_err(|_| bad())?),
            "str" => {
                let t = percent_decode(value).ok_or_else(bad)?;
                if t.len() > 255 {
                    return Err(bad());
                }
                RawField::Str(t)
            }
            _ => {
                let bits = match kind {
                    "u8" => 8,
                    "u16" => 16,
                    "u32" => 32,
                    k => k
                        .strip_prefix('i')
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0),
                };
                if !(1..=32).contains(&bits) {
                    return Err(bad());
                }
                RawField::Int {
                    bits,
                    value: parse_int(value, bits).ok_or_else(bad)?,
                }
            }
        };
        out.push(field);
    }
    Ok(out)
}

/// Writes the fields in order. A file not loaded writes only its length
/// prefix (0).
pub fn write_fields(w: &mut BitWriter, fields: &[RawField]) {
    for f in fields {
        match f {
            RawField::Flag(v) => {
                w.flag(*v);
            }
            RawField::Int { bits, value } => w.int(*value, *bits),
            RawField::F32(v) => w.f32(*v),
            RawField::Str(t) => w.string(t),
            RawField::Bytes { prefix, data, .. } => {
                if *prefix > 0 {
                    w.int(data.len() as u32, *prefix);
                }
                w.raw(data);
            }
        }
    }
}

/// Reads every `file:` and `zfile:` field with `read`. Returns one line
/// per file that could not be read or is too long for its length prefix.
pub fn load_files(fields: &mut [RawField], read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Vec<String> {
    let mut errors = Vec::new();
    for f in fields {
        let RawField::Bytes {
            prefix,
            data,
            file: Some(file),
            compress,
        } = f
        else {
            continue;
        };
        let Some(mut bytes) = read(file) else {
            errors.push(format!("cannot read {file}"));
            continue;
        };
        if *compress {
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
            if z.write_all(&bytes).is_err() {
                errors.push(format!("cannot compress {file}"));
                continue;
            }
            bytes = z.finish().unwrap_or_default();
        }
        if *prefix > 0 && *prefix < 32 && bytes.len() >= 1 << *prefix {
            errors.push(format!(
                "{file} is {} bytes, too long for an int{prefix} length",
                bytes.len()
            ));
            continue;
        }
        *data = bytes;
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_read_and_written_in_order() {
        let f = parse_fields(&["f1", "i4:-1", "u8:0x41", "str:a%20b", "hex@8:0102", "f0"]).unwrap();
        let mut w = BitWriter::new();
        write_fields(&mut w, &f);
        // 1 + 4 + 8 + string + 8 + 16 + 1 bits.
        let mut plain = BitWriter::new();
        plain.string(b"a b");
        assert_eq!(w.bit_len(), 1 + 4 + 8 + plain.bit_len() + 8 + 16 + 1);
        assert_eq!(f[1], RawField::Int { bits: 4, value: 15 });
        assert!(parse_fields(&["i4:16"]).is_err());
        assert!(parse_fields(&["u8@4:1"]).is_err());
        assert!(parse_fields(&["nonsense"]).is_err());
    }

    #[test]
    fn files_are_loaded_and_checked_against_their_prefix() {
        let mut f = parse_fields(&["file@4:a.bin", "zfile:b.bin", "file:missing"]).unwrap();
        let errors = load_files(&mut f, &|name| match name {
            "a.bin" => Some(vec![0; 20]),
            "b.bin" => Some(vec![7; 100]),
            _ => None,
        });
        assert_eq!(errors.len(), 2, "{errors:?}");
        let RawField::Bytes { data, .. } = &f[1] else {
            panic!()
        };
        assert_eq!(data[0], 0x78, "zlib header");
    }
}
