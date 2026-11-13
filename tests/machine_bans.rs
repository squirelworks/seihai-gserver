//! Bans by machine and by address (issue #54), on 127.0.0.1. A 5.00 or
//! later client names its machine on the login line; the login lines here
//! are 5.24r4's (`win,` and the three machine fields, as `seihai-core`
//! sends them, with ids filled in), written by hand so the ids can be set.
//! Staff accounts are not kept out by a machine or address ban.

mod common;

use common::*;
use seihai_core::net::walk_message_for;
use seihai_core::protocol::{read_frame, write_zlib_frame, ClientVersion, Gen5, Gen5Comp};
use seihai_gserver::accounts::{Account, Store};
use seihai_gserver::bans::Bans;
use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

/// PLO_DISCMESSAGE (16), as it goes on the wire.
const DISC: u8 = 16 + 0x20;
const KEY: u8 = 0x2A;

fn gstring(s: &str, out: &mut Vec<u8>) {
    out.push(s.len() as u8 + 0x20);
    out.extend_from_slice(s.as_bytes());
}

/// A 5.24r4 login from a machine with these ids.
fn login(addr: &str, account: &str, machine: [&str; 3]) -> (TcpStream, Gen5) {
    // The key byte goes as a GCHAR.
    let mut line = vec![5 + 0x20, KEY + 0x20];
    line.extend_from_slice(ClientVersion::V524R4.token().as_bytes());
    gstring(account, &mut line);
    gstring(PASSWORD, &mut line);
    let [a, b, c] = machine;
    line.extend_from_slice(format!("win,\"{a}\",\"{b}\",\"{c}\",\"6.2 9200 \"\n").as_bytes());
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(&write_zlib_frame(&line).unwrap()).unwrap();
    (s, cipher())
}

/// 5.24r4's cipher (both ways), keyed as the server keys it.
fn cipher() -> Gen5 {
    if ClientVersion::V524R4.profile().protocol.gen5_round_down {
        Gen5::new(KEY).round_down()
    } else {
        Gen5::new(KEY)
    }
}

/// The first frame's messages: the refusal's text, or `None` when the
/// login went in.
fn refusal(s: &mut TcpStream, cipher: &mut Gen5) -> Option<String> {
    let body = read_frame(s, cipher).unwrap();
    (body.first() == Some(&DISC)).then(|| {
        String::from_utf8_lossy(&body[1..])
            .trim_end_matches('\n')
            .to_string()
    })
}

fn refused(addr: &str, account: &str, machine: [&str; 3]) -> Option<String> {
    let (mut s, mut c) = login(addr, account, machine);
    refusal(&mut s, &mut c)
}

/// A frame from the client: gen 5, not compressed.
fn send(s: &mut TcpStream, cipher: &mut Gen5, msgs: &[Vec<u8>]) {
    let mut b = msgs.concat();
    cipher.xor_body(&mut b, Gen5Comp::None);
    let mut f = ((1 + b.len()) as u16).to_be_bytes().to_vec();
    f.push(Gen5Comp::None as u8);
    f.extend(b);
    s.write_all(&f).unwrap();
}

fn staff(w: &Path, name: &str) {
    let mut a = Account::new(name, "onlinestartlocal.nw", 30.0, 30.0);
    a.set_password(PASSWORD.as_bytes());
    a.staff = true;
    Store::new(w.join("accounts")).save(&a).unwrap();
}

const CHEATER_PC: [&str; 3] = ["0123456789abcdef0123456789abcdef", "", "feedfacefeedface"];
const OTHER_PC: [&str; 3] = ["ffffffffffffffffffffffffffffffff", "", ""];

