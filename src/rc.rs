//! RC and NC logins (stage 4).
//!
//! Slice 1 (issue #52), built: RC logins. Types 1 (the RCs from 2000 to
//! 2002, plain zlib frames) and 6 (the newer RCs, gen-5 frames keyed by
//! the login's key byte) are read by `login::parse`, run through the
//! Seihai secure login like any other login when they send `SEI00001`,
//! and then checked here: the password as for a player (or by the
//! listserver with `VERIACC2`, whose account type byte is the login type),
//! then a `STAFF` key in the account file, then the RC's address against
//! the account's `IPRANGE` ([`ip_allowed`]). A refusal goes out as
//! `PLO_DISCMESSAGE` in the RC's own framing. After the login it gets
//! `PLO_SIGNATURE` and an RC chat line ([`welcome`]); then [`RcSession`]
//! reads the link: the empty `PLI_PONG` (162) every 60 s is taken
//! quietly, anything not served yet is counted for a later slice.
//!
//! Slice 2, built: the player list and RC chat. An RC is not a player and
//! never enters a level, but the world task lists it: it takes an id from
//! the players' id space, as the fork lists RCs among the players. On its
//! login the RC gets `PLO_ADDPLAYER` (55, [`addplayer`]) for every player
//! and every other RC online, and every RC gets "New RC: account" on RC
//! chat. Then, live, every RC gets `PLO_ADDPLAYER` when a player logs in,
//! changes level or nickname (the fork resends it on a props change, and
//! seihai-rc shows that as an update), `PLO_DELPLAYER` (56) when one
//! leaves, and the same for other RCs, which are listed with a level of
//! one space. An RC's own entry is not sent to it. The RC's nickname is
//! `PLI_PLAYERPROPS` (2) prop 0; without one it is `*account`, as the fork
//! shows it. A line of RC chat (`PLI_RC_CHAT` 79) goes to every RC as
//! `nickname: text` on `PLO_RC_CHAT` (74). That form is ours: no capture
//! has a line from one RC as another RC sees it yet.
//!
//! NC (type 3) is still refused as "not served yet" in its own framing.
//! Next slices: player actions (3: messages, admin messages, kick, warp),
//! accounts (4), server options and files (5), NC (6). Behaviour comes
//! from seihai-rc's `docs/PROTOCOL.md` and lab notes.
//!
//! Lessons from the real-RC capture against the gserver-v2 fork
//! (2026-11-04, lab notes `rc-capture-2026-11-04/rc-layouts.md`):
//!
//! - Folder rights (`RC_PLAYERRIGHTSGET` 62 / `RC_PLAYERRIGHTSSET` 84) come
//!   in as Graal CSV, one item per line, quoted when the line has a space
//!   (`"rw levels/*"`). Store the lines and send them back CSV-encoded the
//!   same way. The fork returned them plain, and the real RC split
//!   `rw levels/*` at the space into two lines, so saving again from the RC
//!   would store the wrong rights. Comments (63 / 86) and the ban reason
//!   (64 / 88) are CSV too.
//! - Setting a ban (`RC_PLAYERBANSET` 88) on a player who is online hung
//!   the fork at 100 % CPU. Here the ban must disconnect the player through
//!   the world task like any other leave, and a test must ban an online
//!   player and then read the ban back.

use crate::accounts::Account;
use crate::wire::{gchar, gshort, gstring, latin1, msg};
use std::collections::BTreeMap;
use std::net::IpAddr;

/// `PLO_SIGNATURE` (25): one GCHAR, 73, as to game clients.
const PLO_SIGNATURE: u8 = 25;
/// `PLO_RC_CHAT` (74): a line of RC chat, the rest of the message.
pub const PLO_RC_CHAT: u8 = 74;
/// `PLI_PLAYERPROPS` (2): an RC sends prop 0, its nickname.
const PLI_PLAYERPROPS: u8 = 2;
/// `PLI_PONG` (162): an RC sends it empty every 60 s.
const PLI_PONG: u8 = 162;
/// `PLI_RC_CHAT` (79): a line of RC chat, the rest of the message.
const PLI_RC_CHAT: u8 = 79;
/// `PLO_ADDPLAYER` (55): one entry of the RC's player list.
pub const PLO_ADDPLAYER: u8 = 55;
/// `PLO_DELPLAYER` (56): `GSHORT id`, gone from the list.
pub const PLO_DELPLAYER: u8 = 56;
/// The level an RC is listed in: one space, as the fork lists them.
pub const RC_LEVEL: &[u8] = b" ";

/// Why an RC login with a good password is refused, as the RC shows it.
pub const NOT_STAFF: &str = "This account may not use RC.";
pub const WRONG_PLACE: &str = "RC logins for this account are not allowed from your address.";

/// May `account` log in with an RC from `ip`? `Err` is the text for
/// `PLO_DISCMESSAGE`.
pub fn admit(account: &Account, ip: IpAddr) -> Result<(), &'static str> {
    if !account.staff {
        return Err(NOT_STAFF);
    }
    if !ip_allowed(&account.ip_ranges, ip) {
        return Err(WRONG_PLACE);
    }
    Ok(())
}

