//! Frames on the game socket, per protocol generation.
//!
//! Every frame is a `u16be` length and that many bytes. The first frame a
//! client sends (the login) is plain zlib in every generation. After it:
//!
//! | gen | builds | client to server | server to client |
//! |---|---|---|---|
//! | 2 | 1.21 to 1.39 | zlib | zlib |
//! | 3 | 1.41 to 2.17 | zlib, one LCG byte inserted in each message | zlib |
//! | 4 | 2.19 to 2.21, 3.0, 3.01 (login type 4) | bz2 body, partial XOR, no comp byte | the same, own iterator |
//! | 5 | 2.22 to 6.x | comp byte + partial XOR of the compressed body | the same, own iterator |
//!
//! **Gen 4 is not observed.** No type-4 login or frame has been captured
//! (graal_analysis `protocol/codec-and-framing.md`, server source only).
//! For 2.19 to 2.21, which no binary survives of, graal-client-repro's
//! clients use this same framing as an INVENTED link (Squirrel,
//! 2026-11-13); a real 3.0beta or 3.01 capture would settle it.
//!
//! The ciphers are `seihai-core`'s ([`Gen3`], [`Gen5`]), the ones the
//! Seihai client uses, so both ends share one definition. For gen 3 the
//! messages behind the login line in the login frame already carry their
//! inserted byte (the real 2.17.1 frame decodes that way). For gen 5 both
//! iterators start at the seed with the first frame after the login.

use seihai_core::protocol::{
    bz2_compress, bz2_decompress, choose_comp, zlib_decompress, ClientVersion, EncryptionGen,
    FrameLen, Gen3, Gen5, Gen5Comp,
};
use std::io;

/// The largest frame body this server writes. A `u16` length can say
/// 65535; staying under it means no build needs the 6.x long length.
pub const MAX_OUT_FRAME: usize = 0xFFF0;

/// The largest frame a client may send us. Real client frames are small;
/// this only stops a peer from making us buffer much.
pub const MAX_IN_FRAME: usize = 0xFFFF;

/// The cipher state of one connection.
pub struct Codec {
    gen: EncryptionGen,
    in3: Option<Gen3>,
    in5: Option<Gen5>,
    out5: Option<Gen5>,
    /// 5.12 sends (and so we send it) every body compressed.
    compress_all: bool,
    /// Gen 4 (login type 4): the gen-5 cipher on bz2 bodies with no
    /// compression byte ([`Self::gen4`]).
    gen4: bool,
    /// Gen-3 messages whose inserted byte did not check out, in a row.
    pub bad_in_a_row: u32,
}

impl Codec {
    /// The codec after a login with `key`. `version` is the build the token
    /// named, if the server knows it (it picks 5.12's step count).
    pub fn new(gen: EncryptionGen, key: u8, version: Option<ClientVersion>) -> Self {
        let round_down = version.is_some_and(|v| v.profile().protocol.gen5_round_down);
        let g5 = || {
            if round_down {
                Gen5::new(key).round_down()
            } else {
                Gen5::new(key)
            }
        };
        Codec {
            gen,
            in3: (gen == EncryptionGen::Gen3).then(|| Gen3::new(key)),
            in5: (gen == EncryptionGen::Gen5).then(g5),
            out5: (gen == EncryptionGen::Gen5).then(g5),
            compress_all: round_down,
            gen4: false,
            bad_in_a_row: 0,
        }
    }

    /// The same gen-5 codec with gen-4 frames: every body bz2, no
    /// compression byte, the XOR as for a compressed gen-5 body.
    pub fn gen4(mut self) -> Self {
        self.gen4 = self.out5.is_some();
        self
    }

    pub fn gen(&self) -> EncryptionGen {
        self.gen
    }

