//! What the November 1999 servers (GServer 1.28 beta 2 and 1.3) do that
//! this server took over (`docs/ORIGINAL-SERVERS.md`), with Seihai's own
//! client code on 127.0.0.1: horses left in a level, `PLI_TOALL`, kill
//! claims from builds that send a one-byte id, deaths, alignment that
//! rises with time online, and, under `rules = 1999`, the kill cost and a
//! dead leader handing the level's baddies on.

mod common;

use common::raw::{has, RawClient, Seen};
use common::*;
use seihai_core::protocol::ClientVersion;
use std::path::Path;
use std::time::{Duration, Instant};

const OTHERPLPROPS: u8 = 8;
const PLAYERPROPS: u8 = 9;
const ISLEADER: u8 = 10;
const TOALL: u8 = 13;
const HORSEADD: u8 = 17;
const HORSEDEL: u8 = 18;

fn in_level(m: &Seen) -> bool {
    m.iter().any(|(id, _)| *id == 6)
}

/// A world with a level, `extra` settings and accounts at the given AP.
fn setup(tag: &str, extra: &str, accounts: &[(&str, u8)]) -> std::path::PathBuf {
    let w = world(tag, "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    std::fs::write(w.join("levels/onlinestartlocal.nw"), "GLEVNW01\n").unwrap();
    let cfg = w.join("config/server.txt");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str(extra);
    std::fs::write(&cfg, text).unwrap();
    let store = seihai_gserver::accounts::Store::new(w.join("accounts"));
    for &(name, ap) in accounts {
        let mut a = seihai_gserver::accounts::Account::new(name, "onlinestartlocal.nw", 30.0, 30.0);
        a.alignment = ap;
        a.set_password(PASSWORD.as_bytes());
        store.save(&a).unwrap();
    }
    w
}

/// The id the others know `account` by, as `c`'s build reads it.
fn id_of(c: &RawClient, account: &str, one_byte: bool) -> Vec<u8> {
    c.seen
        .iter()
        .find(|(id, p)| {
            *id == OTHERPLPROPS && p.windows(account.len()).any(|w| w == account.as_bytes())
        })
        .map(|(_, p)| p[..if one_byte { 1 } else { 2 }].to_vec())
        .unwrap_or_else(|| panic!("no {account}"))
}

/// Read until `c` has seen an `OTHERPLPROPS` naming each of `names`,
/// counting what came before.
fn knows(c: &mut RawClient, names: &[&str]) {
    let all = |m: &Seen| {
        names.iter().all(|n| {
            m.iter()
                .any(|(id, p)| *id == OTHERPLPROPS && p.windows(n.len()).any(|w| w == n.as_bytes()))
        })
    };
    if all(&c.seen) {
        return;
    }
    let before = c.seen.clone();
    c.until("the others", |m| {
        let mut both = before.clone();
        both.extend_from_slice(m);
        all(&both)
    });
}

/// Wait until the account file has every line in `want`.
fn saved(w: &Path, account: &str, want: &[&str]) {
    let path = w.join(format!("accounts/{account}.txt"));
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if want.iter().all(|l| text.lines().any(|t| t == *l)) {
            return;
        }
        assert!(Instant::now() < end, "{want:?} in {text}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A horse left in a level is not passed on at once; a player who enters
/// gets it, and when the horse lifetime is up the level hears it go.
#[test]
fn horses_wait_for_arrivals_and_run_out() {
    let w = setup("horses", "horse_lifetime = 2\n", &[]);
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V205, "amy", in_level);
    let mut bob = RawClient::join(&addr, ClientVersion::V222, "bob", in_level);
    knows(&mut amy, &["bob"]);
    // `puthorse ride.gif,53,28`, as the 1.3 capture sends it.
    amy.send(7, b"\x8aX#ride.gif");
    let quiet = bob.settle();
    assert!(!quiet.iter().any(|(id, _)| *id == HORSEADD), "{quiet:?}");
    let cat = RawClient::join(&addr, ClientVersion::V231, "cat", |m| {
        has(m, HORSEADD, b"\x8aX#ride")
    });
    drop(cat);
    bob.until("the horse going", |m| has(m, HORSEDEL, b"\x8aX"));

    // Taken back before its time: nobody is told, and it is gone.
    amy.send(7, b"\x8ad#ride2");
    amy.send(8, b"\x8ad");
    let dan = RawClient::join(&addr, ClientVersion::V222, "dan", in_level);
    let mut dan = dan;
    let later = dan.settle();
    assert!(
        !dan.seen
            .iter()
            .chain(later.iter())
            .any(|(id, _)| *id == HORSEADD),
        "{:?}",
        dan.seen
    );
    let _ = std::fs::remove_dir_all(&w);
}

/// `PLI_TOALL` reaches everyone else online, with the sender's id as each
/// build reads it.
#[test]
fn toall_reaches_everyone() {
    let w = setup("toall", "", &[]);
    std::fs::write(w.join("levels/other.nw"), "GLEVNW01\n").unwrap();
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", in_level);
    let mut bob = RawClient::join(&addr, ClientVersion::V205, "bob", in_level);
    let mut cal = RawClient::join(&addr, ClientVersion::V135, "cal", in_level);
    knows(&mut amy, &["bob", "cal"]);
    knows(&mut bob, &["amy"]);
    knows(&mut cal, &["amy"]);
    let (long, short) = (id_of(&bob, "amy", false), id_of(&cal, "amy", true));
    amy.send(6, b"%hello");
    let mut want = long.clone();
    want.extend(b"%hello");
    bob.until("the toall", |m| has(m, TOALL, &want));
    let mut want = short.clone();
    want.extend(b"%hello");
    cal.until("the toall", |m| has(m, TOALL, &want));
    let back = amy.settle();
    assert!(!back.iter().any(|(id, _)| *id == TOALL), "{back:?}");
    let _ = std::fs::remove_dir_all(&w);
}

/// A 1.35 sends its killer's id as one byte; the claim counts. With the
/// 1999 rules the cost is the victim's (AP + 10) / 20, half to even, and
/// a victim under 10 AP costs nothing and counts nothing.
#[test]
fn a_one_byte_kill_claim_under_the_1999_rules() {
    let w = setup(
        "claim1999",
        "rules = 1999\nalignment_time = 0\n",
        &[("amy", 50), ("bob", 40), ("cal", 5)],
    );
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", in_level);
    let mut bob = RawClient::join(&addr, ClientVersion::V135, "bob", in_level);
    let mut cal = RawClient::join(&addr, ClientVersion::V135, "cal", in_level);
    knows(&mut amy, &["bob", "cal"]);
    knows(&mut bob, &["amy"]);
    knows(&mut cal, &["amy"]);
    let killer = id_of(&bob, "amy", true);
    assert_eq!(killer, id_of(&cal, "amy", true));

    // Cal (5 AP) dies to Amy: nothing.
    cal.send(14, &killer);
    let quiet = amy.settle();
    assert!(!quiet.iter().any(|(id, _)| *id == PLAYERPROPS), "{quiet:?}");

    // Bob (40 AP) dies to Amy: 50 / 20 = 2.5, to even 2, so 48.
    bob.send(14, &killer);
    amy.until("her AP", |m| has(m, PLAYERPROPS, &[32 + 32, 48 + 32]));
    drop(amy);
    saved(&w, "amy", &["KILLS 1", "ALIGN 48"]);
    let _ = std::fs::remove_dir_all(&w);
}

/// The dead status bit counts a death, and under the 1999 rules a dead
/// leader hands the level's baddies to the next player.
#[test]
fn a_death_counts_and_a_dead_leader_hands_off() {
    let w = setup("deaths", "rules = 1999\nalignment_time = 0\n", &[]);
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", |m| {
        in_level(m) && m.iter().any(|(id, _)| *id == ISLEADER)
    });
    let mut bob = RawClient::join(&addr, ClientVersion::V222, "bob", in_level);
    assert!(!bob.seen.iter().any(|(id, _)| *id == ISLEADER));
    knows(&mut amy, &["bob"]);
    // Status 20 with the dead bit (8).
    amy.send(2, &[18 + 32, 28 + 32]);
    bob.until("the lead", |m| m.iter().any(|(id, _)| *id == ISLEADER));
    // Back up and dead again: two deaths.
    amy.send(2, &[18 + 32, 20 + 32]);
    amy.send(2, &[18 + 32, 28 + 32]);
    amy.settle();
    drop(amy);
    saved(&w, "amy", &["DEATHS 2"]);
    let _ = std::fs::remove_dir_all(&w);
}

