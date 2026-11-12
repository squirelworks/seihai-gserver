//! RC logins (stage 4 slice 1, issue #52) on loopback, with the login
//! lines and framing `seihai-rc` uses (`docs/PROTOCOL.md` there): type 6
//! with a key byte and gen-5 frames, type 1 with plain zlib frames, and
//! type 6 through the Seihai secure login. Only `127.0.0.1` is used.

mod common;

use common::*;
use seihai_core::protocol::seclogin::ClientHandshake;
use seihai_core::protocol::{
    read_frame, read_zlib_frame, split_messages, write_zlib_frame, Gen5, Gen5Comp,
};
use seihai_gserver::accounts::{Account, Store};
use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

/// PLO_DISCMESSAGE (16) and the welcome's ids.
const DISC: u8 = 16;
const SIGNATURE: u8 = 25;
const RC_CHAT: u8 = 74;

fn account(w: &Path, name: &str, staff: bool, ranges: &[&str]) {
    let mut a = Account::new(name, "onlinestartlocal.nw", 30.0, 30.0);
    a.set_password(PASSWORD.as_bytes());
    a.staff = staff;
    a.ip_ranges = ranges.iter().map(|r| r.to_string()).collect();
    Store::new(w.join("accounts")).save(&a).unwrap();
}

fn connect(addr: &str, line: &[u8]) -> TcpStream {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(&write_zlib_frame(line).unwrap()).unwrap();
    s
}

/// A login line as seihai-rc writes it.
fn line(client_type: u8, key: Option<u8>, token: &str, account: &str, pw: &str) -> Vec<u8> {
    let mut b = vec![client_type + 0x20];
    if let Some(k) = key {
        b.push(k + 0x20);
    }
    b.extend(token.as_bytes());
    for s in [account, pw] {
        b.push(s.len() as u8 + 0x20);
        b.extend(s.as_bytes());
    }
    b.push(b'\n');
    b
}

fn ids(body: &[u8]) -> Vec<(u8, Vec<u8>)> {
    split_messages(body)
        .into_iter()
        .map(|m| (m.id, m.payload))
        .collect()
}

fn disc(body: &[u8]) -> String {
    let m = ids(body);
    assert_eq!(m[0].0, DISC, "not a DISCMESSAGE: {m:?}");
    String::from_utf8_lossy(&m[0].1).into_owned()
}

/// The welcome frame, then the frames from the world until "New RC".
fn welcomed(mut next: impl FnMut() -> Vec<u8>, account: &str) {
    let m = ids(&next());
    assert_eq!(m[0], (SIGNATURE, vec![73 + 0x20]), "{m:?}");
    assert!(m.iter().any(|(id, _)| *id == RC_CHAT), "{m:?}");
    let new_rc = format!("New RC: {account}");
    for _ in 0..10 {
        let m = ids(&next());
        if m.iter()
            .any(|(id, p)| *id == RC_CHAT && p == new_rc.as_bytes())
        {
            return;
        }
    }
    panic!("no {new_rc:?}");
}

/// A gen-5 frame from the RC: no compression, XORed.
fn gen5_out(cipher: &mut Gen5, body: &[u8]) -> Vec<u8> {
    let mut b = body.to_vec();
    cipher.xor_body(&mut b, Gen5Comp::None);
    let mut f = ((1 + b.len()) as u16).to_be_bytes().to_vec();
    f.push(Gen5Comp::None as u8);
    f.extend(b);
    f
}

