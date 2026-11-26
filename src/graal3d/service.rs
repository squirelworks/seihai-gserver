//! The 3D side of the server: the Torque UDP server on port + 1.
//!
//! A Graal 3D server is two servers on one host: the gserver on TCP port
//! *P*, and a Torque UDP server on *P* + 1, which the 3.153 to 5.x clients
//! open beside the TCP login when the listed name starts with `3 `. This
//! task is that UDP server. It accepts a 3D link only for an account
//! already logged in over TCP from the same address (a 5.x guest sends
//! `guest`, so a guest takes the newest guest login from there), keeps the
//! two together (the 3D link closes with the TCP session), drives the
//! mission start through [`Connection`], and bridges the text channel:
//!
//! - from the client, `events` lines: `s name=value` (a flag), `w weapon`
//!   (getweapon), `t weapon,<name>,<params>` (a serverside trigger), and
//!   `t <kind>,<weapon>,g3dpos,x,y,z`, which moves the player's own object;
//! - to the client: its flags at link time, then every `PLO_FLAGSET` and
//!   `PLO_FLAGDEL` it is sent over TCP, as `set` / `unset` texts (a client
//!   in 3D mode ignores the TCP flag packets).
//!
//! The world (GameObject era): every client gets the objects of
//! `config/graal3dworld.txt` as ghosts, plus one GameObject per linked
//! player, `G3DPlayer_<account>`. A player's own object becomes its
//! control object. Each client reports its position in every packet; the
//! service moves that player's object on everyone else's screen and
//! passes its chat on. A protocol 19 client gets a Sky and a stock TGE
//! Player (or Camera) of its own instead, and every other linked player as
//! a stock TGE Player; its moves reach the others the same way.

use super::caps::{self, Caps};
use super::conn::{ClientState, Connection, Key, Mission, Out};
use super::handshake::{self, ConnectRequest};
use super::mutate::{self, Mutation};
use super::objects::{bits, flags, GameObject, Ghost, Sky, Transform};
use super::scene::{Scene, TgeControl};
use super::tge;
use crate::wire::ids::{PLO_FLAGDEL, PLO_FLAGSET, PLO_RAWDATA};
use seihai_core::protocol::ClientVersion;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

/// How often the service runs every connection.
const FRAME: Duration = Duration::from_millis(10);

/// After the server moves a player's object, its client's own reports of
/// its position are ignored this long: an older report would put it back.
const SERVER_MOVE_HOLD_MS: u64 = 2_000;

/// Whether a build has a Torque 3D side: 3.153 (and the 3.151 that stands
/// in as it) to the 6.00 builds. 3.04 speaks the stock TGE demo protocol
/// and is refused by the game check; the connect request's protocol picks
/// the layer.
pub fn has_graal3d(v: ClientVersion) -> bool {
    use ClientVersion::*;
    matches!(
        v,
        V3153
            | V404R2
            | V404R2Mac
            | V420R6
            | V420R8
            | V500R7
            | V512
            | V512Mac
            | V524R4
            | V530R3
            | V531R4
            | V600R2
            | V600R4
    )
}

/// A TCP login the 3D side may link to.
#[derive(Debug, Clone)]
pub struct TcpPlayer {
    /// Lower case.
    pub account: String,
    pub guest: bool,
    pub ip: IpAddr,
    pub nickname: Vec<u8>,
    /// `name=value` or `name`, as the account keeps them.
    pub flags: Vec<String>,
    /// The login's client token (`G3D14097`, ...) and the build's label
    /// (`5.12`): they pick the client's row in [`super::caps`].
    pub token: String,
    pub label: String,
}

/// The 3D side's options (`graal3d_*` in `config/server.txt`), and what
/// the start-up check found.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// `graal3d_terrainall`: the 5.x TerrainBlock, WaterBlock, ground
    /// cover and chunks go to every GameObject-era client, whatever its
    /// build's row says.
    pub terrain_all: bool,
    /// `graal3d_pushterrain`: right after a 3D link, `heights.info` and
    /// its textures go to the client over TCP unasked, once per account a
    /// run (5.12 fetches the textures only on its second visit).
    pub push_terrain: bool,
    /// What the push sends: [`super::assets::Checked::terrain_files`].
    pub terrain_files: Vec<String>,
    /// `graal3d_trace`: a file that gets one tab-separated line for each
    /// link, each event and control object change a client sends, and
    /// each 2D player shown, for multi-client labs.
    pub trace: Option<PathBuf>,
    /// `graal3d_route = sharp3d`: protocol 24 clients get the Sharp3D
    /// route.
    pub sharp3d: bool,
    /// `graal3d_mutate`: lab only, one seeded mutation a link.
    pub mutation: Option<(Mutation, u32)>,
}

enum Cmd {
    /// The TCP session is over.
    Left(u16),
    /// A flag the player was sent over TCP: set (`name=value`) or unset.
    Flag {
        player: u16,
        set: bool,
        text: Vec<u8>,
    },
}

#[derive(Default)]
struct Shared {
    players: HashMap<u16, TcpPlayer>,
    linked: HashSet<u16>,
    /// Lower-case names of the files a 3D client is told the server does
    /// not have (see [`super::assets::Checked::refused`]).
    refused: std::collections::BTreeSet<String>,
}

/// The handle the rest of the server holds.
pub struct Graal3d {
    shared: Mutex<Shared>,
    tx: mpsc::UnboundedSender<Cmd>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Cmd>>>,
}

impl Default for Graal3d {
    fn default() -> Self {
        Self::new()
    }
}

impl Graal3d {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Graal3d {
            shared: Mutex::new(Shared::default()),
            tx,
            rx: Mutex::new(Some(rx)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Sets the files a 3D client is told the server does not have.
    pub fn set_refused(&self, names: std::collections::BTreeSet<String>) {
        self.lock().refused = names;
    }

    /// Whether `name` (any folder, any case) is a refused file.
    pub fn refuses(&self, name: &str) -> bool {
        let s = self.lock();
        if s.refused.is_empty() {
            return false;
        }
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        s.refused.contains(&base.to_ascii_lowercase())
    }

    /// A 3D-capable client finished its TCP login.
    pub fn tcp_joined(&self, id: u16, player: TcpPlayer) {
        self.lock().players.insert(id, player);
    }

    /// Its TCP session ended; its 3D link goes too.
    pub fn tcp_left(&self, id: u16) {
        let had = self.lock().players.remove(&id).is_some();
        if had {
            let _ = self.tx.send(Cmd::Left(id));
        }
    }

    pub fn is_linked(&self, id: u16) -> bool {
        self.lock().linked.contains(&id)
    }

    /// Frames about to go to player `id` over TCP: its flag changes go to
    /// its 3D link too, and are kept for a link opened later.
    pub fn mirror(&self, id: u16, frames: &[Vec<u8>]) {
        let changes = flag_messages(frames);
        if changes.is_empty() {
            return;
        }
        let linked = {
            let mut s = self.lock();
            if let Some(p) = s.players.get_mut(&id) {
                for (set, text) in &changes {
                    let t = String::from_utf8_lossy(text).into_owned();
                    let name = t.split('=').next().unwrap_or("").to_string();
                    p.flags
                        .retain(|f| f.split('=').next() != Some(name.as_str()));
                    if *set {
                        p.flags.push(t);
                    }
                }
            }
            s.linked.contains(&id)
        };
        if linked {
            for (set, text) in changes {
                let _ = self.tx.send(Cmd::Flag {
                    player: id,
                    set,
                    text,
                });
            }
        }
    }

    /// Opens the UDP side on `addr` and runs it until the server ends.
    pub async fn start(
        self: &Arc<Self>,
        addr: SocketAddr,
        scene: Scene,
        world_dir: PathBuf,
        world: Box<dyn WorldInput>,
        opts: Options,
    ) -> std::io::Result<SocketAddr> {
        let socket = UdpSocket::bind(addr).await?;
        let local = socket.local_addr()?;
        let rx = self
            .rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| std::io::Error::other("the 3D side is already running"))?;
        let service = Service::new(self.clone(), scene, world_dir, world).with_options(opts);
        tokio::spawn(service.run(socket, rx));
        Ok(local)
    }
}

/// A `weapon3d` script as the GS2 bytecode the client runs, compiled by
/// seihai-core's own compiler. The text after a `//#CLIENTSIDE` line, if
/// there is one; GS2 unless the script starts with `//#GS1`. The bytecode
/// ends with one `\n`, as weapon bytecode does on TCP.
pub fn compile_weapon3d(source: &str) -> Result<Vec<u8>, String> {
    use seihai_core::scripting::gs2::compile::{compile, Language, Mode};
    let source = match source.find("//#CLIENTSIDE") {
        Some(i) => source[i..].split_once('\n').map_or("", |(_, rest)| rest),
        None => source,
    };
    let text = source.trim();
    let language = if text.starts_with("//#GS1") {
        Language::Gs1(crate::npcs::LEVEL_ERA)
    } else {
        Language::Gs2
    };
    // Legacy mode takes the loose forms the live servers let through.
    let compiled = compile(text, language, Mode::Legacy).map_err(|e| e.to_string())?;
    let mut code = compiled.script.write();
    code.push(b'\n');
    Ok(code)
}

/// `PLO_FLAGSET` and `PLO_FLAGDEL` bodies in gen-5 frames, skipping raw
/// data. A raw message (a file) may run on over several frames: the bytes
/// still to skip carry over to the next frame, so a file's bytes are never
/// read as messages (a pushed terrain PNG cut into 32000-byte frames sent
/// its bytes to a 5.12 as flags, lab 2026-11-25).
fn flag_messages(frames: &[Vec<u8>]) -> Vec<(bool, Vec<u8>)> {
    let mut out = Vec::new();
    let mut skip = 0usize;
    for frame in frames {
        let mut rest = frame.as_slice();
        if skip > 0 {
            let n = skip.min(rest.len());
            rest = &rest[n..];
            skip -= n;
        }
        while let Some(&first) = rest.first() {
            let id = first.wrapping_sub(32);
            let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
            // An empty line (a newline first) has no id: skip it.
            let body = rest.get(1..end).unwrap_or(&[]);
            rest = rest.get(end + 1..).unwrap_or(&[]);
            if end == 0 {
                continue;
            }
            match id {
                PLO_FLAGSET => out.push((true, body.to_vec())),
                PLO_FLAGDEL => out.push((false, body.to_vec())),
                PLO_RAWDATA => {
                    // The next message is this long, newline and all.
                    let n = seihai_core::protocol::read_gint3(body).unwrap_or(0) as usize;
                    let here = n.min(rest.len());
                    rest = &rest[here..];
                    skip = n - here;
                }
                _ => {}
            }
        }
    }
    out
}

/// What the service asks of the game world.
pub trait WorldInput: Send {
    /// A flag set (`name=value`) from the client's 3D scripts.
    fn flag(&self, player: u16, text: String);
    /// `PLI_TRIGGERACTION`, as a TCP client would send it.
    fn trigger_action(&self, player: u16, raw: &[u8]);
    /// The client's 3D scripts took a weapon (getweapon).
    fn give_weapon(&self, player: u16, name: &[u8]);
    /// The player's TCP position follows its 3D object: classic `X`/`Y`
    /// props, in half tiles, as the player's own prop update.
    fn position(&self, player: u16, raw: &[u8]);
    /// The player's chat prop, from what it said in 3D.
    fn chat(&self, player: u16, text: &[u8]);
    /// The other players in each of these players' levels: 2D players are
    /// shown on 3D screens. `None`: not asked (tests).
    fn level_roster(&self, _players: Vec<u16>) -> Option<oneshot::Receiver<crate::world::Roster>> {
        None
    }
    /// Sends these files to the player over TCP, unasked.
    fn push_files(&self, _player: u16, _names: &[String]) {}
}

/// The server's world task, for the running service.
pub struct ServerWorld(pub Arc<crate::server::Server>);

impl WorldInput for ServerWorld {
    fn flag(&self, player: u16, text: String) {
        self.0.world.flag(player, text, true);
    }
    fn trigger_action(&self, player: u16, raw: &[u8]) {
        self.0.world.trigger_action(player, raw);
    }
    fn give_weapon(&self, player: u16, name: &[u8]) {
        self.0.world.give_weapon(player, name);
    }
    fn position(&self, player: u16, raw: &[u8]) {
        self.0.world.props(player, raw, None);
    }
    fn chat(&self, player: u16, text: &[u8]) {
        // CURCHAT (12): a GCHAR length, then the text (at most 223 bytes).
        let text = &text[..text.len().min(223)];
        let mut raw = vec![12 + 32, text.len() as u8 + 32];
        raw.extend_from_slice(text);
        self.0.world.props(player, &raw, None);
    }
    fn level_roster(&self, players: Vec<u16>) -> Option<oneshot::Receiver<crate::world::Roster>> {
        Some(self.0.world.level_roster(players))
    }
    fn push_files(&self, player: u16, names: &[String]) {
        let files = names
            .iter()
            .filter_map(|n| super::client_file(&self.0.files, n, self.0.graal3d.as_deref()))
            .collect();
        self.0.world.push_files(player, files);
    }
}

struct Session {
    player: u16,
    account: String,
    conn: Connection,
    /// What the client's build takes.
    caps: Option<Caps>,
    /// The 2D players of its level, as last put on its screen.
    shown_2d: BTreeMap<u16, GameObject>,
}

const AVATAR: Key = 0x1_0000;
const CAMERA: Key = 0x2_0000;

/// Where a protocol 21/24 client's Camera sits from the spawn point when the
/// world file has no `camera` line: behind it and above.
pub const DEFAULT_CAMERA_OFFSET: [f32; 3] = [0.0, -5.0, 2.0];
/// The plain Sky of protocols 19 and 21, when the world has none.
const PLAIN_SKY: Key = 0x3_0000;
/// The streamed terrain's TerrainBlock (protocols 21 and 24).
const TERRAIN: Key = 0x4_0000;
/// A 2D (TCP only) player in a 3D player's level.
const PLAYER_2D: Key = 0x5_0000;
/// How often the 2D players are looked up, in milliseconds.
const ROSTER_MS: u64 = 200;

fn player_2d_key(p: u16) -> Key {
    PLAYER_2D + Key::from(p)
}

/// Where the `n`th player to join stands (0: the spawn point). Later ones
/// take the first point no linked player stands on, of 8 points 2 units
/// out, then 16 at 4 units, and so on, so two players never share a point.
pub fn spawn_point(spawn: [f32; 3], taken: &[[f32; 3]]) -> [f32; 3] {
    let free = |p: [f32; 3]| {
        taken
            .iter()
            .all(|t| (t[0] - p[0]).hypot(t[1] - p[1]) >= 1.0)
    };
    if free(spawn) {
        return spawn;
    }
    for ring in 1..=32u32 {
        let points = 8 * ring;
        let radius = 2.0 * ring as f32;
        for i in 0..points {
            let a = std::f32::consts::TAU * i as f32 / points as f32;
            let p = [
                spawn[0] + radius * a.cos(),
                spawn[1] + radius * a.sin(),
                spawn[2],
            ];
            if free(p) {
                return p;
            }
        }
    }
    spawn
}

/// A Torque (x, y) as a TCP tile position: tile (x, -y). `None` off the
/// 64 x 64 map.
pub fn tile_position(p: [f32; 3]) -> Option<(f32, f32)> {
    let (x, y) = (p[0], -p[1]);
    ((0.0..64.0).contains(&x) && (0.0..64.0).contains(&y)).then_some((x, y))
}

/// The Skies a protocol 19 or 21 client gets: the world's, each with its
/// material list padded to the 7 names the client reads, or a plain Sky
/// when the world has none. Both builds read the scene's Sky and its
/// seventh name without a null check (4.04r2 and 4.20r6 Mac labs).
fn padded_skies(objects: &[Ghost]) -> Vec<(Key, Sky)> {
    let mut out: Vec<(Key, Sky)> = objects
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match o {
            Ghost::Sky(k) => Some((
                i as Key + 1,
                Sky {
                    material_list: tge::sky_material_list(&k.material_list, b"g3d_sky"),
                    ..k.clone()
                },
            )),
            _ => None,
        })
        .collect();
    if out.is_empty() {
        out.push((
            PLAIN_SKY,
            Sky {
                bools: [0, 0, 0],
                material_list: tge::sky_material_list(b"", b"g3d_sky"),
                ..Sky::default()
            },
        ));
    }
    out
}

