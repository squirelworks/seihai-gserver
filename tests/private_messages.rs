//! Private messages between players (`PLI_PRIVATEMESSAGE` 28 to
//! `PLO_PRIVATEMESSAGE` 37), with Seihai's own client code on 127.0.0.1.
//! A 2.22 writes to a 2.31; a 1.32, whose ids are one byte, writes to the
//! 2.22; a message to a 1.21, which reads no 37, comes back to the sender
//! as a server message; and a text over 1024 bytes is refused with an
//! admin message (35), as GServer 1.3 does (`docs/ORIGINAL-SERVERS.md`).

mod common;

use common::raw::{RawClient, Seen};
use common::*;
use seihai_core::protocol::ClientVersion;

const OTHERPLPROPS: u8 = 8;
const PM_IN: u8 = 28;
const ADMINMESSAGE: u8 = 35;
const PM_OUT: u8 = 37;

fn in_level(m: &Seen) -> bool {
    m.iter().any(|(id, _)| *id == 6)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The id `c` knows `account` by.
fn id_of(c: &mut RawClient, account: &str, one_byte: bool) -> Vec<u8> {
    let find = |seen: &Seen| {
        seen.iter()
            .find(|(id, p)| *id == OTHERPLPROPS && contains(p, account.as_bytes()))
            .map(|(_, p)| p[..if one_byte { 1 } else { 2 }].to_vec())
    };
    if find(&c.seen).is_none() {
        c.until(account, |m| find(m).is_some());
    }
    find(&c.seen).unwrap()
}

#[test]
fn players_write_to_each_other_in_each_build_s_form() {
    let w = world("pms", "*\n");
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", in_level);
    let mut bob = RawClient::join(&addr, ClientVersion::V231, "bob", in_level);
    let mut cal = RawClient::join(&addr, ClientVersion::V132, "cal", in_level);
    let mut dan = RawClient::join(&addr, ClientVersion::V121, "dan", in_level);

    // 2.22 to 2.31: GSHORT count and ids.
    let bob_id = id_of(&mut amy, "bob", false);
    let mut m = vec![0x20, 0x21];
    m.extend(&bob_id);
    m.extend(b"\"hi bob\"");
    amy.send(PM_IN, &m);
    let got = bob.until("amy's message", |s| {
        s.iter()
            .any(|(id, p)| *id == PM_OUT && contains(p, b"hi bob"))
    });
    let (_, p) = got.iter().find(|(id, _)| *id == PM_OUT).unwrap();
    assert!(contains(p, b"\"Private message:\""), "{p:?}");

    // 1.32 to 2.22: one GCHAR count and id.
    let amy_id = id_of(&mut cal, "amy", true);
    let mut m = vec![0x21];
    m.extend(&amy_id);
    m.extend(b"\"hi amy\"");
    cal.send(PM_IN, &m);
    amy.until("cal's message", |s| {
        s.iter()
            .any(|(id, p)| *id == PM_OUT && contains(p, b"hi amy"))
    });

    // 2.22 to 1.21, which reads no 37: Amy is told.
    let dan_id = id_of(&mut amy, "dan", false);
    let mut m = vec![0x20, 0x21];
    m.extend(&dan_id);
    m.extend(b"\"hi dan\"");
    amy.send(PM_IN, &m);
    let back = amy.until("the bounce", |s| {
        s.iter()
            .any(|(id, p)| *id == PM_OUT && contains(p, b"older Graal version"))
    });
    let (_, p) = back.iter().find(|(id, _)| *id == PM_OUT).unwrap();
    assert!(p.starts_with(&dan_id), "{p:?}");
    assert!(contains(p, b"\"Server message:\""), "{p:?}");
    assert!(!dan.settle().iter().any(|(id, _)| *id == PM_OUT));

    // Too long: an admin message, and Bob gets nothing.
    let mut m = vec![0x20, 0x21];
    m.extend(&bob_id);
    m.push(b'"');
    m.extend(vec![b'x'; 1100]);
    m.push(b'"');
    amy.send(PM_IN, &m);
    amy.until("the refusal", |s| {
        s.iter()
            .any(|(id, p)| *id == ADMINMESSAGE && contains(p, b"too long"))
    });
    assert!(!bob.settle().iter().any(|(id, _)| *id == PM_OUT));
}
