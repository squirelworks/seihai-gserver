//! NPC Control (stage 4 slice 6, issue #52) on 127.0.0.1: the real
//! NPC-server session (`seihai_npcserver::session`) reads a world's NPC
//! files and links up; an NC logs in with seihai-rc's own NC client, sees
//! the NPCs, reads one's attributes and script, sets a new script and
//! deletes an NPC. Scripts go both ways as tokenized lines, the layout a
//! real 2001 NPC Control uses (lab, 2026-11-12); the set is built here,
//! since seihai-rc sends it as one raw text. An RC asking where the NPC-server is (94) is told this
//! server's own address.

use seihai_core::scripting::engine::Era;
use seihai_gserver::accounts::{Account, Store};
use seihai_gserver::npclink::serve_link;
use seihai_gserver::server::{serve, Server};
use seihai_gserver::wire::{detokenize, gchar, gint3, tokenize};
use seihai_npcserver::scripts::Scripts;
use seihai_npcserver::session::{self, Timing};
use seihai_rc::nc::{
    npc_delete_message, npc_get_message, npc_script_get_message, NcEvent, NcSession,
};
use seihai_rc::rc::{npc_server_query, Event, LoginParams, RcKind, Session};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PASSWORD: &str = "labpw";
const TOKEN: &str = "lab-token-0123456789";
const T: Duration = Duration::from_secs(10);

fn world() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("seihai-gserver-nc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["config", "levels", "npcs"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    std::fs::write(dir.join("levels/start.nw"), "GLEVNW01\n").unwrap();
    std::fs::write(
        dir.join("config/server.txt"),
        format!("name = NC Lab\nheartbeat = 1\nnpcserver = external\nnpcserver_token = {TOKEN}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("config/allowedversions.txt"), "*\n").unwrap();
    std::fs::write(
        dir.join("npcs/bell.txt"),
        "LEVEL start.nw\nX 5\nY 6\nIMAGE bell.png\nSCRIPT\nif (created) { message ding; }\nSCRIPTEND\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("npcs/npcControl.txt"),
        "GRNPC001\nNAME Control\nTYPE CONTROL\nSTARTLEVEL \nNPCSCRIPT\nif (playerlogin) { message x; }\nNPCSCRIPTEND\n",
    )
    .unwrap();
    let store = Store::new(dir.join("accounts"));
    for (name, staff) in [("boss", true), ("pleb", false)] {
        let mut a = Account::new(name, "start.nw", 30.0, 30.0);
        a.set_password(PASSWORD.as_bytes());
        a.staff = staff;
        store.save(&a).unwrap();
    }
    dir
}

fn start(world: &Path) -> (String, String, Arc<Server>) {
    let (server, warnings) = seihai_gserver::load_world(world).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let server = Arc::new(server);
    let (tx, rx) = std::sync::mpsc::channel();
    let s = server.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let game = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let link = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send((game.local_addr().unwrap(), link.local_addr().unwrap()))
                .unwrap();
            tokio::spawn(serve_link(s.npc.clone(), link));
            let _ = serve(s, game).await;
        });
    });
    let (g, l) = rx.recv().unwrap();
    (g.to_string(), l.to_string(), server)
}

/// A tokenized text's lines.
fn lines(t: &str) -> Vec<String> {
    detokenize(t.as_bytes())
        .iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect()
}

/// Events until `want` matches one, which is returned.
fn wait_for(s: &mut NcSession, what: &str, want: impl Fn(&NcEvent) -> bool) -> NcEvent {
    let deadline = Instant::now() + T;
    loop {
        match s.next_event(deadline).unwrap() {
            Some(e) if want(&e) => return e,
            Some(_) => {}
            None => panic!("no {what}; lists {:?}", s.lists.npcs),
        }
    }
}