    /// One whole frame to write for `body` (one or more messages).
    pub fn encode(&mut self, body: &[u8]) -> io::Result<Vec<u8>> {
        let frame = match self.out5.as_mut() {
            Some(g) if self.gen4 => {
                let packed = bz2_compress(body)?;
                let mut out = Vec::with_capacity(packed.len() + 2);
                push_len(&mut out, packed.len())?;
                out.extend_from_slice(&g.encode(packed, Gen5Comp::Bz2));
                out
            }
            Some(g) => {
                // Small bodies go plain; the rest go zlib at the fast level,
                // which every gen-5 build reads at any size. bz2 packs a big
                // body a third smaller but costs about 15 times the CPU, and
                // a busy level's tick body passes 4 KB (where `choose_comp`
                // turns to bz2) at a few hundred players: it is only used
                // when the zlib frame would not fit.
                let (comp, packed) = match choose_comp(body.len()) {
                    Gen5Comp::None if !self.compress_all => (Gen5Comp::None, body.to_vec()),
                    _ => {
                        let z = zlib_fast(body)?;
                        if z.len() < MAX_OUT_FRAME {
                            (Gen5Comp::Zlib, z)
                        } else {
                            (Gen5Comp::Bz2, bz2_compress(body)?)
                        }
                    }
                };
                // Check the length before the XOR steps the iterator, so a
                // refused frame leaves the cipher where it was.
                let mut out = Vec::with_capacity(packed.len() + 3);
                push_len(&mut out, packed.len() + 1)?;
                let packed = g.encode(packed, comp);
                out.push(comp as u8);
                out.extend_from_slice(&packed);
                out
            }
            None => {
                let packed = zlib_fast(body)?;
                let mut out = Vec::with_capacity(packed.len() + 2);
                push_len(&mut out, packed.len())?;
                out.extend_from_slice(&packed);
                out
            }
        };
        Ok(frame)
    }

    /// The body of one frame the client sent (the bytes after its length).
    pub fn decode(&mut self, raw: &[u8]) -> io::Result<Vec<u8>> {
        match self.in5.as_mut() {
            Some(g) if self.gen4 => bz2_decompress(&g.decode(raw.to_vec(), Gen5Comp::Bz2)),
            Some(g) => {
                let (&c, rest) = raw.split_first().ok_or_else(|| bad("empty gen-5 frame"))?;
                let comp = match c {
                    0x02 => Gen5Comp::None,
                    0x04 => Gen5Comp::Zlib,
                    0x06 => Gen5Comp::Bz2,
                    other => {
                        return Err(bad(&format!("unknown gen-5 compression byte {other:#04x}")))
                    }
                };
                let body = g.decode(rest.to_vec(), comp);
                match comp {
                    Gen5Comp::None => Ok(body),
                    Gen5Comp::Zlib => zlib_decompress(&body),
                    Gen5Comp::Bz2 => bz2_decompress(&body),
                }
            }
            None => zlib_decompress(raw),
        }
    }

    /// Split a decoded body into messages, each with its `\n`, with the
    /// gen-3 byte taken out. A gen-3 message whose byte does not check out
    /// is dropped and counted in [`Self::bad_in_a_row`].
    pub fn messages(&mut self, body: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for line in body.split_inclusive(|&b| b == b'\n') {
            if line.last() != Some(&b'\n') {
                // A tail without `\n`: not a whole message.
                continue;
            }
            match self.in3.as_mut() {
                Some(g) => match g.decode(line) {
                    Some(m) => {
                        self.bad_in_a_row = 0;
                        out.push(m);
                    }
                    None => self.bad_in_a_row += 1,
                },
                None => out.push(line.to_vec()),
            }
        }
        out
    }
}

/// The length of the frame at the start of `buf` (header, body), once the
/// header is in. `long` reads the 6.x `FF FF` + `u32be` form.
pub fn frame_header(buf: &[u8], long: bool) -> Option<(usize, usize)> {
    let len = if long {
        FrameLen::Long
    } else {
        FrameLen::Short
    };
    len.header(buf)
}

thread_local! {
    /// One zlib state per thread, reset for each frame. Making a fresh one
    /// costs more than compressing a tick's body, and keeping one per
    /// connection costs about 360 KB each (window and hash tables), so the
    /// connections on a worker thread share one: a frame is compressed in
    /// one go, never across an `.await`.
    static ZLIB: std::cell::RefCell<flate2::Compress> =
        std::cell::RefCell::new(flate2::Compress::new(flate2::Compression::new(2), true));
}

