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
//! - Prop 10 is the bow slot before 2.1 (`props.bow_prop10`): it is not kept
//!   from those senders and not sent to those receivers.
//! - The player id (14) is in the message header, not a prop. Joining and
//!   leaving (50 `JOINLEAVELVL`, 51 `PCONNECTED`) are the server's to say.
//!   On logout the lab GServer sent 50 = 0, then 51, to 2.22
//!   (Seihai `lab-verification.md`), and this does the same.

use crate::wire::{gchar, gshort, ids::PLO_OTHERPLPROPS, msg};
use seihai_core::protocol::{
    decode_pos_precise, encode_pos_precise, parse_player_props_with, read_gshort, ClientVersion,
};
use std::collections::BTreeMap;

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
}

impl Props {
    /// Apply a prop stream as `sender`'s build writes it; returns what
    /// changed.
    pub fn apply(&mut self, raw: &[u8], sender: ClientVersion) -> Change {
        let bow = sender.profile().props.bow_prop10;
        let mut c = Change::default();
        for p in parse_player_props_with(raw, bow) {
            match p.id {
                X => c.x = p.gchar().map(|v| v as f32 / 2.0),
                Y => c.y = p.gchar().map(|v| v as f32 / 2.0),
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
        if other.contains(pid) && !(pid == 10 && prof.props.bow_prop10) {
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
    gshort(id, &mut p);
    p.extend(body);
    Some(msg(PLO_OTHERPLPROPS, &p))
}

/// The tail for a player who is now in the receiver's level.
pub const JOINED: [(u8, &[u8]); 1] = [(JOINLEAVELVL, &[0x21])];
/// The tail for a player who went to another level.
pub const LEFT_LEVEL: [(u8, &[u8]); 1] = [(JOINLEAVELVL, &[0x20])];
/// The tail for a player who logged out.
pub const LOGGED_OUT: [(u8, &[u8]); 2] = [(JOINLEAVELVL, &[0x20]), (PCONNECTED, &[])];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::Account;
    use crate::burst::{own_props, Player};
    use seihai_core::protocol::SessionState;

    fn alice() -> Props {
        let mut a = Account::new("alice", "", 30.0, 30.5);
        a.nick = "Alice".into();
        let mut p = Props::default();
        p.apply(
            &own_props(ClientVersion::V222, &Player { id: 1, account: &a }),
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
            let mut st = SessionState::for_version(v);
            st.apply_body(&m);
            assert!(st.ignored.is_empty(), "{v:?}");
            let o = st.others.iter().find(|o| o.id == 7).expect("listed");
            let other = v.profile().props.other;
            if other.contains(0) {
                assert_eq!(o.nick.as_deref(), Some("Alice"), "{v:?}");
            }
            if other.contains(X) || other.contains(X2) {
                assert_eq!((o.x, o.y), (30.0, 30.5), "{v:?}");
            }
            // The whole payload is props the build reads: nothing is cut.
            let prof = v.profile();
            let props = seihai_core::protocol::parse_player_props_in(
                &m[3..m.len() - 1],
                prof.props.bow_prop10,
                Some(&prof.props.other),
            );
            let len: usize = props.iter().map(|p| 1 + p.raw.len()).sum();
            assert_eq!(len, m.len() - 4, "{v:?}");
        }
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
