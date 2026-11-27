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

/// Whether `version` stops reading a frame at a `00` byte. 2.22 and 2.31
/// lose every message after one in the same frame, in an unknown id and a
/// chat prop alike; 4.20r8 does not (Graal_analysis
/// `protocol/probe-targets-2026-11-18.md`, #156). The 1.x builds are taken
/// to do the same as 2.x; none was probed.
pub fn nul_ends_frame(version: ClientVersion) -> bool {
    let label = version.label();
    label.starts_with("1.") || label.starts_with("2.")
}

/// `body` with every `00` byte outside a raw block turned into `?`. A raw
/// block (the message after `PLO_RAWDATA` (100), whose `GINT3` gives its
/// length) is a file or bytecode and goes as it is: such blocks reach 2.x
/// clients whole. Every other `00` can only be in text (a `GCHAR` or
/// `GINT` never is one), so the swap keeps every length right.
///
/// `raw_left` is how much of a raw block the previous frame left over; the
/// second value is what this one leaves for the next.
pub fn scrub_nul(body: &[u8], raw_left: usize) -> (std::borrow::Cow<'_, [u8]>, usize) {
    use std::borrow::Cow;
    let raw_id = crate::wire::ids::PLO_RAWDATA + 0x20;
    let mut out: Option<Vec<u8>> = None;
    let mut i = raw_left.min(body.len());
    let mut left = raw_left - i;
    while i < body.len() {
        let end = body[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(body.len(), |n| i + n);
        let raw_len = (body[i] == raw_id)
            .then(|| seihai_core::protocol::read_gint3(body.get(i + 1..end)?))
            .flatten();
        let text = (i + 1).min(end)..end;
        if body[text.clone()].contains(&0) {
            let o = out.get_or_insert_with(|| body.to_vec());
            for b in &mut o[text] {
                if *b == 0 {
                    *b = b'?';
                }
            }
        }
        i = end + 1;
        if let Some(n) = raw_len {
            let here = n.min(body.len().saturating_sub(i));
            left = n - here;
            i += here;
        }
    }
    (out.map_or(Cow::Borrowed(body), Cow::Owned), left)
}

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
    /// A 1.x or 2.x client: no `00` byte goes out outside a raw block
    /// ([`scrub_nul`]).
    nul_free: bool,
    /// Raw-block bytes still to come in the next frame: a file bigger than
    /// one frame goes out in pieces (`files::frames_of`).
    raw_left: usize,
    /// The login key (for [`Gen5::with_state`]).
    key: u8,
    /// Short compressed client frames that only inflated under the other
    /// gen-5 step rule (see [`Self::decode`]); the cipher follows the client.
    pub step_rule_switches: u32,
    /// Compressed client frames that inflated under neither step rule and
    /// were skipped, in a row (see [`Self::decode`]).
    skipped_in_a_row: u32,
    /// What the last skipped frame was, for the server log.
    skip_note: Option<String>,
}

/// How many compressed client frames in a row may fail to inflate before
/// the client is dropped. A real 5.12 now and then sends one frame flagged
/// zlib whose body is not zlib at all (a lab capture, 2026-11-27: the
/// cipher was in step, and past the 16 XORed bytes the body was plain GS2
/// bytecode, cut short). The client steps its cipher for that frame as for
/// any other, so skipping it keeps both ends in step. A real desync fails
/// every frame after it, so it still drops the client.
pub const MAX_SKIPPED_IN_A_ROW: u32 = 2;