#[test]
fn staff_accounts_log_in_with_an_rc() {
    let w = world("rclogin", "GNW03014\n");
    account(&w, "boss", true, &[]);
    account(&w, "player", false, &[]);
    account(&w, "faraway", true, &["10.0.0.0/8"]);
    account(&w, "exiled", true, &[]);
    std::fs::write(w.join("config/bans.txt"), "exiled\t-\tboss\tgone\n").unwrap();
    let (addr, _server) = start(&w);

    // Type 6, key 0x2A: gen-5 both ways. The allow-list names only a game
    // build; an RC token is not asked about.
    let mut s = connect(&addr, &line(6, Some(0x2A), "GSERV025", "boss", PASSWORD));
    let mut inc = Gen5::new(0x2A);
    welcomed(|| read_frame(&mut s, &mut inc).unwrap(), "boss");
    // The keepalive and a nickname are taken; the link stays up.
    let mut out = Gen5::new(0x2A);
    let mut body = vec![162 + 0x20, b'\n', 2 + 0x20, 0x20, 4 + 0x20];
    body.extend(b"Boss\n");
    s.write_all(&gen5_out(&mut out, &body)).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut probe = [0u8; 1];
    let still_open = matches!(
        std::io::Read::read(&mut s, &mut probe),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut
    );
    assert!(still_open, "the RC link closed");

    // Type 1: plain zlib frames.
    let mut s = connect(&addr, &line(1, None, "GSERV023", "boss", PASSWORD));
    welcomed(|| read_zlib_frame(&mut s).unwrap(), "boss");

    // Refusals, each in the RC's own framing.
    let mut s = connect(&addr, &line(6, Some(5), "GSERV025", "player", PASSWORD));
    assert_eq!(
        disc(&read_frame(&mut s, &mut Gen5::new(5)).unwrap()),
        "This account may not use RC."
    );
    let mut s = connect(&addr, &line(1, None, "GSERV023", "boss", "wrong"));
    assert_eq!(
        disc(&read_zlib_frame(&mut s).unwrap()),
        "Account name or password is wrong."
    );
    let mut s = connect(&addr, &line(1, None, "GSERV023", "faraway", PASSWORD));
    assert_eq!(
        disc(&read_zlib_frame(&mut s).unwrap()),
        "RC logins for this account are not allowed from your address."
    );
    // A banned staff account gets the ban text.
    let mut s = connect(&addr, &line(1, None, "GSERV023", "exiled", PASSWORD));
    assert_eq!(
        disc(&read_zlib_frame(&mut s).unwrap()),
        "You are banned from this server: gone."
    );
    // No account file is made for an RC, even with create_accounts on.
    let mut s = connect(&addr, &line(1, None, "GSERV023", "newcomer", PASSWORD));
    assert_eq!(
        disc(&read_zlib_frame(&mut s).unwrap()),
        "Account name or password is wrong."
    );
    assert!(!w.join("accounts/newcomer.txt").exists());
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn an_rc_logs_in_through_the_secure_login() {
    let w = world("rcsecure", "*\n");
    account(&w, "boss", true, &[]);
    let (addr, server) = start(&w);
    let key = server.identity.as_ref().unwrap().public_key();

    // C1: type 6 and its key, SEI00001, the RC's token, the client key.
    let hs = ClientHandshake::new(&[6 + 0x20, 0x2A + 0x20], "GSERV025", b"").unwrap();
    let mut s = connect(&addr, hs.hello());
    let s1 = read_zlib_frame(&mut s).unwrap();
    let s1 = s1.strip_suffix(b"\n").unwrap_or(&s1);
    let done = hs.finish(s1).unwrap();
    assert_eq!(done.server_key, key);
    // C2: the account and password, sealed.
    let mut tail = vec![4 + 0x20];
    tail.extend(b"boss");
    tail.push(PASSWORD.len() as u8 + 0x20);
    tail.extend(PASSWORD.as_bytes());
    let mut c2 = done.sealer.seal(&tail).unwrap();
    c2.push(b'\n');
    s.write_all(&write_zlib_frame(&c2).unwrap()).unwrap();
    let mut inc = Gen5::new(0x2A);
    welcomed(|| read_frame(&mut s, &mut inc).unwrap(), "boss");
    let _ = std::fs::remove_dir_all(&w);
}
