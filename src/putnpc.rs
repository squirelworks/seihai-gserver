//! NPCs a client puts in its level, and takes away again: `PLI_PUTNPC`
//! (21) and `PLI_NPCDEL` (22). GS1's `putnpc image,script,x,y` sends 21;
//! the server reads the script file itself.
//!
//! Read from GServer 1.3 and 1.28 beta 2 (`docs/ORIGINAL-SERVERS.md`):
//!
//! - 21 is a `GCHAR` length and the image, a `GCHAR` length and the
//!   script file's name, then `GCHAR` x and y in half tiles. The server
//!   reads the script file from its own folder (an NPC without a script if
//!   it has none), makes the NPC in the sender's level and sends it (3) to
//!   everyone there. Later arrivals get it with the level's NPCs.
//! - 22 is the NPC's three-byte id: the NPC goes, and the level is told
//!   (`PLO_NPCDEL`, 29).
//!
//! This server allows them under `rules = 1999`, or with `put_npcs = yes`
//! in `config/server.txt` (`put_npcs = no` turns them off under 1999).
//! The script file is looked up like any file a client asks for
//! (`files.rs`): a plain name, from the world's `files/` folder. Unlike the 1999
//! servers, which let 22 take away any NPC in the level, only an NPC a
//! client put can be taken away here; the level's own NPCs stay.

/// GS1's `putnpc`.
pub const PLI_PUTNPC: u8 = 21;
/// A put NPC taken away.
pub const PLI_NPCDEL: u8 = 22;

/// A put NPC, as 21 carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Put {
    pub image: String,
    pub script: String,
    /// Half tiles.
    pub x2: u8,
    pub y2: u8,
}

/// Read 21. `None` when it is cut short.
pub fn parse(raw: &[u8]) -> Option<Put> {
    let (image, rest) = string(raw)?;
    let (script, rest) = string(rest)?;
    let [x, y, ..] = rest else {
        return None;
    };
    Some(Put {
        image,
        script,
        x2: x.wrapping_sub(0x20),
        y2: y.wrapping_sub(0x20),
    })
}

/// Read 22: the NPC's id.
pub fn parse_del(raw: &[u8]) -> Option<u32> {
    seihai_core::protocol::read_gint3(raw.get(..3)?).map(|v| v as u32)
}

fn string(raw: &[u8]) -> Option<(String, &[u8])> {
    let (&n, rest) = raw.split_first()?;
    let n = usize::from(n.checked_sub(0x20)?);
    let (s, rest) = rest.split_at_checked(n)?;
    Some((seihai_core::protocol::latin1(s), rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_put_and_a_delete() {
        let mut raw = vec![0x20 + 8];
        raw.extend(b"door.png");
        raw.push(0x20 + 8);
        raw.extend(b"door.txt");
        raw.extend([0x20 + 61, 0x20 + 40]);
        assert_eq!(
            parse(&raw),
            Some(Put {
                image: "door.png".into(),
                script: "door.txt".into(),
                x2: 61,
                y2: 40,
            })
        );
        assert_eq!(parse(&raw[..raw.len() - 1]), None);
        assert_eq!(parse(&[0x30, b'a']), None);
        assert_eq!(parse_del(&[0x20, 0x21, 0x25]), Some(0x85));
        assert_eq!(parse_del(&[0x20]), None);
    }
}
