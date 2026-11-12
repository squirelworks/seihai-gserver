//! Other players' props (stage 2.3): what the world keeps of each player's
//! look and position, and `PLO_OTHERPLPROPS` (8) for each receiver's build.
//!
//! Props arrive in the sender's encoding and are kept in one form: the
//! value bytes by prop id, with the position as numbers. Each receiver then
//! gets only the props its build reads (`props.other`), since its prop
//! parser stops at the first id it does not know.
//!
//! - The position goes as X2/Y2 (78/79) where the receiver reads them, else
//!   as the classic half-tile bytes (15/16), so a 2.31 sender that only
//!   sends 78/79 still moves on a 2.05 screen.
//! - Prop 10 is the bow slot before 2.1 (`props.bow_prop10`), except 2.05
//!   rev 2, which uses it as the gani like 2.1 on ([`bow_prop10`]): it is
//!   not kept from bow-slot senders and not sent to bow-slot receivers.
//! - The player id (14) is in the message header, not a prop. 1.35 reads
//!   it as one GCHAR; later builds read a GSHORT (`put_id`). Joining and
//!   leaving (50 `JOINLEAVELVL`, 51 `PCONNECTED`) are the server's to say.
//!   On logout the lab GServer sent 50 = 0, then 51, to 2.22
//!   (Seihai `lab-verification.md`), and this does the same.

use crate::wire::{gchar, gshort, ids::PLO_OTHERPLPROPS, msg};
use seihai_core::protocol::{
    decode_pos_precise, encode_pos_precise, parse_player_props_with, read_gint3, read_gshort,
    ClientVersion,
};
use std::collections::BTreeMap;

/// Writes the player id that heads `PLO_OTHERPLPROPS` the way `receiver`
/// reads it. Original 1.35 reads one GCHAR before the props (exact-build
/// static evidence, graal-client-repro#179); 1.36 on read a GSHORT. A 1.35
/// id is capped at 223, the largest GCHAR.
pub fn put_id(receiver: ClientVersion, id: u16, out: &mut Vec<u8>) {
    if one_byte_id(receiver) {
        out.push(gchar(id.min(223) as u8));
    } else {
        gshort(id, out);
    }
}

/// Whether prop 10 is `receiver`'s bow slot rather than the gani (the
/// profile's `props.bow_prop10`), except for 2.05 rev 2: a real one sends
/// and reads prop 10 as gani text (`sword`, `hurt`), and the lab GServer
/// relays it to and from a 2.22 unchanged, which is how the two hurt each
/// other (`beta4-sword-and-dead-login-2026-11-12.md` in the project files).
/// 2.00 to 2.04 were not tested and keep the profile's bow slot.
pub fn bow_prop10(v: ClientVersion) -> bool {
    v != ClientVersion::V205 && v.profile().props.bow_prop10
}

/// Builds whose `PLO_OTHERPLPROPS` id is one GCHAR.
pub fn one_byte_id(v: ClientVersion) -> bool {
    v == ClientVersion::V135
}

pub const JOINLEAVELVL: u8 = 50;
pub const PCONNECTED: u8 = 51;
const X: u8 = 15;
const Y: u8 = 16;
const X2: u8 = 78;
const Y2: u8 = 79;

/// One player's props as the world keeps them.
#[derive(Debug, Clone, Default)]
pub struct Props {
    /// Value bytes by prop id, without the position.
    values: BTreeMap<u8, Vec<u8>>,
    x: Option<f32>,
    y: Option<f32>,
}

/// What changed in one update, to pass on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Change {
    values: BTreeMap<u8, Vec<u8>>,
    x: Option<f32>,
    y: Option<f32>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.x.is_none() && self.y.is_none()
    }

    /// Add a later change: where both set a prop, the later value stays.
    pub fn merge(&mut self, later: Change) {
        self.values.extend(later.values);
        if later.x.is_some() {
            self.x = later.x;
        }
        if later.y.is_some() {
            self.y = later.y;
        }
    }

    /// A one-`GCHAR` prop in the change (the gmap cell, 43 and 44).
    pub fn byte(&self, id: u8) -> Option<u8> {
        match self.values.get(&id)?.as_slice() {
            [v] => Some(v.wrapping_sub(0x20)),
            _ => None,
        }
    }
}