/// Whether `ip` matches one of `ranges`: an exact address, a pattern with
/// `*` for whole parts (`192.168.1.*`, `10.*`), or `a.b.c.d/n`. No ranges
/// at all: loopback only, so a staff account cannot be used from outside
/// until its owner says from where. An IPv4 address mapped into IPv6 is
/// matched as IPv4.
pub fn ip_allowed(ranges: &[String], ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    };
    if ranges.is_empty() {
        return ip.is_loopback();
    }
    ranges.iter().any(|r| matches_range(r.trim(), ip))
}

fn matches_range(range: &str, ip: IpAddr) -> bool {
    if range == "*" {
        return true;
    }
    if let Some((net, bits)) = range.split_once('/') {
        let (Ok(net), Ok(bits)) = (net.parse::<IpAddr>(), bits.parse::<u32>()) else {
            return false;
        };
        return match (net, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) if bits <= 32 => {
                let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
                u32::from(n) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(a)) if bits <= 128 => {
                let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
                u128::from(n) & mask == u128::from(a) & mask
            }
            _ => false,
        };
    }
    if range.contains('*') {
        let IpAddr::V4(a) = ip else {
            return false;
        };
        let parts: Vec<&str> = range.split('.').collect();
        let octets = a.octets();
        // `10.*` covers everything under 10; `192.168.*.4` each part.
        return parts.len() <= 4
            && parts.iter().enumerate().all(|(i, p)| {
                *p == "*" && (i == parts.len() - 1 || parts.len() == 4)
                    || p.parse::<u8>().is_ok_and(|v| octets[i] == v)
            });
    }
    range.parse::<IpAddr>().is_ok_and(|r| r == ip)
}

/// What an RC gets after a good login: `PLO_SIGNATURE` and a chat line, as
/// the fork sent them in the lab (seihai-rc `docs/PROTOCOL.md`). The
/// player list and "New RC: account" come from the world task after it.
/// The status list and staff guilds come with a later slice.
pub fn welcome(server_name: &str, account: &str) -> Vec<u8> {
    let mut out = msg(PLO_SIGNATURE, &[gchar(73)]);
    out.extend(msg(
        PLO_RC_CHAT,
        &latin1(&format!("Welcome to {server_name}, {account}.")),
    ));
    out
}

/// `PLO_ADDPLAYER` (55): `GSHORT id`, `GSTRING account`, then props 20
/// level (`GSTRING`), 53 list status (`GCHAR`, 0 here), 0 nickname and 82
/// community name (both `GSTRING`), in the order the fork sent them. The
/// community name is the account. `nick` is a `GSTRING` already (a
/// player's prop 0 as the world keeps it).
pub fn addplayer(id: u16, account: &[u8], level: &[u8], nick: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    gshort(id, &mut p);
    gstring(account, &mut p);
    p.push(gchar(20));
    gstring(level, &mut p);
    p.extend([gchar(53), gchar(0), gchar(0)]);
    p.extend_from_slice(nick);
    p.push(gchar(82));
    gstring(account, &mut p);
    msg(PLO_ADDPLAYER, &p)
}

/// `PLO_DELPLAYER` (56).
pub fn delplayer(id: u16) -> Vec<u8> {
    let mut p = Vec::new();
    gshort(id, &mut p);
    msg(PLO_DELPLAYER, &p)
}

/// An RC's name in its list entry and on RC chat: its nickname, or
/// `*account` without one. `GSTRING`.
pub fn rc_nick(account: &str, nick: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    match nick.filter(|n| !n.is_empty()) {
        Some(n) => gstring(n, &mut out),
        None => gstring(&latin1(&format!("*{account}")), &mut out),
    }
    out
}

/// A line of RC chat from `nick` (a `GSTRING`), as every RC gets it.
pub fn chat_line(nick: &[u8], text: &[u8]) -> Vec<u8> {
    let name = nick.get(1..).unwrap_or_default();
    let mut line = name.to_vec();
    line.extend_from_slice(b": ");
    line.extend_from_slice(text);
    msg(PLO_RC_CHAT, &line)
}

/// What an RC sent that the world must hear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RcEvent {
    /// A new nickname, Latin-1.
    Nick(Vec<u8>),
    /// A line of RC chat, Latin-1.
    Chat(Vec<u8>),
}

/// One RC link after its login.
#[derive(Debug, Default)]
pub struct RcSession {
    /// Messages a later slice will answer, by id, and how many.
    pub unhandled: BTreeMap<u8, u32>,
}

