//! Classes: a 4.20r8 that asks with `PLI_UPDATECLASS` (161) gets the
//! bytecode in `PLO_UNKNOWN133` behind `PLO_RAWDATA`. A matching CRC is
//! not answered. A name the world does not have gets an empty 133. A
//! 5.24r4 (which dropped 133) gets nothing.

mod common;

use common::raw::RawClient;
use common::*;
use seihai_core::protocol::ClientVersion;
use std::time::{Duration, Instant};

const SOURCE: &str = "if (created) {\n  setstring client.classok,1;\n}\n";

fn raw_at_133(b: &[u8]) -> Option<usize> {
    // PLO_RAWDATA (`0x84`), its GINT3 count and `\n`, then 133 (`0xa5`).
    (0..b.len().saturating_sub(5)).find(|&i| b[i] == 0x84 && b[i + 4] == b'\n' && b[i + 5] == 0xa5)
}

fn gint5(v: u64) -> [u8; 5] {
    let mut digits = [0u8; 5];
    let mut n = v;
    for d in digits.iter_mut().rev() {
        *d = (n & 0x7F) as u8 + 32;
        n >>= 7;
    }
    digits
}

#[test]
fn a_gs2_build_gets_class_bytecode_for_updateclass() {
    let w = world("class133", "*\n");
    std::fs::create_dir_all(w.join("scripts")).unwrap();
    std::fs::write(w.join("scripts/fixture.txt"), SOURCE).unwrap();
    let (addr, _server) = start(&w);
    let v = ClientVersion::V420R8;
    let mut c = RawClient::join(&addr, v, "cls", |m| m.iter().any(|(id, _)| *id == 6));

    // Cold ask: CRC 0.
    let mut ask = gint5(0).to_vec();
    ask.extend_from_slice(b"fixture");
    c.send(161, &ask);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut body = Vec::new();
    let found = loop {
        if let Some(i) = raw_at_133(&body) {
            break i;
        }
        assert!(Instant::now() < deadline, "no 133: {body:?}");
        if let Ok(Ok(b)) = c.rx.recv_timeout(Duration::from_millis(100)) {
            body.extend(b);
        }
    };
    let n = body[found + 1..found + 4]
        .iter()
        .fold(0usize, |a, c| a << 7 | usize::from(c - 32));
    let block = &body[found + 5..found + 5 + n];
    assert_eq!(block[0], 133 + 32);
    assert_eq!(&block[1..9], b"'fixture");
    assert!(block.ends_with(b"\n\n"));
    let code = &block[9..block.len() - 1];
    let mut sum = flate2::Crc::new();
    sum.update(code);
    let crc = u64::from(sum.sum());

    // Matching CRC: silence.
    body.clear();
    let mut ask2 = gint5(crc).to_vec();
    ask2.extend_from_slice(b"fixture");
    c.send(161, &ask2);
    let end = Instant::now() + Duration::from_secs(1);
    while Instant::now() < end {
        if let Ok(Ok(b)) = c.rx.recv_timeout(Duration::from_millis(50)) {
            body.extend(b);
        }
    }
    assert!(
        raw_at_133(&body).is_none(),
        "matching CRC must not re-send 133: {body:?}"
    );

    // Missing name: empty 133 (name only).
    body.clear();
    let mut miss = gint5(0).to_vec();
    miss.extend_from_slice(b"nosuch");
    c.send(161, &miss);
    let deadline = Instant::now() + Duration::from_secs(10);
    let found = loop {
        if let Some(i) = raw_at_133(&body) {
            break i;
        }
        assert!(Instant::now() < deadline, "no empty 133: {body:?}");
        if let Ok(Ok(b)) = c.rx.recv_timeout(Duration::from_millis(100)) {
            body.extend(b);
        }
    };
    let n = body[found + 1..found + 4]
        .iter()
        .fold(0usize, |a, c| a << 7 | usize::from(c - 32));
    let block = &body[found + 5..found + 5 + n];
    assert_eq!(block[0], 133 + 32);
    // GCHAR length 6 (`&`) then `nosuch`.
    assert_eq!(&block[1..8], b"&nosuch");
    // Name only, then the block's closing `\n`.
    assert_eq!(block.len(), 1 + 1 + 6 + 1);
}

#[test]
fn a_build_without_133_gets_nothing() {
    let w = world("class524", "*\n");
    std::fs::create_dir_all(w.join("scripts")).unwrap();
    std::fs::write(w.join("scripts/fixture.txt"), SOURCE).unwrap();
    let (addr, _server) = start(&w);
    let v = ClientVersion::V524R4;
    let mut c = RawClient::join(&addr, v, "cls5", |m| m.iter().any(|(id, _)| *id == 6));
    let mut ask = gint5(0).to_vec();
    ask.extend_from_slice(b"fixture");
    c.send(161, &ask);
    let mut body = Vec::new();
    let end = Instant::now() + Duration::from_secs(1);
    while Instant::now() < end {
        if let Ok(Ok(b)) = c.rx.recv_timeout(Duration::from_millis(50)) {
            body.extend(b);
        }
    }
    assert!(raw_at_133(&body).is_none(), "{body:?}");
}
