//! NPC Control's classes and weapons on 127.0.0.1, with seihai-rc's own
//! NC client: the class list at login, class get, save and delete
//! (112, 113, 119 answered with 162, 163, 188), and the weapon list, get,
//! save and delete (115 to 118 answered with 167 and 33). Classes live in
//! `scripts/`, weapons in `weapons/`, as the game side reads them. The
//! layouts are seihai-rc's `docs/PROTOCOL.md` (2004 RC and RC3 captures,
//! 2026-11-13); no real NPC Control has run against this server's
//! answers yet.

use seihai_gserver::accounts::{Account, Store};
use seihai_gserver::server::{serve, Server};
use seihai_rc::nc::{
    class_delete_message, class_get_message, class_set_message, weapon_delete_message,
    weapon_get_message, weapon_list_message, weapon_set_message, NcEvent, NcSession,
};
use seihai_rc::wire::ServerTextProfile;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PASSWORD: &str = "labpw";
const T: Duration = Duration::from_secs(10);

fn world() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("seihai-gserver-ncs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["config", "levels", "scripts", "weapons"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    std::fs::write(dir.join("levels/start.nw"), "GLEVNW01\n").unwrap();
    std::fs::write(dir.join("config/server.txt"), "name = NC Scripts\n").unwrap();
    std::fs::write(dir.join("config/allowedversions.txt"), "*\n").unwrap();
    std::fs::write(
        dir.join("scripts/lamp.txt"),
        "if (created) {\n  this.on = 1;\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("weapons/weaponBow.txt"),
        "GRAWP001\nREALNAME Bow\nIMAGE bow.png\nSCRIPT\nif (weaponfired) {\n  shootarrow 0;\n}\nSCRIPTEND\n",
    )
    .unwrap();
    let store = Store::new(dir.join("accounts"));
    let mut a = Account::new("boss", "start.nw", 30.0, 30.0);
    a.set_password(PASSWORD.as_bytes());
    a.staff = true;
    store.save(&a).unwrap();
    dir
}

fn start(world: &Path) -> (String, Arc<Server>) {
    let (server, warnings) = seihai_gserver::load_world(world).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let server = Arc::new(server);
    let (tx, rx) = std::sync::mpsc::channel();
    let s = server.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let game = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(game.local_addr().unwrap()).unwrap();
            let _ = serve(s, game).await;
        });
    });
    (rx.recv().unwrap().to_string(), server)
}

fn wait_for(s: &mut NcSession, what: &str, want: impl Fn(&NcEvent) -> bool) -> NcEvent {
    let deadline = Instant::now() + T;
    loop {
        match s.next_event(deadline).unwrap() {
            Some(e) if want(&e) => return e,
            Some(_) => {}
            None => panic!("no {what}"),
        }
    }
}