impl RcSession {
    /// Take the RC's messages (each with its `\n`); returns what the world
    /// must hear, in order.
    pub fn handle(&mut self, msgs: &[Vec<u8>]) -> Vec<RcEvent> {
        let mut out = Vec::new();
        for m in msgs {
            let m = m.strip_suffix(b"\n").unwrap_or(m);
            let Some((&id, rest)) = m.split_first() else {
                continue;
            };
            match id.wrapping_sub(0x20) {
                PLI_PONG => {}
                PLI_PLAYERPROPS => {
                    // Prop 0, GSTRING nickname.
                    if let [0x20, n, text @ ..] = rest {
                        let n = usize::from(n.wrapping_sub(0x20)).min(text.len());
                        out.push(RcEvent::Nick(text[..n].to_vec()));
                    }
                }
                PLI_RC_CHAT if !rest.is_empty() => out.push(RcEvent::Chat(rest.to_vec())),
                other => *self.unhandled.entry(other).or_default() += 1,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::split_messages;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn address_ranges() {
        let none: Vec<String> = Vec::new();
        assert!(ip_allowed(&none, ip("127.0.0.1")));
        assert!(ip_allowed(&none, ip("::1")));
        assert!(!ip_allowed(&none, ip("192.168.1.5")));
        let r: Vec<String> = ["192.168.1.*", "10.0.0.0/8", "203.0.113.7", "fd00::/8"]
            .map(String::from)
            .to_vec();
        assert!(ip_allowed(&r, ip("192.168.1.200")));
        assert!(!ip_allowed(&r, ip("192.168.2.1")));
        assert!(ip_allowed(&r, ip("10.20.30.40")));
        assert!(!ip_allowed(&r, ip("11.0.0.1")));
        assert!(ip_allowed(&r, ip("203.0.113.7")));
        assert!(ip_allowed(&r, ip("::ffff:203.0.113.7")));
        assert!(ip_allowed(&r, ip("fd12::1")));
        assert!(
            !ip_allowed(&r, ip("127.0.0.1")),
            "a list replaces the loopback default"
        );
        assert!(ip_allowed(&["10.*".into()], ip("10.9.8.7")));
        assert!(ip_allowed(&["192.*.*.4".into()], ip("192.1.2.4")));
        assert!(!ip_allowed(&["192.*.*.4".into()], ip("192.1.2.5")));
        assert!(ip_allowed(&["*".into()], ip("198.51.100.1")));
        assert!(!ip_allowed(
            &["junk".into(), "1.2.3.4/40".into()],
            ip("1.2.3.4")
        ));
    }

    #[test]
    fn staff_and_place() {
        let mut a = Account::new("admin", "start.nw", 30.0, 30.0);
        assert_eq!(admit(&a, ip("127.0.0.1")), Err(NOT_STAFF));
        a.staff = true;
        assert_eq!(admit(&a, ip("127.0.0.1")), Ok(()));
        assert_eq!(admit(&a, ip("192.168.1.5")), Err(WRONG_PLACE));
        a.ip_ranges = vec!["192.168.1.*".into()];
        assert_eq!(admit(&a, ip("192.168.1.5")), Ok(()));
        // The file keeps both.
        let back = Account::parse("admin", &a.to_text());
        assert!(back.staff);
        assert_eq!(back.ip_ranges, a.ip_ranges);
        assert!(!Account::parse("x", "STAFF 0\n").staff);
        assert!(Account::parse("x", "STAFF\n").staff);
    }

    #[test]
    fn the_welcome_and_what_an_rc_sends() {
        let w = welcome("Lab", "admin");
        let m = split_messages(&w);
        assert_eq!(m[0].id, PLO_SIGNATURE);
        assert_eq!(m[0].payload, [gchar(73)]);
        assert_eq!(m[1].id, PLO_RC_CHAT);
        assert_eq!(m.len(), 2);

        let mut s = RcSession::default();
        let nick = msg(PLI_PLAYERPROPS, &[0x20, 0x20 + 4, b'B', b'o', b's', b's']);
        let out = s.handle(&[
            msg(PLI_PONG, b""),
            nick,
            msg(PLI_RC_CHAT, b"hello"),
            msg(PLI_RC_CHAT, b""),
            msg(63, b"to all"),
        ]);
        assert_eq!(
            out,
            [
                RcEvent::Nick(b"Boss".to_vec()),
                RcEvent::Chat(b"hello".to_vec())
            ]
        );
        assert_eq!(s.unhandled.get(&PLI_RC_CHAT), Some(&1), "the empty line");
        assert_eq!(s.unhandled.get(&63), Some(&1));
        assert!(!s.unhandled.contains_key(&PLI_PONG));
    }

    #[test]
    fn list_entries_and_chat() {
        let nick = rc_nick("admin", None);
        assert_eq!(nick, b"&*admin");
        assert_eq!(rc_nick("admin", Some(b"")), nick);
        let e = addplayer(7, b"admin", RC_LEVEL, &nick);
        let m = &split_messages(&e)[0];
        assert_eq!(m.id, PLO_ADDPLAYER);
        let mut want = vec![0x20, 0x27, 0x25];
        want.extend(b"admin");
        want.extend([0x20 + 20, 0x21, b' ', 0x20 + 53, 0x20, 0x20]);
        want.extend(b"&*admin");
        want.extend([0x20 + 82, 0x25]);
        want.extend(b"admin");
        assert_eq!(m.payload, want);
        assert_eq!(split_messages(&delplayer(7))[0].payload, [0x20, 0x27]);
        let c = chat_line(&rc_nick("admin", Some(b"Boss")), b"hi all");
        assert_eq!(split_messages(&c)[0].payload, b"Boss: hi all");
    }
}
