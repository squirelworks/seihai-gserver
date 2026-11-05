//! Stage 3.3: bombs, shots, fire and blasts, with Seihai's own client code
//! on 127.0.0.1. What one player sends reaches the others in the level as
//! GServer-v2 beta4 passes it on (lab capture, 2026-11-05): the sender's
//! id first, except for a bomb's removal, and the payload as sent. The
//! sender gets no echo, and a player in another level gets none of it.

mod common;

use common::raw::{has, RawClient, Seen};
use common::*;
use seihai_core::net::levelwarp_message;
use seihai_core::protocol::ClientVersion;

const START: &str = "GLEVNW01\nLINK house.nw 30 0 2 1 30 60\n";
const HOUSE: &str = "GLEVNW01\n";

fn in_level(m: &Seen) -> bool {
    m.iter().any(|(id, _)| *id == 6)
}

#[test]
fn bombs_and_shots_reach_the_level() {
    let w = world("bombs", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), START).unwrap();
    std::fs::write(w.join("levels/house.nw"), HOUSE).unwrap();
    // Players start on the link into the house, so Dee's warp is allowed.
    let cfg = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str("start_y = 1\n");
    std::fs::write(&cfg, text).unwrap();
    let (addr, _server) = start(&w);

    let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", in_level);
    let mut bob = RawClient::join(&addr, ClientVersion::V205, "bob", in_level);
    let mut cal = RawClient::join(&addr, ClientVersion::V139, "cal", in_level);
    // Dee is somewhere else: she logs in and walks into the house.
    let mut dee = RawClient::join(&addr, ClientVersion::V222, "dee", in_level);
    dee.sess
        .conn
        .send_messages(&[levelwarp_message(30.0, 60.0, "house.nw")])
        .unwrap();
    dee.until("the house", |m| has(m, 6, b"house.nw"));
    let id_of = |c: &RawClient, account: &str| {
        // The others' id as the others see it: an OTHERPLPROPS whose
        // account prop names them.
        c.seen
            .iter()
            .find(|(id, p)| *id == 8 && p.windows(account.len()).any(|w| w == account.as_bytes()))
            .map(|(_, p)| p[..2].to_vec())
            .unwrap_or_else(|| panic!("no {account}"))
    };
    bob.settle();
    cal.settle();
    let amy_id = id_of(&bob, "amy");

    let sends: [(u8, &[u8], u8, bool); 5] = [
        (4, &[0x5c, 0x5c, 0x21, 0x2e], 11, true),
        (5, &[0x5c, 0x5c], 12, false),
        (9, &[0x87, 0x79, 0x20, 0x8b, 0x21], 19, true),
        (10, &[0x5c, 0x5c, 0x22], 20, true),
        (27, &[0x5c, 0x5c, 0x22, 0x23], 36, true),
    ];
    for (pli, payload, plo, named) in sends {
        amy.send(pli, payload);
        let mut want = if named { amy_id.clone() } else { Vec::new() };
        want.extend_from_slice(payload);
        bob.until(&format!("{plo}"), |m| has(m, plo, &want));
        cal.until(&format!("{plo}"), |m| has(m, plo, &want));
    }
    let echo = amy.settle();
    for (_, _, plo, _) in sends {
        assert!(
            !echo.iter().any(|(id, _)| *id == plo),
            "echo {plo}: {echo:?}"
        );
    }
    let away = dee.settle();
    for (_, _, plo, _) in sends {
        assert!(
            !away.iter().any(|(id, _)| *id == plo),
            "{plo} reached the house"
        );
    }
    let _ = std::fs::remove_dir_all(&w);
}