/// `ban after N in S` bans the machine with the account: a new account on
/// that machine is kept out, one on another machine and a staff account on
/// the same one are not, and lifting the account's ban lifts the machine.
#[test]
fn a_policy_ban_takes_the_machine_too() {
    let w = world("machine-ban", "*\n");
    staff(&w, "boss");
    let cfg = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("anticheat = on\nanticheat.teleport = correct, ban after 1 in 60\n");
    std::fs::write(&cfg, text).unwrap();
    let (addr, _server) = start(&w);

    // Dee jumps across the level on her first step and is banned for it.
    let (mut s, mut inc) = login(&addr, "dee", CHEATER_PC);
    assert_eq!(refusal(&mut s, &mut inc), None);
    let mut out = cipher();
    send(
        &mut s,
        &mut out,
        &[walk_message_for(ClientVersion::V524R4, 2.0, 2.0, 2)],
    );
    let closed = (0..100).any(|_| match read_frame(&mut s, &mut inc) {
        Ok(b) => b.first() == Some(&DISC),
        Err(_) => true,
    });
    assert!(closed, "not kicked");
    std::thread::sleep(Duration::from_millis(300));

    let bans = Bans::new(&w);
    let mut keys: Vec<String> = bans
        .read()
        .unwrap()
        .into_iter()
        .map(|b| b.account)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "dee",
            "machine:0123456789abcdef0123456789abcdef",
            "machine:feedfacefeedface",
        ]
    );
    let m = refused(&addr, "deealt", ["", "", "FEEDFACEFEEDFACE"]).unwrap();
    assert!(m.contains("anti-cheat: teleport"), "{m}");
    assert_eq!(refused(&addr, "neighbour", OTHER_PC), None);
    assert_eq!(refused(&addr, "boss", CHEATER_PC), None, "staff");
    // Seihai's own client sends empty machine fields: nothing to match.
    assert_eq!(refused(&addr, "fresh", ["", "", ""]), None);

    assert!(bans.remove("Dee").unwrap());
    assert!(bans.read().unwrap().is_empty());
    assert_eq!(refused(&addr, "deealt", CHEATER_PC), None);
    let _ = std::fs::remove_dir_all(&w);
}

/// Bans an admin writes by hand: a machine, and the address. Staff get in
/// through both; an account ban still holds for staff.
#[test]
fn machine_and_address_bans_from_the_file() {
    let w = world("ip-ban", "*\n");
    staff(&w, "boss");
    staff(&w, "rogue");
    std::fs::write(
        w.join("config/bans.txt"),
        "machine:FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF\t-\tboss\tshared cheats\n\
         rogue\t-\tboss\tstaff gone bad\n",
    )
    .unwrap();
    let (addr, _server) = start(&w);

    assert_eq!(
        refused(&addr, "amy", OTHER_PC).as_deref(),
        Some("You are banned from this server: shared cheats.")
    );
    assert_eq!(refused(&addr, "amy", CHEATER_PC), None);
    assert_eq!(refused(&addr, "boss", OTHER_PC), None);
    assert!(refused(&addr, "rogue", CHEATER_PC).is_some());

    // The whole of 127.0.0.1, as an IPv4-mapped address would also be.
    let bans = Bans::new(&w);
    bans.add(seihai_gserver::bans::Ban {
        account: seihai_gserver::bans::parse_key("ip:::ffff:127.0.0.1").unwrap(),
        until: None,
        admin: "boss".into(),
        reason: "lab".into(),
        with: String::new(),
    })
    .unwrap();
    assert_eq!(
        refused(&addr, "amy", CHEATER_PC).as_deref(),
        Some("You are banned from this server: lab.")
    );
    // A build with no machine fields is kept out by the address too.
    let m = common::refused(&addr, ClientVersion::V222, "bea", PASSWORD);
    assert_eq!(m.as_deref(), Some("You are banned from this server: lab."));
    assert_eq!(refused(&addr, "boss", CHEATER_PC), None, "staff");
    assert!(bans.remove("ip:127.0.0.1").unwrap());
    assert_eq!(refused(&addr, "amy", CHEATER_PC), None);
    let _ = std::fs::remove_dir_all(&w);
}
