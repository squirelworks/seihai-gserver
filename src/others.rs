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
//! - Prop 10 is the bow slot before 2.1, except 2.05 rev 2, which uses it
//!   as the gani like 2.1 on (`ServerProfile::prop10_bow`): it is not kept
//!   from bow-slot senders and not sent to bow-slot receivers.
//! - The player id (14) is in the message header, not a prop. 1.35 reads
//!   it as one GCHAR; later builds read a GSHORT (`put_id`). Joining and
//!   leaving (50 `JOINLEAVELVL`, 51 `PCONNECTED`) are the server's to say.
//!   On logout the lab GServer sent 50 = 0, then 51, to 2.22
//!   (Seihai `lab-verification.md`), and this does the same.

use crate::profile::{ClientProfile, ServerProfile};
use crate::wire::{gchar, gshort, ids::PLO_OTHERPLPROPS, msg};
use seihai_core::protocol::{
    decode_pos_precise, encode_pos_precise, parse_player_props_with, read_gint3, read_gshort,
    ClientVersion,
};
use std::collections::BTreeMap;

/// Writes the player id that heads `PLO_OTHERPLPROPS` the way `receiver`
/// reads it. Builds up to 1.35 read one GCHAR before the props (1.35 from
/// exact-build static evidence, graal-client-repro#179; 1.21 to 1.34 from
/// the packet interface notes, one GCHAR through 1.35); 1.36 on read a
/// GSHORT. A one-byte id is capped at 223, the largest GCHAR.
pub fn put_id(receiver: impl Into<ClientProfile>, id: u16, out: &mut Vec<u8>) {
    if receiver.into().server.one_byte_id {
        out.push(gchar(id.min(223) as u8));
    } else {
        gshort(id, out);
    }
}

/// Whether prop 10 is `v`'s bow slot rather than the gani
/// (`ServerProfile::prop10_bow`, which has the 2.05 rev 2 exception).
pub fn bow_prop10(v: ClientVersion) -> bool {
    ServerProfile::of(v).prop10_bow
}

/// Builds whose `PLO_OTHERPLPROPS` id is one GCHAR
/// (`ServerProfile::one_byte_id`).
pub fn one_byte_id(v: ClientVersion) -> bool {
    ServerProfile::of(v).one_byte_id
}

pub const JOINLEAVELVL: u8 = 50;
pub const PCONNECTED: u8 = 51;
const SWORD: u8 = 8;
const SHIELD: u8 = 9;
const HEAD: u8 = 11;
const STATUS: u8 = 18;
const BODY: u8 = 35;
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
    /// The kept status (18) came from a build whose value 4 is
    /// "swimming" (`ServerProfile::status_4_swims`).
    status_swims: bool,
}

/// What changed in one update, to pass on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Change {
    values: BTreeMap<u8, Vec<u8>>,
    x: Option<f32>,
    y: Option<f32>,
    /// As `Props::status_swims`, for the status in this change.
    status_swims: bool,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.x.is_none() && self.y.is_none()
    }

    /// Add a later change: where both set a prop, the later value stays.
    pub fn merge(&mut self, later: Change) {
        if later.values.contains_key(&STATUS) {
            self.status_swims = later.status_swims;
        }
        self.values.extend(later.values);
        if later.x.is_some() {
            self.x = later.x;
        }
        if later.y.is_some() {
            self.y = later.y;
        }
    }

    /// Whether the change sets prop `id`.
    pub fn has(&self, id: u8) -> bool {
        self.values.contains_key(&id)
    }

    /// A one-`GCHAR` prop in the change (the gmap cell, 43 and 44).
    pub fn byte(&self, id: u8) -> Option<u8> {
        match self.values.get(&id)?.as_slice() {
            [v] => Some(v.wrapping_sub(0x20)),
            _ => None,
        }
    }
}

/// The head an RC has in a player's list, as on beta4 (lab run
/// 2026-11-12).
pub const RC_HEAD: &[u8] = b"head25.png";

