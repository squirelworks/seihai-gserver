//! A player's props from an RC (stage 4 slice 7, issue #52), as seihai-rc
//! saw them from a real RC against the fork (its `docs/PROTOCOL.md`,
//! "Comments, account ban, rights and props", and the lab note
//! `2026-11-04-rc-props-set.md` there).
//!
//! | RC sends | Server answers |
//! |---|---|
//! | 73 `RC_PLAYERPROPSGET2`: `GSHORT` player id | 72 for that player's account |
//! | 74 `RC_PLAYERPROPSGET3`: `GSTRING` account, an empty `GSTRING` | 72 with player id 0 |
//! | 76 `RC_PLAYERPROPSSET2`: `GSTRING` account, `GSTRING` world, `GCHAR n` and `n` bytes of props, the 72 tail, `GCHAR 0` | nothing |
//!
//! 72 is `GSHORT` player id, `GSTRING` account, `GSTRING` world, `GCHAR n`
//! and `n` bytes of props, `GSHORT` flag count and a `GSTRING` each,
//! `GSHORT` opened-chest count, `GCHAR` weapon count and a `GSTRING` each.
//!
//! The props are the account file's, written as a 2.22 reads its own
//! props ([`props_block`]), with every prop the real RC sends back in 76
//! (so seihai-rc's `players props --set` can rebuild it): ones this server
//! keeps no value for (26 to 28, 30, 36) go as 0, the world is `main`. The
//! chest count is always 0, because what follows a non-zero one is not
//! known. The weapons are the account's `WEAPON` lines.
//!
//! A set applies the props an account keeps (`apply`) and replaces the
//! flags, as the fork does; chests are kept. The values are the account
//! file's.
//!
//! A player online is read and set through the world. A get shows the
//! file with what the world holds now put over it (level, position, the
//! counted props, chests and flags, as their logout would write them). A
//! set sends the [`LIVE`] props to the player as the NPC-server's
//! `SET_PLAYER_PROPS` does (the player and their level see them at once)
//! and replaces the world's flags, then writes the file too, so the props
//! the logout does not write (head, colours, sword and the rest) stay.
//! Level and position are left to the warp (82); the logout writes the
//! world's. The player's client is not sent the new flags.

use crate::accounts::Account;
use crate::wire::{gchar, gint3, gint5, gshort, gstring, latin1, msg, text};
use seihai_core::protocol::{parse_player_props, read_gint3, read_gshort};
use std::collections::BTreeMap;

pub const PLI_RC_PLAYERPROPSGET2: u8 = 73;
pub const PLI_RC_PLAYERPROPSGET3: u8 = 74;
pub const PLI_RC_PLAYERPROPSSET2: u8 = 76;
pub const PLO_RC_PLAYERPROPSGET: u8 = 72;

/// The props of a 76 that go to a player online: what [`apply`] keeps,
/// without the level and position.
pub const LIVE: [u8; 13] = [1, 2, 3, 4, 5, 6, 8, 9, 11, 13, 18, 32, 35];

/// What a 76 carries that this server reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Set {
    pub account: String,
    pub props: BTreeMap<u8, Vec<u8>>,
    pub flags: Vec<String>,
}

fn read_gstring(b: &[u8], i: &mut usize) -> Option<Vec<u8>> {
    let n = usize::from(b.get(*i)?.wrapping_sub(0x20));
    let s = b.get(*i + 1..*i + 1 + n)?.to_vec();
    *i += 1 + n;
    Some(s)
}

/// 74: the account. `None` when it does not read.
pub fn parse_get3(rest: &[u8]) -> Option<String> {
    let mut i = 0;
    Some(text(&read_gstring(rest, &mut i)?).trim().to_string()).filter(|a| !a.is_empty())
}

/// 76.
pub fn parse_set(rest: &[u8]) -> Option<Set> {
    let mut i = 0;
    let account = text(&read_gstring(rest, &mut i)?);
    read_gstring(rest, &mut i)?; // the world
    let block = read_gstring(rest, &mut i)?;
    let props = parse_player_props(&block)
        .into_iter()
        .map(|p| (p.id, p.raw))
        .collect();
    let n = read_gshort(rest.get(i..)?)?;
    i += 2;
    let mut flags = Vec::new();
    for _ in 0..n {
        flags.push(text(&read_gstring(rest, &mut i)?));
    }
    Some(Set {
        account,
        props,
        flags,
    })
}