/// Alignment rises by one for each `alignment_time` online.
#[test]
fn alignment_rises_with_time_online() {
    let w = setup("align", "alignment_time = 1\n", &[("amy", 99)]);
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", in_level);
    amy.until("100 AP", |m| has(m, PLAYERPROPS, &[32 + 32, 100 + 32]));
    // Never past 100.
    let end = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < end {
        let more = amy.settle();
        assert!(!more.iter().any(|(id, _)| *id == PLAYERPROPS), "{more:?}");
    }
    let _ = std::fs::remove_dir_all(&w);
}

/// `rules = 1999`: arrows past 99 are cut to 99 and the player is told
/// (`PLO_PLAYERPROPS`), and a new nickname reaches a player in another
/// level, as on the 1999 servers.
#[test]
fn prop_limits_and_nicknames_everywhere_under_1999_rules() {
    let w = setup("rules-1999-props", "rules = 1999\n", &[]);
    std::fs::write(w.join("levels/house.nw"), "GLEVNW01\n").unwrap();
    let store = seihai_gserver::accounts::Store::new(w.join("accounts"));
    for (name, level) in [("amy", "onlinestartlocal.nw"), ("bob", "house.nw")] {
        let mut a = seihai_gserver::accounts::Account::new(name, level, 30.0, 30.0);
        a.set_password(PASSWORD.as_bytes());
        store.save(&a).unwrap();
    }
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V132, "amy", in_level);
    let mut bob = RawClient::join(&addr, ClientVersion::V132, "bob", in_level);
    amy.settle();
    bob.settle();

    amy.send(2, &[4 + 32, 120 + 32]);
    amy.until("arrows cut to 99", |m| {
        has(m, PLAYERPROPS, &[4 + 32, 99 + 32])
    });

    let mut nick = vec![32, 32 + 4];
    nick.extend(b"Amy2");
    amy.send(2, &nick);
    bob.until("the new nickname", |m| {
        m.iter()
            .any(|(id, p)| *id == OTHERPLPROPS && p.windows(4).any(|w| w == b"Amy2"))
    });
}