#[test]
fn an_nc_edits_classes_and_weapons() {
    let w = world();
    let (addr, _server) = start(&w);
    let mut nc = NcSession::login(&addr, "boss", PASSWORD, T).unwrap();

    // The class list comes with the login.
    wait_for(&mut nc, "the class list", |e| {
        *e == NcEvent::Class("lamp".into())
    });
    nc.send(&[class_get_message("lamp").unwrap()]).unwrap();
    let got = wait_for(&mut nc, "162", |e| matches!(e, NcEvent::ClassScript { .. }));
    let NcEvent::ClassScript { name, script } = got else {
        unreachable!()
    };
    assert_eq!(name, "lamp");
    assert_eq!(
        script.lines().collect::<Vec<_>>(),
        ["if (created) {", "  this.on = 1;", "}"]
    );

    // A new class: 163, the file, and the same text back.
    nc.send(&[class_set_message(
        ServerTextProfile::SeihaiGserver,
        "door",
        "if (created) {\n  this.open = 0;\n}",
    )
    .unwrap()])
        .unwrap();
    wait_for(&mut nc, "163 door", |e| *e == NcEvent::Class("door".into()));
    let file = std::fs::read_to_string(w.join("scripts/door.txt")).unwrap();
    assert_eq!(file, "if (created) {\n  this.open = 0;\n}");
    nc.send(&[class_get_message("door").unwrap()]).unwrap();
    wait_for(
        &mut nc,
        "door's script",
        |e| matches!(e, NcEvent::ClassScript { name, script } if name == "door" && script.contains("this.open")),
    );
    // A path is not a class name.
    nc.send(&[class_set_message(ServerTextProfile::SeihaiGserver, "../x", "y").unwrap()])
        .unwrap();
    wait_for(
        &mut nc,
        "the refusal",
        |e| matches!(e, NcEvent::Message { id: 74, payload } if payload.starts_with(b"error:")),
    );
    assert!(!w.join("x.txt").exists());
    // A deterministic filesystem error must not acknowledge a deletion or
    // discard the live class. The following get is an ordered wire barrier.
    let class_path = w.join("scripts/door.txt");
    let class_backup = w.join("scripts/door.saved");
    std::fs::rename(&class_path, &class_backup).unwrap();
    std::fs::create_dir(&class_path).unwrap();
    nc.send(&[
        class_delete_message("door").unwrap(),
        class_get_message("door").unwrap(),
    ])
    .unwrap();
    wait_for(&mut nc, "retained class after failed delete", |e| {
        assert_ne!(*e, NcEvent::ClassDeleted("door".into()));
        matches!(e, NcEvent::ClassScript { name, script } if name == "door" && script.contains("this.open"))
    });
    std::fs::remove_dir(&class_path).unwrap();
    std::fs::rename(&class_backup, &class_path).unwrap();
    assert!(seihai_gserver::classes::Classes::load(&w.join("scripts"))
        .get("door")
        .is_some());
    nc.send(&[class_delete_message("door").unwrap()]).unwrap();
    wait_for(&mut nc, "188", |e| {
        *e == NcEvent::ClassDeleted("door".into())
    });
    assert!(!w.join("scripts/door.txt").exists());
    assert!(seihai_gserver::classes::Classes::load(&w.join("scripts"))
        .get("door")
        .is_none());

    // Weapons.
    nc.send(&[weapon_list_message()]).unwrap();
    wait_for(&mut nc, "167", |e| {
        *e == NcEvent::Weapons(vec!["Bow".into()])
    });
    nc.send(&[weapon_get_message("Bow").unwrap()]).unwrap();
    let got = wait_for(&mut nc, "33", |e| matches!(e, NcEvent::Weapon(_)));
    let NcEvent::Weapon(ws) = got else {
        unreachable!()
    };
    assert_eq!((ws.name.as_str(), ws.image.as_str()), ("Bow", "bow.png"));
    assert!(ws.script.contains("shootarrow 0;"), "{:?}", ws.script);

    nc.send(&[
        weapon_set_message("Lamp", "lamp.png", "if (weaponfired) {\n  say2 light;\n}\n").unwrap(),
    ])
    .unwrap();
    wait_for(&mut nc, "167 with Lamp", |e| {
        *e == NcEvent::Weapons(vec!["Bow".into(), "Lamp".into()])
    });
    let file = std::fs::read_to_string(w.join("weapons/weaponLamp.txt")).unwrap();
    assert!(file.contains("REALNAME Lamp\nIMAGE lamp.png\nSCRIPT\nif (weaponfired) {\n  say2 light;\n}\nSCRIPTEND\n"), "{file}");
    nc.send(&[weapon_get_message("Lamp").unwrap()]).unwrap();
    wait_for(
        &mut nc,
        "Lamp's script",
        |e| matches!(e, NcEvent::Weapon(ws) if ws.name == "Lamp" && ws.script.contains("say2 light;")),
    );

    let weapon_path = w.join("weapons/weaponBow.txt");
    let weapon_backup = w.join("weapons/Bow.saved");
    std::fs::rename(&weapon_path, &weapon_backup).unwrap();
    std::fs::create_dir(&weapon_path).unwrap();
    nc.send(&[
        weapon_delete_message("Bow").unwrap(),
        weapon_get_message("Bow").unwrap(),
    ])
    .unwrap();
    wait_for(&mut nc, "weapon delete failure", |e| {
        if let NcEvent::Message { id: 74, payload } = e {
            assert!(!payload.starts_with(b"Weapon Bow deleted"));
            return payload == b"error: weapon Bow not deleted";
        }
        false
    });
    wait_for(&mut nc, "retained weapon after failed delete", |e| {
        if let NcEvent::Message { id: 74, payload } = e {
            assert!(!payload.starts_with(b"Weapon Bow deleted"));
        }
        matches!(e, NcEvent::Weapon(ws) if ws.name == "Bow" && ws.image == "bow.png")
    });
    std::fs::remove_dir(&weapon_path).unwrap();
    std::fs::rename(&weapon_backup, &weapon_path).unwrap();
    assert!(seihai_gserver::weapons::Weapons::load(&w.join("weapons"))
        .get("Bow")
        .is_some());
    nc.send(&[weapon_delete_message("Bow").unwrap()]).unwrap();
    wait_for(&mut nc, "167 without Bow", |e| {
        *e == NcEvent::Weapons(vec!["Lamp".into()])
    });
    assert!(!w.join("weapons/weaponBow.txt").exists());
    let reloaded = seihai_gserver::weapons::Weapons::load(&w.join("weapons"));
    assert!(reloaded.get("Bow").is_none());
    assert!(reloaded.get("Lamp").is_some());
    nc.close();
    let _ = std::fs::remove_dir_all(&w);
}
