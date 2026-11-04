//! Login types this server knows but does not serve yet (RC, NC) are
//! refused in their own framing, so the client shows the reason
//! (issue #16). The login lines are the ones `seihai-rc` sends; the
//! replies are read the way it reads them. Only `127.0.0.1` is used.

mod common;

use common::*;
use seihai_core::protocol::{read_frame, read_zlib_frame, write_zlib_frame, Gen5};
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

/// PLO_DISCMESSAGE (16), as it goes on the wire.
const DISC: u8 = 16 + 0x20;

fn connect(addr: &str, line: &[u8]) -> TcpStream {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(&write_zlib_frame(line).unwrap()).unwrap();
    s
}

fn disc_text(body: &[u8]) -> String {
    assert_eq!(body.first(), Some(&DISC), "not a DISCMESSAGE: {body:?}");
    String::from_utf8_lossy(&body[1..])
        .trim_end_matches('\n')
        .to_string()
}

#[test]
fn rc_and_nc_logins_get_the_reason_in_their_own_frames() {
    let w = world("unserved", "*\n");
    let (addr, _server) = start(&w);

    // Type 6, key 0x2A ('J'): the reply is a gen-5 frame keyed by it.
    let mut s = connect(&addr, b"&JGSERV025%admin\"pw\n");
    let mut cipher = Gen5::new(0x2A);
    let body = read_frame(&mut s, &mut cipher).unwrap();
    assert_eq!(disc_text(&body), "This server does not take RC logins yet.");

    // Type 1 (old RC) and type 3 (NC) read plain zlib frames.
    for (line, name) in [
        (&b"!GSERV025%admin\"pw\n"[..], "RC"),
        (&b"#NCL11012%admin\"pw\n"[..], "NC"),
    ] {
        let mut s = connect(&addr, line);
        let body = read_zlib_frame(&mut s).unwrap();
        assert_eq!(
            disc_text(&body),
            format!("This server does not take {name} logins yet.")
        );
    }

    // A type nobody knows still gets the plain zlib guess.
    let mut s = connect(&addr, b"(JZZZ12345%admin\"pw\n");
    let body = read_zlib_frame(&mut s).unwrap();
    assert_eq!(disc_text(&body), "This server cannot read that login.");

    let _ = std::fs::remove_dir_all(&w);
}