impl Codec {
    /// Common transport envelope for the project NPC action bridge. Scratch
    /// codecs use the existing encoder; no real connection state is advanced.
    /// XOR keys/iterator positions change bytes, not encoded lengths.
    pub(crate) fn fits_npc_action(body: &[u8]) -> bool {
        let mut compressed = Self::new(EncryptionGen::Gen5, 0, None);
        compressed.compress_all = true;
        [
            Self::new(EncryptionGen::Gen2, 0, None),
            Self::new(EncryptionGen::Gen5, 0, None).gen4(),
            Self::new(EncryptionGen::Gen5, 0, None),
            compressed,
        ]
        .iter_mut()
        .all(|codec| codec.encode(body).is_ok())
    }

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
            nul_free: version.is_some_and(nul_ends_frame),
            raw_left: 0,
            key,
            step_rule_switches: 0,
            skipped_in_a_row: 0,
            skip_note: None,
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
        let scrubbed;
        let body = if self.nul_free {
            let (out, left) = scrub_nul(body, self.raw_left);
            self.raw_left = left;
            scrubbed = out;
            &scrubbed[..]
        } else {
            body
        };
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
        let key = self.key;
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
                let before = g.clone();
                let body = g.decode(rest.to_vec(), comp);
                let inflate = |body: &[u8]| match comp {
                    Gen5Comp::None => Ok(body.to_vec()),
                    Gen5Comp::Zlib => zlib_decompress(body),
                    Gen5Comp::Bz2 => bz2_decompress(body),
                };
                match inflate(&body) {
                    Ok(b) => {
                        self.skipped_in_a_row = 0;
                        Ok(b)
                    }
                    Err(e) => {
                        // The two step rules differ only on a compressed
                        // body under 16 bytes whose length is not a
                        // multiple of 4. If the other rule inflates it, the
                        // client steps that way: keep its state and go on.
                        let short = comp != Gen5Comp::None
                            && rest.len() < 16
                            && !rest.len().is_multiple_of(4);
                        if short {
                            let mut other = if before.is_round_down() {
                                Gen5::with_state(before.state(), key)
                            } else {
                                Gen5::with_state(before.state(), key).round_down()
                            };
                            let alt = other.decode(rest.to_vec(), comp);
                            if let Ok(b) = inflate(&alt) {
                                *g = other;
                                self.step_rule_switches += 1;
                                self.skipped_in_a_row = 0;
                                // One routine ciphers both directions in
                                // the client, so its receive side steps
                                // the same way.
                                if let Some(o) = self.out5.as_mut() {
                                    let flipped = Gen5::with_state(o.state(), key);
                                    *o = if o.is_round_down() {
                                        flipped
                                    } else {
                                        flipped.round_down()
                                    };
                                }
                                return Ok(b);
                            }
                        }
                        let what = format!(
                            "{e}: client frame type {c:#04x}, {} bytes, cipher {:08x}{}, starts {}",
                            rest.len(),
                            before.state(),
                            if before.is_round_down() {
                                " (round down)"
                            } else {
                                ""
                            },
                            hex_head(&body),
                        );
                        // The cipher has stepped for this frame as the
                        // client's did: skip it, unless too many in a row
                        // say the two are out of step.
                        if self.skipped_in_a_row < MAX_SKIPPED_IN_A_ROW {
                            self.skipped_in_a_row += 1;
                            self.skip_note = Some(what);
                            return Ok(Vec::new());
                        }
                        Err(bad(&what))
                    }
                }
            }
            None => zlib_decompress(raw),
        }
    }

    /// The last skipped client frame, once: a compressed frame that
    /// inflated under neither step rule (see [`MAX_SKIPPED_IN_A_ROW`]).
    pub fn take_skip_note(&mut self) -> Option<String> {
        self.skip_note.take()
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

/// The first bytes of a deciphered body, for a decode error.
fn hex_head(body: &[u8]) -> String {
    body.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No `00` reaches a 2.x client outside a raw block (#156).
    #[test]
    fn nul_bytes_go_to_2x_only_in_raw_blocks() {
        use crate::wire::{ids::PLO_RAWDATA, msg, raw_block};
        let mut body = msg(20, b"chat\0here");
        body.extend(raw_block(102, b"file\0\0data"));
        body.extend(msg(21, b"\0next"));
        let (out, left) = scrub_nul(&body, 0);
        let mut want = msg(20, b"chat?here");
        want.extend(raw_block(102, b"file\0\0data"));
        want.extend(msg(21, b"?next"));
        assert_eq!(&out[..], &want[..]);
        assert_eq!(left, 0);
        // Nothing to do: the body is not copied.
        let clean = msg(PLO_RAWDATA, b"   ");
        assert!(matches!(
            scrub_nul(&clean, 0).0,
            std::borrow::Cow::Borrowed(_)
        ));
        // A raw block cut over two frames keeps its bytes in both.
        let block = raw_block(102, &[0u8; 40]);
        let (first, rest) = block.split_at(20);
        let (a, left) = scrub_nul(first, 0);
        assert_eq!(&a[..], first);
        assert_eq!(left, 40 + 2 - (20 - 5));
        let mut second = rest.to_vec();
        second.extend(msg(21, b"\0"));
        let (b, left) = scrub_nul(&second, left);
        assert_eq!(&b[..rest.len()], rest);
        assert_eq!(&b[rest.len()..], &msg(21, b"?")[..]);
        assert_eq!(left, 0);
        assert!(nul_ends_frame(ClientVersion::V222));
        assert!(nul_ends_frame(ClientVersion::V231));
        assert!(nul_ends_frame(ClientVersion::V139));
        assert!(!nul_ends_frame(ClientVersion::V420R8));
        assert!(!nul_ends_frame(ClientVersion::V603R7));
    }
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

    /// A short compressed client frame (under 16 bytes, length not a
    /// multiple of 4) that only inflates under the other step rule is read
    /// that way, and both ciphers then follow the client. Anything else
    /// that does not inflate is an error that says what came in.
    #[test]
    fn a_short_frame_under_the_other_step_rule_is_followed() {
        let key = 0x40;
        // A 46-byte chat that zlib packs to 15 bytes.
        let body: Vec<u8> = b"\x22\x2c\x4c"
            .iter()
            .copied()
            .chain(std::iter::repeat_n(b'a', 44))
            .chain(*b"\n")
            .collect();
        let packed = zlib_compress(&body).unwrap();
        assert!(
            packed.len() < 16 && !packed.len().is_multiple_of(4),
            "{}",
            packed.len()
        );
        for (server_down, client) in [(true, Gen5::new(key)), (false, Gen5::new(key).round_down())]
        {
            let v = if server_down {
                ClientVersion::V512
            } else {
                ClientVersion::V222
            };
            assert_eq!(v.profile().protocol.gen5_round_down, server_down);
            let mut server = Codec::new(EncryptionGen::Gen5, key, Some(v));
            let mut client_out = client.clone();
            let mut client_in = client;
            for _ in 0..3 {
                let mut frame = vec![Gen5Comp::Zlib as u8];
                frame.extend(client_out.encode(packed.clone(), Gen5Comp::Zlib));
                assert_eq!(server.decode(&frame).unwrap(), body);
            }
            assert_eq!(server.step_rule_switches, 1);
            // What we send back reads with the client's own rule.
            let out = server.encode(&body).unwrap();
            assert_eq!(
                read_frame(&mut out.as_slice(), &mut client_in).unwrap(),
                body
            );
        }
        // A frame that inflates under neither rule says what came in.
        let mut server = Codec::new(EncryptionGen::Gen5, key, Some(ClientVersion::V512));
        let junk = [0x04, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];
        assert_eq!(server.decode(&junk).unwrap(), b"");
        let note = server.take_skip_note().unwrap();
        assert!(
            note.contains("client frame type 0x04, 13 bytes, cipher 04a80b38 (round down)"),
            "{note}"
        );
        assert_eq!(server.take_skip_note(), None);
    }

    /// A real 5.12 now and then sends a frame flagged zlib whose body is
    /// not zlib (lab, 2026-11-27: in step, plain GS2 bytecode past the
    /// XORed bytes). It is skipped and the frames after it read, so the
    /// client stays on; a run of them (a real desync) still drops it.
    #[test]
    fn a_compressed_frame_that_is_not_zlib_is_skipped() {
        let key = 0x35;
        let mut client = Gen5::new(key).round_down();
        let mut server = Codec::new(EncryptionGen::Gen5, key, Some(ClientVersion::V512));
        let send = |client: &mut Gen5, packed: Vec<u8>| {
            let mut frame = vec![Gen5Comp::Zlib as u8];
            frame.extend(client.encode(packed, Gen5Comp::Zlib));
            frame
        };
        let good: Vec<u8> = b"7basepackage.gupd\n".repeat(4);
        // Made-up bytecode-like bytes, as long as the lab frame: segment
        // headers, a string table, cut short, then `\n`.
        let mut not_zlib = vec![0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 2];
        not_zlib.extend(b"\0\0\0\x20\0\0\0\x01onWeaponFired\0\0\0\0\x21onTimeout\0");
        not_zlib.resize(139, b'x');
        not_zlib.push(b'\n');
        assert_eq!(not_zlib.len(), 140);

        let f = send(&mut client, zlib_compress(&good).unwrap());
        assert_eq!(server.decode(&f).unwrap(), good);
        let f = send(&mut client, not_zlib.clone());
        assert_eq!(server.decode(&f).unwrap(), b"");
        let note = server.take_skip_note().unwrap();
        assert!(
            note.contains("client frame type 0x04, 140 bytes")
                && note.contains("starts 0000000100000004"),
            "{note}"
        );
        // In step: the next frames read.
        for _ in 0..2 {
            let f = send(&mut client, zlib_compress(&good).unwrap());
            assert_eq!(server.decode(&f).unwrap(), good);
        }
        // One such frame between good ones, again and again, is fine.
        for _ in 0..5 {
            let f = send(&mut client, not_zlib.clone());
            assert_eq!(server.decode(&f).unwrap(), b"");
            let f = send(&mut client, zlib_compress(&good).unwrap());
            assert_eq!(server.decode(&f).unwrap(), good);
        }
        // Out of step: every frame fails, and the third in a row drops it.
        let mut wrong = Gen5::with_state(0x1234_5678, key).round_down();
        for i in 0..=MAX_SKIPPED_IN_A_ROW {
            let f = send(&mut wrong, zlib_compress(&good).unwrap());
            let r = server.decode(&f);
            if i < MAX_SKIPPED_IN_A_ROW {
                assert_eq!(r.unwrap(), b"");
            } else {
                let e = r.unwrap_err().to_string();
                assert!(e.contains("client frame type 0x04"), "{e}");
            }
        }
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

    fn action_transport(mode: u8, key: u8) -> Codec {
        match mode {
            0 => Codec::new(EncryptionGen::Gen2, key, None),
            1 => Codec::new(EncryptionGen::Gen5, key, None).gen4(),
            2 => Codec::new(EncryptionGen::Gen5, key, None),
            3 => {
                let mut codec = Codec::new(EncryptionGen::Gen5, key, None);
                codec.compress_all = true;
                codec
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn npc_action_preflight_uses_existing_encoded_limits_and_disposable_state() {
        let mut header = Vec::new();
        push_len(&mut header, MAX_OUT_FRAME).unwrap();
        assert_eq!(header, (MAX_OUT_FRAME as u16).to_be_bytes());
        assert!(push_len(&mut Vec::new(), MAX_OUT_FRAME + 1).is_err());

        for body in [b"P       edge\n".to_vec(), vec![b'x'; 131_070]] {
            let expected = (0..4).all(|mode| action_transport(mode, 0).encode(&body).is_ok());
            assert_eq!(Codec::fits_npc_action(&body), expected);
            for mode in 0..4 {
                let baseline = action_transport(mode, 0).encode(&body).unwrap().len();
                for key in [0, 0x33, 127] {
                    let mut codec = action_transport(mode, key);
                    codec.encode(b"P       before\n").unwrap();
                    assert_eq!(codec.encode(&body).unwrap().len(), baseline);
                }
            }
            for version in [
                ClientVersion::V222,
                ClientVersion::V512,
                ClientVersion::V512Mac,
                ClientVersion::V603R7,
            ] {
                for key in [0, 0x33, 127] {
                    let mut codec = Codec::new(EncryptionGen::Gen5, key, Some(version));
                    let mode = if codec.compress_all { 3 } else { 2 };
                    let expected = action_transport(mode, 0).encode(&body).unwrap().len();
                    codec.encode(b"P       before\n").unwrap();
                    assert_eq!(codec.encode(&body).unwrap().len(), expected);
                }
            }
        }
        let mut actual = Codec::new(EncryptionGen::Gen5, 0x33, Some(ClientVersion::V222));
        let mut untouched = Codec::new(EncryptionGen::Gen5, 0x33, Some(ClientVersion::V222));
        assert_eq!(
            actual.encode(b"P       first\n").unwrap(),
            untouched.encode(b"P       first\n").unwrap()
        );
        assert!(Codec::fits_npc_action(b"P       checked\n"));
        assert_eq!(
            actual.encode(b"P       after\n").unwrap(),
            untouched.encode(b"P       after\n").unwrap()
        );
    }

    #[test]
    fn existing_v222_codec_scrubs_nul_in_action_text() {
        let client_message =
            seihai_core::net::trigger_action_message(0.0, 0.0, "control", "nul\0here");
        let mut payload = Vec::new();
        crate::wire::gshort(0, &mut payload);
        payload.extend_from_slice(&client_message[1..client_message.len() - 1]);
        let body = crate::wire::msg(crate::wire::ids::PLO_TRIGGERACTION, &payload);
        let mut server = Codec::new(EncryptionGen::Gen5, 0x33, Some(ClientVersion::V222));
        let mut client = Gen5::new(0x33);
        let wire = server.encode(&body).unwrap();
        let plain = seihai_core::protocol::read_frame(&mut wire.as_slice(), &mut client).unwrap();
        let mut state = seihai_core::protocol::SessionState::for_version(ClientVersion::V222);
        state.apply_body(&plain);
        assert_eq!(state.triggers.len(), 1);
        assert_eq!(state.triggers[0].params, "nul?here");
    }
}
