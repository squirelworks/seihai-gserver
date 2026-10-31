//! seihai-gserver: a Graal game server in Rust on Seihai's core.
//!
//! Stage 1 (see `docs/PLAN.md`): listen, log in every protocol generation,
//! check the version allow-list and the flat-file accounts, send the login
//! burst, then keep the idle session alive. Levels, other players and
//! everything else come in later stages; their modules are skeletons with
//! the plan in their doc comments. See `docs/ARCHITECTURE.md`.

pub mod accounts;
pub mod allow;
pub mod burst;
pub mod codec;
pub mod config;
pub mod files;
pub mod levels;
pub mod listlink;
pub mod login;
pub mod npclink;
pub mod rc;
pub mod secure;
pub mod server;
pub mod wire;
pub mod world;

use std::path::Path;
use std::sync::Arc;

/// Load a world folder: its settings and its allow-list. Returns the
/// server and the warnings to print.
pub fn load_world(world: &Path) -> std::io::Result<(server::Server, Vec<String>)> {
    let (cfg, mut warnings) = config::Config::load(world)?;
    let allow = match std::fs::read_to_string(cfg.allowed_versions_path()) {
        Ok(text) => {
            let (list, w) = allow::AllowList::parse(&text);
            warnings.extend(w);
            if list.is_empty() {
                warnings.push("allowedversions.txt lets no client in".into());
            }
            list
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            warnings.push(
                "no config/allowedversions.txt: every build seihai-core knows is allowed".into(),
            );
            allow::AllowList::all_known()
        }
        Err(e) => return Err(e),
    };
    Ok((server::Server::new(cfg, allow), warnings))
}

/// Load the secure-login key (`SEI00001`) named by `secure_login_key`,
/// making it on first start, and print its fingerprint. Does nothing when
/// the setting is empty. Kept out of [`load_world`] so that reading a world
/// never writes to it.
pub fn load_secure_identity(server: &mut server::Server) -> std::io::Result<()> {
    use seihai_core::protocol::seclogin::{self, ServerIdentity};
    let Some(path) = server.cfg.secure_login_key_path() else {
        eprintln!("secure login (SEI00001): off");
        return Ok(());
    };
    let (id, created) = ServerIdentity::load_or_create(&path).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("secure login key {}: {e}", path.display()),
        )
    })?;
    eprintln!(
        "secure login (SEI00001): {} key {}, fingerprint {}",
        if created { "made a new" } else { "loaded" },
        seclogin::encode_key(&id.public_key()),
        seclogin::fingerprint(&id.public_key())
    );
    server.identity = Some(id);
    Ok(())
}

/// Bind and serve until the listener fails.
pub async fn run(server: Arc<server::Server>) -> std::io::Result<()> {
    let addr = std::net::SocketAddr::new(server.cfg.bind, server.cfg.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!(
        "{} listening on {}",
        server.cfg.name,
        listener.local_addr()?
    );
    server::serve(server, listener).await
}