/// The account's props as 72 carries them, in id order.
pub fn props_block(a: &Account) -> Vec<u8> {
    let mut b = Vec::new();
    let mut put = |id: u8, v: &[u8]| {
        b.push(gchar(id));
        b.extend_from_slice(v);
    };
    let s = |t: &str| {
        let mut v = Vec::new();
        gstring(&latin1(t), &mut v);
        v
    };
    let i3 = |n: u32| {
        let mut v = Vec::new();
        gint3(n, &mut v);
        v
    };
    put(0, &s(&a.nick));
    put(1, &[gchar(a.max_hp.min(20))]);
    put(2, &[gchar((a.hp * 2.0).clamp(0.0, 40.0) as u8)]);
    put(3, &i3(a.rupees));
    put(4, &[gchar(a.arrows.min(99))]);
    put(5, &[gchar(a.bombs.min(99))]);
    put(6, &[gchar(a.glove.min(3))]);
    put(7, &[gchar(1)]);
    put(8, &power_image(a.sword.0, 30, &a.sword.1));
    put(9, &power_image(a.shield.0, 10, &a.shield.1));
    put(10, &s("idle"));
    let head = latin1(&a.head);
    let head = &head[..head.len().min(60)];
    let mut h = vec![gchar(100 + head.len() as u8)];
    h.extend_from_slice(head);
    put(11, &h);
    put(13, &a.colors.map(gchar));
    put(15, &[gchar((a.x * 2.0).clamp(0.0, 223.0) as u8)]);
    put(16, &[gchar((a.y * 2.0).clamp(0.0, 223.0) as u8)]);
    put(18, &[gchar(a.status & !0x02)]);
    put(20, &s(&a.level));
    put(26, &[gchar(0)]);
    put(27, &i3(0));
    put(28, &i3(0));
    put(29, &i3(0));
    let mut ip = Vec::new();
    gint5(0, &mut ip);
    put(30, &ip);
    put(32, &[gchar(a.alignment.min(100))]);
    put(34, &s(&a.name));
    put(35, &s(&a.body));
    put(36, &i3(0));
    b
}

fn power_image(power: u8, bias: u8, image: &str) -> Vec<u8> {
    if image.is_empty() {
        return vec![gchar(power)];
    }
    let mut v = vec![gchar(power.saturating_add(bias))];
    gstring(&latin1(&image[..image.len().min(60)]), &mut v);
    v
}

/// 72 for `a`, as player `id` (0 for an account asked for by name).
pub fn reply(id: u16, a: &Account) -> Vec<u8> {
    let mut p = Vec::new();
    gshort(id, &mut p);
    gstring(&latin1(&a.name), &mut p);
    gstring(b"main", &mut p);
    let mut block = props_block(a);
    if block.len() > 223 {
        // The GCHAR length cannot say more; the nickname goes first.
        block = props_block(&Account {
            nick: String::new(),
            ..a.clone()
        });
    }
    gstring(&block, &mut p);
    let flags: Vec<&String> = a.flags.iter().take(usize::from(u16::MAX >> 2)).collect();
    gshort(flags.len() as u16, &mut p);
    for f in flags {
        gstring(&latin1(f), &mut p);
    }
    gshort(0, &mut p);
    let weapons: Vec<&String> = a.weapons.iter().take(223).collect();
    p.push(gchar(weapons.len() as u8));
    for w in weapons {
        gstring(&latin1(w), &mut p);
    }
    msg(PLO_RC_PLAYERPROPSGET, &p)
}

