//! seihai-gserver: a Graal game server in Rust on Seihai's core.
//!
//! Stage 1 (see `docs/PLAN.md`): listen, log in every protocol generation,
//! check the version allow-list and the flat-file accounts, send the login
//! burst, then keep the idle session alive. Levels, other players and
//! everything else come in later stages; their modules are skeletons with
//! the plan in their doc comments. See `docs/ARCHITECTURE.md`.

pub mod accounts;
pub mod allow;
pub mod anticheat;
pub mod baddies;
pub mod bans;
pub mod board;
pub mod burst;
pub mod classes;
pub mod codec;
pub mod config;
pub mod coords;
pub mod files;
pub mod gmaps;
pub mod gpak;
pub mod graal3d;
pub mod horses;
pub mod import;
pub mod levels;
pub mod listlink;
pub mod login;
pub mod nc;
pub mod npclink;
pub mod npcs;
pub mod others;
pub mod ping;
pub mod png;
pub mod profile;
pub mod putnpc;
pub mod ratelimit;
pub mod rc;
pub mod rcaccounts;
pub mod rcfiles;
pub mod rcprops;
pub mod rcserver;
pub mod relay;
pub mod rules;
pub mod secure;
pub mod server;
pub mod shoot;
pub mod weapons;
pub mod wire;
pub mod wordfilter;
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
    if cfg.npcserver == config::NpcMode::External
        && !(16..=256).contains(&cfg.npcserver_token.len())
    {
        warnings.push(
            "npcserver = external needs npcserver_token of 16 to 256 characters; no NPC-server can log in".into(),
        );
    }
    if !cfg.listserver_hq_password.is_empty() && !cfg.listserver_secure {
        warnings.push(
            "listserver_hq_password goes to the listserver in the clear: listserver_secure is off"
                .into(),
        );
    }
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

/// Start the listserver link when `listserver` is set (see [`listlink`]).
/// Call it once the port is final: the listing carries it.
pub fn start_listlink(server: &mut server::Server) {
    server.listlink = listlink::ListLink::start(&server.cfg);
    if server.listlink.is_none() {
        eprintln!("listserver link: off");
    } else if server.cfg.listserver_accounts {
        eprintln!("listserver link: accounts are checked by the listserver");
    }
}

/// Opens the 3D side (`graal3d`) on UDP `port` with the world's
/// `config/graal3dworld.txt`, and prints the lines it did not take.
pub async fn start_graal3d(
    server: &Arc<server::Server>,
    g: &Arc<graal3d::Graal3d>,
    port: u16,
) -> std::io::Result<std::net::SocketAddr> {
    let path = server.cfg.world.join("config").join("graal3dworld.txt");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Graal3D: no config/graal3dworld.txt; the 3D world is empty");
            String::new()
        }
        Err(e) => return Err(e),
    };
    let mut refused = std::collections::BTreeSet::new();
    let mut scene = load_3d_world(server, "graal3dworld.txt", &text, &mut refused);
    scene.file = Some(path.clone());
    // More worlds: config/graal3dworlds/<name>.txt, entered from the 2D
    // level of that name or with a g3dworld trigger.
    let mut worlds = Vec::new();
    let dir = server.cfg.world.join("config").join("graal3dworlds");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            let is_txt = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt"));
            let Some(name) = p.file_stem().and_then(|n| n.to_str()).filter(|_| is_txt) else {
                continue;
            };
            let name = name.to_ascii_lowercase();
            if name.is_empty() || name == "default" {
                eprintln!("Graal3D: graal3dworlds/{name}.txt: that name is the default world's; not loaded");
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                eprintln!("Graal3D: cannot read graal3dworlds/{name}.txt");
                continue;
            };
            let label = format!("graal3dworlds/{name}.txt");
            let mut w = load_3d_world(server, &label, &text, &mut refused);
            w.file = Some(p.clone());
            eprintln!("Graal3D: 3D world {name} ({} objects)", w.objects.len());
            worlds.push((name, w));
        }
    }
    g.set_refused(refused);
    let trace = server.cfg.graal3d_trace.as_str();
    let opts = graal3d::service::Options {
        terrain_all: server.cfg.graal3d_terrainall,
        push_terrain: server.cfg.graal3d_pushterrain,
        worlds,
        trace: (!trace.is_empty()).then(|| std::path::PathBuf::from(trace)),
        sharp3d: server.cfg.graal3d_sharp3d,
        mutation: server.cfg.graal3d_mutate,
        edit: server.cfg.graal3d_edit,
        scope: server.cfg.graal3d_scope_options(),
        send_ms: server.cfg.graal3d_sendms,
    };
    if server.cfg.graal3d_edit {
        eprintln!("Graal3D: staff may edit the 3D worlds in game (graal3d_edit = staff); world files are saved with backups");
    }
    graal3d::bits::set_packet_string_prefix(server.cfg.graal3d_stringprefix);
    if !server.cfg.graal3d_stringprefix {
        eprintln!("Graal3D: game packet strings go without the prefix flag (graal3d_stringprefix = false, lab)");
    }
    let addr = std::net::SocketAddr::new(server.cfg.bind, port);
    g.start(
        addr,
        scene,
        server.cfg.world.clone(),
        Box::new(graal3d::service::ServerWorld(server.clone())),
        opts,
    )
    .await
}