/// A classic `X`/`Y` byte in tiles. A byte under 0x20 is a step off
/// the level's left or top edge: a 2.05 walking past the edge where no
/// link is writes -0.5 as 0x1F, which must not read as 127.5.
pub fn classic_pos(b: u8) -> f32 {
    (i16::from(b) - 0x20) as f32 / 2.0
}

impl Props {
    /// Apply a prop stream as `sender`'s build writes it; returns what
    /// changed.
    pub fn apply(&mut self, raw: &[u8], sender: ClientVersion) -> Change {
        let bow = bow_prop10(sender);
        let mut c = Change::default();
        for p in parse_player_props_with(raw, bow) {
            match p.id {
                X => c.x = p.raw.first().map(|&b| classic_pos(b)),
                Y => c.y = p.raw.first().map(|&b| classic_pos(b)),
                X2 => c.x = read_gshort(&p.raw).map(decode_pos_precise),
                Y2 => c.y = read_gshort(&p.raw).map(decode_pos_precise),
                10 if bow => {}
                14 | JOINLEAVELVL | PCONNECTED => {}
                id => {
                    if self.values.get(&id) != Some(&p.raw) {
                        c.values.insert(id, p.raw.clone());
                    }
                }
            }
        }
        self.values
            .extend(c.values.iter().map(|(k, v)| (*k, v.clone())));
        if c.x.is_some() {
            self.x = c.x;
        }
        if c.y.is_some() {
            self.y = c.y;
        }
        c
    }

    /// Put back a change the anti-cheat refused: the position goes back to
    /// `x`, `y`, the props `ids` to their values before, and the change
    /// passes on none of them.
    pub fn put_back(
        &mut self,
        change: &mut Change,
        x: f32,
        y: f32,
        before: &[(u8, Option<Vec<u8>>)],
    ) {
        self.x = Some(x);
        self.y = Some(y);
        change.x = None;
        change.y = None;
        for (id, v) in before {
            change.values.remove(id);
            match v {
                Some(v) => self.values.insert(*id, v.clone()),
                None => self.values.remove(id),
            };
        }
    }

    /// Put props `before` back (their values before a change the
    /// anti-cheat refused), and take them out of `change`. The position is
    /// left alone.
    pub fn restore(&mut self, change: &mut Change, before: &[(u8, Option<Vec<u8>>)]) {
        for (id, v) in before {
            change.values.remove(id);
            match v {
                Some(v) => self.values.insert(*id, v.clone()),
                None => self.values.remove(id),
            };
        }
    }

    /// Whether the change moves the player.
    pub fn moves(change: &Change) -> bool {
        change.x.is_some() || change.y.is_some()
    }

    /// A move the server made (a warp's arrival), as a change.
    pub fn moved(&mut self, x: f32, y: f32) -> Change {
        self.x = Some(x);
        self.y = Some(y);
        Change {
            x: self.x,
            y: self.y,
            ..Change::default()
        }
    }

    /// The kept position, in tiles.
    pub fn x(&self) -> Option<f32> {
        self.x
    }

    pub fn y(&self) -> Option<f32> {
        self.y
    }

    /// The kept value bytes by prop id (no position).
    pub fn values(&self) -> &BTreeMap<u8, Vec<u8>> {
        &self.values
    }

    /// Set one prop's value bytes; returns whether it changed.
    fn set(&mut self, id: u8, v: Vec<u8>) -> bool {
        if self.values.get(&id) == Some(&v) {
            return false;
        }
        self.values.insert(id, v);
        true
    }

    /// Everything kept, as a change.
    pub fn all(&self) -> Change {
        Change {
            values: self.values.clone(),
            x: self.x,
            y: self.y,
        }
    }
}

/// `PLO_OTHERPLPROPS` for `receiver` about player `id`: the props of
/// `change` its build reads, then `tail` (server props such as 50 and 51,
/// value bytes included). `None` when the build does not read message 8 or
/// nothing is left to say.
pub fn message(
    receiver: ClientVersion,
    id: u16,
    change: &Change,
    tail: &[(u8, &[u8])],
) -> Option<Vec<u8>> {
    let prof = receiver.profile();
    if !prof.protocol.plo.contains(PLO_OTHERPLPROPS) {
        return None;
    }
    let other = &prof.props.other;
    let mut body = Vec::new();
    let mut put = |pid: u8, v: &[u8]| {
        if other.contains(pid) && !(pid == 10 && bow_prop10(receiver)) {
            body.push(gchar(pid));
            body.extend_from_slice(v);
        }
    };
    for (&pid, v) in &change.values {
        put(pid, v);
    }
    let precise = other.contains(X2);
    for (v, classic, fine) in [(change.x, X, X2), (change.y, Y, Y2)] {
        if let Some(v) = v {
            if precise {
                let mut w = Vec::new();
                gshort(encode_pos_precise(v), &mut w);
                put(fine, &w);
            } else {
                put(classic, &[gchar((v * 2.0).clamp(0.0, 223.0) as u8)]);
            }
        }
    }
    for (pid, v) in tail {
        put(*pid, v);
    }
    if body.is_empty() {
        return None;
    }
    let mut p = Vec::new();
    put_id(receiver, id, &mut p);
    p.extend(body);
    Some(msg(PLO_OTHERPLPROPS, &p))
}

