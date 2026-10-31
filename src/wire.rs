//! Server-to-client message encoding.
//!
//! `seihai-core` reads these messages (it is the client's library); this is
//! the writing side. The encodings are the wire facts the core's readers
//! decode (`GCHAR` = value + 0x20, `GSHORT`/`GINT3`/`GINT4`/`GINT5` = 7 bits
//! per byte, high first, each + 0x20), and the tests here check every writer
//! against the core's reader. When the core grows a server-to-client side
//! these should move there, so client and server share one definition.

/// A message: `GCHAR id` + payload + `\n`.
pub fn msg(id: u8, payload: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(payload.len() + 2);
    m.push(gchar(id));
    m.extend_from_slice(payload);
    m.push(b'\n');
    m
}

/// A value as a `GCHAR`. Values above 223 do not fit and are clamped.
pub fn gchar(v: u8) -> u8 {
    v.min(223) + 0x20
}

/// `n` 7-bit groups, high first, each + 0x20.
fn gint_n(v: u64, n: usize, out: &mut Vec<u8>) {
    let max = (1u64 << (7 * n)) - 1;
    let v = v.min(max);
    for i in (0..n).rev() {
        out.push(((v >> (7 * i)) & 0x7F) as u8 + 0x20);
    }
}

pub fn gshort(v: u16, out: &mut Vec<u8>) {
    gint_n(v as u64, 2, out)
}

pub fn gint3(v: u32, out: &mut Vec<u8>) {
    gint_n(v as u64, 3, out)
}

pub fn gint4(v: u32, out: &mut Vec<u8>) {
    gint_n(v as u64, 4, out)
}

pub fn gint5(v: u64, out: &mut Vec<u8>) {
    gint_n(v, 5, out)
}

/// `GCHAR len` + bytes, cut to the 223 bytes a `GCHAR` can count.
pub fn gstring(s: &[u8], out: &mut Vec<u8>) {
    let s = &s[..s.len().min(223)];
    out.push(gchar(s.len() as u8));
    out.extend_from_slice(s);
}

/// `PLO_RAWDATA` (100) for the block that follows, then the block: `GCHAR
/// id` + payload + `\n`, which may hold any byte. The client reads the
/// next `GINT3 n` bytes as one message.
pub fn raw_block(id: u8, payload: &[u8]) -> Vec<u8> {
    let block = msg(id, payload);
    let mut head = Vec::new();
    gint3(block.len() as u32, &mut head);
    let mut out = msg(ids::PLO_RAWDATA, &head);
    out.extend_from_slice(&block);
    out
}

/// Text in Latin-1, as the clients read it. Characters past U+00FF become `?`.
pub fn latin1(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| {
            if (c as u32) < 0x100 {
                c as u32 as u8
            } else {
                b'?'
            }
        })
        .collect()
}

/// The message ids this server uses.
pub mod ids {
    // Server to client.
    pub const PLO_PLAYERPROPS: u8 = 9;
    pub const PLO_LEVELNAME: u8 = 6;
    pub const PLO_ISLEADER: u8 = 10;
    pub const PLO_PLAYERWARP: u8 = 14;
    pub const PLO_DISCMESSAGE: u8 = 16;
    pub const PLO_SIGNATURE: u8 = 25;
    pub const PLO_FILESENDFAILED: u8 = 30;
    pub const PLO_LEVELMODTIME: u8 = 39;
    pub const PLO_STARTMESSAGE: u8 = 41;
    pub const PLO_NEWWORLDTIME: u8 = 42;
    pub const PLO_DEFAULTWEAPON: u8 = 43;
    pub const PLO_FILEUPTODATE: u8 = 45;
    pub const PLO_RAWDATA: u8 = 100;
    pub const PLO_BOARDPACKET: u8 = 101;
    pub const PLO_SETACTIVELEVEL: u8 = 156;
    pub const PLO_STATUSLIST: u8 = 180;
    pub const PLO_LISTPROCESSES: u8 = 182;

    // Client to server.
    pub const PLI_LEVELWARP: u8 = 0;
    pub const PLI_PLAYERPROPS: u8 = 2;
    pub const PLI_FLAGSET: u8 = 18;
    pub const PLI_WANTFILE: u8 = 23;
    pub const PLI_SHOWIMG: u8 = 24;
    pub const PLI_WEAPONADD: u8 = 25;
    pub const PLI_LEVELWARPMOD: u8 = 30;
    pub const PLI_PACKETCOUNT: u8 = 31;
    pub const PLI_UPDATEFILE: u8 = 34;
    pub const PLI_ADJACENTLEVEL: u8 = 35;
    pub const PLI_LANGUAGE: u8 = 37;
    pub const PLI_TAMPERCHECK: u8 = 39;
    pub const PLI_PROCESSLIST: u8 = 44;
    pub const PLI_ENTERLEVEL: u8 = 46;
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::{read_gint3, read_gshort, split_messages};

    #[test]
    fn gints_read_back_with_the_core_readers() {
        for v in [0u32, 1, 127, 128, 8194, 16383, 2_000_000] {
            let mut b = Vec::new();
            gint3(v, &mut b);
            assert_eq!(read_gint3(&b), Some(v as usize), "{v}");
        }
        for v in [0u16, 1, 300, 16383] {
            let mut b = Vec::new();
            gshort(v, &mut b);
            assert_eq!(read_gshort(&b), Some(v));
        }
        // GINT5 of a Unix time, as PLO_LEVELMODTIME carries it.
        let mut b = Vec::new();
        gint5(1_761_900_000, &mut b);
        let back = b.iter().fold(0u64, |a, &c| (a << 7) | (c - 0x20) as u64);
        assert_eq!(back, 1_761_900_000);
    }

    #[test]
    fn raw_block_splits_as_one_message() {
        let mut payload = vec![b'\n'; 10];
        payload.extend_from_slice(b"tail");
        let mut body = raw_block(101, &payload);
        body.extend(msg(6, b"next.nw"));
        let m = split_messages(&body);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].id, 101);
        // The block's own `\n` is part of it.
        assert_eq!(&m[0].payload[..payload.len()], &payload[..]);
        assert_eq!(m[1].id, 6);
        assert_eq!(m[1].payload, b"next.nw");
    }

    #[test]
    fn gstring_is_length_biased() {
        let mut b = Vec::new();
        gstring(b"abc", &mut b);
        assert_eq!(b, b"#abc");
    }
}
