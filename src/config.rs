//! The world folder and its settings.
//!
//! ```text
//! <world>/
//!   config/server.txt            settings, `key = value` per line, `#` comments
//!   config/allowedversions.txt   the version allow-list (see `allow`)
//!   accounts/<account>.txt       one file per account (see `accounts`)
//! ```
//!
//! Every setting has a default, so an empty world folder runs: a lab
//! server on `127.0.0.1` that takes every build `seihai-core` knows.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Server settings, read from `config/server.txt`.
#[derive(Debug, Clone)]
pub struct Config {
    pub world: PathBuf,
    /// Server name, for logs (and later the listserver).
    pub name: String,
    /// Address to listen on. Default `127.0.0.1`: a server is reachable
    /// from other machines only when someone says so.
    pub bind: IpAddr,
    pub port: u16,
    /// Level a new account starts in, and its position in tiles.
    pub start_level: String,
    pub start_x: f32,
    pub start_y: f32,
    /// The status names sent at login (`PLO_STATUSLIST`), comma-separated.
    pub status_list: String,
    /// The welcome text sent at login (`PLO_STARTMESSAGE`); empty sends none.
    pub start_message: String,
    /// Make an account on its first login, with the password it gave.
    /// Off by default; meant for a lab.
    pub create_accounts: bool,
    /// How often the world time goes out to clients that read it. It keeps
    /// an idle link busy, so a dead peer is noticed. 0 turns it off.
    pub heartbeat: Duration,
    /// Close a connection that has sent nothing for this long. 0 (the
    /// default) never does: real clients standing still can stay quiet.
    pub idle_timeout: Duration,
    /// How long a new connection gets to send its login frame.
    pub login_timeout: Duration,
    /// Tile number the generated start board is filled with (stage 1 has
    /// no level files yet).
    pub board_tile: u16,
    pub max_players: usize,
    /// The Seihai secure login (`SEI00001`, Seihai's
    /// `documentation/secure-login.md`): the file with this server's
    /// Ed25519 key, relative to the world folder, made on first start.
    /// Empty: secure logins are refused.
    pub secure_login_key: PathBuf,
    /// Refuse logins that send the password in the clear (every real Graal
    /// client). Off by default.
    pub require_secure_login: bool,
}

impl Config {
    /// The defaults, for a world at `world`.
    pub fn defaults(world: &Path) -> Self {
        Config {
            world: world.to_path_buf(),
            name: "Seihai Lab".into(),
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 14802,
            start_level: "onlinestartlocal.nw".into(),
            start_x: 30.0,
            start_y: 30.5,
            status_list: "Online,Away,DND".into(),
            start_message: String::new(),
            create_accounts: false,
            heartbeat: Duration::from_secs(5),
            idle_timeout: Duration::ZERO,
            login_timeout: Duration::from_secs(10),
            board_tile: 0x1FF,
            max_players: 256,
            secure_login_key: PathBuf::from("config/seihai-login.key"),
            require_secure_login: false,
        }
    }

    /// Read `config/server.txt` in `world` (a missing file gives the
    /// defaults). Returns the settings and a warning per line not taken.
    pub fn load(world: &Path) -> std::io::Result<(Self, Vec<String>)> {
        let mut cfg = Self::defaults(world);
        let path = world.join("config").join("server.txt");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((cfg, Vec::new())),
            Err(e) => return Err(e),
        };
        let warnings = cfg.apply(&text);
        Ok((cfg, warnings))
    }

    /// Apply `key = value` lines. Returns a warning per line not taken.
    pub fn apply(&mut self, text: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                warnings.push(format!("server.txt line {}: no `=`", n + 1));
                continue;
            };
            if let Err(e) = self.set(k.trim(), v.trim()) {
                warnings.push(format!("server.txt line {}: {e}", n + 1));
            }
        }
        warnings
    }

    /// Set one setting by name.
    pub fn set(&mut self, key: &str, v: &str) -> Result<(), String> {
        fn num<T: std::str::FromStr>(v: &str) -> Result<T, String> {
            v.parse().map_err(|_| format!("`{v}` is not a number"))
        }
        fn flag(v: &str) -> Result<bool, String> {
            match v.to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => Ok(true),
                "false" | "no" | "off" | "0" => Ok(false),
                _ => Err(format!("`{v}` is not true or false")),
            }
        }
        let secs = |v: &str| {
            num::<f64>(v).and_then(|s| {
                Duration::try_from_secs_f64(s).map_err(|_| format!("`{v}` is not a duration"))
            })
        };
        match key {
            "name" => self.name = v.to_string(),
            "bind" => {
                self.bind = v
                    .parse()
                    .map_err(|_| format!("`{v}` is not an IP address"))?
            }
            "port" => self.port = num(v)?,
            "start_level" => self.start_level = v.to_string(),
            "start_x" => self.start_x = num(v)?,
            "start_y" => self.start_y = num(v)?,
            "status_list" => self.status_list = v.to_string(),
            "start_message" => self.start_message = v.to_string(),
            "create_accounts" => self.create_accounts = flag(v)?,
            "heartbeat" => self.heartbeat = secs(v)?,
            "idle_timeout" => self.idle_timeout = secs(v)?,
            "login_timeout" => self.login_timeout = secs(v)?,
            "board_tile" => self.board_tile = num::<u16>(v)? & 0x0FFF,
            "max_players" => self.max_players = num(v)?,
            "secure_login_key" => self.secure_login_key = PathBuf::from(v),
            "require_secure_login" => self.require_secure_login = flag(v)?,
            _ => return Err(format!("unknown setting `{key}`")),
        }
        Ok(())
    }

    pub fn accounts_dir(&self) -> PathBuf {
        self.world.join("accounts")
    }

    /// The secure-login key file, or `None` when secure logins are off.
    pub fn secure_login_key_path(&self) -> Option<PathBuf> {
        (!self.secure_login_key.as_os_str().is_empty())
            .then(|| self.world.join(&self.secure_login_key))
    }

    pub fn allowed_versions_path(&self) -> PathBuf {
        self.world.join("config").join("allowedversions.txt")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_read() {
        let mut c = Config::defaults(Path::new("w"));
        let w = c.apply("# comment\nport = 14810\nbind=0.0.0.0\ncreate_accounts = yes # lab\nheartbeat = 0.5\nnope\nfoo = 1\nport = x\n");
        assert_eq!(c.port, 14810);
        assert_eq!(c.bind, IpAddr::from([0, 0, 0, 0]));
        assert!(c.create_accounts);
        assert_eq!(c.heartbeat, Duration::from_millis(500));
        assert_eq!(w.len(), 3, "{w:?}");
    }

    #[test]
    fn defaults_are_loopback() {
        let c = Config::defaults(Path::new("w"));
        assert!(c.bind.is_loopback());
        assert!(!c.create_accounts);
        assert!(!c.require_secure_login);
        assert_eq!(
            c.secure_login_key_path(),
            Some(PathBuf::from("w/config/seihai-login.key"))
        );
        let mut c = c;
        c.apply("secure_login_key =\n");
        assert_eq!(c.secure_login_key_path(), None);
    }
}