#[test]
fn an_nc_lists_reads_sets_and_deletes_npcs() {
    let w = world();
    let (addr, link, _server) = start(&w);
    let npc_world = w.clone();
    std::thread::spawn(move || {
        let mut scripts = Scripts::with_world(Era::V217, &npc_world);
        let timing = Timing {
            tick: Duration::from_millis(20),
            ..Timing::default()
        };
        let _ = session::run(
            &link,
            TOKEN.as_bytes(),
            timing,
            Some(&mut scripts),
            &mut |_| {},
        );
    });

    // Not staff: refused.
    assert!(NcSession::login(&addr, "pleb", PASSWORD, T).is_err());

    let mut nc = NcSession::login(&addr, "boss", PASSWORD, T).unwrap();
    let end = Instant::now() + T;
    while nc.lists.npcs.len() < 2 {
        assert!(Instant::now() < end, "{:?}", nc.lists.npcs);
        let _ = nc
            .next_event(Instant::now() + Duration::from_millis(200))
            .unwrap();
    }
    let row = |id: u32| {
        let r = &nc.lists.npcs[&id];
        (r.name.clone(), r.kind.clone(), r.level.clone())
    };
    let s = |v: &str| Some(v.to_string());
    assert_eq!(row(1), (s("bell"), s(""), s("start.nw")));
    assert_eq!(row(2), (s("Control"), s("CONTROL"), s("")));

    nc.send(&[npc_get_message(2).unwrap()]).unwrap();
    let NcEvent::Attributes(a) = wait_for(&mut nc, "attributes", |e| {
        matches!(e, NcEvent::Attributes(_))
    }) else {
        unreachable!()
    };
    assert!(
        a.contains("name=Control") && a.contains("type=CONTROL"),
        "{a}"
    );

    nc.send(&[npc_script_get_message(1).unwrap()]).unwrap();
    let got = wait_for(&mut nc, "script", |e| {
        matches!(e, NcEvent::NpcScript { .. })
    });
    // The script's lines come tokenized, as the real 2001 NC reads them.
    let NcEvent::NpcScript { id: 1, script } = got else {
        panic!("{got:?}")
    };
    assert_eq!(lines(&script), ["if (created) { message ding; }"]);

    // A new script, two lines, sent the way the real 2001 NC sends it:
    // in the file, and read back the same.
    let mut set = vec![gchar(109)];
    gint3(1, &mut set);
    set.extend(tokenize(&[
        b"if (created) {".to_vec(),
        b"  message dong, dong; }".to_vec(),
    ]));
    nc.send(&[set]).unwrap();
    let end = Instant::now() + T;
    while !std::fs::read_to_string(w.join("npcs/bell.txt"))
        .unwrap()
        .contains("dong")
    {
        assert!(Instant::now() < end, "the file did not change");
        std::thread::sleep(Duration::from_millis(20));
    }
    nc.send(&[npc_script_get_message(1).unwrap()]).unwrap();
    // The set restarts the level, but the NPC stays on the list.
    let got = wait_for(&mut nc, "the new script", |e| {
        assert_ne!(*e, NcEvent::NpcRemoved(1), "a script set removed the NPC");
        matches!(e, NcEvent::NpcScript { script, .. } if script.contains("dong"))
    });
    let NcEvent::NpcScript { id: 1, script } = got else {
        panic!("{got:?}")
    };
    assert_eq!(
        lines(&script),
        ["if (created) {", "  message dong, dong; }"]
    );
    let file = std::fs::read_to_string(w.join("npcs/bell.txt")).unwrap();
    assert!(
        file.contains("if (created) {\n  message dong, dong; }\n"),
        "{file}"
    );

    // Deleted: 159 comes back once the file is put aside.
    nc.send(&[npc_delete_message(1).unwrap()]).unwrap();
    wait_for(&mut nc, "the delete", |e| *e == NcEvent::NpcRemoved(1));
    assert!(w.join("npcs/bell.txt.deleted").exists());
    nc.close();

    // An RC asking where the NPC-server is gets this server's address.
    let mut rc = Session::login(
        &addr,
        &LoginParams {
            kind: RcKind::Gen5,
            token: RcKind::Gen5.default_token().into(),
            account: "boss".into(),
            password: PASSWORD.into(),
            key: 77,
            nickname: None,
            identity: None,
        },
        T,
    )
    .unwrap();
    rc.send_messages(&[npc_server_query(5)]).unwrap();
    let deadline = Instant::now() + T;
    let found = loop {
        match rc.next_event(deadline).unwrap() {
            Some(Event::NpcServerAddr(a)) => break a,
            Some(_) => {}
            None => panic!("no 79"),
        }
    };
    assert_eq!(found.player_id, 5);
    assert_eq!(found.raw, addr.replace(':', ","));
    rc.close();
    let _ = std::fs::remove_dir_all(&w);
}