/// An RC (or a far player) as an entry in a player's list: its nickname
/// (prop 0), head (11, [`RC_HEAD`]) and account (34) in
/// `PLO_OTHERPLPROPS`, with no level. GServer-v2
/// beta4 sends one for an RC to every player when the RC logs in, and a
/// real 2.22 queues a PM from the RC under that entry in its player list
/// (lab run 2026-11-12). `None` for a build that has no such list.
pub fn rc_entry(
    receiver: impl Into<ClientProfile>,
    id: u16,
    nick: &[u8],
    account: &[u8],
) -> Option<Vec<u8>> {
    list_entry(receiver, id, nick, account, Some(RC_HEAD))
}

/// A sender with no level as an entry in a player's list: nickname (0),
/// `head` (11) if given, account (34).
fn list_entry(
    receiver: impl Into<ClientProfile>,
    id: u16,
    nick: &[u8],
    account: &[u8],
    head: Option<&[u8]>,
) -> Option<Vec<u8>> {
    use crate::wire::gstring;
    let receiver = receiver.into();
    let prof = receiver.core;
    if !receiver.reads(PLO_OTHERPLPROPS) {
        return None;
    }
    let mut p = Vec::new();
    put_id(receiver, id, &mut p);
    if prof.props.other.contains(0) {
        p.push(gchar(0));
        gstring(nick, &mut p);
    }
    if let Some(head) = head.filter(|_| prof.props.other.contains(11)) {
        // `GCHAR` 100 + length, then the name.
        p.push(gchar(11));
        p.push(gchar(100 + head.len().min(123) as u8));
        p.extend_from_slice(&head[..head.len().min(123)]);
    }
    if prof.props.other.contains(34) {
        p.push(gchar(34));
        gstring(account, &mut p);
    }
    Some(msg(PLO_OTHERPLPROPS, &p))
}

/// A private message from `from` (a player or an RC id) to a `receiver`
/// build: `PLO_OTHERPLPROPS` with the sender's nickname (0) and account
/// (34), where the build reads them, so the client can name the sender,
/// then `PLO_PRIVATEMESSAGE` (37): `GSHORT from`, then `"","Private
/// message:",` and `text`, which is already tokenized (one quoted item or
/// more). A build whose player id is one `GCHAR` ([`one_byte_id`]) gets
/// the id that way and the text with no labels, as both 1999 servers
/// write 37 (`docs/ORIGINAL-SERVERS.md`). A build without either message
/// gets nothing of it.
pub fn private_message(
    receiver: impl Into<ClientProfile>,
    from: u16,
    nick: &[u8],
    account: &[u8],
    text: &[u8],
) -> Vec<u8> {
    use crate::wire::ids::PLO_PRIVATEMESSAGE;
    let receiver = receiver.into();
    let mut out = list_entry(receiver, from, nick, account, None).unwrap_or_default();
    if receiver.reads(PLO_PRIVATEMESSAGE) {
        let mut p = Vec::new();
        put_id(receiver, from, &mut p);
        if !receiver.server.one_byte_id {
            p.extend_from_slice(b"\"\",\"Private message:\",");
        }
        p.extend_from_slice(text);
        out.extend(msg(PLO_PRIVATEMESSAGE, &p));
    }
    out
}

/// `PLO_PRIVATEMESSAGE` (37) that tells `receiver` its message to `about`
/// went nowhere: the target's id, then `"Server message:"` and `text`,
/// as the 1999 servers bounce a message to a build that cannot read one.
/// `None` when the receiver reads no 37.
pub fn server_message(
    receiver: impl Into<ClientProfile>,
    about: u16,
    text: &str,
) -> Option<Vec<u8>> {
    use crate::wire::ids::PLO_PRIVATEMESSAGE;
    let receiver = receiver.into();
    if !receiver.reads(PLO_PRIVATEMESSAGE) {
        return None;
    }
    let mut p = Vec::new();
    put_id(receiver, about, &mut p);
    p.extend_from_slice(b"\"Server message:\",");
    p.extend(crate::wire::tokenize(&[crate::wire::latin1(text)]));
    Some(msg(PLO_PRIVATEMESSAGE, &p))
}