/// zlib at level 2 on the zlib-rs backend. Frames go out every tick to
/// every player, so compression is most of the server's CPU at a few
/// hundred players in a level; this costs about a third of the default
/// level's time for the same size. Clients inflate any level.
fn zlib_fast(body: &[u8]) -> io::Result<Vec<u8>> {
    ZLIB.with(|z| {
        let mut z = z.borrow_mut();
        z.reset();
        // Room for an incompressible body, the zlib header and the checksum.
        let mut out = Vec::with_capacity(body.len() + body.len() / 1000 + 64);
        loop {
            let read = z.total_in() as usize;
            let st = z
                .compress_vec(&body[read..], &mut out, flate2::FlushCompress::Finish)
                .map_err(io::Error::other)?;
            match st {
                flate2::Status::StreamEnd => return Ok(out),
                _ => out.reserve(out.capacity().max(64)),
            }
        }
    })
}

fn push_len(out: &mut Vec<u8>, n: usize) -> io::Result<()> {
    if n > MAX_OUT_FRAME {
        return Err(bad(&format!("frame of {n} bytes is too long to send")));
    }
    out.extend_from_slice(&(n as u16).to_be_bytes());
    Ok(())
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::{read_frame, read_zlib_frame, write_zlib_frame, zlib_compress};

    /// A big body (a busy level's tick) goes zlib, not bz2, and reads back.
    #[test]
    fn big_bodies_go_zlib() {
        let mut server = Codec::new(EncryptionGen::Gen5, 0x21, Some(ClientVersion::V231));
        let mut client = Gen5::new(0x21);
        let body: Vec<u8> = (0..9000u32).map(|i| b'a' + (i * 7 % 23) as u8).collect();
        let frame = server.encode(&body).unwrap();
        assert_eq!(frame[2], Gen5Comp::Zlib as u8);
        assert_eq!(
            read_frame(&mut frame.as_slice(), &mut client).unwrap(),
            body
        );
    }

    /// What we send a gen-5 client reads back with the client's own reader,
    /// frame after frame (the iterators stay in step).
    #[test]
    fn gen5_frames_read_back_with_the_client_reader() {
        for v in [
            ClientVersion::V222,
            ClientVersion::V512,
            ClientVersion::V603R7,
        ] {
            let mut server = Codec::new(EncryptionGen::Gen5, 0x33, Some(v));
            let round_down = v.profile().protocol.gen5_round_down;
            let mut client = if round_down {
                Gen5::new(0x33).round_down()
            } else {
                Gen5::new(0x33)
            };
            for body in [
                b"a\n".to_vec(),
                vec![b'x'; 100],
                vec![b'y'; 9000],
                b"tiny\n".to_vec(),
            ] {
                let frame = server.encode(&body).unwrap();
                let got = read_frame(&mut frame.as_slice(), &mut client).unwrap();
                assert_eq!(got, body, "{v:?}");
            }
        }
    }

    /// Gen 4 (login type 4, 3.0 and 3.01): bz2 bodies with no compression
    /// byte, XORed as a compressed gen-5 body, both ways.
    #[test]
    fn gen4_frames_both_ways() {
        let key = 0x2a;
        let mut server = Codec::new(EncryptionGen::Gen5, key, None).gen4();
        let mut client_in = Gen5::new(key);
        let mut client_out = Gen5::new(key);
        for body in [b"a\n".to_vec(), vec![b'x'; 100], vec![b'y'; 9000]] {
            let frame = server.encode(&body).unwrap();
            let len = u16::from_be_bytes([frame[0], frame[1]]) as usize;
            assert_eq!(len, frame.len() - 2, "no compression byte in the length");
            let plain = client_in.decode(frame[2..].to_vec(), Gen5Comp::Bz2);
            assert_eq!(bz2_decompress(&plain).unwrap(), body);
            let sent = client_out.encode(bz2_compress(&body).unwrap(), Gen5Comp::Bz2);
            assert_eq!(server.decode(&sent).unwrap(), body);
        }
    }

    /// Frames the client writes (its `send_messages` path) decode here.
    #[test]
    fn gen5_client_frames_decode() {
        let key = 0x51;
        let mut client = Gen5::new(key);
        let mut server = Codec::new(EncryptionGen::Gen5, key, Some(ClientVersion::V231));
        for body in [b"E English\n".to_vec(), vec![b'z'; 70]] {
            let comp = choose_comp(body.len());
            let packed = match comp {
                Gen5Comp::None => body.clone(),
                _ => zlib_compress(&body).unwrap(),
            };
            let packed = client.encode(packed, comp);
            let frame = seihai_core::protocol::write_frame(comp, &packed);
            assert_eq!(server.decode(&frame[2..]).unwrap(), body);
        }
    }

    #[test]
    fn gen3_inserted_bytes_come_out() {
        let key = 0x12;
        let mut client = Gen3::new(key);
        let msgs: Vec<&[u8]> = vec![b"EEnglish\n", b"\"0f\n", b"7abc\n"];
        let mut body = Vec::new();
        for m in &msgs {
            body.extend(client.encode(m));
        }
        let frame = write_zlib_frame(&body).unwrap();
        let mut server = Codec::new(EncryptionGen::Gen3, key, Some(ClientVersion::V205));
        let decoded = server.decode(&frame[2..]).unwrap();
        let got = server.messages(&decoded);
        assert_eq!(got, msgs.iter().map(|m| m.to_vec()).collect::<Vec<_>>());
        assert_eq!(server.bad_in_a_row, 0);
        // Server to client is plain zlib.
        let out = server.encode(b"hello\n").unwrap();
        assert_eq!(read_zlib_frame(&mut out.as_slice()).unwrap(), b"hello\n");
    }

    #[test]
    fn oversized_frames_are_refused() {
        let mut c = Codec::new(EncryptionGen::Gen5, 1, None);
        // Random bytes do not compress: well over one frame.
        let mut x = 0x1234_5678u32;
        let body: Vec<u8> = (0..200_000)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (x >> 16) as u8
            })
            .collect();
        assert!(c.encode(&body).is_err());
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    use std::time::Instant;

    fn z(level: u32, b: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), Compression::new(level));
        e.write_all(b).unwrap();
        e.finish().unwrap()
    }

    /// `cargo test --release --lib bench_compress -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_compress() {
        // A tick body: N movers' X2/Y2 updates, as `others::message` writes them.
        for movers in [10usize, 150, 500] {
            let mut body = Vec::new();
            for id in 0..movers {
                let mut m = vec![8 + 32];
                crate::wire::gshort(id as u16 + 1, &mut m);
                m.push(78 + 32);
                crate::wire::gshort(
                    seihai_core::protocol::encode_pos_precise(10.0 + id as f32 * 0.37),
                    &mut m,
                );
                m.push(79 + 32);
                crate::wire::gshort(
                    seihai_core::protocol::encode_pos_precise(20.0 + id as f32 * 0.11),
                    &mut m,
                );
                m.push(b'\n');
                body.extend(m);
            }
            let reps = 2000;
            let mut row = format!("{movers:>4} movers, {:>5} B:", body.len());
            for (name, f) in [
                ("zlib6", &(|b: &[u8]| z(6, b)) as &dyn Fn(&[u8]) -> Vec<u8>),
                ("zlib1", &|b: &[u8]| z(1, b)),
                ("bz2", &|b: &[u8]| bz2_compress(b).unwrap()),
            ] {
                let s = Instant::now();
                let mut out = 0;
                for _ in 0..reps {
                    out = std::hint::black_box(f(&body)).len();
                }
                row += &format!(
                    "  {name} {:>6.1} us -> {out} B",
                    s.elapsed().as_secs_f64() / reps as f64 * 1e6
                );
            }
            eprintln!("{row}");
        }
    }
}
