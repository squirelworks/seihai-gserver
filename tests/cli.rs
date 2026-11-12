//! The `seihai-gserver` command's subcommands that do not start a server.

use std::path::PathBuf;
use std::process::Command;

fn world(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("seihai-gserver-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("config/server.txt"), "name = Cli Lab\n").unwrap();
    dir
}

fn run(w: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_seihai-gserver"))
        .args(args)
        .arg("--world")
        .arg(w)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn key_fingerprint_prints_the_key_the_server_loads() {
    use seihai_core::protocol::seclogin::{self, ServerIdentity};
    let w = world("fp");
    let key = w.join("config/seihai-login.key");

    // No key yet: it says so and makes none.
    let (ok, out, err) = run(&w, &["key-fingerprint"]);
    assert!(!ok && out.is_empty(), "{out}");
    assert!(err.contains("no secure login key"), "{err}");
    assert!(!key.exists());

    // The key a first start makes (the same call the server uses).
    let (id, made) = ServerIdentity::load_or_create(&key).unwrap();
    assert!(made);
    let public = id.public_key();
    let (ok, out, err) = run(&w, &["key-fingerprint"]);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        format!(
            "key {}\nfingerprint {}\n",
            seclogin::encode_key(&public),
            seclogin::fingerprint(&public)
        )
    );

    // Secure login off.
    std::fs::write(w.join("config/server.txt"), "secure_login_key =\n").unwrap();
    let (ok, _, err) = run(&w, &["key-fingerprint"]);
    assert!(!ok);
    assert!(err.contains("secure login is off"), "{err}");

    // A stray argument is refused.
    let (ok, _, err) = run(&w, &["key-fingerprint", "extra"]);
    assert!(!ok);
    assert!(err.contains("unknown argument `extra`"), "{err}");
    let _ = std::fs::remove_dir_all(&w);
}
