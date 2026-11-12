//! `import-v2` (roadmap #56): a made-up GServer-v2 server folder, in the
//! layout `docs/GSERVER-V2-WORLD.md` describes, becomes a world this server
//! starts on; its staff log in with an RC and its bans hold. Only
//! `127.0.0.1` is used.

mod common;

use common::*;
use seihai_core::protocol::ClientVersion;
use seihai_gserver::accounts::Store;
use seihai_rc::rc::{LoginParams, RcKind, Session};
use std::path::PathBuf;
use std::time::Duration;

fn v2_folder(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("seihai-gserver-v2src-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("accounts")).unwrap();
    std::fs::write(
        dir.join("config/serveroptions.txt"),
        "# Test server\nname = Import Lab\ndescription = moved over\n#startlevel = nowhere.nw\n\
         serverport = 0\nmaxplayers = 40\nstaff = (Owners),Mara\nonlystaff = false\n\
         flaghack_movement = true\t# a hack\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("config/allowedversions.txt"),
        "// builds\nGNW22122:GNW28015 // 2.17 to 2.31\n//GNW13110\n",
    )
    .unwrap();
    let account = |name: &str, extra: &str| {
        format!(
            "GRACC001\nNAME {name}\nNICK {name}\nLEVEL onlinestartlocal.nw\nX 20.00\nY 21.50\n\
             MAXHP 4.00\nHP 4.00\nRUPEES 33\nGLOVEP 1\nSWORDP 2\nSWORD sword2.png\nDEATHS 1\n{extra}"
        )
    };
    std::fs::write(
        dir.join("accounts/defaultaccount.txt"),
        account("default", "LEVEL onlinestartlocal.nw\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("accounts/Mara.txt"),
        account(
            "Mara",
            "IPRANGE *.*.*.*\nFOLDERRIGHT rw world/levels/*\nLOCALRIGHTS 513\n",
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("accounts/Ned.txt"),
        account("Ned", "BANNED 1\nBANREASON cheating\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("accounts/(npcserver).txt"),
        account("(npcserver)", ""),
    )
    .unwrap();
    dir
}

#[test]
fn a_gserver_v2_folder_becomes_a_world() {
    let src = v2_folder("lab");
    let w = std::env::temp_dir().join(format!("seihai-gserver-v2dst-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&w);
    let r = seihai_gserver::import::import_v2(&src, &w).unwrap();
    let left = r.left.join("\n");
    assert!(
        left.contains("account (npcserver): not a valid name"),
        "{left}"
    );
    assert!(
        left.contains("not carried over: flaghack_movement, onlystaff"),
        "{left}"
    );
    assert!(
        left.contains("account keys not carried over: DEATHS"),
        "{left}"
    );
    // A second import into the same folder is refused.
    assert!(seihai_gserver::import::import_v2(&src, &w).is_err());

    let store = Store::new(w.join("accounts"));
    let mara = store.load("Mara").unwrap().unwrap();
    assert!(mara.staff);
    assert_eq!(
        (mara.x, mara.y, mara.rupees, mara.max_hp),
        (20.0, 21.5, 33, 4)
    );
    assert_eq!(mara.sword, (2, "sword2.png".to_string()));
    assert_eq!(mara.folder_rights, ["rw levels/*"]);
    assert_eq!(mara.rc_rights, 513);
    let ned = store.load("Ned").unwrap().unwrap();
    assert!(!ned.staff);

    // The world loads and starts as imported.
    let (server, warnings) = seihai_gserver::load_world(&w).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(server.cfg.name, "Import Lab");
    assert_eq!(server.cfg.start_level, "onlinestartlocal.nw");
    assert_eq!((server.cfg.start_x, server.cfg.start_y), (20.0, 21.5));
    drop(server);
    let mut mara = mara;
    mara.set_password(PASSWORD.as_bytes());
    store.save(&mara).unwrap();
    let mut ned = ned;
    ned.set_password(PASSWORD.as_bytes());
    store.save(&ned).unwrap();
    let (addr, _server) = start(&w);

    // Staff from the list, from anywhere (`*.*.*.*`): an RC gets in.
    Session::login(
        &addr,
        &LoginParams {
            kind: RcKind::Gen5,
            token: RcKind::Gen5.default_token().into(),
            account: "Mara".into(),
            password: PASSWORD.into(),
            key: 3,
            nickname: None,
        },
        Duration::from_secs(10),
    )
    .unwrap();
    // The ban came over; the allow-list's range lets 2.22 try.
    assert_eq!(
        refused(&addr, ClientVersion::V222, "Ned", PASSWORD).as_deref(),
        Some("You are banned from this server: cheating.")
    );
    let _ = std::fs::remove_dir_all(&w);
    let _ = std::fs::remove_dir_all(&src);
}
