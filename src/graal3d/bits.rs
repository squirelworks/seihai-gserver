//! The Torque bit stream: how every UDP packet of the link is packed.
//!
//! Bits go least significant first within each byte, bytes in order. An
//! N-bit field is written low bit first, so a `u32` on a byte boundary is
//! four little-endian bytes. A `f32` is its raw 32 bits.
//!
//! Strings come in two forms:
//! - [`BitWriter::string`] / [`BitReader::string`]: a flag (1 = Huffman
//!   coded, 0 = raw), an 8-bit length, then the codes or the raw bytes.
//!   Out-of-band packets (the handshake) use this form.
//! - [`BitWriter::pstring`] / [`BitReader::pstring`]: strings inside data
//!   packets. One more flag comes first; when it is set, an 8-bit count
//!   of characters is taken from the previous string in the same packet
//!   and the rest follows as a plain string. Each reader and writer keeps
//!   that previous string, empty at the start of a packet.

use super::huffman::Huffman;
use std::fmt;

/// The longest string either form carries (its length is 8 bits).
pub const MAX_STRING: usize = 255;

/// A read ran past the end of the packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfBits {
    /// The bit position the read started at.
    pub at: usize,
    /// How many bits it wanted.
    pub wanted: usize,
    /// How many bits the packet has.
    pub total: usize,
}

impl fmt::Display for OutOfBits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "read of {} bits at bit {} runs past the packet's {} bits",
            self.wanted, self.at, self.total
        )
    }
}

impl std::error::Error for OutOfBits {}

pub type Result<T> = std::result::Result<T, OutOfBits>;

/// Writes a packet bit by bit.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
    /// The previous `pstring` of this packet.
    previous: Vec<u8>,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bits written so far.
    pub fn bit_len(&self) -> usize {
        self.bits
    }

    /// The packet, padded with 0 bits to a whole byte.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn flag(&mut self, on: bool) -> bool {
        if self.bits.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if on {
            self.bytes[self.bits / 8] |= 1 << (self.bits % 8);
        }
        self.bits += 1;
        on
    }

    /// The low `n` bits of `value`, low bit first. `n` is 0 to 32.
    pub fn int(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        debug_assert!(
            n == 32 || value >> n == 0,
            "{value} does not fit in {n} bits"
        );
        for i in 0..n {
            self.flag(value >> i & 1 == 1);
        }
    }

    pub fn u8(&mut self, value: u8) {
        self.int(value.into(), 8);
    }

    pub fn u32(&mut self, value: u32) {
        self.int(value, 32);
    }

    pub fn f32(&mut self, value: f32) {
        self.int(value.to_bits(), 32);
    }

    /// Raw bytes, eight bits each.
    pub fn raw(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.u8(b);
        }
    }

    /// A string: Huffman coded when that is shorter than the raw bytes,
    /// as Torque writes it. Longer than [`MAX_STRING`] is cut.
    pub fn string(&mut self, text: &[u8]) {
        let text = &text[..text.len().min(MAX_STRING)];
        let h = Huffman::get();
        if self.flag(h.coded_bits(text) < text.len() * 8) {
            self.u8(text.len() as u8);
            for &b in text {
                let (code, len) = h.code(b);
                self.int(code, len.into());
            }
        } else {
            self.u8(text.len() as u8);
            self.raw(text);
        }
    }

    /// A string inside a data packet that never shares a prefix (flag
    /// 0). Readers take it whatever their previous string was, so a
    /// payload written on its own can be copied into any packet.
    pub fn pstring_alone(&mut self, text: &[u8]) {
        self.flag(false);
        self.string(text);
    }

    /// Copies every bit `other` holds onto the end of this packet.
    pub fn append(&mut self, other: &BitWriter) {
        for i in 0..other.bits {
            self.flag(other.bytes[i / 8] >> (i % 8) & 1 == 1);
        }
    }

    /// A string inside a data packet. Shares a prefix with the previous
    /// one when more than two characters match, as Torque does.
    pub fn pstring(&mut self, text: &[u8]) {
        let text = &text[..text.len().min(MAX_STRING)];
        let same = self
            .previous
            .iter()
            .zip(text)
            .take_while(|(a, b)| a == b)
            .count();
        text.clone_into(&mut self.previous);
        if self.flag(same > 2) {
            self.u8(same as u8);
            self.string(&text[same..]);
        } else {
            self.string(text);
        }
    }
}

