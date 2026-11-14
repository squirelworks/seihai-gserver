//! A player flag a GS1 script writes by its plain name, compiled to GS2 by
//! Seihai (seihai-core#30). A GS2 client sends only `client.` flags, so the
//! compiled script writes `client.gs1_<flag>`; the server keeps the plain
//! flag and tells the client at once with `PLO_FLAGSET` (28) or
//! `PLO_FLAGDEL` (31). Real `client.` flags are kept as sent. Seihai's own
//! client code as a 5.24r4 on 127.0.0.1.

mod common;

use common::raw::{has, RawClient, Seen};
use common::*;
use seihai_core::protocol::ClientVersion;

const PLI_FLAGSET: u8 = 18;
const PLI_FLAGDEL: u8 = 19;
const PLO_FLAGSET: u8 = 28;
const PLO_FLAGDEL: u8 = 31;

fn in_level(m: &Seen) -> bool {
    m.iter().any(|(id, _)| *id == 6)
}

#[test]
fn a_lowered_gs1_flag_is_kept_by_its_plain_name() {
    let w = world("gs1flags", "*\n");
    let (addr, server) = start(&w);
    let v = ClientVersion::V524R4;
    let mut c = RawClient::join(&addr, v, "gina", in_level);

    c.send(PLI_FLAGSET, b"client.gs1_gotkey");
    c.until("the plain flag", |m| has(m, PLO_FLAGSET, b"gotkey"));
    c.send(PLI_FLAGSET, b"client.gs1_door=open");
    c.until("the plain flag", |m| has(m, PLO_FLAGSET, b"door=open"));
    c.send(PLI_FLAGSET, b"client.hasflags");
    c.send(PLI_FLAGSET, b"client.gs1_temp");
    c.until("the plain flag", |m| has(m, PLO_FLAGSET, b"temp"));
    c.send(PLI_FLAGDEL, b"client.gs1_temp");
    let got = c.until("the plain clear", |m| has(m, PLO_FLAGDEL, b"temp"));
    // A real `client.` flag is not echoed.
    assert!(!got
        .iter()
        .any(|(id, p)| *id == PLO_FLAGSET && p.starts_with(b"client.")));

    let rt = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(rt.block_on(server.world.save_all()), 1);
    let text = std::fs::read_to_string(w.join("accounts/gina.txt")).unwrap();
    let flags: Vec<_> = text.lines().filter(|l| l.starts_with("FLAG ")).collect();
    assert!(flags.contains(&"FLAG gotkey"), "{text}");
    assert!(flags.contains(&"FLAG door=open"), "{text}");
    assert!(flags.contains(&"FLAG client.hasflags"), "{text}");
    assert!(
        !flags
            .iter()
            .any(|f| f.contains("temp") || f.contains("gs1_")),
        "{text}"
    );
    drop(c);

    // After a relogin the plain flags come in the login burst.
    let c = RawClient::join(&addr, v, "gina", in_level);
    assert!(has(&c.seen, PLO_FLAGSET, b"gotkey"), "{:?}", c.seen);
    assert!(has(&c.seen, PLO_FLAGSET, b"door=open"), "{:?}", c.seen);
    let _ = std::fs::remove_dir_all(&w);
}