/// One 3D world file read, its terrain loaded and its files checked; the
/// files it refuses are added to `refused`.
fn load_3d_world(
    server: &Arc<server::Server>,
    label: &str,
    text: &str,
    refused: &mut std::collections::BTreeSet<String>,
) -> graal3d::scene::Scene {
    let (mut scene, errors) = graal3d::scene::Scene::parse(text);
    for e in errors {
        eprintln!("Graal3D: {label} {e} (not understood)");
    }
    for note in scene.load_terrain(&server.cfg.world) {
        eprintln!("Graal3D: {label} terrain {note}");
    }
    // Every file the world names, looked up as a 3D client asks for it,
    // and checked for the faults that crashed real clients.
    let find = |name: &str| {
        server
            .files
            .find(name)
            .or_else(|| graal3d::extensionless_texture(&server.files, name))
            .map(|f| (f.name, f.data))
    };
    let heights_info = scene
        .terrain
        .as_ref()
        .and_then(|_| server.files.find("heights.info"))
        .map(|f| f.data);
    // Clients ask for heights.info by its bare name, so the one the file
    // folders find must be the terrain's own (and every world's terrain
    // shares it).
    if let Some(t) = &scene.terrain {
        let own = server.cfg.world.join(&t.folder).join("heights.info");
        if !own.is_file() && heights_info.is_some() {
            eprintln!(
                "Graal3D: ERROR {label} terrain: the heights.info the clients get is not the one in {}",
                t.folder
            );
        }
    }
    // Lab: the files of the rawobject and rawevent lines.
    let read = |name: &str| -> Option<Vec<u8>> {
        let p = std::path::Path::new(name);
        p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
            .then(|| std::fs::read(server.cfg.world.join(p)).ok())
            .flatten()
    };
    for o in &mut scene.objects {
        if let graal3d::objects::Ghost::Raw(r) = o {
            for e in graal3d::raw::load_files(&mut r.fields, &read) {
                eprintln!("Graal3D: {label} rawobject {}: {e}", r.class);
            }
        }
    }
    for ev in &mut scene.raw_events {
        for e in graal3d::raw::load_files(&mut ev.fields, &read) {
            eprintln!("Graal3D: {label} rawevent {}: {e}", ev.class);
        }
    }
    let checked = graal3d::assets::check_scene_files(
        &mut scene,
        &find,
        heights_info,
        server.cfg.graal3d_shapecheck,
    );
    for note in &checked.notes {
        eprintln!("Graal3D: {label}: {note}");
    }
    refused.extend(checked.refused);
    scene.push_files = checked.terrain_files;
    scene
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
    if server.cfg.npcserver != config::NpcMode::Off {
        let link_addr =
            std::net::SocketAddr::new(server.cfg.npcserver_bind, server.cfg.npcserver_port);
        let link = tokio::net::TcpListener::bind(link_addr).await?;
        let link_addr = link.local_addr()?;
        eprintln!(
            "NPC-server link ({:?}) listening on {link_addr}",
            server.cfg.npcserver
        );
        tokio::spawn(npclink::serve_link(server.npc.clone(), link));
        if server.cfg.npcserver == config::NpcMode::Auto {
            let binary = npclink::npcserver_binary(&server.cfg.npcserver_path);
            // The child runs here: an all-addresses bind is reached on loopback.
            let mut link_addr = link_addr;
            if link_addr.ip().is_unspecified() {
                link_addr.set_ip(std::net::IpAddr::from([127, 0, 0, 1]));
            }
            tokio::spawn(npclink::supervise(
                binary,
                link_addr,
                server.cfg.world.clone(),
                server.npc.token().to_vec(),
            ));
        }
    }
    if let Some(g) = &server.graal3d {
        let port = match server.cfg.graal3d_port {
            0 => listener.local_addr()?.port().wrapping_add(1),
            p => p,
        };
        let at = start_graal3d(&server, g, port).await?;
        eprintln!("Graal3D: listening on UDP {at}");
    }
    if server.cfg.lobby_port != 0 {
        let lobby_addr = std::net::SocketAddr::new(server.cfg.bind, server.cfg.lobby_port);
        let lobby = tokio::net::TcpListener::bind(lobby_addr).await?;
        let warp = server::LobbyWarp {
            name: server.cfg.name.clone(),
            host: server.cfg.lobby_host.clone(),
            port: listener.local_addr()?.port(),
        };
        eprintln!(
            "Login lobby listening on {}, sending 4.x to 6.x on to {}:{}",
            lobby.local_addr()?,
            warp.host,
            warp.port
        );
        tokio::spawn(server::serve_lobby(server.clone(), lobby, warp));
    }
    server::serve(server, listener).await
}