fn avatar_key(p: u16) -> Key {
    AVATAR + u32::from(p)
}

/// A protocol 21/24 Camera: `offset` from the spawn point, turned by
/// `rotation` (axis, angle) or else facing the spawn point.
pub fn camera_transform(
    spawn: [f32; 3],
    offset: [f32; 3],
    rotation: Option<[f32; 4]>,
) -> Transform {
    let mut at = spawn;
    for (p, o) in at.iter_mut().zip(offset) {
        *p += o;
    }
    match rotation {
        Some(r) => Transform {
            position: at,
            axis: [r[0], r[1], r[2]],
            angle: r[3],
        },
        None => Transform::facing(at, spawn),
    }
}

fn camera_key(p: u16) -> Key {
    CAMERA + u32::from(p)
}

/// The UDP side's own state; owned by its task, or by a test.
pub struct Service {
    hub: Arc<Graal3d>,
    scene: Scene,
    world_dir: PathBuf,
    world: Box<dyn WorldInput>,
    secret: [u8; 32],
    started: Instant,
    sessions: HashMap<SocketAddr, Session>,
    by_player: HashMap<u16, SocketAddr>,
    avatars: BTreeMap<u16, GameObject>,
    /// Protocol 19: each linked player's TGE Player state as last sent to
    /// the others (seihai-gserver#188).
    tge_seen: BTreeMap<u16, tge::PlayerState>,
    moved_until: HashMap<u16, u64>,
    /// After a server move: the position the client was still reporting.
    /// Its reports of that spot are set aside until it reports another.
    stale_report: HashMap<u16, [f32; 3]>,
    /// The last position each client reported.
    last_report: HashMap<u16, [f32; 3]>,
    /// The half-tile TCP position last set for each player.
    tcp_position: HashMap<u16, (u8, u8)>,
    unmatched: HashSet<String>,
    /// Datagrams to send, for the task (or a test) to take.
    outbox: Vec<(SocketAddr, Vec<u8>)>,
    opts: Options,
    trace_file: Option<std::fs::File>,
    /// Accounts already sent the terrain's files (`graal3d_pushterrain`).
    pushed_to: HashSet<String>,
    roster: Option<oneshot::Receiver<crate::world::Roster>>,
    next_roster: u64,
}

impl Service {
    pub fn new(
        hub: Arc<Graal3d>,
        scene: Scene,
        world_dir: PathBuf,
        world: Box<dyn WorldInput>,
    ) -> Self {
        let mut secret = [0u8; 32];
        let _ = getrandom::getrandom(&mut secret);
        Service {
            hub,
            scene,
            world_dir,
            world,
            secret,
            started: Instant::now(),
            sessions: HashMap::new(),
            by_player: HashMap::new(),
            avatars: BTreeMap::new(),
            tge_seen: BTreeMap::new(),
            moved_until: HashMap::new(),
            stale_report: HashMap::new(),
            last_report: HashMap::new(),
            tcp_position: HashMap::new(),
            unmatched: HashSet::new(),
            outbox: Vec::new(),
            opts: Options::default(),
            trace_file: None,
            pushed_to: HashSet::new(),
            roster: None,
            next_roster: 0,
        }
    }