/// The tail for a player who is now in the receiver's level.
pub const JOINED: [(u8, &[u8]); 1] = [(JOINLEAVELVL, &[0x21])];
/// The tail for a player who went to another level.
pub const LEFT_LEVEL: [(u8, &[u8]); 1] = [(JOINLEAVELVL, &[0x20])];
/// The tail for a player who logged out.
pub const LOGGED_OUT: [(u8, &[u8]); 2] = [(JOINLEAVELVL, &[0x20]), (PCONNECTED, &[])];

/// Give `item` (a chest item index, `seihai_core::assets::CHEST_ITEMS`)
/// to a player: the counted props it raises (rupees 3, bombs 5, arrows 4,
/// hearts 1 and 2, glove 6). Returns the props that changed, in the 2.22
/// encoding; items this server does not count yet change nothing.
pub fn give_item(props: &mut Props, item: u8) -> Vec<(u8, Vec<u8>)> {
    let byte = |p: &Props, id: u8| match p.values.get(&id).map(Vec::as_slice) {
        Some([v, ..]) => v.wrapping_sub(0x20),
        _ => 0,
    };
    let rupees = |p: &Props| {
        p.values
            .get(&3)
            .and_then(|v| read_gint3(v))
            .map_or(0, |r| r as u32)
    };
    let mut new: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut add_rupees = |p: &Props, n: u32| {
        let mut v = Vec::new();
        crate::wire::gint3((rupees(p) + n).min(MAX_RUPEES), &mut v);
        new.push((3, v));
    };
    match item {
        0 => add_rupees(props, 1),
        1 => add_rupees(props, 5),
        2 => add_rupees(props, 30),
        19 => add_rupees(props, 100),
        3 => new.push((5, vec![gchar(byte(props, 5).saturating_add(5).min(99))])),
        4 => new.push((4, vec![gchar(byte(props, 4).saturating_add(5).min(99))])),
        5 => {
            let max = byte(props, 1).saturating_mul(2);
            new.push((2, vec![gchar(byte(props, 2).saturating_add(2).min(max))]));
        }
        11 => {
            let max = byte(props, 1).saturating_add(1).min(MAX_HEARTS);
            new.push((1, vec![gchar(max)]));
            new.push((2, vec![gchar(max * 2)]));
        }
        6 | 16 => {
            let glove = if item == 6 { 1 } else { 2 };
            new.push((6, vec![gchar(byte(props, 6).max(glove))]));
        }
        _ => {}
    }
    new.retain(|(id, v)| props.set(*id, v.clone()));
    new
}

/// The player's own position, for `own_message`: classic half tiles (15,
/// 16) and precise (78, 79); each build reads the ones it has.
pub fn own_position(x: f32, y: f32) -> Vec<(u8, Vec<u8>)> {
    let half = |v: f32| vec![gchar((v * 2.0).clamp(0.0, 223.0) as u8)];
    let fine = |v: f32| {
        let mut w = Vec::new();
        gshort(encode_pos_precise(v), &mut w);
        w
    };
    vec![(X, half(x)), (Y, half(y)), (X2, fine(x)), (Y2, fine(y))]
}

/// Rupees stop here: the most a `GINT3` carries.
pub const MAX_RUPEES: u32 = (1 << 21) - 1;
/// Hearts stop here.
pub const MAX_HEARTS: u8 = 20;