/// Whether a message `id` holds `part` anywhere in its payload.
fn holds(m: &Seen, id: u8, part: &[u8]) -> bool {
    m.iter()
        .any(|(i, p)| *i == id && p.windows(part.len()).any(|w| w == part))
}

/// `rules = 1999`, a build before 1.3: prop 25 is the player-killer time.
/// It sets status bit 0x20 for that many seconds, the player and the level
/// are told, and while the flag is on the player's own hearts are not
/// taken (GServer 1.28 beta 2). Under `v2` prop 25 does nothing.
#[test]
fn the_player_killer_flag_before_1_3() {
    let w = setup("pk-flag", "rules = 1999\nalignment_time = 0\n", &[]);
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V121, "amy", in_level);
    let mut bob = RawClient::join(&addr, ClientVersion::V139, "bob", in_level);
    knows(&mut amy, &["bob"]);
    knows(&mut bob, &["amy"]);
    // Status 20 (the account's) with 0x20: 52.
    let flagged = [18 + 32, 52 + 32];
    let plain = [18 + 32, 20 + 32];
    // Two seconds.
    amy.send(2, &[25 + 32, 32, 34]);
    amy.until("the flag", |m| holds(m, PLAYERPROPS, &flagged));
    bob.until("the flag", |m| holds(m, OTHERPLPROPS, &flagged));
    // Hearts while flagged and alive: not taken, nobody hears them.
    amy.send(2, &[2 + 32, 1 + 32]);
    let quiet = bob.settle();
    assert!(!holds(&quiet, OTHERPLPROPS, &[2 + 32, 1 + 32]), "{quiet:?}");
    // Two seconds on, the flag is gone, and hearts are taken again.
    amy.until("the flag off", |m| holds(m, PLAYERPROPS, &plain));
    bob.until("the flag off", |m| holds(m, OTHERPLPROPS, &plain));
    amy.send(2, &[2 + 32, 1 + 32]);
    bob.until("the hearts", |m| holds(m, OTHERPLPROPS, &[2 + 32, 1 + 32]));
    drop((amy, bob));
    let _ = std::fs::remove_dir_all(&w);

    // `v2`: no flag.
    let w = setup("pk-flag-v2", "alignment_time = 0\n", &[]);
    let (addr, _server) = start(&w);
    let mut amy = RawClient::join(&addr, ClientVersion::V121, "amy", in_level);
    amy.settle();
    amy.send(2, &[25 + 32, 32, 34]);
    let quiet = amy.settle();
    assert!(!holds(&quiet, PLAYERPROPS, &flagged), "{quiet:?}");
    let _ = std::fs::remove_dir_all(&w);
}

