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
//! `PLO_DISCMESSAGE` in the RC's own framing. An RC is not a player: it
//! never joins the world. After the login it gets `PLO_SIGNATURE` and an
//! RC chat line ([`welcome`]); then [`RcSession`] keeps the link: the
//! empty `PLI_PONG` (162) every 60 s and the RC's nickname are taken
//! quietly, anything else is counted for a later slice.
//!
//! NC (type 3) is still refused as "not served yet" in its own framing.
//! Next slices: the player list and RC chat (2), player actions (3),
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
use crate::wire::{gchar, latin1, msg};
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
/// the fork sent them in the lab (seihai-rc `docs/PROTOCOL.md`). The rest
/// of that burst (the player list, status list, staff guilds) comes with
/// slice 2.
pub fn welcome(server_name: &str, account: &str) -> Vec<u8> {
    let mut out = msg(PLO_SIGNATURE, &[gchar(73)]);
    out.extend(msg(
        PLO_RC_CHAT,
        &latin1(&format!("Welcome to {server_name}, {account}.")),
    ));
    out.extend(msg(PLO_RC_CHAT, &latin1(&format!("New RC: {account}"))));
    out
}

/// One RC link after its login.
#[derive(Debug, Default)]
pub struct RcSession {
    /// The nickname the RC sent (prop 0), if any.
    pub nick: Option<String>,
    /// Messages a later slice will answer, by id, and how many.
    pub unhandled: BTreeMap<u8, u32>,
}

impl RcSession {
    /// Take the RC's messages (each with its `\n`); returns the bodies to
    /// send back. Nothing is answered in slice 1.
    pub fn handle(&mut self, msgs: &[Vec<u8>]) -> Vec<Vec<u8>> {
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
                        self.nick = Some(text[..n].iter().map(|&c| c as char).collect());
                    }
                }
                other => *self.unhandled.entry(other).or_default() += 1,
            }
        }
        Vec::new()
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
        assert!(m[1..].iter().all(|m| m.id == PLO_RC_CHAT));
        assert_eq!(m[2].payload, b"New RC: admin");

        let mut s = RcSession::default();
        let nick = msg(PLI_PLAYERPROPS, &[0x20, 0x20 + 4, b'B', b'o', b's', b's']);
        let out = s.handle(&[msg(PLI_PONG, b""), nick, msg(79, b"hello")]);
        assert!(out.is_empty());
        assert_eq!(s.nick.as_deref(), Some("Boss"));
        assert_eq!(s.unhandled.get(&79), Some(&1));
        assert!(!s.unhandled.contains_key(&PLI_PONG));
    }
}
