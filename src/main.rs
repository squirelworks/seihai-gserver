//! The `seihai-gserver` command.

use seihai_gserver::accounts::{Account, Store};
use seihai_gserver::login::valid_account_name;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

const USAGE: &str = "\
usage:
  seihai-gserver [--world DIR] [--bind IP] [--port N] [--create-accounts] [--verbose]
      run the server (the world folder defaults to the current folder)
  seihai-gserver relay [--bind IP] [--port N] [--max-clients N]
      run the separate 1998 room/peer relay (default 127.0.0.1:14801);
      clients supply their own levels and game state; no accounts or listserver
  seihai-gserver account NAME --password-env VAR [--world DIR]
      make an account, or set its password, from the environment variable VAR
  seihai-gserver ban NAME [--for 30m|12h|3d|2w] [--reason TEXT] [--admin NAME] [--world DIR]
      ban an account, or with ip:ADDR or machine:ID an address or a machine
      (not for staff accounts); no --for: until unbanned; it takes effect at
      the next login
  seihai-gserver unban NAME [--world DIR]
      lift a ban (an account's lifts the machine bans the anti-cheat made with it)
  seihai-gserver key-fingerprint [--world DIR]
      print the secure-login (SEI00001) public key and its fingerprint; reads
      the key file and never makes one
  seihai-gserver import-v2 SRC [--world DIR]
      make a new world from a GServer-v2 server folder SRC: settings, the
      version list, accounts, staff, bans, levels and files
      (docs/GSERVER-V2-WORLD.md)
  seihai-gserver --version";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("seihai-gserver {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let world = take_value(&mut args, "--world")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    match args.first().map(String::as_str) {
        Some("account") => return account_command(&world, &args[1..]),
        Some("ban") => return ban_command(&world, &args[1..]),
        Some("unban") => return unban_command(&world, &args[1..]),
        Some("key-fingerprint") => return key_fingerprint_command(&world, &args[1..]),
        Some("import-v2") => return import_command(&world, &args[1..]),
        Some("relay") => {
            return match run_relay(&args[1..]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("seihai-gserver relay: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {}
    }
    match run_server(&world, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("seihai-gserver: {e}");
            ExitCode::FAILURE
        }
    }
}

fn relay_options(args: &[String]) -> Result<(std::net::SocketAddr, usize), String> {
    let mut bind = "127.0.0.1".parse::<std::net::IpAddr>().unwrap();
    let mut port = 14801;
    let mut max_clients = 128;
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        if !matches!(flag.as_str(), "--bind" | "--port" | "--max-clients") {
            return Err(format!("unknown relay argument `{flag}`"));
        }
        let value = args
            .next()
            .ok_or_else(|| format!("{flag}: needs a value"))?;
        match flag.as_str() {
            "--bind" => {
                bind = value
                    .parse()
                    .map_err(|_| "--bind: expected an IP address")?
            }
            "--port" => {
                port = value
                    .parse::<u16>()
                    .map_err(|_| "--port: expected 1 to 65535")?;
                if port == 0 {
                    return Err("--port: expected 1 to 65535".into());
                }
            }
            "--max-clients" => {
                max_clients = value
                    .parse::<usize>()
                    .map_err(|_| "--max-clients: expected 1 to 9999")?;
                if !(1..=9999).contains(&max_clients) {
                    return Err("--max-clients: expected 1 to 9999".into());
                }
            }
            _ => unreachable!(),
        }
    }
    Ok((std::net::SocketAddr::new(bind, port), max_clients))
}

fn run_relay(args: &[String]) -> Result<(), String> {
    let (address, max_clients) = relay_options(args)?;
    let config = seihai_gserver::relay::RelayConfig {
        max_clients,
        ..Default::default()
    };
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(address).await.map_err(|e| e.to_string())?;
        eprintln!("1998 peer relay listening on {}; local-level mode; no account/world service", listener.local_addr().map_err(|e| e.to_string())?);
        tokio::select! {
            result = seihai_gserver::relay::run(listener, config) => result.map_err(|e| e.to_string()),
            _ = stop_signal() => Ok(()),
        }
    })
}

fn take_value(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    args.remove(i);
    (i < args.len()).then(|| args.remove(i))
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

fn run_server(world: &std::path::Path, mut args: Vec<String>) -> Result<(), String> {
    let (mut server, warnings) =
        seihai_gserver::load_world(world).map_err(|e| format!("world {}: {e}", world.display()))?;
    for w in warnings {
        eprintln!("warning: {w}");
    }
    if let Some(b) = take_value(&mut args, "--bind") {
        server.cfg.set("bind", &b)?;
    }
    if let Some(p) = take_value(&mut args, "--port") {
        server.cfg.set("port", &p)?;
    }
    if take_flag(&mut args, "--create-accounts") {
        server.cfg.create_accounts = true;
    }
    server.verbose = take_flag(&mut args, "--verbose");
    seihai_gserver::load_secure_identity(&mut server).map_err(|e| e.to_string())?;
    if let Some(a) = args.first() {
        return Err(format!("unknown argument `{a}`\n{USAGE}"));
    }
    seihai_gserver::start_listlink(&mut server);
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let server = Arc::new(server);
        tokio::select! {
            r = seihai_gserver::run(server.clone()) => r.map_err(|e| e.to_string()),
            _ = stop_signal() => {
                // Players still online keep what they did, as on a logout.
                let n = server.world.save_all().await;
                eprintln!("stopping: saved {n} online player(s)");
                Ok(())
            }
        }
    })
}

