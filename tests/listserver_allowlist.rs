//! A listserver with a `gservers` allow-list, on 127.0.0.1: the real
//! seihai-listserver (its library, on an ephemeral port) lists a gserver
//! only after `SERVERHQPASS` matches the row's `hq_password`. A gserver
//! with `listserver_hq_password` set is listed; one without it connects
//! but stays off the list, as before this setting existed.

use seihai_gserver::server::{serve, Server};
use seihai_listserver::config::GserverAllow;
use seihai_listserver::{Config, Running};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("seihai-gserver-allow-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("config")).unwrap();
    d
}

fn listserver(d: &Path) -> Running {
    std::fs::write(d.join("accounts.txt"), "").unwrap();
    seihai_listserver::start(Config {
        bind: "127.0.0.1".into(),
        gserver_port: 0,
        client_ports: vec![0],
        accounts: d.join("accounts.txt"),
        secure_login_key: d.join("seihai-login.key"),
        gservers: vec![GserverAllow {
            ip: "127.0.0.1".into(),
            hq_password: "lab hq pw".into(),
        }],
        ..Config::default()
    })
    .unwrap()
}

/// A gserver in its own world, registering with `ls`.
fn gserver(tag: &str, ls: &Running, extra: &str) -> (PathBuf, Arc<Server>) {
    let w = dir(tag);
    std::fs::write(
        w.join("config/server.txt"),
        format!("name = {tag}\nlistserver = {}\n{extra}", ls.gserver_addr),
    )
    .unwrap();
    std::fs::write(w.join("config/allowedversions.txt"), "*\n").unwrap();
    let (mut server, warnings) = seihai_gserver::load_world(&w).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    seihai_gserver::start_listlink(&mut server);
    let server = Arc::new(server);
    let s = server.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _ = serve(s, l).await;
        });
    });
    let end = Instant::now() + Duration::from_secs(10);
    while !server.listlink.as_ref().unwrap().connected() {
        assert!(Instant::now() < end, "{tag}: listserver link never came up");
        std::thread::sleep(Duration::from_millis(20));
    }
    (w, server)
}

fn listed(ls: &Running, name: &str) -> bool {
    ls.shared.servers.id_by_name(name.as_bytes()).is_some()
}

#[test]
fn only_the_gserver_with_the_hq_password_is_listed() {
    let d = dir("list");
    let ls = listserver(&d);

    // No password: the link is up, but NEWSERVER is ignored.
    let (w1, _plain) = gserver("NoPass", &ls, "");

    // The row's password: listed.
    let (w2, _hq) = gserver("WithPass", &ls, "listserver_hq_password = lab hq pw\n");
    let end = Instant::now() + Duration::from_secs(10);
    while !listed(&ls, "WithPass") {
        assert!(
            Instant::now() < end,
            "the gserver with the HQ password was never listed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(500));
    assert!(!listed(&ls, "NoPass"), "listed without an HQ password");

    for p in [d, w1, w2] {
        let _ = std::fs::remove_dir_all(p);
    }
}