/// Apply the props of a 76 that the account keeps, and its flags.
pub fn apply(a: &mut Account, set: &Set) {
    let p = &set.props;
    let byte = |id: u8| match p.get(&id).map(Vec::as_slice) {
        Some([v, ..]) => Some(v.wrapping_sub(0x20)),
        _ => None,
    };
    let string = |v: &[u8]| v.get(1..).map(text).unwrap_or_default();
    if let Some(v) = byte(1) {
        a.max_hp = v.min(20);
    }
    if let Some(v) = byte(2) {
        a.hp = f32::from(v.min(40)) / 2.0;
    }
    if let Some(v) = p.get(&3).and_then(|v| read_gint3(v)) {
        a.rupees = v as u32;
    }
    if let Some(v) = byte(4) {
        a.arrows = v;
    }
    if let Some(v) = byte(5) {
        a.bombs = v;
    }
    if let Some(v) = byte(6) {
        a.glove = v;
    }
    for (id, bias, slot) in [(8u8, 30u8, &mut a.sword), (9, 10, &mut a.shield)] {
        if let Some(v) = p.get(&id) {
            let power = v[0].wrapping_sub(0x20);
            *slot = if v.len() > 1 {
                (power.saturating_sub(bias), string(&v[1..]))
            } else {
                (power, String::new())
            };
        }
    }
    if let Some(v) = p.get(&11).filter(|v| v.len() > 1) {
        a.head = text(&v[1..]);
    }
    if let Some(v) = p.get(&13).filter(|v| v.len() == 5) {
        for (c, b) in a.colors.iter_mut().zip(v) {
            *c = b.wrapping_sub(0x20);
        }
    }
    if let Some(v) = byte(15) {
        a.x = f32::from(v) / 2.0;
    }
    if let Some(v) = byte(16) {
        a.y = f32::from(v) / 2.0;
    }
    if let Some(v) = byte(18) {
        a.status = v & !0x02;
    }
    if let Some(v) = p.get(&20) {
        let l = string(v);
        if !l.is_empty() {
            a.level = l;
        }
    }
    if let Some(v) = byte(32) {
        a.alignment = v.min(100);
    }
    if let Some(v) = p.get(&35) {
        a.body = string(v);
    }
    a.flags = set
        .flags
        .iter()
        .map(|f| f.replace(['\n', '\r'], " "))
        .filter(|f| !f.trim().is_empty())
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::split_messages;

    #[test]
    fn a_reply_reads_back_and_a_set_applies() {
        let mut a = Account::new("test1", "onlinestartlocal.nw", 30.0, 30.5);
        a.rupees = 1234;
        a.flags = vec!["quest=2".into()];
        let r = reply(7, &a);
        let m = &split_messages(&r)[0];
        assert_eq!(m.id, PLO_RC_PLAYERPROPSGET);
        // Every prop decodes to the block's last byte.
        let mut i = 2;
        read_gstring(&m.payload, &mut i).unwrap();
        read_gstring(&m.payload, &mut i).unwrap();
        let block = read_gstring(&m.payload, &mut i).unwrap();
        let props = parse_player_props(&block);
        let used: usize = props.iter().map(|p| 1 + p.raw.len()).sum();
        assert_eq!(used, block.len());

        // A 76 from those props, with changes.
        let mut set = Set {
            account: "test1".into(),
            props: props.into_iter().map(|p| (p.id, p.raw)).collect(),
            flags: vec!["quest=3".into(), "met.boss".into()],
        };
        let mut g = Vec::new();
        gint3(5, &mut g);
        set.props.insert(3, g);
        set.props.insert(2, vec![gchar(5)]);
        let mut b = a.clone();
        apply(&mut b, &set);
        assert_eq!((b.rupees, b.hp), (5, 2.5));
        assert_eq!(b.flags, ["quest=3", "met.boss"]);
        // Everything else as it was.
        let mut same = b.clone();
        same.rupees = a.rupees;
        same.hp = a.hp;
        same.flags = a.flags.clone();
        assert_eq!(same, a);
    }

    #[test]
    fn the_reply_ends_with_the_accounts_weapons() {
        let mut a = Account::new("test1", "onlinestartlocal.nw", 30.0, 30.5);
        let r = reply(7, &a);
        assert!(split_messages(&r)[0]
            .payload
            .ends_with(&[gchar(0), gchar(0), gchar(0)]));
        a.weapons = vec!["bow".into(), "Lantern".into()];
        let r = reply(7, &a);
        let mut want = vec![gchar(0), gchar(0), gchar(2)];
        gstring(b"bow", &mut want);
        gstring(b"Lantern", &mut want);
        assert!(split_messages(&r)[0].payload.ends_with(&want));
    }
}
