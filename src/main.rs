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
  seihai-gserver account NAME --password-env VAR [--world DIR]
      make an account, or set its password, from the environment variable VAR
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
    if args.first().map(String::as_str) == Some("account") {
        return account_command(&world, &args[1..]);
    }
    match run_server(&world, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("seihai-gserver: {e}");
            ExitCode::FAILURE
        }
    }
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
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let server = Arc::new(server);
        tokio::select! {
            r = seihai_gserver::run(server) => r.map_err(|e| e.to_string()),
            _ = tokio::signal::ctrl_c() => {
                eprintln!("stopping");
                Ok(())
            }
        }
    })
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
