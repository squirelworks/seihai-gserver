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
//! Player (or Camera) of its own instead.

use super::conn::{ClientState, Connection, Key, Mission, Out};
use super::handshake::{self, ConnectRequest};
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
use tokio::sync::mpsc;

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
    ) -> std::io::Result<SocketAddr> {
        let socket = UdpSocket::bind(addr).await?;
        let local = socket.local_addr()?;
        let rx = self
            .rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| std::io::Error::other("the 3D side is already running"))?;
        let service = Service::new(self.clone(), scene, world_dir, world);
        tokio::spawn(service.run(socket, rx));
        Ok(local)
    }
}

/// `PLO_FLAGSET` and `PLO_FLAGDEL` bodies in gen-5 frames, skipping raw data.
fn flag_messages(frames: &[Vec<u8>]) -> Vec<(bool, Vec<u8>)> {
    let mut out = Vec::new();
    for frame in frames {
        let mut rest = frame.as_slice();
        while let Some(&first) = rest.first() {
            let id = first.wrapping_sub(32);
            let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
            let body = &rest[1..end];
            rest = rest.get(end + 1..).unwrap_or(&[]);
            match id {
                PLO_FLAGSET => out.push((true, body.to_vec())),
                PLO_FLAGDEL => out.push((false, body.to_vec())),
                PLO_RAWDATA => {
                    // The next message is this long, newline and all.
                    let n = seihai_core::protocol::read_gint3(body).unwrap_or(0) as usize;
                    rest = rest.get(n..).unwrap_or(&[]);
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
}

struct Session {
    player: u16,
    account: String,
    conn: Connection,
}

const AVATAR: Key = 0x1_0000;
const CAMERA: Key = 0x2_0000;

/// Where a protocol 21/24 client's Camera sits from the spawn point when the
/// world file has no `camera` line: behind it and above.
pub const DEFAULT_CAMERA_OFFSET: [f32; 3] = [0.0, -5.0, 2.0];
/// The plain Sky of protocols 19 and 21, when the world has none.
const PLAIN_SKY: Key = 0x3_0000;

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
    moved_until: HashMap<u16, u64>,
    unmatched: HashSet<String>,
    /// Datagrams to send, for the task (or a test) to take.
    outbox: Vec<(SocketAddr, Vec<u8>)>,
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
            moved_until: HashMap::new(),
            unmatched: HashSet::new(),
            outbox: Vec::new(),
        }
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
                let accept = handshake::connect_accept(req.sequence, s.conn.profile());
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
        let profile = handshake::profile(req.current_protocol).expect("checked");
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
        let conn = Connection::new(profile, req.sequence, now);
        self.sessions.insert(
            from,
            Session {
                player: id,
                account: player.account.clone(),
                conn,
            },
        );
        self.by_player.insert(id, from);
        self.hub.lock().linked.insert(id);

        let avatar = GameObject {
            name: format!("G3DPlayer_{}", player.account).into_bytes(),
            shape: self.scene.player_shape.clone(),
            transform: self.scene.spawn,
            nickname: player.nickname.clone(),
            flags: flags::SHADOW,
            ..GameObject::default()
        };
        self.avatars.insert(id, avatar.clone());
        self.ghost_world(from);
        for (addr, s) in self.sessions.iter_mut() {
            if *addr != from {
                s.conn
                    .set_ghost(avatar_key(id), Ghost::GameObject(avatar.clone()));
            }
        }
        self.outbox
            .push((from, handshake::connect_accept(req.sequence, profile)));
        log(&format!(
            "{} linked from {} (protocol {}, {})",
            player.account,
            from.ip(),
            profile.protocol,
            profile.clients
        ));
        self.drain_all();
    }

    fn ghost_world(&mut self, at: SocketAddr) {
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
                        position: scene.spawn.position,
                        ..tge::PlayerState::default()
                    },
                    shape: scene.tge_player_shape.clone(),
                })
            } else {
                Ghost::TgeCamera(tge::Camera {
                    position: scene.spawn.position,
                    ..tge::Camera::default()
                })
            };
            s.conn.set_ghost(camera_key(id), control);
            s.conn.set_control_object(camera_key(id));
            return;
        }
        // 4.20r6 (protocol 21) reads the scene's Sky and that Sky's seventh
        // texture name on its first 3D frame without a null check, as
        // protocol 19 does, and crashed there with no Sky or with six names
        // (lab 2026-11-20, real Mac 4.20r6 in a Tiger VM). So it gets
        // protocol 19's Skies. 5.12 (protocol 24) draws without them.
        let p21 = s.conn.profile().protocol == 21;
        for (i, o) in scene.objects.iter().enumerate() {
            if !(p21 && matches!(o, Ghost::Sky(_))) {
                s.conn.set_ghost(i as Key + 1, o.clone());
            }
        }
        if p21 {
            for (key, sky) in padded_skies(&scene.objects) {
                s.conn.set_ghost(key, Ghost::Sky(sky));
            }
        }
        for (other, a) in &self.avatars {
            s.conn
                .set_ghost(avatar_key(*other), Ghost::GameObject(a.clone()));
        }
        s.conn.set_control_object(avatar_key(id));
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
            }
        }
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
        if let Some(s) = self.sessions.get_mut(&at) {
            if !lines.is_empty() {
                s.conn.send_text(b"", b"events", b"", lines.as_bytes());
            }
        }
    }

    fn send_weapons(&mut self, at: SocketAddr) {
        let weapons = self.scene.weapons.clone();
        for w in weapons {
            let script = match std::fs::read(self.world_dir.join(&w.file)) {
                Ok(s) => s,
                Err(e) => {
                    log(&format!(
                        "weapon3d {}: cannot read {}: {e}",
                        w.name,
                        w.file.display()
                    ));
                    continue;
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
        if let Some(s) = self.sessions.get_mut(&at) {
            let kind: &[u8] = if set { b"set" } else { b"unset" };
            s.conn.send_text(b"", kind, b"", text);
        }
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
                    let fields: Vec<&str> = text.split(',').collect();
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
        let held = self.moved_until.get(&player).is_some_and(|t| now < *t);
        if !held {
            a.transform.position = st.position;
            if st.changed & bits::TRANSFORM != 0 {
                a.transform = st.object.transform;
            }
        }
        if st.changed & bits::CHAT != 0 {
            a.chat = st.object.chat.clone();
        }
        if st.changed & bits::VELOCITY != 0 {
            a.velocity = st.object.velocity;
        }
        if *a == before {
            return;
        }
        let a = a.clone();
        let at = self.by_player.get(&player).copied();
        for (addr, s) in self.sessions.iter_mut() {
            if Some(*addr) != at {
                s.conn
                    .set_ghost(avatar_key(player), Ghost::GameObject(a.clone()));
            }
        }
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
        let a = a.clone();
        self.moved_until.insert(player, now + SERVER_MOVE_HOLD_MS);
        for s in self.sessions.values_mut() {
            s.conn
                .set_ghost(avatar_key(player), Ghost::GameObject(a.clone()));
        }
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
            self.moved_until.remove(&s.player);
            for other in self.sessions.values_mut() {
                other.conn.remove_ghost(avatar_key(s.player));
            }
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
    fn two_players_see_each_other_and_their_moves() {
        let (mut s, hub, calls) = service(
            "sun 0 0 -1 1 1 1 1 0.2 0.2 0.2 1\n\
             object house house.dif 1 2 3\n\
             playershape labcube.3ds\n\
             spawn 0 0 1\n",
        );
        hub.tcp_joined(1, tcp("one"));
        hub.tcp_joined(2, tcp("two"));
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