    /// Sets the options; opens the trace file.
    pub fn with_options(mut self, opts: Options) -> Self {
        if let Some(path) = &opts.trace {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                Ok(f) => {
                    log(&format!("tracing every 3D client to {}", path.display()));
                    self.trace_file = Some(f);
                }
                Err(e) => log(&format!(
                    "cannot open graal3d_trace file {}: {e}",
                    path.display()
                )),
            }
        }
        if let Some((m, seed)) = opts.mutation {
            log(&format!(
                "LAB MUTATION {}:{seed} on every 3D link (graal3d_mutate). Never on a public server.",
                m.name()
            ));
        }
        if opts.sharp3d {
            log("protocol 24 clients get the Sharp3D route (graal3d_route = sharp3d)");
        }
        self.opts = opts;
        self
    }

    /// One trace line: milliseconds, the account, the protocol (0: the
    /// server's own relay), what happened.
    fn trace(&mut self, account: &str, protocol: u32, what: &str) {
        use std::io::Write;
        let now = self.now();
        if let Some(f) = &mut self.trace_file {
            let _ = writeln!(f, "{now}\t{account}\t{protocol}\t{what}");
        }
    }

    /// Whether a client with `caps` on `protocol` takes the 5.x terrain.
    fn takes_terrain(&self, caps: Option<&Caps>, tge: bool) -> bool {
        caps.is_some_and(Caps::takes_terrain) || (self.opts.terrain_all && !tge)
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    async fn run(mut self, socket: UdpSocket, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        let mut buf = vec![0u8; 2048];
        let mut frame = tokio::time::interval(FRAME);
        frame.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                r = socket.recv_from(&mut buf) => {
                    if let Ok((n, from)) = r {
                        let now = self.now();
                        self.datagram(from, &buf[..n], now);
                    }
                }
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Left(id)) => self.player_left(id),
                    Some(Cmd::Flag { player, set, text }) => self.flag_to_client(player, set, &text),
                    None => break,
                },
                _ = frame.tick() => {
                    let now = self.now();
                    self.update(now);
                }
            }
            for (to, data) in self.outbox.drain(..) {
                let _ = socket.send_to(&data, to).await;
            }
        }
        for (addr, s) in &self.sessions {
            let _ = socket
                .send_to(
                    &s.conn.disconnect_packet(b"The server is shutting down."),
                    addr,
                )
                .await;
        }
    }

    /// Datagrams to send since the last call.
    pub fn take_outbox(&mut self) -> Vec<(SocketAddr, Vec<u8>)> {
        std::mem::take(&mut self.outbox)
    }

    fn digest(&self, from: &SocketAddr, sequence: u32) -> handshake::Digest {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(self.secret);
        h.update(from.to_string().as_bytes());
        h.update(sequence.to_le_bytes());
        let d = h.finalize();
        let mut out = [0u32; 4];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u32::from_le_bytes(d[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
        }
        out
    }

    /// One datagram from `from`.
    pub fn datagram(&mut self, from: SocketAddr, data: &[u8], now: u64) {
        let Some(&first) = data.first() else {
            return;
        };
        if handshake::is_data_packet(data) {
            if let Some(s) = self.sessions.get_mut(&from) {
                s.conn.receive(data, now);
            }
            self.drain(from);
            return;
        }
        match first {
            handshake::kind::CHALLENGE_REQUEST => {
                if let Some(seq) = handshake::read_challenge_request(data) {
                    let d = self.digest(&from, seq);
                    self.outbox
                        .push((from, handshake::challenge_response(seq, &d)));
                }
            }
            handshake::kind::CONNECT_REQUEST => self.connect_request(from, data, now),
            handshake::kind::DISCONNECT => {
                let seq = handshake::read_disconnect(data);
                if self.sessions.get(&from).map(|s| s.conn.sequence()) == seq && seq.is_some() {
                    self.close(from, "client disconnected", false);
                }
            }
            _ => {} // Torque's master-server and LAN queries are not answered
        }
    }

    /// The TCP login a connect request belongs to.
    fn find_tcp_player(&self, requested: &str, ip: IpAddr) -> Option<(u16, TcpPlayer)> {
        let s = self.hub.lock();
        let same_ip = |p: &TcpPlayer| canonical(p.ip) == canonical(ip);
        if let Some((id, p)) = s
            .players
            .iter()
            .find(|(_, p)| p.account == requested && same_ip(p))
        {
            return Some((*id, p.clone()));
        }
        if requested != "guest" {
            return None;
        }
        // A 5.x guest sends "guest"; the gserver names guests pc:<n>. Take
        // a guest from the same address, one without a link first, the
        // newest login on a tie.
        s.players
            .iter()
            .filter(|(_, p)| p.guest && same_ip(p))
            .max_by_key(|(id, _)| (!self.by_player.contains_key(id), **id))
            .map(|(id, p)| (*id, p.clone()))
    }

    fn connect_request(&mut self, from: SocketAddr, data: &[u8], now: u64) {
        let Some(req) = ConnectRequest::read(data) else {
            return;
        };
        if req.digest != self.digest(&from, req.sequence) {
            return;
        }
        // A repeat of a request already accepted: the accept again.
        if let Some(s) = self.sessions.get(&from) {
            if s.conn.sequence() == req.sequence {
                let accept = self.accept(req.sequence, s.conn.profile()).0;
                self.outbox.push((from, accept));
                return;
            }
            self.close(from, "reconnected", false);
        }
        if let Some(reason) = req.check() {
            self.outbox.push((
                from,
                handshake::connect_reject(req.sequence, reason.as_bytes()),
            ));
            return;
        }
        let mut profile = handshake::profile(req.current_protocol).expect("checked");
        if self.opts.sharp3d {
            profile = handshake::sharp3d_profile(req.current_protocol).unwrap_or(profile);
        }
        let requested = String::from_utf8_lossy(req.account()).to_ascii_lowercase();
        // The client opens TCP and UDP together, and the TCP login may not
        // be done yet. Until it is, say nothing: the client repeats.
        let Some((id, player)) = self.find_tcp_player(&requested, from.ip()) else {
            if self.unmatched.len() > 1024 {
                self.unmatched.clear();
            }
            if self.unmatched.insert(format!("{},{requested}", from.ip())) {
                log(&format!(
                    "{requested} asked for a 3D link from {}, but no TCP login from there matches it (yet)",
                    from.ip()
                ));
            }
            return;
        };
        // One link per player.
        if let Some(old) = self.by_player.get(&id).copied() {
            self.close(old, "Another 3D connection was opened.", true);
        }
        let mut conn = Connection::new(profile, req.sequence, now);
        conn.set_tracing(self.trace_file.is_some());
        conn.set_mutation(self.opts.mutation);
        let caps = caps::for_client(&player.token, &player.label, profile.protocol);
        if let Some(c) = &caps {
            log(&format!("{} is {}", player.account, c.describe()));
        }
        self.sessions.insert(
            from,
            Session {
                player: id,
                account: player.account.clone(),
                conn,
                caps,
                shown_2d: BTreeMap::new(),
            },
        );
        self.by_player.insert(id, from);
        self.hub.lock().linked.insert(id);

        let taken: Vec<[f32; 3]> = self
            .avatars
            .iter()
            .filter(|(p, _)| **p != id)
            .map(|(_, a)| a.transform.position)
            .collect();
        let mut start = self.scene.spawn;
        start.position = spawn_point(start.position, &taken);
        let avatar = GameObject {
            name: format!("G3DPlayer_{}", player.account).into_bytes(),
            shape: self.scene.player_shape.clone(),
            transform: start,
            nickname: player.nickname.clone(),
            flags: flags::SHADOW,
            movetype: self.scene.player_movetype.clone(),
            ..GameObject::default()
        };
        self.avatars.insert(id, avatar.clone());
        self.push_terrain(id, &player.account, caps.as_ref(), profile);
        self.ghost_world(from);
        self.follow_on_tcp(id);
        self.broadcast_avatar(id, Some(from));
        let (accept, said) = self.accept(req.sequence, profile);
        if !said.is_empty() {
            let hex: String = accept.iter().map(|b| format!("{b:02x}")).collect();
            let line = format!("{said}; packet {hex}");
            log(&format!("{}: {line}", player.account));
            self.trace(&player.account, profile.protocol, &line);
        }
        self.outbox.push((from, accept));
        log(&format!(
            "{} linked from {} (protocol {}, {})",
            player.account,
            from.ip(),
            profile.protocol,
            profile.clients
        ));
        let at = start.position;
        let line = format!(
            "linked from {} ({}); {} other 3D players; spawn {:.2},{:.2},{:.2}",
            from.ip(),
            profile.clients,
            self.sessions.len() - 1,
            at[0],
            at[1],
            at[2]
        );
        self.trace(&player.account, profile.protocol, &line);
        self.drain_all();
    }

    /// The connect accept, with an accept mutation's changes and a line
    /// saying what they are (empty without one).
    fn accept(&self, sequence: u32, profile: &'static handshake::Profile) -> (Vec<u8>, String) {
        match self.opts.mutation.filter(|(m, _)| m.in_accept()) {
            Some((m, seed)) => {
                let (tweak, said) = mutate::accept_tweak(m, seed, profile);
                (
                    handshake::connect_accept_tweaked(sequence, profile, &tweak),
                    format!("mutation {}:{seed} in the connect accept: {said}", m.name()),
                )
            }
            None => (handshake::connect_accept(sequence, profile), String::new()),
        }
    }

    /// `graal3d_pushterrain`: the terrain's files over TCP, once per
    /// account a run, to a build that takes the terrain and reads its
    /// textures (4.20r8 never fetches them).
    fn push_terrain(
        &mut self,
        id: u16,
        account: &str,
        caps: Option<&Caps>,
        profile: &handshake::Profile,
    ) {
        if !self.opts.push_terrain
            || self.scene.terrain.is_none()
            || !self.takes_terrain(caps, profile.tge)
        {
            return;
        }
        if caps.is_some_and(|c| !c.row.terrain_textures) {
            self.trace(
                account,
                profile.protocol,
                "no terrain file push: this build never fetches terrain textures or heights.info",
            );
            return;
        }
        if !self.pushed_to.insert(account.to_string()) {
            return;
        }
        let files = self.opts.terrain_files.clone();
        self.world.push_files(id, &files);
        let line = format!(
            "pushed {} terrain files over TCP (graal3d_pushterrain)",
            files.len()
        );
        log(&format!("{account}: {line}"));
        self.trace(account, profile.protocol, &line);
    }

    fn ghost_world(&mut self, at: SocketAddr) {
        // Every other linked player, as a stock TGE Player for a protocol 19
        // client (the GameObject form goes to the others).
        let me = self.sessions.get(&at).map(|s| s.player);
        let others: Vec<(u16, Ghost)> = self
            .avatars
            .keys()
            .filter(|p| Some(**p) != me)
            .filter_map(|p| self.tge_avatar(*p).map(|g| (*p, g)))
            .collect();
        let start = me
            .and_then(|p| self.avatars.get(&p))
            .map_or(self.scene.spawn.position, |a| a.transform.position);
        let scene = &self.scene;
        let s = self.sessions.get_mut(&at).expect("just added");
        let id = s.player;
        if s.conn.profile().tge {
            // 3.153 and 4.04r2: a Sky (the world's, else a plain one: 4.04r2
            // reads the scene's Sky without a null check) and one Player, or
            // with `tgecontrol camera` a Camera, as the control object.
            for (key, sky) in padded_skies(&scene.objects) {
                s.conn.set_ghost(key, Ghost::Sky(sky));
            }
            let player =
                scene.tge_control == TgeControl::Player && !scene.tge_player_shape.is_empty();
            if scene.tge_control == TgeControl::Player && !player {
                log(&format!(
                    "no tgeplayershape in graal3dworld.txt, so {} (protocol 19) gets a Camera",
                    s.account
                ));
            }
            let control = if player {
                Ghost::TgePlayer(tge::Player {
                    state: tge::PlayerState {
                        position: start,
                        ..tge::PlayerState::default()
                    },
                    shape: scene.tge_player_shape.clone(),
                })
            } else {
                Ghost::TgeCamera(tge::Camera {
                    position: start,
                    ..tge::Camera::default()
                })
            };
            s.conn.set_ghost(camera_key(id), control);
            s.conn.set_control_object(camera_key(id));
            for (other, g) in others {
                s.conn.set_ghost(avatar_key(other), g);
            }
            // 3.153 asks for each Sky texture as <stem>.gif.
            if let Some(c) = s.caps.filter(|c| c.row.sky_gif) {
                for o in &scene.objects {
                    if let Ghost::Sky(sky) = o {
                        let names: Vec<String> = String::from_utf8_lossy(&sky.material_list)
                            .split(',')
                            .filter(|n| !n.is_empty() && *n != "-")
                            .map(|n| {
                                let stem = n.rsplit_once('.').map_or(n, |(a, _)| a);
                                format!("{stem}.gif")
                            })
                            .collect();
                        log(&format!(
                            "{}'s build ({}) asks for the sky textures as .gif: {}",
                            s.account,
                            c.label,
                            names.join(", ")
                        ));
                    }
                }
            }
            for (i, o) in scene.objects.iter().enumerate() {
                if matches!(o, Ghost::Raw(_)) {
                    s.conn.set_ghost(i as Key + 1, o.clone());
                }
            }
            self.send_raw_events(at, false);
            return;
        }
        // 4.20r6 (protocol 21) reads the scene's Sky and that Sky's seventh
        // texture name on its first 3D frame without a null check, as
        // protocol 19 does, and crashed there with no Sky or with six names
        // (lab 2026-11-20, real Mac 4.20r6 in a Tiger VM). So it gets
        // protocol 19's Skies. 5.12 (protocol 24) draws without them.
        let p21 = s.conn.profile().protocol == 21;
        let caps = s.caps;
        let mut left_out = 0;
        let terrain_all = self.opts.terrain_all;
        let offers = |o: &Ghost| match &caps {
            Some(c) => c.offers(o, terrain_all),
            None => {
                !matches!(
                    o,
                    Ghost::Terrain(_) | Ghost::Water(_) | Ghost::Replicator(_)
                ) || terrain_all
            }
        };
        for (i, o) in scene.objects.iter().enumerate() {
            if p21 && matches!(o, Ghost::Sky(_)) {
                continue;
            }
            if offers(o) {
                s.conn.set_ghost(i as Key + 1, o.clone());
            } else {
                left_out += 1;
            }
        }
        if left_out > 0 {
            log(&format!(
                "{}'s build ({}) does not take the 5.x terrain or water layout: {left_out} objects not sent",
                s.account,
                caps.map_or("unknown", |c| c.label)
            ));
        }
        if let Some(c) = caps.filter(|c| c.row.requires_sky) {
            if !scene.objects.iter().any(|o| matches!(o, Ghost::Sky(_))) && !p21 {
                log(&format!(
                    "{}'s build ({}) crashes without a Sky, and graal3dworld.txt has no sky line",
                    s.account, c.label
                ));
            }
        }
        if p21 {
            for (key, sky) in padded_skies(&scene.objects) {
                s.conn.set_ghost(key, Ghost::Sky(sky));
            }
        }
        // The terrain's settings; its chunks follow the client's request.
        if let Some(t) = &scene.terrain {
            let g = Ghost::Terrain(Box::new(t.clone()));
            if offers(&g) {
                s.conn.set_ghost(TERRAIN, g);
            }
        }
        for (other, a) in &self.avatars {
            s.conn
                .set_ghost(avatar_key(*other), Ghost::GameObject(a.clone()));
        }
        // The Sharp3D route's control object is the Camera (below).
        let sharp3d = s.conn.profile().ghost_always;
        if !sharp3d {
            s.conn.set_control_object(avatar_key(id));
        }
        // Every protocol 21/24 client gets a Camera of its own. The 5.12
        // client draws its 3D view only through one: with just the control
        // object the window stays black (lab 2026-11-20, real 5.12 on
        // Windows 10). `camera` in the world file moves it.
        let off = scene.camera_offset.unwrap_or(DEFAULT_CAMERA_OFFSET);
        let t = camera_transform(scene.spawn.position, off, scene.camera_rotation);
        let camera = GameObject {
            name: format!("G3DCamera_{}", s.account).into_bytes(),
            transform: t,
            ..GameObject::default()
        };
        s.conn.set_ghost(camera_key(id), Ghost::Camera(camera));
        s.conn.set_camera_object(camera_key(id));
        if sharp3d {
            s.conn.set_control_object(camera_key(id));
        }
        self.send_raw_events(at, false);
    }

    /// Lab: the world's `rawevent` lines (`late`: the ones for after the
    /// mission start).
    fn send_raw_events(&mut self, at: SocketAddr, late: bool) {
        let events: Vec<_> = self
            .scene
            .raw_events
            .iter()
            .filter(|e| e.late == late)
            .cloned()
            .collect();
        if events.is_empty() {
            return;
        }
        let Some(s) = self.sessions.get_mut(&at) else {
            return;
        };
        let mut sent = 0;
        let mut missing = std::collections::BTreeSet::new();
        for e in events {
            if s.conn.send_raw_event(&e.class, &e.fields) {
                sent += 1;
            } else {
                missing.insert(e.class);
            }
        }
        let account = s.account.clone();
        if sent > 0 {
            let when = if late { " after the mission start" } else { "" };
            log(&format!("{account} gets {sent} raw events{when}"));
        }
        for class in missing {
            log(&format!(
                "{account}: its client has no event class {class} (or the event is too long): not sent"
            ));
        }
    }

    /// Runs every connection: sends what is due, closes the quiet ones.
    pub fn update(&mut self, now: u64) {
        let mut dead = Vec::new();
        let addrs: Vec<SocketAddr> = self.sessions.keys().copied().collect();
        for a in addrs {
            let alive = self
                .sessions
                .get_mut(&a)
                .is_some_and(|s| s.conn.update(now));
            self.drain(a);
            if !alive {
                dead.push(a);
            }
        }
        for a in dead {
            self.close(a, "The 3D connection timed out.", false);
        }
        self.share_tge_moves();
        self.poll_roster(now);
    }

    /// Asks the world for the 2D players in each 3D player's level, 5
    /// times a second, and shows what it answers.
    fn poll_roster(&mut self, now: u64) {
        if let Some(rx) = &mut self.roster {
            match rx.try_recv() {
                Ok(r) => {
                    self.roster = None;
                    self.show_2d_players(r);
                }
                Err(oneshot::error::TryRecvError::Empty) => return,
                Err(oneshot::error::TryRecvError::Closed) => self.roster = None,
            }
        }
        if now < self.next_roster || self.sessions.is_empty() {
            return;
        }
        self.next_roster = now + ROSTER_MS;
        let players: Vec<u16> = self.sessions.values().map(|s| s.player).collect();
        self.roster = self.world.level_roster(players);
    }

    /// A 3D client draws only the objects it is ghosted, so a 2D player in
    /// its level never showed. Each gets a GameObject at its tile
    /// position, Torque (x, -y), at the spawn's height, with its nickname
    /// and chat; it goes when the player leaves the level.
    pub fn show_2d_players(&mut self, roster: crate::world::Roster) {
        let z = self.scene.spawn.position[2];
        let mut traces = Vec::new();
        for (me, others) in roster {
            let Some(at) = self.by_player.get(&me).copied() else {
                continue;
            };
            let want: BTreeMap<u16, GameObject> = others
                .into_iter()
                .filter(|o| !self.by_player.contains_key(&o.id))
                .map(|o| {
                    let mut transform = self.scene.spawn;
                    transform.position = [o.x, -o.y, z];
                    (
                        o.id,
                        GameObject {
                            name: format!("G2DPlayer_{}", o.account).into_bytes(),
                            shape: self.scene.player_shape.clone(),
                            transform,
                            nickname: o.nick,
                            chat: o.chat,
                            flags: flags::SHADOW,
                            ..GameObject::default()
                        },
                    )
                })
                .collect();
            let tge_shape = self.scene.tge_player_shape.clone();
            let tge_others = self.scene.tge_control == TgeControl::Player && !tge_shape.is_empty();
            let Some(s) = self.sessions.get_mut(&at) else {
                continue;
            };
            let protocol = s.conn.profile().protocol;
            let tge = s.conn.profile().tge;
            let gone: Vec<u16> = s
                .shown_2d
                .keys()
                .filter(|id| !want.contains_key(id))
                .copied()
                .collect();
            for id in gone {
                s.conn.remove_ghost(player_2d_key(id));
                if let Some(o) = s.shown_2d.remove(&id) {
                    traces.push((
                        s.account.clone(),
                        protocol,
                        format!("2D player gone: {}", String::from_utf8_lossy(&o.name)),
                    ));
                }
            }
            for (id, o) in want {
                let before = s.shown_2d.get(&id);
                if before == Some(&o) {
                    continue;
                }
                if tge {
                    // Protocol 19 has no GameObject: a stock Player on the
                    // PlayerData its own Player uses, or nothing.
                    if tge_others {
                        s.conn.set_ghost(
                            player_2d_key(id),
                            Ghost::TgePlayer(tge::Player {
                                state: tge::PlayerState {
                                    position: o.transform.position,
                                    ..tge::PlayerState::default()
                                },
                                shape: tge_shape.clone(),
                            }),
                        );
                    }
                } else {
                    s.conn
                        .set_ghost(player_2d_key(id), Ghost::GameObject(o.clone()));
                }
                let p = o.transform.position;
                let chat = if o.chat.is_empty() {
                    String::new()
                } else {
                    format!(" chat={}", String::from_utf8_lossy(&o.chat))
                };
                traces.push((
                    s.account.clone(),
                    protocol,
                    format!(
                        "2D player {} {}: at={:.3},{:.3},{:.3}{chat}",
                        if before.is_none() { "shown" } else { "update" },
                        String::from_utf8_lossy(&o.name),
                        p[0],
                        p[1],
                        p[2]
                    ),
                ));
                s.shown_2d.insert(id, o);
            }
        }
        for (a, p, t) in traces {
            self.trace(&a, p, &t);
        }
    }

    /// Protocol 19 clients move their own Player with moves: send each
    /// changed Player to everyone else, as a TGE Player to protocol 19 and
    /// as its GameObject to protocol 21/24 (seihai-gserver#188).
    fn share_tge_moves(&mut self) {
        let moved: Vec<(u16, tge::PlayerState)> = self
            .sessions
            .values()
            .filter_map(|s| s.conn.control_player().map(|st| (s.player, st)))
            .filter(|(p, st)| self.tge_seen.get(p) != Some(st))
            .collect();
        for (p, st) in moved {
            self.tge_seen.insert(p, st);
            if let Some(a) = self.avatars.get_mut(&p) {
                a.transform.position = st.position;
            }
            let at = self.by_player.get(&p).copied();
            self.broadcast_avatar(p, at);
            self.follow_on_tcp(p);
        }
    }

    /// Player `p` as a stock TGE Player, for protocol 19 screens: its own
    /// Player state when it plays on protocol 19, else at its object's
    /// position. `None` without a `tgeplayershape`.
    fn tge_avatar(&self, p: u16) -> Option<Ghost> {
        if self.scene.tge_player_shape.is_empty() {
            return None;
        }
        let state = self
            .tge_seen
            .get(&p)
            .copied()
            .unwrap_or_else(|| tge::PlayerState {
                position: self
                    .avatars
                    .get(&p)
                    .map_or(self.scene.spawn.position, |a| a.transform.position),
                ..tge::PlayerState::default()
            });
        Some(Ghost::TgePlayer(tge::Player {
            state,
            shape: self.scene.tge_player_shape.clone(),
        }))
    }

    /// Sends player `p`'s object to every session but `skip`, in the form
    /// each one reads (the connection drops the other era's form).
    fn broadcast_avatar(&mut self, p: u16, skip: Option<SocketAddr>) {
        let Some(a) = self.avatars.get(&p).cloned() else {
            return;
        };
        let tge_form = self.tge_avatar(p);
        let own = self.by_player.get(&p).copied();
        for (addr, s) in self.sessions.iter_mut() {
            if Some(*addr) == skip {
                continue;
            }
            s.conn
                .set_ghost(avatar_key(p), Ghost::GameObject(a.clone()));
            // A protocol 19 player's own Player is its control object.
            if let Some(g) = tge_form.as_ref().filter(|_| Some(*addr) != own) {
                s.conn.set_ghost(avatar_key(p), g.clone());
            }
        }
    }

    fn drain_all(&mut self) {
        let addrs: Vec<SocketAddr> = self.sessions.keys().copied().collect();
        for a in addrs {
            self.drain(a);
        }
    }

    /// Acts on what one connection put out.
    fn drain(&mut self, at: SocketAddr) {
        let Some(s) = self.sessions.get_mut(&at) else {
            return;
        };
        let out = s.conn.take_out();
        let (player, account) = (s.player, s.account.clone());
        for o in out {
            match o {
                Out::Send(d) => self.outbox.push((at, d)),
                Out::Log(t) => log(&format!("{account}: {t}")),
                Out::Text(t) => self.client_text(player, &t),
                Out::ClientState(st) => self.client_state(player, *st),
                Out::Mission(m) => self.mission(at, m),
                Out::TerrainRequest(stamps) => self.terrain_request(at, &stamps),
                Out::Trace(t) => {
                    let protocol = self
                        .sessions
                        .get(&at)
                        .map_or(0, |s| s.conn.profile().protocol);
                    self.trace(&account, protocol, &t);
                }
            }
        }
    }

    /// Sends every chunk whose stamp differs from the client's.
    fn terrain_request(&mut self, at: SocketAddr, stamps: &[u32]) {
        let Some(s) = self.sessions.get(&at) else {
            return;
        };
        let (caps, tge, account) = (s.caps, s.conn.profile().tge, s.account.clone());
        if self.scene.terrain.is_none() {
            log(&format!(
                "{account} asked for terrain chunks; the world has no terrain line"
            ));
            return;
        }
        if !self.takes_terrain(caps.as_ref(), tge) {
            log(&format!(
                "{account} asked for terrain chunks; its build ({}) does not take the 5.x terrain layout",
                caps.map_or("unknown", |c| c.label)
            ));
            return;
        }
        let Some(t) = &self.scene.terrain else {
            return;
        };
        let events = t.answer(stamps);
        let Some(s) = self.sessions.get_mut(&at) else {
            return;
        };
        log(&format!(
            "{}: sending {} terrain chunks",
            s.account,
            events.len()
        ));
        for ev in events {
            s.conn.send_event(ev);
        }
    }

    /// Sets the player's TCP position from its 3D object, when that is on
    /// the map and moved by half a tile or more, so serverside scripts and
    /// 2D players in its level see where it is.
    fn follow_on_tcp(&mut self, player: u16) {
        let Some(a) = self.avatars.get(&player) else {
            return;
        };
        let Some((x, y)) = tile_position(a.transform.position) else {
            return;
        };
        let half = ((x * 2.0) as u8, (y * 2.0) as u8);
        if self.tcp_position.get(&player) == Some(&half) {
            return;
        }
        self.tcp_position.insert(player, half);
        // X (15) and Y (16), GCHAR encoded.
        let raw = [15 + 32, half.0 + 32, 16 + 32, half.1 + 32];
        self.world.position(player, &raw);
    }

    fn mission(&mut self, at: SocketAddr, m: Mission) {
        let Some(s) = self.sessions.get(&at) else {
            return;
        };
        let (player, account, tge) = (s.player, s.account.clone(), s.conn.profile().tge);
        match m {
            Mission::Phase1Sent => {
                self.send_initial_flags(at, player);
                self.send_weapons(at);
            }
            Mission::Done => {
                log(&format!("{account} finished the mission start"));
                // The client loads a Sky's textures only when it unpacks the
                // Sky, and they arrive over TCP after the first: again.
                if !tge {
                    let keys: Vec<Key> = self
                        .scene
                        .objects
                        .iter()
                        .enumerate()
                        .filter(|(_, o)| matches!(o, Ghost::Sky(_)))
                        .map(|(i, _)| i as Key + 1)
                        .collect();
                    if let Some(s) = self.sessions.get_mut(&at) {
                        for k in keys {
                            s.conn.resend_ghost(k);
                        }
                    }
                }
                self.send_raw_events(at, true);
            }
            _ => {}
        }
    }

    fn send_initial_flags(&mut self, at: SocketAddr, player: u16) {
        let flags = self
            .hub
            .lock()
            .players
            .get(&player)
            .map(|p| p.flags.clone())
            .unwrap_or_default();
        let mut lines = String::new();
        // Server flags go once more as `set` texts, each on its own: the
        // client's `events` `s` lines did not reach a 5.12 script's
        // server.x (lab, 2026-11-26), and `set` is the path a change takes.
        let server_flags: Vec<String> = flags
            .iter()
            .filter(|f| f.starts_with("server.") || f.starts_with("serverr."))
            .cloned()
            .collect();
        for f in flags {
            lines.push_str("s ");
            lines.push_str(&f);
            lines.push('\n');
        }
        // Where the player's object starts, for a script that moves it.
        if let Some(a) = self.avatars.get(&player) {
            let p = a.transform.position;
            lines.push_str(&format!("s clientr.g3dpos={} {} {}\n", p[0], p[1], p[2]));
        }
        let Some(s) = self.sessions.get_mut(&at) else {
            return;
        };
        if lines.is_empty() {
            return;
        }
        s.conn.send_text(b"", b"events", b"", lines.as_bytes());
        let (account, protocol) = (s.account.clone(), s.conn.profile().protocol);
        for l in lines.lines() {
            self.trace(&account, protocol, &format!("text out: events {l}"));
        }
        for f in server_flags {
            self.flag_to_client(player, true, f.as_bytes());
        }
    }

    fn send_weapons(&mut self, at: SocketAddr) {
        let weapons = self.scene.weapons.clone();
        if weapons.is_empty() {
            return;
        }
        let server_weapons = weapons
            .iter()
            .any(|w| w.file.is_none())
            .then(|| crate::weapons::Weapons::load(&self.world_dir.join("weapons")));
        for w in weapons {
            let script = match &w.file {
                Some(file) => match std::fs::read(self.world_dir.join(file)) {
                    Ok(s) => s,
                    Err(e) => {
                        log(&format!(
                            "weapon3d {}: cannot read {}: {e}",
                            w.name,
                            file.display()
                        ));
                        continue;
                    }
                },
                // The server's weapon: its clientside part.
                None => match server_weapons.as_ref().and_then(|ws| ws.get(&w.name)) {
                    Some(sw) => {
                        let src = sw.source.as_str();
                        let client = src
                            .find("//#CLIENTSIDE")
                            .map_or(src, |i| &src[i + "//#CLIENTSIDE".len()..]);
                        client.as_bytes().to_vec()
                    }
                    None => {
                        log(&format!(
                            "weapon3d {}: the server has no weapon of that name",
                            w.name
                        ));
                        continue;
                    }
                },
            };
            // The client's script reader takes LF lines; a CR stays in the
            // token before it.
            let source: Vec<u8> = script.into_iter().filter(|b| *b != b'\r').collect();
            // A 5.x client in 3D mode runs a weaponscript as GS2 bytecode.
            let script = match compile_weapon3d(&String::from_utf8_lossy(&source)) {
                Ok(code) => code,
                Err(e) => {
                    log(&format!(
                        "weapon3d {}: not compiled ({e}); sending the source",
                        w.name
                    ));
                    source
                }
            };
            if let Some(s) = self.sessions.get_mut(&at) {
                // The client reads characters 1 to 5 of addweapon's text
                // as a number; spaces read as 0.
                s.conn
                    .send_text(b"", b"addweapon", w.name.as_bytes(), b"      ");
                s.conn
                    .send_text(b"", b"weaponscript", w.name.as_bytes(), &script);
            }
        }
    }

    fn flag_to_client(&mut self, player: u16, set: bool, text: &[u8]) {
        let Some(at) = self.by_player.get(&player).copied() else {
            return;
        };
        let Some(s) = self.sessions.get_mut(&at) else {
            return;
        };
        let kind = if set { "set" } else { "unset" };
        s.conn.send_text(b"", kind.as_bytes(), b"", text);
        let (account, protocol) = (s.account.clone(), s.conn.profile().protocol);
        let line = format!(
            "text out: {kind} {}",
            String::from_utf8_lossy(text).trim_end()
        );
        self.trace(&account, protocol, &line);
    }

    fn client_text(&mut self, player: u16, t: &super::events::BigData) {
        if !t.weapon.is_empty() || t.kind != b"events" {
            log(&format!(
                "player {player}: text ignored ({}, {}, {})",
                String::from_utf8_lossy(&t.weapon),
                String::from_utf8_lossy(&t.kind),
                String::from_utf8_lossy(&t.option)
            ));
            return;
        }
        for line in t.text.split(|b| *b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.len() < 3 || line[1] != b' ' {
                continue;
            }
            let arg = &line[2..];
            match line[0] {
                b's' => self
                    .world
                    .flag(player, String::from_utf8_lossy(arg).into_owned()),
                b'w' => self.world.give_weapon(player, arg),
                b't' => {
                    let text = String::from_utf8_lossy(arg);
                    // Fields may come quoted ("g3dpos").
                    let fields: Vec<&str> = text
                        .split(',')
                        .map(|f| f.trim().trim_matches('"'))
                        .collect();
                    if fields.len() == 6 && fields[2] == "g3dpos" {
                        let pos = format!("{} {} {}", fields[3], fields[4], fields[5]);
                        if !self.move_avatar(player, &pos) {
                            log(&format!("player {player}: g3dpos not moved ({text})"));
                        }
                    } else if let Some(rest) = arg.strip_prefix(b"weapon,") {
                        // A serverside triggeraction from a weapon: GINT3 0,
                        // x 0, y 0, then the action.
                        let mut raw = vec![0x20u8; 5];
                        raw.extend_from_slice(b"serverside,");
                        raw.extend_from_slice(rest);
                        self.world.trigger_action(player, &raw);
                    } else {
                        log(&format!("player {player}: trigger ignored ({text})"));
                    }
                }
                _ => {}
            }
        }
    }

    fn client_state(&mut self, player: u16, st: ClientState) {
        let now = self.now();
        let Some(a) = self.avatars.get_mut(&player) else {
            return;
        };
        let before = a.clone();
        let near = |p: [f32; 3], q: [f32; 3]| p.iter().zip(q).all(|(a, b)| (a - b).abs() < 0.01);
        self.last_report.insert(player, st.position);
        // After a server move the client goes on reporting where it was;
        // those reports are set aside until it reports somewhere else.
        let stale = self
            .stale_report
            .get(&player)
            .is_some_and(|p| near(*p, st.position));
        if !stale {
            self.stale_report.remove(&player);
        }
        let held = stale || self.moved_until.get(&player).is_some_and(|t| now < *t);
        if !held {
            a.transform.position = st.position;
            if st.changed & bits::TRANSFORM != 0 {
                a.transform = st.object.transform;
            }
        }
        let chat_changed = st.changed & bits::CHAT != 0 && a.chat != st.object.chat;
        if st.changed & bits::CHAT != 0 {
            a.chat = st.object.chat.clone();
        }
        // What else the client changes on its own object reaches the others:
        // scale (section 2; the Linux 4.20 drew (2, 2, 2) twice the size),
        // shape, skin, attributes, and nickname with its colour.
        if st.changed & bits::SCALE != 0 {
            a.scale = st.object.scale;
        }
        if st.changed & bits::SHAPE != 0 {
            a.shape = st.object.shape.clone();
        }
        if st.changed & bits::SKIN != 0 {
            a.skin = st.object.skin.clone();
        }
        for i in 0..8 {
            if st.changed & (bits::ATTR1 << i) != 0 {
                a.attr[i] = st.object.attr[i].clone();
            }
        }
        if st.changed & bits::NICKNAME != 0 {
            a.nickname = st.object.nickname.clone();
            a.nick_color = st.object.nick_color;
        }
        // 3D chat becomes the player's chat prop, so 2D clients see it.
        if chat_changed {
            let chat = st.object.chat.clone();
            self.world.chat(player, &chat);
        }
        if *a == before {
            return;
        }
        let at = self.by_player.get(&player).copied();
        self.broadcast_avatar(player, at);
        self.follow_on_tcp(player);
    }

    /// Moves a player's own object for everyone, its own screen included
    /// (a 5.x script cannot reach it). `position` is "x y z".
    pub fn move_avatar(&mut self, player: u16, position: &str) -> bool {
        let v: Vec<f32> = position
            .split_whitespace()
            .filter_map(|w| w.trim().parse().ok())
            .filter(|f: &f32| f.is_finite())
            .collect();
        let Ok(to) = <[f32; 3]>::try_from(v) else {
            return false;
        };
        let now = self.now();
        let Some(a) = self.avatars.get_mut(&player) else {
            return false;
        };
        if a.transform.position == to {
            return true;
        }
        a.transform.position = to;
        self.moved_until.insert(player, now + SERVER_MOVE_HOLD_MS);
        if let Some(p) = self.last_report.get(&player) {
            self.stale_report.insert(player, *p);
        }
        // A protocol 19 player's own Player moves too, so the next move it
        // sends starts from there.
        if let Some(st) = self.tge_seen.get_mut(&player) {
            st.position = to;
            let ghost = Ghost::TgePlayer(tge::Player {
                state: *st,
                shape: self.scene.tge_player_shape.clone(),
            });
            if let Some(s) = self
                .by_player
                .get(&player)
                .and_then(|at| self.sessions.get_mut(at))
            {
                s.conn.set_ghost(camera_key(player), ghost);
            }
        }
        self.broadcast_avatar(player, None);
        self.follow_on_tcp(player);
        true
    }

    fn player_left(&mut self, id: u16) {
        if let Some(at) = self.by_player.get(&id).copied() {
            self.close(at, "You have been disconnected from the game server.", true);
        }
    }

    fn close(&mut self, at: SocketAddr, reason: &str, tell: bool) {
        let Some(s) = self.sessions.remove(&at) else {
            return;
        };
        if tell {
            self.outbox
                .push((at, s.conn.disconnect_packet(reason.as_bytes())));
        }
        if self.by_player.get(&s.player) == Some(&at) {
            self.by_player.remove(&s.player);
            self.hub.lock().linked.remove(&s.player);
            self.avatars.remove(&s.player);
            self.tge_seen.remove(&s.player);
            self.moved_until.remove(&s.player);
            self.stale_report.remove(&s.player);
            self.last_report.remove(&s.player);
            self.tcp_position.remove(&s.player);
            let mut deleted = 0;
            for other in self.sessions.values_mut() {
                other.conn.remove_ghost(avatar_key(s.player));
                deleted += 1;
            }
            log(&format!(
                "{}: object deleted on {deleted} other clients",
                s.account
            ));
        }
        log(&format!(
            "{}: link from {} closed ({reason}), {} moves received",
            s.account,
            at.ip(),
            s.conn.moves_received()
        ));
    }

    /// Links open now.
    pub fn link_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn connection(&self, at: &SocketAddr) -> Option<&Connection> {
        self.sessions.get(at).map(|s| &s.conn)
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn log(text: &str) {
    eprintln!("Graal3D: {text}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graal3d::bits::{BitReader, BitWriter};
    use crate::graal3d::conn::tests::FakeClient;
    use crate::graal3d::handshake::{PROTOCOL_19, PROTOCOL_24};

    #[derive(Default)]
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl WorldInput for Recorder {
        fn flag(&self, player: u16, text: String) {
            self.0.lock().unwrap().push(format!("flag {player} {text}"));
        }
        fn trigger_action(&self, player: u16, raw: &[u8]) {
            self.0
                .lock()
                .unwrap()
                .push(format!("trigger {player} {}", String::from_utf8_lossy(raw)));
        }
        fn give_weapon(&self, player: u16, name: &[u8]) {
            self.0
                .lock()
                .unwrap()
                .push(format!("weapon {player} {}", String::from_utf8_lossy(name)));
        }
        fn chat(&self, player: u16, text: &[u8]) {
            self.0
                .lock()
                .unwrap()
                .push(format!("chat {player} {}", String::from_utf8_lossy(text)));
        }
        fn position(&self, player: u16, raw: &[u8]) {
            let half = |b: u8| f32::from(b - 32) / 2.0;
            self.0.lock().unwrap().push(format!(
                "position {player} {} {}",
                half(raw[1]),
                half(raw[3])
            ));
        }
    }

    fn ip() -> IpAddr {
        IpAddr::from([127, 0, 0, 1])
    }

    fn tcp(account: &str) -> TcpPlayer {
        TcpPlayer {
            account: account.into(),
            guest: account.starts_with("pc:"),
            ip: ip(),
            nickname: b"Nick".to_vec(),
            flags: vec!["client.hello=1".into()],
            token: String::new(),
            label: String::new(),
        }
    }

    fn service(scene: &str) -> (Service, Arc<Graal3d>, Arc<Mutex<Vec<String>>>) {
        let hub = Arc::new(Graal3d::new());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (scene, errors) = Scene::parse(scene);
        assert!(errors.is_empty());
        let s = Service::new(
            hub.clone(),
            scene,
            PathBuf::from("."),
            Box::new(Recorder(calls.clone())),
        );
        (s, hub, calls)
    }

    fn connect_request(seq: u32, digest: [u32; 4], protocol: u32, account: &[u8]) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.u8(handshake::kind::CONNECT_REQUEST);
        w.u32(seq);
        for d in digest {
            w.u32(d);
        }
        w.string(b"GameConnection");
        w.u32(0);
        w.u32(0xffff_ffff);
        w.string(b"Graal3D");
        w.u32(protocol);
        w.u32(protocol);
        w.string(b"pw");
        w.u32(2);
        w.string(account);
        w.string(b"");
        w.into_bytes()
    }

    /// Challenge and connect from `from`; the accept, if any.
    fn handshake(
        s: &mut Service,
        from: SocketAddr,
        protocol: u32,
        account: &[u8],
    ) -> Option<Vec<u8>> {
        s.datagram(from, &[0x1a, 0, 4, 0, 0], 0);
        let out = s.take_outbox();
        let mut r = BitReader::new(&out[0].1);
        assert_eq!(r.u8().unwrap(), handshake::kind::CHALLENGE_RESPONSE);
        assert_eq!(r.u32().unwrap(), 0x400);
        let mut d = [0; 4];
        for x in &mut d {
            *x = r.u32().unwrap();
        }
        s.datagram(from, &connect_request(0x400, d, protocol, account), 0);
        s.take_outbox()
            .into_iter()
            .find(|(to, p)| *to == from && p[0] == handshake::kind::CONNECT_ACCEPT)
            .map(|(_, p)| p)
    }

    /// Runs the link of each client a few rounds, answering the mission start.
    fn run(
        s: &mut Service,
        clients: &mut [(SocketAddr, FakeClient)],
        from: u64,
        rounds: u64,
    ) -> u64 {
        let mut now = from;
        for _ in 0..rounds {
            for (at, c) in clients.iter_mut() {
                let p = c.packet();
                s.datagram(*at, &p, now);
            }
            now += 60;
            s.update(now);
            for (to, d) in s.take_outbox() {
                for (at, c) in clients.iter_mut() {
                    if *at == to && d[0] & 1 == 1 {
                        c.take(&d);
                    }
                }
            }
            for (_, c) in clients.iter_mut() {
                for (cmd, ack) in [
                    (&b"MissionStartPhase1"[..], &b"MissionStartPhase1Ack"[..]),
                    (b"MissionStartPhase2", b"MissionStartPhase2Ack"),
                ] {
                    let marker = [b"sent:", cmd].concat();
                    if c.answered(cmd) && !c.answered(&marker) {
                        c.commands.push(crate::graal3d::events::Command {
                            name: marker,
                            args: vec![],
                        });
                        c.events.send_command(ack, &[]);
                    }
                }
            }
        }
        now
    }

    /// [`run`] for protocol 19 clients: also answers DataBlocksDone and
    /// Phase 3, the way 4.04r2 does.
    fn run_tge(
        s: &mut Service,
        clients: &mut [(SocketAddr, FakeClient)],
        from: u64,
        rounds: u64,
    ) -> u64 {
        let mut now = from;
        for _ in 0..rounds {
            for (at, c) in clients.iter_mut() {
                let p = c.packet();
                s.datagram(*at, &p, now);
            }
            now += 60;
            s.update(now);
            for (to, d) in s.take_outbox() {
                for (at, c) in clients.iter_mut() {
                    if *at == to && d[0] & 1 == 1 {
                        c.take(&d);
                    }
                }
            }
            for (_, c) in clients.iter_mut() {
                for ev in std::mem::take(&mut c.got) {
                    if let crate::graal3d::events::Event::ConnectionMessage {
                        sequence,
                        message,
                        ..
                    } = ev
                    {
                        if message == tge::message::DATABLOCKS_DONE {
                            c.events
                                .send(crate::graal3d::events::Event::ConnectionMessage {
                                    sequence,
                                    message: tge::message::DATABLOCKS_DOWNLOAD_DONE,
                                    ghost_count: 0,
                                });
                        }
                    }
                }
                for (cmd, ack) in [
                    (&b"MissionStartPhase1"[..], &b"MissionStartPhase1Ack"[..]),
                    (b"MissionStartPhase2", b"MissionStartPhase2Ack"),
                    (b"MissionStartPhase3", b"MissionStartPhase3Ack"),
                ] {
                    let marker = [b"sent:", cmd].concat();
                    if c.answered(cmd) && !c.answered(&marker) {
                        c.commands.push(crate::graal3d::events::Command {
                            name: marker,
                            args: vec![],
                        });
                        c.events.send_command(ack, &[]);
                    }
                }
            }
        }
        now
    }

    #[test]
    fn a_2d_player_in_the_level_is_shown_and_taken_off() {
        let (mut s, hub, _) = service("playershape p.dts\nspawn 0 0 3\n");
        hub.tcp_joined(1, tcp("one"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        handshake(&mut s, a, 24, b"one").unwrap();
        let mut clients = [(a, FakeClient::new(&PROTOCOL_24))];
        let now = run(&mut s, &mut clients, 0, 10);
        let two = crate::world::LevelPlayer {
            id: 7,
            account: "two".into(),
            nick: b"Two".to_vec(),
            chat: b"hi".to_vec(),
            x: 30.0,
            y: 20.5,
        };
        // A linked 3D player is not shown twice.
        let linked = crate::world::LevelPlayer {
            id: 1,
            ..two.clone()
        };
        s.show_2d_players(vec![(1, vec![two.clone(), linked])]);
        let c = s.connection(&a).unwrap();
        let Some(Ghost::GameObject(o)) = c.ghost(player_2d_key(7)) else {
            panic!("no 2D player");
        };
        assert_eq!(o.name, b"G2DPlayer_two");
        assert_eq!(o.transform.position, [30.0, -20.5, 3.0]);
        assert_eq!(
            (o.nickname.as_slice(), o.chat.as_slice()),
            (&b"Two"[..], &b"hi"[..])
        );
        assert!(c.ghost(player_2d_key(1)).is_none());
        run(&mut s, &mut clients, now, 4);
        assert!(s.connection(&a).unwrap().ghost_delivered(player_2d_key(7)));
        s.show_2d_players(vec![(1, vec![])]);
        run(&mut s, &mut clients, now + 300, 4);
        assert!(s.connection(&a).unwrap().ghost(player_2d_key(7)).is_none());
    }

    #[test]
    fn terrain_goes_by_the_build_row_unless_terrainall() {
        let dir = std::env::temp_dir();
        for all in [false, true] {
            let hub = Arc::new(Graal3d::new());
            let (scene, errors) = Scene::parse("water 0 0 0 64 64 2 water\nspawn 0 0 1\n");
            assert!(errors.is_empty());
            let mut s = Service::new(
                hub.clone(),
                scene,
                dir.clone(),
                Box::new(Recorder::default()),
            )
            .with_options(Options {
                terrain_all: all,
                ..Options::default()
            });
            let mut old = tcp("old");
            old.token = "G3D22126".into();
            hub.tcp_joined(1, old);
            let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
            handshake(&mut s, a, 24, b"old").unwrap();
            let takes = caps::for_client("G3D22126", "", 24).unwrap().takes_water();
            let has = s.connection(&a).unwrap().ghost(1).is_some();
            assert_eq!(has, takes || all, "terrainall {all}");
        }
    }

    #[test]
    fn the_sharp3d_route_makes_the_camera_the_control_object() {
        let hub = Arc::new(Graal3d::new());
        let (scene, _) = Scene::parse("spawn 0 0 1\n");
        let mut s = Service::new(
            hub.clone(),
            scene,
            PathBuf::from("."),
            Box::new(Recorder::default()),
        )
        .with_options(Options {
            sharp3d: true,
            ..Options::default()
        });
        hub.tcp_joined(1, tcp("one"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        let accept = handshake(&mut s, a, 24, b"one").unwrap();
        assert!(s.connection(&a).unwrap().profile().ghost_always);
        assert_eq!(
            accept,
            handshake::connect_accept(0x400, &handshake::PROTOCOL_24_SHARP3D)
        );
        let mut c = FakeClient::new(&handshake::PROTOCOL_24_SHARP3D);
        c.camera_control = true;
        let mut clients = [(a, c)];
        run(&mut s, &mut clients, 0, 12);
        let c = s.connection(&a).unwrap();
        assert_eq!(c.mission(), Mission::Done);
        assert!(c.control_delivered());
    }

    #[test]
    fn an_accept_mutation_changes_the_accept() {
        let hub = Arc::new(Graal3d::new());
        let mut s = Service::new(
            hub.clone(),
            Scene::default(),
            PathBuf::from("."),
            Box::new(Recorder::default()),
        )
        .with_options(Options {
            mutation: Some((Mutation::AcceptProtocol, 5)),
            ..Options::default()
        });
        hub.tcp_joined(1, tcp("one"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        let accept = handshake(&mut s, a, 24, b"one").unwrap();
        let (tweak, _) = mutate::accept_tweak(Mutation::AcceptProtocol, 5, &PROTOCOL_24);
        assert_eq!(
            accept,
            handshake::connect_accept_tweaked(0x400, &PROTOCOL_24, &tweak)
        );
        assert_ne!(accept, handshake::connect_accept(0x400, &PROTOCOL_24));
    }

    #[test]
    fn a_link_waits_for_the_tcp_login_from_the_same_address() {
        let (mut s, hub, _) = service("");
        let from: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        assert!(
            handshake(&mut s, from, 24, b"lab").is_none(),
            "no TCP login yet"
        );
        hub.tcp_joined(
            3,
            TcpPlayer {
                ip: IpAddr::from([10, 0, 0, 9]),
                ..tcp("lab")
            },
        );
        assert!(
            handshake(&mut s, from, 24, b"lab").is_none(),
            "another address"
        );
        hub.tcp_joined(3, tcp("lab"));
        let accept = handshake(&mut s, from, 24, b"LAB").expect("accepted");
        assert_eq!(accept, handshake::connect_accept(0x400, &PROTOCOL_24));
        assert!(hub.is_linked(3));
        // A repeat of the request gets the accept again, not a second link.
        assert!(handshake(&mut s, from, 24, b"lab").is_some());
        assert_eq!(s.link_count(), 1);
    }

    #[test]
    fn a_bad_digest_gets_no_answer_and_a_bad_game_a_reject() {
        let (mut s, hub, _) = service("");
        hub.tcp_joined(1, tcp("lab"));
        let from: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        s.datagram(from, &connect_request(0x400, [1, 2, 3, 4], 24, b"lab"), 0);
        assert!(s.take_outbox().is_empty());
        assert!(handshake(&mut s, from, 9, b"lab").is_none());
    }

    #[test]
    fn a_guest_takes_the_guest_login_from_its_address() {
        let (mut s, hub, _) = service("");
        hub.tcp_joined(4, tcp("pc:1"));
        hub.tcp_joined(9, tcp("pc:2"));
        hub.tcp_joined(5, tcp("someone"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:40001".parse().unwrap();
        assert!(handshake(&mut s, a, 24, b"guest").is_some());
        assert!(hub.is_linked(9), "the newest guest first");
        assert!(handshake(&mut s, b, 24, b"guest").is_some());
        assert!(hub.is_linked(4), "then the one without a link");
    }

    #[test]
    fn later_players_start_around_the_spawn_point() {
        let spawn = [0.0, 0.0, 1.0];
        assert_eq!(spawn_point(spawn, &[]), spawn);
        let second = spawn_point(spawn, &[spawn]);
        assert!((second[0].hypot(second[1]) - 2.0).abs() < 1e-4);
        assert_eq!(second[2], 1.0);
        let mut taken = vec![spawn];
        for _ in 0..8 {
            taken.push(spawn_point(spawn, &taken));
        }
        // The ninth later player is on the 4-unit ring.
        let next = spawn_point(spawn, &taken);
        assert!((next[0].hypot(next[1]) - 4.0).abs() < 1e-4);
    }

    #[test]
    fn torque_positions_map_to_tiles() {
        assert_eq!(tile_position([5.0, -6.0, 1.0]), Some((5.0, 6.0)));
        assert_eq!(tile_position([5.0, 6.0, 1.0]), None);
        assert_eq!(tile_position([70.0, -6.0, 1.0]), None);
    }

    #[test]
    fn a_terrain_is_ghosted_and_its_changed_chunks_sent() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("g3d-svc-terrain-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("t")).unwrap();
        let mut stamps = vec![0u32; 256];
        for (i, name) in ["heights0-0.dat", "heights1-0.dat", "heights0-1.dat"]
            .iter()
            .enumerate()
        {
            let inflated = [2u8, i as u8, 0];
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&inflated).unwrap();
            std::fs::write(dir.join("t").join(name), e.finish().unwrap()).unwrap();
            if i == 0 {
                stamps[0] = crate::graal3d::ground::stamp(&inflated);
            }
        }
        let hub = Arc::new(Graal3d::new());
        let (mut scene, errors) = Scene::parse(
            "terrain t 8
water 0 0 0 1024 1024 2 water
             replicator DynamicGrass 0 0 0 4 1 3 grass.dts
spawn 0 0 1
",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert!(scene.load_terrain(&dir).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
        let mut s = Service::new(hub.clone(), scene, dir, Box::new(Recorder::default()));
        hub.tcp_joined(1, tcp("one"));
        hub.tcp_joined(2, tcp("old"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:40001".parse().unwrap();
        handshake(&mut s, a, 24, b"one").unwrap();
        handshake(&mut s, b, 19, b"old").unwrap();
        let mut clients = [(a, FakeClient::new(&PROTOCOL_24))];
        let now = run(&mut s, &mut clients, 0, 10);
        // Terrain, water, replicator, both players and its Camera.
        assert_eq!(s.connection(&a).unwrap().ghost_count(), 6);
        // Protocol 19 gets none of the ground: a Sky and its Camera.
        assert_eq!(s.connection(&b).unwrap().ghost_count(), 2);

        // A client holding chunk (0, 0) gets the other two.
        clients[0]
            .1
            .events
            .send(crate::graal3d::events::Event::TerrainRequest(stamps));
        run(&mut s, &mut clients, now, 6);
        let chunks = clients[0]
            .1
            .got
            .iter()
            .filter(|e| e.class_name() == Some("TerrainChunkEvent"))
            .count();
        assert_eq!(chunks, 2);
    }

    #[test]
    fn a_3d_players_tcp_position_follows_its_object() {
        let (mut s, hub, calls) = service(
            "playershape labcube.3ds
spawn 3 -4 1
",
        );
        hub.tcp_joined(1, tcp("one"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        handshake(&mut s, a, 24, b"one").unwrap();
        assert!(s.move_avatar(1, "5 -6.5 1"));
        // Off the map: left alone.
        assert!(s.move_avatar(1, "5 6 1"));
        let calls = calls.lock().unwrap().clone();
        let pos: Vec<&String> = calls.iter().filter(|c| c.starts_with("position")).collect();
        assert_eq!(pos, ["position 1 3 4", "position 1 5 6.5"]);
    }

    #[test]
    fn after_a_server_move_the_old_reports_are_set_aside() {
        let (mut s, hub, _) = service(
            "playershape labcube.3ds
spawn 0 0 1
",
        );
        hub.tcp_joined(1, tcp("one"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        handshake(&mut s, a, 24, b"one").unwrap();
        let mut clients = [(a, FakeClient::new(&PROTOCOL_24))];
        // The fake client reports 0 0 0 in every packet.
        let now = run(&mut s, &mut clients, 0, 6);
        assert!(s.move_avatar(1, "5 6 7"));
        // Well past the two-second hold.
        run(&mut s, &mut clients, now, 60);
        assert_eq!(s.avatars[&1].transform.position, [5.0, 6.0, 7.0]);
    }

    #[test]
    fn a_movetype_sends_a_server_move_as_movement_data() {
        let (mut s, hub, _) = service(
            "playershape labcube.3ds
spawn 0 0 1
movetype interpolate
",
        );
        hub.tcp_joined(1, tcp("one"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        handshake(&mut s, a, 24, b"one").unwrap();
        let mut clients = [(a, FakeClient::new(&PROTOCOL_24))];
        let now = run(&mut s, &mut clients, 0, 6);
        assert_eq!(s.avatars[&1].movetype, b"interpolate");
        let before = s.avatars[&1].clone();
        assert!(s.move_avatar(1, "5 6 7"));
        let m = s.avatars[&1].changes_from(&before);
        assert_eq!(m & (bits::MOVE_DATA | bits::TRANSFORM), bits::MOVE_DATA);
        // The update parses on the client side, section 7 and all.
        run(&mut s, &mut clients, now, 4);
        assert!(s.connection(&a).unwrap().control_delivered());
    }

    #[test]
    fn two_players_see_each_other_and_their_moves() {
        let (mut s, hub, calls) = service(
            "sun 0 0 -1 1 1 1 1 0.2 0.2 0.2 1\n\
             object house house.dif 1 2 3\n\
             playershape labcube.3ds\n\
             spawn 0 0 1\n",
        );
        hub.tcp_joined(1, tcp("one"));
        hub.tcp_joined(2, tcp("two"));
        // A server flag the world sent at the TCP join, before the link.
        let mut f = vec![PLO_FLAGSET + 32];
        f.extend_from_slice(b"server.door=open\n");
        hub.mirror(1, &[f]);
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:40001".parse().unwrap();
        handshake(&mut s, a, 24, b"one").unwrap();
        handshake(&mut s, b, 24, b"two").unwrap();
        let mut clients = [
            (a, FakeClient::new(&PROTOCOL_24)),
            (b, FakeClient::new(&PROTOCOL_24)),
        ];
        let now = run(&mut s, &mut clients, 0, 12);
        for at in [a, b] {
            let c = s.connection(&at).unwrap();
            assert!(c.control_delivered(), "{at}");
            // The Sun, the house, both players and its own Camera.
            assert_eq!(c.ghost_count(), 5);
        }
        // The initial flags went out as one events text.
        let texts: Vec<String> = clients[0]
            .1
            .got
            .iter()
            .filter_map(|e| match e {
                crate::graal3d::events::Event::BigDataChunk { bytes, .. } => {
                    Some(String::from_utf8_lossy(bytes).into_owned())
                }
                _ => None,
            })
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("s client.hello=1\n")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("s clientr.g3dpos=0 0 1\n")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("s server.door=open\n")),
            "{texts:?}"
        );
        assert!(
            clients[0].1.got.iter().any(|e| matches!(
                e,
                crate::graal3d::events::Event::BigDataStart { kind, .. } if kind == b"set"
            )),
            "server flags go as set texts too"
        );

        // A g3dpos trigger moves player one's object on both screens.
        assert!(s.move_avatar(1, "5 6 7"));
        let mut c2 = s.connection(&b).unwrap().ghost_index(avatar_key(1));
        assert!(c2.take().is_some());
        let _ = run(&mut s, &mut clients, now, 4);

        // Text lines from the client reach the world.
        let mut text = Vec::new();
        for ev in crate::graal3d::events::big_data_events(
            1,
            b"",
            b"events",
            b"",
            b"s client.walked=1\nw -bomb\nt weapon,-bomb,boom\n",
        ) {
            text.push(ev);
        }
        for ev in text {
            clients[0].1.events.send(ev);
        }
        let _ = run(&mut s, &mut clients, now + 300, 2);
        let calls = calls.lock().unwrap().clone();
        assert!(
            calls.contains(&"flag 1 client.walked=1".to_string()),
            "{calls:?}"
        );
        assert!(calls.contains(&"weapon 1 -bomb".to_string()), "{calls:?}");
        assert!(
            calls.contains(&"trigger 1      serverside,-bomb,boom".to_string()),
            "{calls:?}"
        );
    }

    #[test]
    fn the_tcp_session_ending_closes_the_link_with_a_disconnect() {
        let (mut s, hub, _) = service("");
        hub.tcp_joined(1, tcp("one"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        handshake(&mut s, a, 24, b"one").unwrap();
        s.player_left(1);
        let out = s.take_outbox();
        assert_eq!(out[0].1[0], handshake::kind::DISCONNECT);
        assert_eq!(s.link_count(), 0);
        assert!(!hub.is_linked(1));
    }

    #[test]
    fn a_protocol_24_client_gets_a_camera_without_a_camera_line() {
        let (mut s, hub, _) = service("spawn 1 2 3\n");
        hub.tcp_joined(1, tcp("one"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        handshake(&mut s, a, 24, b"one").unwrap();
        let c = s.connection(&a).unwrap();
        assert!(c.ghost_index(camera_key(1)).is_some());
        assert!(c.ghost_index(avatar_key(1)).is_some());
    }

    /// 4.20r6 (protocol 21) crashed on its first 3D frame with no Sky and
    /// with a six-name Sky; it gets protocol 19's Skies. Protocol 24 gets
    /// the world's as written.
    #[test]
    fn a_protocol_21_client_gets_a_sky_with_seven_names() {
        let names = |g: Option<&Ghost>| match g {
            Some(Ghost::Sky(k)) => k.material_list.split(|b| *b == b',').count(),
            other => panic!("{other:?}"),
        };
        let (mut s, hub, _) = service("spawn 1 2 3\n");
        hub.tcp_joined(1, tcp("one"));
        hub.tcp_joined(2, tcp("two"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:40001".parse().unwrap();
        handshake(&mut s, a, 21, b"one").unwrap();
        handshake(&mut s, b, 24, b"two").unwrap();
        assert_eq!(names(s.connection(&a).unwrap().ghost(PLAIN_SKY)), 7);
        assert!(s.connection(&b).unwrap().ghost(PLAIN_SKY).is_none());

        let (mut s, hub, _) = service("sky a,b,c,d,e,f 500\nspawn 1 2 3\n");
        hub.tcp_joined(1, tcp("one"));
        hub.tcp_joined(2, tcp("two"));
        handshake(&mut s, a, 21, b"one").unwrap();
        handshake(&mut s, b, 24, b"two").unwrap();
        let (p21, p24) = (s.connection(&a).unwrap(), s.connection(&b).unwrap());
        assert!(p21.ghost(PLAIN_SKY).is_none());
        assert_eq!(names(p21.ghost(1)), 7);
        assert_eq!(names(p24.ghost(1)), 6);
    }

    #[test]
    fn a_protocol_19_client_gets_a_sky_and_a_camera_without_a_player_shape() {
        let (mut s, hub, _) = service("spawn 1 2 3\n");
        hub.tcp_joined(1, tcp("one"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        handshake(&mut s, a, 19, b"one").unwrap();
        let c = s.connection(&a).unwrap();
        assert_eq!(c.ghost_count(), 2);
        assert!(c.ghost_index(PLAIN_SKY).is_some());
        assert!(c.ghost_index(camera_key(1)).is_some());
        let _ = PROTOCOL_19;
    }

    #[test]
    fn protocol_19_players_see_each_other_and_their_moves() {
        let (mut s, hub, _) = service("spawn 0 0 1\ntgeplayershape octahedron.dts\n");
        hub.tcp_joined(1, tcp("one"));
        hub.tcp_joined(2, tcp("two"));
        let a: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        let b: SocketAddr = "127.0.0.1:40001".parse().unwrap();
        handshake(&mut s, a, 19, b"one").unwrap();
        handshake(&mut s, b, 19, b"two").unwrap();
        for at in [a, b] {
            // The Sky, its own Player and the other's (seihai-gserver#188).
            assert_eq!(s.connection(&at).unwrap().ghost_count(), 3, "{at}");
        }
        assert!(s
            .connection(&a)
            .unwrap()
            .ghost_index(avatar_key(2))
            .is_some());
        assert!(s
            .connection(&b)
            .unwrap()
            .ghost_index(avatar_key(1))
            .is_some());
        assert!(s
            .connection(&a)
            .unwrap()
            .ghost_index(avatar_key(1))
            .is_none());
        let mut clients = [
            (a, FakeClient::new(&PROTOCOL_19)),
            (b, FakeClient::new(&PROTOCOL_19)),
        ];
        let mut now = run_tge(&mut s, &mut clients, 0, 14);
        assert!(s.connection(&a).unwrap().control_delivered());
        // Player one walks forward; player two's copy of it follows.
        for _ in 0..6 {
            clients[0].1.moves = vec![
                tge::Move {
                    y: 32,
                    ..tge::Move::default()
                };
                3
            ];
            now = run_tge(&mut s, &mut clients[..], now, 1);
        }
        let own = s.connection(&a).unwrap().control_player().unwrap();
        assert!(own.position[1] > 0.0, "{own:?}");
        match s.connection(&b).unwrap().ghost(avatar_key(1)) {
            Some(Ghost::TgePlayer(p)) => assert_eq!(p.state.position, own.position),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn flags_sent_over_tcp_are_kept_and_mirrored() {
        let hub = Graal3d::new();
        hub.tcp_joined(1, tcp("one"));
        let mut f = vec![PLO_FLAGSET + 32];
        f.extend_from_slice(b"client.hello=2\n");
        hub.mirror(1, &[f]);
        assert_eq!(
            hub.lock().players[&1].flags,
            vec!["client.hello=2".to_string()]
        );
        let mut f = vec![PLO_FLAGDEL + 32];
        f.extend_from_slice(b"client.hello\n");
        hub.mirror(1, &[f]);
        assert!(hub.lock().players[&1].flags.is_empty());
    }

    #[test]
    fn a_file_cut_over_frames_is_not_read_as_flags() {
        let set = crate::wire::gchar(PLO_FLAGSET);
        // A file whose bytes hold a flag message and newlines.
        let mut data = vec![b'x'; 40_000];
        data[32_005] = b'\n';
        data[32_006] = set;
        data[32_007..32_010].copy_from_slice(b"b=2");
        data[32_010] = b'\n';
        let f = crate::files::Found {
            name: "big.png".into(),
            data,
            mod_time: 1,
        };
        let v = seihai_core::protocol::ClientVersion::V512;
        let mut frames = crate::files::send_file(v, &f).unwrap();
        assert!(frames.len() > 1);
        frames.push([&[set][..], b"a=1\n"].concat());
        assert_eq!(flag_messages(&frames), vec![(true, b"a=1".to_vec())]);
    }

    #[test]
    fn a_weapon3d_script_is_compiled_to_bytecode() {
        let src = "//#CLIENTSIDE\r\nfunction onCreated() {\n  this.x = 1;\n}\n";
        let src: String = src.chars().filter(|c| *c != '\r').collect();
        let code = compile_weapon3d(&src).unwrap();
        assert_eq!(code.last(), Some(&b'\n'));
        assert!(!code.starts_with(b"function"));
        assert!(compile_weapon3d("function {{{").is_err());
    }

    #[test]
    fn an_empty_line_in_a_frame_is_skipped() {
        let set = [PLO_FLAGSET + 32];
        let frame = [&b"\n"[..], &set, b"a=1\n\n"].concat();
        assert_eq!(flag_messages(&[frame]), vec![(true, b"a=1".to_vec())]);
        assert!(flag_messages(&[b"\n".to_vec(), Vec::new()]).is_empty());
    }

    #[test]
    fn flag_messages_are_found_and_raw_data_skipped() {
        let mut f = vec![PLO_FLAGSET + 32];
        f.extend_from_slice(b"a=1\n");
        f.push(PLO_RAWDATA + 32);
        let mut n = Vec::new();
        crate::wire::gint3(6, &mut n);
        f.extend_from_slice(&n);
        f.push(b'\n');
        f.extend_from_slice(b"<\n\n\n\n\n"); // six raw bytes with newlines in them
        f.push(PLO_FLAGDEL + 32);
        f.extend_from_slice(b"b\n");
        assert_eq!(
            flag_messages(&[f]),
            vec![(true, b"a=1".to_vec()), (false, b"b".to_vec())]
        );
    }

    #[test]
    fn the_3d_builds() {
        assert!(has_graal3d(ClientVersion::V3153));
        assert!(has_graal3d(ClientVersion::V512));
        assert!(!has_graal3d(ClientVersion::V304));
        assert!(!has_graal3d(ClientVersion::V6015));
    }
}