/// Reads a packet bit by bit.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    /// The previous `pstring` of this packet.
    previous: Vec<u8>,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            previous: Vec::new(),
        }
    }

    /// The bit the next read starts at.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Bits left, counting the padding of the last byte.
    pub fn remaining(&self) -> usize {
        self.data.len() * 8 - self.pos
    }

    /// Moves to bit `pos`, for skipping a payload by its bit length.
    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() * 8 {
            return Err(self.short(pos.saturating_sub(self.pos)));
        }
        self.pos = pos;
        Ok(())
    }

    fn short(&self, wanted: usize) -> OutOfBits {
        OutOfBits {
            at: self.pos,
            wanted,
            total: self.data.len() * 8,
        }
    }

    fn need(&self, n: usize) -> Result<()> {
        if n > self.remaining() {
            Err(self.short(n))
        } else {
            Ok(())
        }
    }

    pub fn flag(&mut self) -> Result<bool> {
        self.need(1)?;
        let on = self.data[self.pos / 8] >> (self.pos % 8) & 1 == 1;
        self.pos += 1;
        Ok(on)
    }

    /// An `n`-bit field, low bit first. `n` is 0 to 32.
    pub fn int(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 32);
        self.need(n as usize)?;
        let mut v = 0u32;
        for i in 0..n {
            v |= u32::from(self.flag()?) << i;
        }
        Ok(v)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.int(8)? as u8)
    }

    pub fn u32(&mut self) -> Result<u32> {
        self.int(32)
    }

    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.int(32)?))
    }

    /// `n` raw bytes, eight bits each.
    pub fn raw(&mut self, n: usize) -> Result<Vec<u8>> {
        self.need(n * 8)?;
        (0..n).map(|_| self.u8()).collect()
    }

    /// A string, Huffman coded or raw as its flag says.
    pub fn string(&mut self) -> Result<Vec<u8>> {
        if self.flag()? {
            let len = self.u8()? as usize;
            let h = Huffman::get();
            let start = self.pos;
            let mut out = Vec::with_capacity(len);
            for _ in 0..len {
                let b = h.decode(|| self.flag().ok());
                match b {
                    Some(b) => out.push(b),
                    None => {
                        return Err(OutOfBits {
                            at: start,
                            wanted: self.remaining() + 1,
                            total: self.data.len() * 8,
                        })
                    }
                }
            }
            Ok(out)
        } else {
            let len = self.u8()? as usize;
            self.raw(len)
        }
    }

    /// A string inside a data packet, which may reuse a prefix of the
    /// previous one.
    pub fn pstring(&mut self) -> Result<Vec<u8>> {
        let text = if self.flag()? {
            let keep = self.u8()? as usize;
            let mut text = self.previous[..keep.min(self.previous.len())].to_vec();
            text.extend(self.string()?);
            text
        } else {
            self.string()?
        };
        text.clone_into(&mut self.previous);
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_go_low_first() {
        let mut w = BitWriter::new();
        w.flag(true);
        w.int(0, 1);
        w.int(2, 9);
        assert_eq!(w.bytes(), &[0b0000_1001, 0]);
        let mut w = BitWriter::new();
        w.u32(0x400);
        assert_eq!(w.into_bytes(), [0x00, 0x04, 0x00, 0x00]);
    }

    #[test]
    fn reads_the_captured_first_data_packet_of_a_512_client() {
        // From the 5.12 lab: header seq 1, ack 0, data, no ack bytes, then
        // the rate flags: current 0, max 1 with 32 ms and 450 bytes.
        let mut r = BitReader::new(&[0x05, 0x00, 0x00, 0x04, 0x41, 0xb8, 0x00]);
        assert!(r.flag().unwrap(), "data-link packet");
        assert_eq!(r.int(1).unwrap(), 0, "connect sequence bit");
        assert_eq!(r.int(9).unwrap(), 1, "sequence");
        assert_eq!(r.int(9).unwrap(), 0, "highest ack");
        assert_eq!(r.int(2).unwrap(), 0, "data");
        assert_eq!(r.int(3).unwrap(), 0, "ack bytes");
        assert!(!r.flag().unwrap(), "current rate unchanged");
        assert!(r.flag().unwrap(), "max rate");
        assert_eq!(r.int(10).unwrap(), 32);
        assert_eq!(r.int(10).unwrap(), 450);
    }

    #[test]
    fn numbers_round_trip_off_a_byte_boundary() {
        let mut w = BitWriter::new();
        w.flag(true);
        w.u32(0xdead_beef);
        w.f32(-1.5);
        w.int(0x2a5, 10);
        w.raw(b"ab");
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert!(r.flag().unwrap());
        assert_eq!(r.u32().unwrap(), 0xdead_beef);
        assert_eq!(r.f32().unwrap(), -1.5);
        assert_eq!(r.int(10).unwrap(), 0x2a5);
        assert_eq!(r.raw(2).unwrap(), b"ab");
    }

    #[test]
    fn plain_text_is_huffman_coded_and_reads_back() {
        let mut w = BitWriter::new();
        w.string(b"GameConnection");
        w.string(b"Graal3D");
        w.string(b"");
        w.string(&[0xff, 0xfe, 0x01]);
        let n = w.bit_len();
        let bytes = w.into_bytes();
        assert_eq!(bytes[0] & 1, 1, "coded");
        assert!(n < 1 + 8 + 14 * 8 + 1 + 8 + 7 * 8 + 9 + 9 + 3 * 8);
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.string().unwrap(), b"GameConnection");
        assert_eq!(r.string().unwrap(), b"Graal3D");
        assert_eq!(r.string().unwrap(), b"");
        assert_eq!(r.string().unwrap(), [0xff, 0xfe, 0x01]);
        assert_eq!(r.position(), n);
    }

    #[test]
    fn raw_strings_read_too() {
        // flag 0, length 2, "hi": what a writer that never codes sends.
        let mut w = BitWriter::new();
        w.flag(false);
        w.u8(2);
        w.raw(b"hi");
        let bytes = w.into_bytes();
        assert_eq!(BitReader::new(&bytes).string().unwrap(), b"hi");
    }

    #[test]
    fn packet_strings_share_prefixes() {
        let mut w = BitWriter::new();
        w.pstring(b"G3DPlayer_alice");
        w.pstring(b"G3DPlayer_bob");
        w.pstring(b"Gx");
        let shared = w.bit_len();
        let mut plain = BitWriter::new();
        for s in [&b"G3DPlayer_alice"[..], b"G3DPlayer_bob", b"Gx"] {
            plain.flag(false);
            plain.string(s);
        }
        assert!(
            shared < plain.bit_len(),
            "the second name reuses ten characters"
        );
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.pstring().unwrap(), b"G3DPlayer_alice");
        assert_eq!(r.pstring().unwrap(), b"G3DPlayer_bob");
        assert_eq!(r.pstring().unwrap(), b"Gx");
        // A writer that never shares (flag 0) reads the same.
        let bytes = plain.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.pstring().unwrap(), b"G3DPlayer_alice");
        assert_eq!(r.pstring().unwrap(), b"G3DPlayer_bob");
    }

    #[test]
    fn appended_bits_land_unaligned() {
        let mut inner = BitWriter::new();
        inner.int(0b101, 3);
        inner.pstring_alone(b"hi");
        let mut w = BitWriter::new();
        w.flag(true);
        w.append(&inner);
        assert_eq!(w.bit_len(), 1 + inner.bit_len());
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        assert!(r.flag().unwrap());
        assert_eq!(r.int(3).unwrap(), 0b101);
        assert_eq!(r.pstring().unwrap(), b"hi");
    }

    #[test]
    fn short_packets_are_an_error_not_a_panic() {
        let mut r = BitReader::new(&[0xff]);
        assert_eq!(
            r.int(9),
            Err(OutOfBits {
                at: 0,
                wanted: 9,
                total: 8
            })
        );
        let mut r = BitReader::new(&[0x01, 0x05]);
        assert!(r.string().is_err(), "coded, 5 characters, nothing after");
        assert!(BitReader::new(&[]).flag().is_err());
    }
}
