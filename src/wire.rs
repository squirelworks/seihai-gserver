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

/// Read a GINT5 (five GCHARs, high 7 bits first); short input reads as 0.
pub fn read_gint5(b: &[u8]) -> u64 {
    if b.len() < 5 {
        return 0;
    }
    b[..5]
        .iter()
        .fold(0, |v, &c| (v << 7) | u64::from(c.wrapping_sub(0x20) & 0x7F))
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

/// The message ids this server names.
///
/// Each line is the layout this server sends or reads, and who it is for.
/// "Confirmed" means the bytes were checked against a client or against
/// this server's own writer. "Inferred" means the role is the best reading
/// and is not settled. Ids with no constant are listed in `docs/packet-ids.md`.
/// Names added for a later stage are not sent yet.
pub mod ids {
    // Server to client. Sent today.
    /// Level board. Confirmed: an empty one ends a gmap cell.
    pub const PLO_LEVELBOARD: u8 = 0;
    /// One link line. Confirmed.
    pub const PLO_LEVELLINK: u8 = 1;
    /// Baddy props. Confirmed: this server relays them.
    pub const PLO_BADDYPROPS: u8 = 2;
    /// NPC props. Confirmed: this server relays them.
    pub const PLO_NPCPROPS: u8 = 3;
    /// One chest. Confirmed.
    pub const PLO_LEVELCHEST: u8 = 4;
    /// One sign. Confirmed.
    pub const PLO_LEVELSIGN: u8 = 5;
    /// Level name. Confirmed.
    pub const PLO_LEVELNAME: u8 = 6;
    /// Another player's props. Confirmed. Only the props that player's build lists.
    pub const PLO_OTHERPLPROPS: u8 = 8;
    /// The player's own props. Confirmed.
    pub const PLO_PLAYERPROPS: u8 = 9;
    /// Empty. Confirmed: the player leads the level.
    pub const PLO_ISLEADER: u8 = 10;
    /// A bomb. Confirmed: relayed from the client.
    pub const PLO_BOMBADD: u8 = 11;
    /// A bomb removed. Confirmed: relayed from the client.
    pub const PLO_BOMBDEL: u8 = 12;
    /// Half-tile warp: x, y, level name. Confirmed.
    pub const PLO_PLAYERWARP: u8 = 14;
    /// Text, then the link closes. Confirmed.
    pub const PLO_DISCMESSAGE: u8 = 16;
    /// An arrow. Confirmed: relayed from the client.
    pub const PLO_ARROWADD: u8 = 19;
    /// A fire spy. Confirmed: relayed from the client.
    pub const PLO_FIRESPY: u8 = 20;
    /// An item on the floor. Confirmed.
    pub const PLO_ITEMADD: u8 = 22;
    /// An item removed. Confirmed.
    pub const PLO_ITEMDEL: u8 = 23;
    /// One GCHAR. Confirmed: this server sends 73.
    pub const PLO_SIGNATURE: u8 = 25;
    /// A hurt baddy. Confirmed: relayed.
    pub const PLO_BADDYHURT: u8 = 27;
    /// One flag, `name` or `name=value`. Confirmed.
    pub const PLO_FLAGSET: u8 = 28;
    /// File name. Confirmed: the file will not be sent.
    pub const PLO_FILESENDFAILED: u8 = 30;
    /// An explosion. Confirmed: relayed.
    pub const PLO_EXPLOSION: u8 = 36;
    /// A private message. Confirmed.
    pub const PLO_PRIVATEMESSAGE: u8 = 37;
    /// GINT5 modification time. Confirmed.
    pub const PLO_LEVELMODTIME: u8 = 39;
    /// A hurt player. Confirmed.
    pub const PLO_HURTPLAYER: u8 = 40;
    /// The start message. Confirmed.
    pub const PLO_STARTMESSAGE: u8 = 41;
    /// World time. Confirmed: one step every 5 s.
    pub const PLO_NEWWORLDTIME: u8 = 42;
    /// One GCHAR, the default weapon. Confirmed.
    pub const PLO_DEFAULTWEAPON: u8 = 43;
    /// The client's copy of a file is current. Confirmed. Not sent before 2.1.
    pub const PLO_FILEUPTODATE: u8 = 45;
    /// Pixel warp: GCHAR x*2, y*2, z, gmap x, gmap y, then the level. Confirmed.
    pub const PLO_PLAYERWARP2: u8 = 49;
    /// Start of a large file. Confirmed.
    pub const PLO_LARGEFILESTART: u8 = 68;
    /// End of a large file. Confirmed.
    pub const PLO_LARGEFILEEND: u8 = 69;
    /// Size of a large file. Confirmed.
    pub const PLO_LARGEFILESIZE: u8 = 84;
    /// GINT3 length; the bytes follow outside the line. Confirmed.
    pub const PLO_RAWDATA: u8 = 100;
    /// A level board in a raw block. Confirmed.
    pub const PLO_BOARDPACKET: u8 = 101;
    /// A file body. Confirmed.
    pub const PLO_FILE: u8 = 102;
    /// Level name. Confirmed: chests and NPCs apply to this level.
    pub const PLO_SETACTIVELEVEL: u8 = 156;
    /// Status-list text. Confirmed.
    pub const PLO_STATUSLIST: u8 = 180;
    /// Empty in the login burst. Confirmed: asks for the process list.
    pub const PLO_LISTPROCESSES: u8 = 182;

    /// `{104}{name}{byte}{GINT5 lo}{GINT5 hi}`: DES key for an encrypted
    /// package. Confirmed for the key when the byte is 1; other byte values
    /// are unknown. Sent before a `.gpake`. 3.153 to 6.03r7.
    /// Seihai name until 2026-11-11: `PLO_FILEKEY`.
    pub const PLO_FILEDECRYPTKEY: u8 = 104;
    // Named for a later stage. Not sent yet.
    /// `{108}{sub}{GSHORT value}`. Confirmed for sub 0: the client answers
    /// [`PLI_PONG`]. Sub 1 is stored and nothing that reads it was traced, so
    /// do not send it. 4.20r6, 4.20r8, 6.01r1, 6.03r7. Not 5.x, not before 4.20.
    pub const PLO_PING: u8 = 108;
    /// `{109}{GINT5 mod time}{local name}{url}`. Confirmed layout. Inferred:
    /// the client stores the download under the local name. 5.24r4 to 6.03r7.
    /// Seihai name until 2026-11-11: `PLO_DOWNLOADURL`.
    pub const PLO_HTTPDOWNLOAD: u8 = 109;
    /// Header `type,name,cache flag,GINT5,GINT5` (the two GINT5 values are the
    /// DES key), then the body. Confirmed. With the cache flag set, the client
    /// writes the body it received.
    pub const PLO_NPCWEAPONSCRIPT: u8 = 140;
    /// Empty. Confirmed: sets a one-shot flag. Inferred: the flag means login
    /// is done. 2.05 to 6.03r7, after the login burst.
    /// Seihai name until 2026-11-11: `PLO_LOGGEDIN`.
    pub const PLO_ONLINEMODE: u8 = 168;
    /// `{193}{GINT5 crc32}{weapon name}`: use the cached weapon script when
    /// the CRC matches. Confirmed. 4.04r2 to 5.12. Not a modification time.
    /// Seihai name until 2026-11-11: `PLO_WEAPONCRC`.
    pub const PLO_WEAPONHASH: u8 = 193;
    /// Empty. Confirmed: clears the loaded weapon and class scripts.
    /// 4.04r2 to 6.03r7.
    /// Seihai name until 2026-11-11: `PLO_CLEARWEAPONS`.
    pub const PLO_RESETCLIENT: u8 = 194;
    /// Use the cached script: the same DES key as [`PLO_NPCWEAPONSCRIPT`] and
    /// the CRC32 of the plain body. Confirmed. No body in this message.
    pub const PLO_LOADSCRIPT: u8 = 197;

    // Client to server.
    /// Half-tile warp: GCHAR x*2, y*2, level name. Confirmed.
    pub const PLI_LEVELWARP: u8 = 0;
    /// The player's props. Confirmed.
    pub const PLI_PLAYERPROPS: u8 = 2;
    /// NPC props. Confirmed.
    pub const PLI_NPCPROPS: u8 = 3;
    /// A bomb. Confirmed.
    pub const PLI_BOMBADD: u8 = 4;
    /// A bomb removed. Confirmed.
    pub const PLI_BOMBDEL: u8 = 5;
    /// An arrow. Confirmed.
    pub const PLI_ARROWADD: u8 = 9;
    /// A fire spy. Confirmed.
    pub const PLI_FIRESPY: u8 = 10;
    /// An item: GCHAR x*2, y*2, item. Confirmed.
    pub const PLI_ITEMADD: u8 = 12;
    /// An item removed. Confirmed.
    pub const PLI_ITEMDEL: u8 = 13;
    /// A kill claim. Confirmed.
    pub const PLI_CLAIMPKER: u8 = 14;
    /// Baddy props. Confirmed.
    pub const PLI_BADDYPROPS: u8 = 15;
    /// A hurt baddy. Confirmed.
    pub const PLI_BADDYHURT: u8 = 16;
    /// `name` or `name=value`. Confirmed.
    pub const PLI_FLAGSET: u8 = 18;
    /// A flag removed. Confirmed.
    pub const PLI_FLAGDEL: u8 = 19;
    /// GCHAR x, y, in tiles. Confirmed.
    pub const PLI_OPENCHEST: u8 = 20;
    /// A file name. Confirmed.
    pub const PLI_WANTFILE: u8 = 23;
    /// An image update. Confirmed: read and not answered.
    pub const PLI_SHOWIMG: u8 = 24;
    /// `{25}{name}{image}{script}`, same shape as a weapon add from the server.
    /// Confirmed: 1.21 to 1.37 send this for every weapon they hold. Accepted
    /// and not answered until weapons are a stage.
    /// Seihai name until 2026-11-11: `PLI_WEAPONUPLOAD`.
    pub const PLI_TOWEAPON: u8 = 25;
    /// A hurt player. Confirmed.
    pub const PLI_HURTPLAYER: u8 = 26;
    /// An explosion. Confirmed.
    pub const PLI_EXPLOSION: u8 = 27;
    /// GINT5 modification time, then a warp. Confirmed.
    pub const PLI_LEVELWARPMOD: u8 = 30;
    /// A packet count. Confirmed: 1.33 to 1.39, about every 10 s. Not a login.
    pub const PLI_PACKETCOUNT: u8 = 31;
    /// The player took an item. Confirmed.
    pub const PLI_ITEMTAKE: u8 = 32;
    /// A leading 0 byte, then the default weapon id. Confirmed: 1.38 and later
    /// send this instead of [`PLI_TOWEAPON`]. Accepted and not answered yet.
    pub const PLI_WEAPONADD: u8 = 33;
    /// GINT5 modification time, then a file name. Confirmed.
    pub const PLI_UPDATEFILE: u8 = 34;
    /// GINT5 modification time, then a level name. Confirmed: a gmap neighbour.
    pub const PLI_ADJACENTLEVEL: u8 = 35;
    /// The client's language. Confirmed: read and not answered.
    pub const PLI_LANGUAGE: u8 = 37;
    /// Two hashes. Confirmed: read and not answered. Never required.
    pub const PLI_TAMPERCHECK: u8 = 39;
    /// The process list, answering [`PLO_LISTPROCESSES`]. Confirmed: read and not answered.
    pub const PLI_PROCESSLIST: u8 = 44;
    /// Entered a level. Confirmed: read and not answered.
    pub const PLI_ENTERLEVEL: u8 = 46;
    /// The script name. Confirmed: the client asks again when its cache does not match.
    pub const PLI_UPDATESCRIPT: u8 = 158;
    /// Game clients: `!` and a GSHORT of the [`PLO_PING`] value, capped at
    /// 28671. RCs: empty, every 60 s. Confirmed layout. Inferred that it is a ping.
    /// Seihai name until 2026-11-11: `PLI_PING`.
    pub const PLI_PONG: u8 = 162;

    /// True when stage 1 reads `id` and does not answer it, so it is not
    /// logged as unhandled.
    pub fn stage1_quiet(id: u8) -> bool {
        matches!(
            id,
            PLI_LANGUAGE
                | PLI_PACKETCOUNT
                | PLI_TOWEAPON
                | PLI_WEAPONADD
                | PLI_TAMPERCHECK
                | PLI_PROCESSLIST
                | PLI_SHOWIMG
                | PLI_ENTERLEVEL
        )
    }
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

    #[test]
    fn weapon_ids_and_the_quiet_set() {
        assert_eq!(ids::PLI_TOWEAPON, 25);
        assert_eq!(ids::PLI_WEAPONADD, 33);
        assert!(ids::stage1_quiet(ids::PLI_TOWEAPON));
        assert!(ids::stage1_quiet(ids::PLI_WEAPONADD));
        assert!(!ids::stage1_quiet(7));
        assert_eq!(ids::PLI_PONG, 162);
        assert_eq!(ids::PLO_FILEDECRYPTKEY, 104);
        assert_eq!(ids::PLO_PING, 108);
        assert_eq!(ids::PLO_HTTPDOWNLOAD, 109);
        assert_eq!(ids::PLO_ONLINEMODE, 168);
        assert_eq!(ids::PLO_WEAPONHASH, 193);
        assert_eq!(ids::PLO_RESETCLIENT, 194);
        assert_eq!(ids::PLO_NPCWEAPONSCRIPT, 140);
        assert_eq!(ids::PLO_LOADSCRIPT, 197);
        assert_eq!(ids::PLI_UPDATESCRIPT, 158);
    }
}
