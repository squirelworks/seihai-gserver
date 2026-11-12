//! The RC file browser (stage 4 slice 5, issue #52) on 127.0.0.1 with
//! seihai-rc's own file link: list, get and put within the account's
//! folder rights.

mod common;

use common::*;
use seihai_gserver::accounts::{Account, Store};
use seihai_rc::files::FileError;
use seihai_rc::rc::{LoginParams, RcKind, Session};
use std::time::Duration;

const T: Duration = Duration::from_secs(10);

#[test]
fn an_rc_lists_gets_and_puts_files_within_its_rights() {
    let w = world("rcfiles", "*\n");
    std::fs::create_dir_all(w.join("levels")).unwrap();
    // A level with line feeds and a byte over 0x7F.
    let level = b"GLEVNW01\nBOARD 0 0 64 0 aa\nSIGN 1 1\n\xe9t\xe9\nSIGNEND\n".to_vec();
    std::fs::write(w.join("levels/start.nw"), &level).unwrap();
    std::fs::write(w.join("levels/notes.txt"), "read me").unwrap();
    let mut a = Account::new("boss", "onlinestartlocal.nw", 30.0, 30.0);
    a.set_password(PASSWORD.as_bytes());
    a.staff = true;
    a.folder_rights = vec!["rw levels/*.nw".into(), "r levels/*".into()];
    Store::new(w.join("accounts")).save(&a).unwrap();
    let (addr, _server) = start(&w);

    let mut s = Session::login(
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

    assert_eq!(s.list_dir("levels", T).unwrap(), ["notes.txt", "start.nw"]);
    assert_eq!(s.get_file("start.nw", T).unwrap(), level);
    assert_eq!(s.get_file("notes.txt", T).unwrap(), b"read me");

    // A put on a .nw, with line feeds, and read back from the disk.
    let new = b"GLEVNW01\nBOARD 0 0 64 0 bb\n".to_vec();
    s.put_file("new.nw", &new, T).unwrap();
    assert_eq!(std::fs::read(w.join("levels/new.nw")).unwrap(), new);
    assert_eq!(s.get_file("new.nw", T).unwrap(), new);

    // Read only, and no rights at all: refused, the files untouched.
    assert!(refused(s.put_file("notes.txt", b"changed", T)));
    assert_eq!(
        std::fs::read(w.join("levels/notes.txt")).unwrap(),
        b"read me"
    );
    assert!(refused(s.list_dir("config", T)));
    assert!(refused(s.get_file("missing.nw", T)));
    let _ = std::fs::remove_dir_all(&w);
}

fn refused<T>(r: Result<T, FileError>) -> bool {
    matches!(r, Err(FileError::Refused(_)))
}