/// A player prop's value bytes (2.22 encoding) as `receiver` reads them
/// (issue #45, `docs/CLIENT-PROFILES.md`, image names row):
///
/// - Sword (8), shield (9), head (11) and body (35) name `.png` art. A
///   build whose art is `.gif` (`ServerProfile::image_ext`) gets a stock
///   `swordN`, `shieldN` or `headN` as its preset number, so it draws its
///   own built-in image and asks for no file, and any other `.png` name
///   with its own extension.
/// - Status (18): where value 4 means "swimming" to the receiver
///   (`ServerProfile::status_4_swims`), it is cleared, unless `swims` says
///   the value came from such a build and already means that. A player's
///   own status always loses it: the build works out its own swimming
///   from the tiles.
///
/// Anything else, and any value that does not parse, goes unchanged.
pub fn for_receiver<'a>(
    receiver: &ClientProfile,
    pid: u8,
    v: &'a [u8],
    swims: bool,
) -> std::borrow::Cow<'a, [u8]> {
    use std::borrow::Cow;
    let server = &receiver.server;
    match pid {
        STATUS if server.status_4_swims && !swims => match v {
            [s, rest @ ..] => {
                let mut w = vec![gchar(s.wrapping_sub(0x20) & !4)];
                w.extend_from_slice(rest);
                Cow::Owned(w)
            }
            _ => Cow::Borrowed(v),
        },
        SWORD | SHIELD | HEAD | BODY if server.image_ext != ".png" => {
            image_for(server.image_ext, pid, v).map_or(Cow::Borrowed(v), Cow::Owned)
        }
        _ => Cow::Borrowed(v),
    }
}

/// [`for_receiver`]'s image rule; `None` when nothing changes.
fn image_for(ext: &str, pid: u8, v: &[u8]) -> Option<Vec<u8>> {
    let byte = |i: usize| v.get(i).map(|b| b.wrapping_sub(0x20));
    // (the byte before the name, the preset limit and bias, the stock stem)
    let (lead, name) = match pid {
        SWORD | SHIELD => {
            let (limit, bias, stem) = if pid == SWORD {
                (4, 30, "sword")
            } else {
                (3, 10, "shield")
            };
            let b = byte(0)?;
            if b <= limit {
                return None; // already a preset
            }
            let len = byte(1)? as usize;
            let name = v.get(2..2 + len)?;
            let power = b - bias;
            if (1..=limit).contains(&power) && stock(name, stem) == Some(power) {
                return Some(vec![gchar(power)]);
            }
            (Some(v[0]), name)
        }
        HEAD => {
            let b = byte(0)?;
            if b < 100 {
                return None;
            }
            let name = v.get(1..1 + (b - 100) as usize)?;
            if let Some(n) = stock(name, "head").filter(|n| *n < 100) {
                return Some(vec![gchar(n)]);
            }
            (None, name)
        }
        _ => {
            let len = byte(0)? as usize;
            (None, v.get(1..1 + len)?)
        }
    };
    let stem = name
        .len()
        .checked_sub(4)
        .filter(|&i| name[i..].eq_ignore_ascii_case(b".png"))?;
    let mut renamed = name[..stem].to_vec();
    renamed.extend_from_slice(ext.as_bytes());
    let mut w = Vec::new();
    match (pid, lead) {
        (HEAD, _) => {
            let renamed = &renamed[..renamed.len().min(123)];
            w.push(gchar(100 + renamed.len() as u8));
            w.extend_from_slice(renamed);
        }
        (_, Some(lead)) => {
            w.push(lead);
            crate::wire::gstring(&renamed, &mut w);
        }
        _ => crate::wire::gstring(&renamed, &mut w),
    }
    Some(w)
}