/// `rules = 1999`: a player claiming an NPC another player in the level
/// already carries is refused and told; under `v2` both keep it.
#[test]
fn one_carrier_per_npc_under_1999_rules() {
    let npc = [24 + 32, 0x20, 0x21, 0x25];
    for (tag, extra, refused) in [
        ("carry-1999", "rules = 1999\n", true),
        ("carry-v2", "", false),
    ] {
        let w = setup(tag, extra, &[]);
        let (addr, _server) = start(&w);
        let mut amy = RawClient::join(&addr, ClientVersion::V222, "amy", in_level);
        let mut bob = RawClient::join(&addr, ClientVersion::V222, "bob", in_level);
        knows(&mut amy, &["bob"]);
        knows(&mut bob, &["amy"]);
        amy.send(2, &npc);
        bob.until("amy's carry", |m| holds(m, OTHERPLPROPS, &npc));
        // Facing right (3) while lifting: sprite 3 + 4 * 6.
        let mut claim = vec![17 + 32, 27 + 32];
        claim.extend_from_slice(&npc);
        bob.send(2, &claim);
        let back = bob.settle();
        let told = holds(
            &back,
            PLAYERPROPS,
            &[17 + 32, 3 + 32, 24 + 32, 0x20, 0x20, 0x20],
        );
        assert_eq!(told, refused, "{back:?}");
        let _ = std::fs::remove_dir_all(&w);
    }
}

/// `rules = 1999`: a new account starts at 50 AP ("you start at 50", the
/// 1.3 readme); under `v2` at 0.
#[test]
fn new_accounts_start_at_50_ap_under_1999_rules() {
    for (tag, extra, ap) in [
        ("ap50-1999", "rules = 1999\n", "ALIGN 50"),
        ("ap50-v2", "", "ALIGN 0"),
    ] {
        let w = setup(tag, extra, &[]);
        let (addr, _server) = start(&w);
        let amy = RawClient::join(&addr, ClientVersion::V135, "newcomer", in_level);
        drop(amy);
        saved(&w, "newcomer", &[ap]);
        let _ = std::fs::remove_dir_all(&w);
    }
}