/// `PLO_PLAYERPROPS` (9) for the player's own build with `changed` (2.22
/// encoding, no position), keeping only the props the build reads as its
/// own. `None` when nothing is left.
pub fn own_message(version: ClientVersion, changed: &[(u8, Vec<u8>)]) -> Option<Vec<u8>> {
    let own = &version.profile().props.own;
    let mut body = Vec::new();
    for (id, v) in changed {
        if own.contains(*id) {
            body.push(gchar(*id));
            body.extend_from_slice(v);
        }
    }
    (!body.is_empty()).then(|| msg(crate::wire::ids::PLO_PLAYERPROPS, &body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Account;
    use crate::burst::{own_props, Player};
    use seihai_core::protocol::SessionState;

    #[test]
    fn chest_items_raise_the_counted_props() {
        let mut p = alice();
        let rupees = |p: &Props| read_gint3(&p.values()[&3]).unwrap();
        let byte = |p: &Props, id: u8| p.values()[&id][0] - 0x20;
        let (r, b, d) = (rupees(&p), byte(&p, 5), byte(&p, 4));
        assert_eq!(give_item(&mut p, 2).len(), 1);
        assert_eq!(rupees(&p), r + 30);
        give_item(&mut p, 19);
        assert_eq!(rupees(&p), r + 130);
        give_item(&mut p, 3);
        give_item(&mut p, 4);
        assert_eq!((byte(&p, 5), byte(&p, 4)), (b + 5, d + 5));
        // A heart fills two half hearts, up to the maximum.
        let max = byte(&p, 1);
        assert!(give_item(&mut p, 5).is_empty(), "already full");
        give_item(&mut p, 11);
        assert_eq!((byte(&p, 1), byte(&p, 2)), (max + 1, (max + 1) * 2));
        // Items the server does not count change nothing.
        assert!(give_item(&mut p, 10).is_empty());
        // The own message has only what the build reads as its own.
        let changed = give_item(&mut p, 0);
        let m = own_message(ClientVersion::V222, &changed).unwrap();
        assert_eq!(m[0], 9 + 0x20);
        let mut st = SessionState::for_version(ClientVersion::V222);
        st.apply_body(&m);
        assert_eq!(st.own_rupees, Some(r as u32 + 131));
        assert!(own_message(ClientVersion::V222, &[]).is_none());
    }

    fn alice() -> Props {
        let mut a = Account::new("alice", "", 30.0, 30.5);
        a.nick = "Alice".into();
        let mut p = Props::default();
        p.apply(
            &own_props(
                ClientVersion::V222,
                &Player {
                    id: 1,
                    account: &a,
                    leader: false,
                },
            ),
            ClientVersion::V222,
        );
        p
    }

    /// Every build reads the whole message, and sees the nickname and the
    /// position where it reads them.
    #[test]
    fn every_build_reads_a_join() {
        let p = alice();
        for &v in ClientVersion::all_versions() {
            let Some(m) = message(v, 7, &p.all(), &JOINED) else {
                assert!(
                    !v.profile().protocol.plo.contains(PLO_OTHERPLPROPS),
                    "{v:?}"
                );
                continue;
            };
            let other = v.profile().props.other;
            // 1.35's id is one GCHAR; seihai-core's session model reads a
            // GSHORT for every build, so it is not an oracle for 1.35.
            let id_len = if one_byte_id(v) { 1 } else { 2 };
            if one_byte_id(v) {
                assert_eq!(m[1], gchar(7), "{v:?}");
            } else {
                let mut st = SessionState::for_version(v);
                st.apply_body(&m);
                assert!(st.ignored.is_empty(), "{v:?}");
                let o = st.others.iter().find(|o| o.id == 7).expect("listed");
                if other.contains(0) {
                    assert_eq!(o.nick.as_deref(), Some("Alice"), "{v:?}");
                }
                if other.contains(X) || other.contains(X2) {
                    assert_eq!((o.x, o.y), (30.0, 30.5), "{v:?}");
                }
            }
            // The whole payload is props the build reads: nothing is cut.
            let prof = v.profile();
            let props = seihai_core::protocol::parse_player_props_in(
                &m[1 + id_len..m.len() - 1],
                bow_prop10(v),
                Some(&prof.props.other),
            );
            let len: usize = props.iter().map(|p| 1 + p.raw.len()).sum();
            assert_eq!(len, m.len() - 2 - id_len, "{v:?}");
            if one_byte_id(v) {
                let nick = props.iter().find(|p| p.id == 0).expect("nick");
                assert!(nick.raw.ends_with(b"Alice"), "{v:?}");
            }
        }
    }

    #[test]
    fn a_step_off_the_top_edge_is_negative() {
        let mut p = alice();
        let c = p.apply(&[gchar(X), gchar(66), gchar(Y), 0x1F], ClientVersion::V205);
        assert_eq!((c.x, c.y), (Some(33.0), Some(-0.5)));
        let c = p.apply(&[gchar(Y), 0x20], ClientVersion::V205);
        assert_eq!(c.y, Some(0.0));
        let c = p.apply(&[gchar(Y), 0xFF], ClientVersion::V205);
        assert_eq!(c.y, Some(111.5));
    }

    #[test]
    fn a_2_05_swing_reaches_a_2_22_and_back() {
        // The bytes a real 2.05 rev 2 and a 2.22 sent (lab capture): prop
        // 10, length 5, `sword`.
        let swing = [gchar(10), gchar(5), b's', b'w', b'o', b'r', b'd'];
        for (from, to) in [
            (ClientVersion::V205, ClientVersion::V222),
            (ClientVersion::V222, ClientVersion::V205),
        ] {
            let mut p = Props::default();
            let c = p.apply(&swing, from);
            assert_eq!(
                c.values.get(&10).map(Vec::as_slice),
                Some(&swing[1..]),
                "{from:?}"
            );
            let m = message(to, 3, &c, &[]).unwrap();
            let body = &seihai_core::protocol::split_messages(&m)[0];
            let mut want = Vec::new();
            put_id(to, 3, &mut want);
            want.extend_from_slice(&swing);
            assert_eq!(body.payload, want, "{from:?} to {to:?}");
        }
        // A build before 2.05 rev 2 keeps the bow slot: no gani to it.
        assert!(bow_prop10(ClientVersion::V139));
        let mut p = Props::default();
        let c = p.apply(&swing, ClientVersion::V222);
        assert!(message(ClientVersion::V139, 3, &c, &[]).is_none());
    }

    #[test]
    fn a_precise_move_reaches_a_classic_receiver() {
        let mut p = alice();
        // 2.31 walks with X2/Y2 only.
        let mut raw = vec![gchar(X2)];
        gshort(encode_pos_precise(12.5), &mut raw);
        raw.push(gchar(Y2));
        gshort(encode_pos_precise(40.0), &mut raw);
        let c = p.apply(&raw, ClientVersion::V231);
        assert_eq!((c.x, c.y), (Some(12.5), Some(40.0)));
        assert!(c.values.is_empty());
        for v in [
            ClientVersion::V205,
            ClientVersion::V222,
            ClientVersion::V231,
        ] {
            let m = message(v, 3, &c, &[]).unwrap();
            let mut st = SessionState::for_version(v);
            st.apply_body(&m);
            assert_eq!((st.others[0].x, st.others[0].y), (12.5, 40.0), "{v:?}");
        }
    }

    #[test]
    fn unchanged_values_and_server_props_are_not_passed_on() {
        let mut p = alice();
        let mut raw = vec![gchar(0)];
        crate::wire::gstring(b"Alice", &mut raw);
        raw.extend([gchar(JOINLEAVELVL), gchar(1), gchar(PCONNECTED)]);
        assert!(p.apply(&raw, ClientVersion::V222).is_empty());
        // The bow slot of a 1.39 sender is not kept.
        let raw = [gchar(10), gchar(3)];
        assert!(p.apply(&raw, ClientVersion::V139).is_empty());
    }

    #[test]
    fn merged_changes_keep_the_latest_value() {
        let mut a = Change::default();
        a.values.insert(0, b"one".to_vec());
        a.values.insert(1, b"x".to_vec());
        a.x = Some(1.0);
        a.y = Some(2.0);
        let mut b = Change::default();
        b.values.insert(0, b"two".to_vec());
        b.x = Some(3.0);
        a.merge(b);
        assert_eq!(a.values[&0], b"two");
        assert_eq!(a.values[&1], b"x");
        assert_eq!((a.x, a.y), (Some(3.0), Some(2.0)));
    }

    #[test]
    fn logout_removes_the_player() {
        let p = alice();
        for v in [ClientVersion::V222, ClientVersion::V231] {
            let mut st = SessionState::for_version(v);
            st.apply_body(&message(v, 9, &p.all(), &JOINED).unwrap());
            assert_eq!(st.others.len(), 1);
            st.apply_body(&message(v, 9, &Change::default(), &LOGGED_OUT).unwrap());
            assert!(st.others.is_empty(), "{v:?}");
        }
    }
}