/// `N` when `name` is `<stem>N.png` (any case), the stock art's name.
fn stock(name: &[u8], stem: &str) -> Option<u8> {
    let name = std::str::from_utf8(name).ok()?.to_ascii_lowercase();
    let n = name.strip_prefix(stem)?.strip_suffix(".png")?;
    if n.is_empty() || n.len() > 2 || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
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
        if c.values.contains_key(&STATUS) {
            c.status_swims = ServerProfile::of(sender).status_4_swims;
            self.status_swims = c.status_swims;
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

    /// The 1999 servers' prop limits (`rules = 1999`,
    /// `docs/ORIGINAL-SERVERS.md`), applied to a change just taken: max
    /// hearts over 20, hearts over the max, arrows and bombs over 99 and
    /// magic over 100 are cut down and the player is told (returned, for
    /// `PLO_PLAYERPROPS`); a sword power from 5 to 9 becomes 4, colours
    /// past 18 become 18, a sprite from 132 becomes 0 and AP past 100
    /// becomes 100, without telling the player. The cut values are what
    /// the others get and what is kept.
    pub fn limit_1999(&mut self, change: &mut Change) -> Vec<(u8, Vec<u8>)> {
        let mut told = Vec::new();
        let byte = |v: &Vec<u8>| v.first().map(|b| b.wrapping_sub(0x20));
        let max_hearts = self.values.get(&1).and_then(byte).unwrap_or(3).min(20);
        let ids: Vec<u8> = change.values.keys().copied().collect();
        for id in ids {
            let v = change.values[&id].clone();
            let mut new = v.clone();
            let mut tell = false;
            let mut cap = |limit: u8, answered: bool, new: &mut Vec<u8>| {
                if let Some(b) = new.first_mut() {
                    if b.wrapping_sub(0x20) > limit {
                        *b = gchar(limit);
                        tell |= answered;
                    }
                }
            };
            match id {
                1 => cap(20, true, &mut new),
                2 => cap(max_hearts.saturating_mul(2), true, &mut new),
                4 | 5 => cap(99, true, &mut new),
                26 => cap(100, true, &mut new),
                32 => cap(100, false, &mut new),
                8 => {
                    if let Some(b) = new.first_mut() {
                        if (5..=9).contains(&b.wrapping_sub(0x20)) {
                            *b = gchar(4);
                        }
                    }
                }
                13 => {
                    for b in new.iter_mut().take(5) {
                        if b.wrapping_sub(0x20) > 18 {
                            *b = gchar(18);
                        }
                    }
                }
                17 => {
                    if let Some(b) = new.first_mut() {
                        if b.wrapping_sub(0x20) >= 132 {
                            *b = gchar(0);
                        }
                    }
                }
                _ => {}
            }
            if new != v {
                change.values.insert(id, new.clone());
                self.values.insert(id, new.clone());
                if tell {
                    told.push((id, new));
                }
            }
        }
        told
    }

    /// The props `ids` of a change, alone.
    pub fn only(change: &Change, ids: &[u8]) -> Change {
        Change {
            values: change
                .values
                .iter()
                .filter(|(k, _)| ids.contains(k))
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
            status_swims: change.status_swims,
            ..Change::default()
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
            status_swims: self.status_swims,
        }
    }
}

/// `PLO_OTHERPLPROPS` for `receiver` about player `id`: the props of
/// `change` its build reads, then `tail` (server props such as 50 and 51,
/// value bytes included). `None` when the build does not read message 8 or
/// nothing is left to say.
pub fn message(
    receiver: impl Into<ClientProfile>,
    id: u16,
    change: &Change,
    tail: &[(u8, &[u8])],
) -> Option<Vec<u8>> {
    let receiver = receiver.into();
    if !receiver.reads(PLO_OTHERPLPROPS) {
        return None;
    }
    let other = &receiver.core.props.other;
    let mut body = Vec::new();
    let mut put = |pid: u8, v: &[u8]| {
        if other.contains(pid) && !(pid == 10 && receiver.server.prop10_bow) {
            body.push(gchar(pid));
            body.extend_from_slice(&for_receiver(&receiver, pid, v, change.status_swims));
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
pub fn own_message(to: impl Into<ClientProfile>, changed: &[(u8, Vec<u8>)]) -> Option<Vec<u8>> {
    let to = to.into();
    let own = &to.core.props.own;
    let mut body = Vec::new();
    for (id, v) in changed {
        if own.contains(*id) {
            body.push(gchar(*id));
            body.extend_from_slice(&for_receiver(&to, *id, v, false));
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

    /// The value bytes of prop `pid` in a `PLO_OTHERPLPROPS` for a build
    /// with a two-byte id (parsed with the receiver's own prop set).
    fn prop_in(m: &[u8], to: ClientVersion, pid: u8) -> Option<Vec<u8>> {
        let bow = bow_prop10(to);
        let other = to.profile().props.other;
        let body = &m[1 + if one_byte_id(to) { 1 } else { 2 }..];
        seihai_core::protocol::parse_player_props_in(body, bow, Some(&other))
            .into_iter()
            .find(|p| p.id == pid)
            .map(|p| p.raw)
    }

    /// A 1.x build reads status value 4 as "swimming" and draws a swimmer
    /// with no body (#45; the GServer-v2 fork's #27 rerun with a real
    /// 1.39). It loses the bit, unless the player is itself 1.x, whose
    /// bit 4 is its own swimming.
    #[test]
    fn status_4_is_cleared_for_1x_receivers() {
        let p = alice(); // STATUS 20 (male, weapons) from a 2.22
        let to139 = message(ClientVersion::V139, 7, &p.all(), &JOINED).unwrap();
        assert_eq!(
            prop_in(&to139, ClientVersion::V139, STATUS),
            Some(vec![gchar(16)])
        );
        let to222 = message(ClientVersion::V222, 7, &p.all(), &JOINED).unwrap();
        assert_eq!(
            prop_in(&to222, ClientVersion::V222, STATUS),
            Some(vec![gchar(20)])
        );

        let mut swimmer = Props::default();
        let c = swimmer.apply(&[gchar(STATUS), gchar(20)], ClientVersion::V139);
        let m = message(ClientVersion::V139, 7, &c, &[]).unwrap();
        assert_eq!(
            prop_in(&m, ClientVersion::V139, STATUS),
            Some(vec![gchar(20)])
        );
        let m = message(ClientVersion::V141, 7, &swimmer.all(), &[]).unwrap();
        assert_eq!(
            prop_in(&m, ClientVersion::V141, STATUS),
            Some(vec![gchar(20)])
        );
        // A later 2.x status for the same player is cleared again.
        let c = swimmer.apply(&[gchar(STATUS), gchar(21)], ClientVersion::V222);
        let m = message(ClientVersion::V139, 7, &c, &[]).unwrap();
        assert_eq!(
            prop_in(&m, ClientVersion::V139, STATUS),
            Some(vec![gchar(17)])
        );
        // The own status always loses it.
        let own = own_message(ClientVersion::V139, &[(STATUS, vec![gchar(20)])]).unwrap();
        assert_eq!(
            own,
            msg(
                crate::wire::ids::PLO_PLAYERPROPS,
                &[gchar(STATUS), gchar(16)]
            )
        );
    }

    /// Stock art goes to a `.gif` build as its preset number, other art
    /// with `.gif`; `.png` builds and 2.05 rev 2 get the names unchanged.
    #[test]
    fn player_art_for_gif_builds() {
        let gif = ClientProfile::of(ClientVersion::V139);
        let named = |lead: Option<u8>, name: &[u8]| {
            let mut v: Vec<u8> = lead.into_iter().collect();
            crate::wire::gstring(name, &mut v);
            v
        };
        let head = |name: &[u8]| {
            let mut v = vec![gchar(100 + name.len() as u8)];
            v.extend_from_slice(name);
            v
        };
        let f = |pid, v: &[u8]| for_receiver(&gif, pid, v, false).into_owned();
        // Stock sword, shield and head: the preset number alone.
        assert_eq!(
            f(SWORD, &named(Some(gchar(31)), b"sword1.png")),
            vec![gchar(1)]
        );
        assert_eq!(
            f(SHIELD, &named(Some(gchar(12)), b"Shield2.PNG")),
            vec![gchar(2)]
        );
        assert_eq!(f(HEAD, &head(b"head0.png")), vec![gchar(0)]);
        assert_eq!(f(HEAD, &head(b"head17.png")), vec![gchar(17)]);
        // A stock name with another power keeps the name, as `.gif`.
        assert_eq!(
            f(SWORD, &named(Some(gchar(33)), b"sword1.png")),
            named(Some(gchar(33)), b"sword1.gif")
        );
        // Custom art: `.gif`.
        assert_eq!(f(HEAD, &head(b"myhead.png")), head(b"myhead.gif"));
        assert_eq!(f(BODY, &named(None, b"body.png")), named(None, b"body.gif"));
        // Presets and names already `.gif` are left alone.
        assert_eq!(f(SWORD, &[gchar(3)]), vec![gchar(3)]);
        assert_eq!(f(HEAD, &[gchar(5)]), vec![gchar(5)]);
        assert_eq!(f(HEAD, &head(b"me.gif")), head(b"me.gif"));
        // A short value is passed on unchanged rather than cut.
        assert_eq!(
            f(SWORD, &[gchar(31), gchar(9), b's']),
            vec![gchar(31), gchar(9), b's']
        );
        for v in [
            ClientVersion::V205,
            ClientVersion::V222,
            ClientVersion::V603R7,
        ] {
            let p = ClientProfile::of(v);
            let h = head(b"head0.png");
            assert_eq!(&*for_receiver(&p, HEAD, &h, false), &h[..], "{v:?}");
        }
        // Through the writers: a 1.39 gets the presets from alice's props.
        let p = alice();
        let m = message(ClientVersion::V139, 7, &p.all(), &JOINED).unwrap();
        assert_eq!(prop_in(&m, ClientVersion::V139, HEAD), Some(vec![gchar(0)]));
        assert_eq!(
            prop_in(&m, ClientVersion::V139, SWORD),
            Some(vec![gchar(1)])
        );
        assert_eq!(
            prop_in(&m, ClientVersion::V139, SHIELD),
            Some(vec![gchar(1)])
        );
        let a = Account::new("bob", "", 30.0, 30.5);
        let own = own_props(
            ClientVersion::V139,
            &Player {
                id: 2,
                account: &a,
                leader: true,
            },
        );
        let parsed = seihai_core::protocol::parse_player_props_in(
            &own,
            true,
            Some(&ClientVersion::V139.profile().props.own),
        );
        let get = |id| parsed.iter().find(|p| p.id == id).map(|p| p.raw.clone());
        assert_eq!(get(HEAD), Some(vec![gchar(0)]));
        assert_eq!(get(STATUS).map(|s| (s[0] - 0x20) & 4), Some(0));
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
            // Up to 1.35 the id is one GCHAR; seihai-core's session model reads a
            // GSHORT for those builds, so it is not an oracle for them.
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

    #[test]
    fn an_rc_entry_as_beta4_sends_it() {
        // beta4 to a real 2.22 (lab run 2026-11-12): id, prop 0 `XRC`,
        // prop 11 `head25.png` as 0x8e and the name, ..., prop 34 `xrc`.
        let m = rc_entry(ClientVersion::V222, 4, b"XRC", b"xrc").unwrap();
        let body = &seihai_core::protocol::split_messages(&m)[0].payload;
        assert!(body.starts_with(b" $ #XRC+\x8ehead25.png"), "{body:?}");
        assert!(body.ends_with(b"B#xrc"));
        // A far player's PM carries no head.
        let pm = private_message(ClientVersion::V222, 4, b"Far", b"far", b"\"hi\"");
        assert!(!pm.windows(4).any(|w| w == b"head"));
    }
}