/// Ctrl-C, or SIGTERM where there is one (a service manager's stop).
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn account_command(world: &std::path::Path, args: &[String]) -> ExitCode {
    let mut args = args.to_vec();
    let var = take_value(&mut args, "--password-env");
    let (Some(name), Some(var)) = (args.first().cloned(), var) else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    if !valid_account_name(&name) {
        eprintln!("`{name}`: account names are 1 to 32 letters, digits, `_`, `-`, `.` or `@`");
        return ExitCode::FAILURE;
    }
    let Ok(password) = std::env::var(&var) else {
        eprintln!("environment variable {var} is not set");
        return ExitCode::FAILURE;
    };
    let cfg = match seihai_gserver::config::Config::load(world) {
        Ok((c, _)) => c,
        Err(e) => {
            eprintln!("world {}: {e}", world.display());
            return ExitCode::FAILURE;
        }
    };
    let store = Store::new(cfg.accounts_dir());
    let result = store.load(&name).and_then(|a| {
        let made = a.is_none();
        let mut a =
            a.unwrap_or_else(|| Account::new(&name, &cfg.start_level, cfg.start_x, cfg.start_y));
        a.set_password(password.as_bytes());
        store.save(&a).map(|()| made)
    });
    match result {
        Ok(true) => eprintln!("account {name} made"),
        Ok(false) => eprintln!("account {name}: password set"),
        Err(e) => {
            eprintln!("account {name}: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn ban_command(world: &std::path::Path, args: &[String]) -> ExitCode {
    use seihai_gserver::bans::{parse_key, parse_span, Ban, Bans};
    let mut args = args.to_vec();
    let span = take_value(&mut args, "--for");
    let reason = take_value(&mut args, "--reason").unwrap_or_default();
    let admin = take_value(&mut args, "--admin").unwrap_or_else(|| "console".into());
    let (Some(name), None) = (args.first().cloned(), args.get(1)) else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let Some(name) = parse_key(&name) else {
        eprintln!("`{name}`: not an account name, ip:ADDR or machine:ID");
        return ExitCode::FAILURE;
    };
    let until = match span.as_deref().map(parse_span) {
        None => None,
        Some(Some(s)) => Some(seihai_gserver::bans::now() + s),
        Some(None) => {
            eprintln!("--for: expected a span such as 30m, 12h, 3d or 2w");
            return ExitCode::FAILURE;
        }
    };
    let bans = Bans::new(world);
    match bans.add(Ban {
        account: name.clone(),
        until,
        admin,
        reason,
        with: String::new(),
    }) {
        Ok(()) => {
            eprintln!("{name} banned ({})", bans.path().display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ban {name}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn unban_command(world: &std::path::Path, args: &[String]) -> ExitCode {
    let Some(name) = args.first() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    // `ip:` and `machine:` keys in the form the file keeps them.
    let key = seihai_gserver::bans::parse_key(name).unwrap_or_else(|| name.clone());
    match seihai_gserver::bans::Bans::new(world).remove(&key) {
        Ok(true) => {
            eprintln!("{name}: ban lifted");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            eprintln!("{name} is not banned");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("unban {name}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `key-fingerprint`: the key the server would load, without starting it
/// and without making a key when there is none.
fn key_fingerprint_command(world: &std::path::Path, args: &[String]) -> ExitCode {
    use seihai_core::protocol::seclogin::{self, ServerIdentity};
    if let Some(a) = args.first() {
        eprintln!("unknown argument `{a}`\n{USAGE}");
        return ExitCode::FAILURE;
    }
    let cfg = match seihai_gserver::config::Config::load(world) {
        Ok((c, _)) => c,
        Err(e) => {
            eprintln!("world {}: {e}", world.display());
            return ExitCode::FAILURE;
        }
    };
    let Some(path) = cfg.secure_login_key_path() else {
        eprintln!("secure login is off (secure_login_key is empty)");
        return ExitCode::FAILURE;
    };
    if !path.is_file() {
        eprintln!(
            "no secure login key at {} yet; the server makes it on its first start",
            path.display()
        );
        return ExitCode::FAILURE;
    }
    match ServerIdentity::load_or_create(&path) {
        Ok((id, _)) => {
            let key = id.public_key();
            println!("key {}", seclogin::encode_key(&key));
            println!("fingerprint {}", seclogin::fingerprint(&key));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("secure login key {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}

fn import_command(world: &std::path::Path, args: &[String]) -> ExitCode {
    let [src] = args else {
        eprintln!("usage: seihai-gserver import-v2 SRC [--world DIR]");
        return ExitCode::FAILURE;
    };
    match seihai_gserver::import::import_v2(std::path::Path::new(src), world) {
        Ok(r) => {
            for d in r.done {
                println!("wrote {d}");
            }
            for l in r.left {
                println!("left: {l}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("import-v2: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod relay_cli_tests {
    use super::relay_options;

    #[test]
    fn relay_is_loopback_by_default_and_requires_explicit_valid_overrides() {
        let (address, limit) = relay_options(&[]).unwrap();
        assert_eq!(address.to_string(), "127.0.0.1:14801");
        assert_eq!(limit, 128);
        let args = ["--bind", "::1", "--port", "14802", "--max-clients", "8"].map(String::from);
        assert_eq!(
            relay_options(&args).unwrap(),
            ("[::1]:14802".parse().unwrap(), 8)
        );
        for args in [
            vec!["--port"],
            vec!["--port", "0"],
            vec!["--bind", "localhost"],
            vec!["--max-clients", "0"],
            vec!["--max-clients", "10000"],
            vec!["--create-accounts"],
        ] {
            assert!(
                relay_options(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err()
            );
        }
    }
}
