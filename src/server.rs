//! Listening, login and the idle session.
//!
//! One tokio task per connection. Who is online, and which player ids are
//! taken, belongs to the world task (`world.rs`); a connection asks it for
//! an id after the account check and tells it when it closes.

use crate::accounts::{Account, Store};
use crate::allow::AllowList;
use crate::burst::{self, Player};
use crate::codec::{frame_header, Codec, MAX_IN_FRAME};
use crate::config::{Config, NpcMode};
use crate::listlink::{Delivery, ListedPlayer};
use crate::login::{self, Login};
use crate::secure;
use crate::wire::{gchar, gint4, gstring, ids::*, latin1, msg};
use seihai_core::protocol::{
    parse_player_props_with, zlib_decompress, ClientVersion, EncryptionGen,
};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Gen-3 messages in a row whose inserted byte is wrong before the client
/// is dropped: past that the two iterators are out of step for good.
const MAX_BAD_IN_A_ROW: u32 = 3;

pub struct Server {
    pub cfg: Config,
    pub allow: AllowList,
    extra_tokens: Vec<String>,
    store: Store,
    /// The world task's handle: who is online (see `world`).
    pub world: crate::world::World,
    /// The anti-cheat policy and log, shared with the world.
    pub anticheat: Arc<crate::anticheat::AntiCheat>,
    /// The files the world serves (see `files`).
    pub files: crate::files::FileStore,
    /// The NPC-server link's shared state (see `npclink`).
    pub npc: Arc<crate::npclink::Hub>,
    started: Instant,
    pub verbose: bool,
    /// First sight of a client message stage 1 does not handle. Verbose
    /// mode logs the same ids once.
    unknown: Mutex<BTreeSet<u8>>,
    /// The secure-login key (`SEI00001`); `None` when secure logins are off.
    pub identity: Option<seihai_core::protocol::seclogin::ServerIdentity>,
    /// The listserver link (see `listlink`); `None` when `listserver` is
    /// empty. Set by `run`.
    pub listlink: Option<Arc<crate::listlink::ListLink>>,
    /// Logins a minute per address (`login_rate`).
    login_rate: crate::ratelimit::LoginRate,
    /// The cap on password checks at once (`password_checks`).
    password_checks: crate::ratelimit::PasswordChecks,
}

impl Server {
    pub fn new(cfg: Config, allow: AllowList) -> Self {
        let store = Store::new(cfg.accounts_dir());
        let extra_tokens = allow.unknown_tokens();
        let started_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let token = match cfg.npcserver {
            NpcMode::Off => Vec::new(),
            // A fresh token per start; `supervise` hands it to the child.
            NpcMode::Auto => crate::npclink::fresh_token().unwrap_or_default(),
            NpcMode::External => {
                let t = cfg.npcserver_token.as_bytes();
                if (16..=256).contains(&t.len()) {
                    t.to_vec()
                } else {
                    Vec::new()
                }
            }
        };
        let npc = Arc::new(crate::npclink::Hub::new(
            token,
            &cfg.name,
            &cfg.levels_dir(),
        ));
        let world = crate::world::World::new(
            cfg.max_players,
            crate::world::LevelSource {
                dir: cfg.levels_dir(),
                blank_tile: cfg.board_tile,
                blank_mod_time: started_unix,
            },
        );
        // A player who logs out is written back to their account file.
        let saving = store.clone();
        world.set_saver(Box::new(move |l: crate::world::Leaving| {
            let saved = saving.load(&l.account).and_then(|a| match a {
                Some(mut a) => {
                    l.apply_to(&mut a);
                    saving.save(&a)
                }
                None => Ok(()),
            });
            if let Err(e) = saved {
                eprintln!("could not save the account {}: {e}", l.account);
            }
        }));
        let anticheat = Arc::new(crate::anticheat::AntiCheat::new(
            cfg.anticheat.clone(),
            &cfg.world,
        ));
        world.set_anticheat(anticheat.clone());
        // The world feeds the NPC-server link and applies what it sends.
        world.set_npc(npc.clone());
        world.set_server_flags_file(cfg.world.join("config").join("serverflags.txt"));
        world.set_weapons_dir(cfg.world.join("weapons"));
        let login_rate = crate::ratelimit::LoginRate::new(cfg.login_rate, cfg.login_rate_loopback);
        let password_checks = crate::ratelimit::PasswordChecks::new(cfg.password_check_slots());
        Server {
            files: crate::files::FileStore::new(&cfg.world),
            npc,
            world,
            anticheat,
            login_rate,
            password_checks,
            cfg,
            allow,
            extra_tokens,
            store,
            started: Instant::now(),
            verbose: false,
            unknown: Mutex::new(BTreeSet::new()),
            identity: None,
            listlink: None,
        }
    }

    /// The most password checks that ran at once since the start.
    pub fn peak_password_checks(&self) -> usize {
        self.password_checks.peak()
    }

    /// Accounts online now, lower-case and sorted.
    pub fn online_accounts(&self) -> Vec<String> {
        self.world.online_accounts()
    }

    /// Client message ids stage 1 has seen and does not handle.
    pub fn unknown_ids(&self) -> BTreeSet<u8> {
        self.unknown
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn note_unknown(&self, id: u8) {
        self.unknown
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id);
    }

    fn log(&self, peer: &SocketAddr, text: &str) {
        eprintln!(
            "[{:>9.3}] {peer} {text}",
            self.started.elapsed().as_secs_f64()
        );
    }

    /// Graal world time: one step every 5 s since the server started.
    fn world_time(&self) -> u32 {
        (self.started.elapsed().as_secs() / 5) as u32
    }

    /// Remember an image the listserver sent, so the next login has it.
    fn remember_image(&self, account: &str, kind: u8, name: &str) {
        let Ok(Some(mut a)) = self.store.load(account) else {
            return;
        };
        match kind {
            0 => a.head = name.to_string(),
            1 => a.body = name.to_string(),
            2 => a.sword.1 = name.to_string(),
            3 => a.shield.1 = name.to_string(),
            _ => return,
        }
        if let Err(e) = self.store.save(&a) {
            eprintln!("listserver file: could not store the image for {account}: {e}");
        }
    }
}

/// Start the world task and accept connections until the listener fails.
pub async fn serve(server: Arc<Server>, listener: TcpListener) -> io::Result<()> {
    serve_on(server, listener, None).await
}

/// Where the Login lobby sends 4.x to 6.x builds: the name, host and port
/// of `PLO_SERVERWARP` (178).
#[derive(Debug, Clone)]
pub struct LobbyWarp {
    pub name: String,
    pub host: String,
    pub port: u16,
}

impl LobbyWarp {
    /// The 178 payload as the lab lobby sent it to real 5.24r4 and 6.03r7
    /// (`docs/lab/five-clients-2026-11-12.md`): `0`, the name, the host
    /// and the port, split on commas by the lobby builds, so no field may
    /// hold one.
    pub fn payload(&self) -> Vec<u8> {
        let clean = |s: &str| s.replace([',', '\n', '\r'], " ");
        crate::wire::latin1(&format!(
            "0,{},{},{}",
            clean(&self.name),
            clean(&self.host),
            self.port
        ))
    }
}

/// The Login lobby (stage 6) on its own listener: logins as on the game
/// port, and builds with a lobby are sent on with `warp` after the burst.
pub async fn serve_lobby(
    server: Arc<Server>,
    listener: TcpListener,
    warp: LobbyWarp,
) -> io::Result<()> {
    serve_on(server, listener, Some(Arc::new(warp))).await
}

async fn serve_on(
    server: Arc<Server>,
    listener: TcpListener,
    lobby: Option<Arc<LobbyWarp>>,
) -> io::Result<()> {
    server.world.start();
    loop {
        let (stream, peer) = listener.accept().await?;
        let _ = stream.set_nodelay(true);
        let s = server.clone();
        let lobby = lobby.clone();
        tokio::spawn(async move {
            let mut conn = Conn {
                server: s.clone(),
                stream,
                peer,
                account: None,
                player: None,
                lobby,
            };
            let result = conn.run().await;
            if let Some(id) = conn.player.take() {
                s.world.left(id);
                if let Some(l) = &s.listlink {
                    l.player_removed(id);
                }
            }
            if let Some(name) = conn.account.take() {
                s.log(
                    &peer,
                    &format!(
                        "{name} left ({})",
                        result
                            .as_ref()
                            .err()
                            .map_or("closed".to_string(), |e| e.to_string())
                    ),
                );
            } else if let Err(e) = result {
                if s.verbose {
                    s.log(&peer, &format!("closed before login: {e}"));
                }
            }
        });
    }
}

struct Conn {
    server: Arc<Server>,
    stream: TcpStream,
    peer: SocketAddr,
    /// Lower-case account name, once the world has it online.
    account: Option<String>,
    /// Player id, once logged in.
    player: Option<u16>,
    /// This connection came in on the Login lobby's port.
    lobby: Option<Arc<LobbyWarp>>,
}

/// Bytes read but not yet a whole frame.
struct InBuf {
    buf: Vec<u8>,
    long: bool,
}

impl InBuf {
    /// Whether a whole frame (or one too big, which `take_frame` refuses)
    /// is already in.
    fn has_frame(&self) -> bool {
        frame_header(&self.buf, self.long).is_some_and(|(head, len)| {
            self.buf.len() >= head + len
                || len > MAX_IN_FRAME.max(if self.long { 1 << 20 } else { 0 })
        })
    }

    /// The next whole frame's bytes after its header, if one is in.
    fn take_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        let Some((head, len)) = frame_header(&self.buf, self.long) else {
            return Ok(None);
        };
        if len > MAX_IN_FRAME.max(if self.long { 1 << 20 } else { 0 }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("client frame of {len} bytes"),
            ));
        }
        if self.buf.len() < head + len {
            return Ok(None);
        }
        let frame = self.buf[head..head + len].to_vec();
        self.buf.drain(..head + len);
        Ok(Some(frame))
    }
}

impl Conn {
    async fn run(&mut self) -> io::Result<()> {
        let cfg = self.server.cfg.clone();
        let mut inbuf = InBuf {
            buf: Vec::new(),
            long: false,
        };

        // The login frame: plain zlib in every generation.
        let first = tokio::time::timeout(cfg.login_timeout, async {
            loop {
                if let Some(f) = inbuf.take_frame()? {
                    return Ok::<_, io::Error>(f);
                }
                read_more(&mut self.stream, &mut inbuf.buf).await?;
            }
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no login frame in time"))??;
        let body = zlib_decompress(&first)?;
        let nl = body
            .iter()
            .position(|&b| b == b'\n')
            .map_or(body.len(), |i| i + 1);
        let (line, after) = body.split_at(nl);

        // The Seihai secure login: C1 here, S1 back, then C2 stands in for
        // the classic login frame. The rebuilt classic line goes through the
        // same parse and checks as every other login.
        let secure_at = secure::token_at(line);
        let rebuilt;
        let (line, after) = match secure_at {
            None => (line, after),
            Some(at) => {
                let Some(identity) = self.server.identity.as_ref() else {
                    self.server.log(
                        &self.peer,
                        "secure login refused: secure_login_key is empty",
                    );
                    let mut c = Codec::new(EncryptionGen::Gen2, 0, None);
                    return self
                        .refuse(&mut c, "Secure login is turned off on this server.")
                        .await;
                };
                let reply = match secure::respond(line, at, identity) {
                    Ok(r) => r,
                    Err(e) => {
                        self.server
                            .log(&self.peer, &format!("secure login dropped: {e}"));
                        return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
                    }
                };
                self.stream
                    .write_all(&secure::plain_frame(&reply.s1)?)
                    .await?;
                self.stream.flush().await?;
                let c2 = tokio::time::timeout(cfg.login_timeout, async {
                    loop {
                        if let Some(f) = inbuf.take_frame()? {
                            return Ok::<_, io::Error>(f);
                        }
                        read_more(&mut self.stream, &mut inbuf.buf).await?;
                    }
                })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "no sealed login in time")
                })??;
                rebuilt = match secure::open(
                    &zlib_decompress(&c2)?,
                    reply.opener,
                    &reply.prefix,
                    &reply.inner,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        self.server
                            .log(&self.peer, &format!("secure login dropped: {e}"));
                        return Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
                    }
                };
                let (l, a) = rebuilt.split_at(
                    rebuilt
                        .iter()
                        .position(|&b| b == b'\n')
                        .map_or(rebuilt.len(), |i| i + 1),
                );
                (l, a)
            }
        };

        let login = match login::parse(line, &self.server.extra_tokens) {
            Ok(l) => l,
            Err(e) => {
                self.server.log(&self.peer, &format!("login refused: {e}"));
                // A known type that is not served yet (RC, NC) is refused
                // in its own framing, so the client shows the reason. For
                // anything else the generation is not known: a plain zlib
                // frame is the best guess (it is what gen 2 and gen 3 read).
                let mut c = match e {
                    login::LoginError::NotServed { gen, key, .. } => Codec::new(gen, key, None),
                    _ => Codec::new(EncryptionGen::Gen2, 0, None),
                };
                return self.refuse(&mut c, &e.client_text()).await;
            }
        };
        let version = burst::shape_version(login.version, login.gen);
        let mut codec = Codec::new(login.gen, login.key, login.version);
        if !self.server.login_rate.allow(self.peer.ip()) {
            self.server.log(
                &self.peer,
                &format!("{}: too many logins from this address", login.account),
            );
            return self
                .refuse(
                    &mut codec,
                    "Too many logins from your address; try again in a minute.",
                )
                .await;
        }
        inbuf.long = login
            .version
            .is_some_and(|v| v.profile().protocol.gen5_long_frames);
        let label = login::build_label(login.client_type, login.version);
        let who = format!(
            "{} [{}{} {}, {:?}]",
            login.account,
            if secure_at.is_some() {
                "SEI00001 inner "
            } else {
                ""
            },
            login.token,
            label,
            login.gen
        );
        if secure_at.is_none() && cfg.require_secure_login {
            self.server.log(
                &self.peer,
                &format!("{who}: password in the clear, refused (require_secure_login)"),
            );
            return self
                .refuse(
                    &mut codec,
                    "This server needs the Seihai secure login (start Seihai with --secure-login).",
                )
                .await;
        }

        // An RC (stage 4): its own checks and session; it is not a player,
        // and its token is not a game build, so the allow-list is not asked.
        if login::is_rc(login.client_type) {
            return self.run_rc(&login, codec, inbuf, &who, after).await;
        }
        // NPC Control (stage 4 slice 6): staff only, like an RC.
        if login::is_nc(login.client_type) {
            return self.run_nc(&login, codec, inbuf, &who, after).await;
        }

        if !self.server.allow.allows(&login.token) {
            self.server
                .log(&self.peer, &format!("{who}: version not allowed"));
            return self
                .refuse(
                    &mut codec,
                    "Your client version is not allowed on this server.",
                )
                .await;
        }
        let mut account = match self.check_account(&login, true).await? {
            Ok(a) => a,
            Err(why) => {
                self.server.log(&self.peer, &format!("{who}: {why}"));
                return self.refuse(&mut codec, why).await;
            }
        };
        // Banned by account, or (not for staff) by address or machine.
        let machines = crate::login::machine_ids(&login.tail);
        let banned = crate::bans::Bans::new(&cfg.world).find_login(&crate::bans::Who {
            account: &account.name,
            ip: Some(self.peer.ip()),
            machines: &machines,
            staff: account.staff,
        });
        let banned = banned.unwrap_or_else(|e| {
            self.server
                .log(&self.peer, &format!("bans.txt not read: {e}"));
            None
        });
        if let Some(b) = banned {
            let on = if b.account.eq_ignore_ascii_case(&account.name) {
                String::new()
            } else {
                format!(" ({})", b.account)
            };
            self.server.log(&self.peer, &format!("{who}: banned{on}"));
            return self.refuse(&mut codec, &b.client_text()).await;
        }
        // A player who logged out dead logs in at 0 hearts, as GServer-v2
        // beta4 does (issue #51), unless the world turns `revive_dead` on.
        // The account file keeps what happened; only the login changes.
        if self.server.cfg.revive_dead {
            account.revive();
        }
        let key = login.account.to_ascii_lowercase();
        // What the world sends this player; framed and sent below.
        let (outbox, mut from_world) = tokio::sync::mpsc::unbounded_channel();
        let start = if account.level.is_empty() {
            cfg.start_level.as_str()
        } else {
            account.level.as_str()
        };
        let joining = crate::world::Joining {
            account: key.clone(),
            name: account.name.clone(),
            level: start.to_string(),
            version,
            gen: login.gen,
            // The world keeps props in the 2.22 encoding, as the
            // NPC-server link does; the id is in the message header.
            props: burst::own_props(
                ClientVersion::V222,
                &Player {
                    id: 0,
                    account: &account,
                    leader: false,
                },
            ),
            chests: account.chests.clone(),
            kills: account.kills,
            flags: account.flags.clone(),
            staff: account.staff,
            weapons: account.weapons.clone(),
            machines: machines.clone(),
        };
        let (id, level, gmap, leader) = match self.server.world.join(joining, outbox).await {
            Some(Ok(j)) => (j.id, j.level, j.gmap, j.leader),
            Some(Err(why)) => {
                if why == crate::world::Refused::InUse {
                    self.server
                        .log(&self.peer, &format!("{who}: already online"));
                }
                return self.refuse(&mut codec, why.text()).await;
            }
            None => {
                return self
                    .refuse(&mut codec, "The server is shutting down.")
                    .await
            }
        };
        self.account = Some(key);
        self.player = Some(id);
        {
            let level = level.name.as_str();
            if let Some(l) = &self.server.listlink {
                l.player_added(ListedPlayer {
                    id,
                    account: account.name.clone(),
                    nick: account.nick.clone(),
                    level: level.to_string(),
                    x: account.x,
                    y: account.y,
                    alignment: account.alignment,
                });
            }
        }
        self.server
            .log(&self.peer, &format!("{who} logged in as player {id}"));

        // Whatever came behind the login line in its frame: the language,
        // and for some builds the tamper check or file checks.
        let early = codec.messages(after);
        let frames = burst::login_burst(
            &cfg,
            version,
            login.gen,
            &Player {
                id,
                account: &account,
                leader,
            },
            &level,
            gmap.as_ref(),
            self.server.world_time(),
        );
        for f in frames {
            self.send(&mut codec, &f).await?;
        }
        // On the lobby's port, a build with a Login lobby is sent on to
        // the game port. It leaves the world first, so its login there is
        // not turned away as already online; then the client closes.
        if let Some(warp) = self.lobby.clone() {
            if version.profile().protocol.lobby.is_some() {
                if let Some(id) = self.player.take() {
                    self.server.world.left(id);
                    if let Some(l) = &self.server.listlink {
                        l.player_removed(id);
                    }
                }
                let m = crate::wire::msg(PLO_SERVERWARP, &warp.payload());
                self.send(&mut codec, &m).await?;
                self.server.log(
                    &self.peer,
                    &format!("{who}: sent on to {}:{}", warp.host, warp.port),
                );
                // Wait for the client to close, or give up after a while.
                let mut sink = [0u8; 256];
                let _ = tokio::time::timeout(Duration::from_secs(30), async {
                    while let Ok(n) = self.stream.read(&mut sink).await {
                        if n == 0 {
                            break;
                        }
                    }
                })
                .await;
                return Ok(());
            }
        }
        // Packages named in `gpaks`, the same way a file request is answered.
        let packages = self.server.cfg.gpaks.clone();
        for name in &packages {
            let found = self.server.files.find(name);
            let key = self.server.files.package_key(name);
            let asked = crate::wire::latin1(name);
            for frame in crate::files::offer(version, &asked, name, found.as_ref(), None, key) {
                self.send(&mut codec, &frame).await?;
            }
        }
        let mut session = Session {
            id,
            version,
            stats: BTreeMap::new(),
            nickname: account.nick.clone(),
            account: account.name.clone(),
            sword_power: account.sword.0,
            shield_power: account.shield.0,
            rate: Default::default(),
            score: Default::default(),
            staff: account.staff,
            machines,
            flooding: None,
            kicked: false,
            ping: (!cfg.ping.is_zero() && crate::profile::ServerProfile::of(version).ping)
                .then(crate::ping::Pinger::default),
        };
        for f in session.handle(&self.server, &self.peer, &early) {
            self.send(&mut codec, &f).await?;
        }
        // A listserver message can arrive while the burst is going out.
        let queued = session.link_out(&self.server);
        if !queued.is_empty() {
            self.send(&mut codec, &queued).await?;
        }

        // The idle session.
        let mut heartbeat = (!cfg.heartbeat.is_zero()).then(|| {
            let mut t = tokio::time::interval_at(
                tokio::time::Instant::now() + cfg.heartbeat,
                cfg.heartbeat,
            );
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            t
        });
        let sends_time = version.profile().protocol.plo.contains(PLO_NEWWORLDTIME);
        let mut ping_timer = session.ping.is_some().then(|| {
            let mut t = tokio::time::interval_at(tokio::time::Instant::now() + cfg.ping, cfg.ping);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            t
        });
        let listlink = self.server.listlink.clone();
        let mut last_heard = Instant::now();
        loop {
            let idle_left = (!cfg.idle_timeout.is_zero())
                .then(|| cfg.idle_timeout.saturating_sub(last_heard.elapsed()));
            tokio::select! {
                r = fill(&mut self.stream, &mut inbuf) => {
                    r?;
                    last_heard = Instant::now();
                    let mut reply: Vec<Vec<u8>> = Vec::new();
                    while let Some(frame) = inbuf.take_frame()? {
                        let body = codec.decode(&frame)?;
                        let msgs = codec.messages(&body);
                        if codec.bad_in_a_row >= MAX_BAD_IN_A_ROW {
                            self.server.log(&self.peer, "gen-3 bytes out of step; dropping the client");
                            return self.refuse(&mut codec, "Disconnected for sending invalid packets.").await;
                        }
                        reply.extend(session.handle(&self.server, &self.peer, &msgs));
                        if session.kicked {
                            self.server.log(&self.peer, "kicked by the anti-cheat");
                            return self.refuse(&mut codec, "Disconnected by the server.").await;
                        }
                    }
                    for f in reply {
                        self.send(&mut codec, &f).await?;
                    }
                }
                _ = tick(ping_timer.as_mut()) => {
                    if let Some(p) = session.ping.as_mut() {
                        let m = p.ping(Instant::now());
                        if cfg.ping_misses > 0 && p.missed >= cfg.ping_misses {
                            self.server.log(&self.peer, &format!("{} pings unanswered", p.missed));
                            return self.refuse(&mut codec, "Disconnected for not answering.").await;
                        }
                        self.send(&mut codec, &m).await?;
                    }
                }
                _ = tick(heartbeat.as_mut()) => {
                    if sends_time {
                        let mut t = Vec::new();
                        gint4(self.server.world_time(), &mut t);
                        self.send(&mut codec, &msg(PLO_NEWWORLDTIME, &t)).await?;
                    }
                }
                _ = sleep_opt(idle_left) => {
                    self.server.log(&self.peer, "idle timeout");
                    return self.refuse(&mut codec, "Disconnected for being idle.").await;
                }
                Some(frames) = from_world.recv() => {
                    // An empty item: the anti-cheat or an RC kicks the
                    // player (the world's log or the RC's says which).
                    if frames.is_empty() {
                        self.server.log(&self.peer, "kicked by the server");
                        return self.refuse(&mut codec, "Disconnected by the server.").await;
                    }
                    // The world decides the frames (gen 2 needs the board
                    // in a frame of its own), and an item's frames go out
                    // together: no reply of this task's lands between.
                    for f in frames {
                        self.send(&mut codec, &f).await?;
                    }
                }
                _ = list_ready(listlink.as_deref()) => {
                    let reply = session.link_out(&self.server);
                    if !reply.is_empty() {
                        self.send(&mut codec, &reply).await?;
                    }
                }
            }
        }
    }

    /// The account for a login, made on first login when the world allows
    /// it. The outer error is an I/O failure; the inner one is the reason
    /// to give the client.
    async fn check_account(
        &self,
        login: &Login,
        create: bool,
    ) -> io::Result<Result<Account, &'static str>> {
        let store = self.server.store.clone();
        let cfg = self.server.cfg.clone();
        let name = login.account.clone();
        let password = login.password.clone();
        let account_type = login.client_type;
        let link = self
            .server
            .listlink
            .clone()
            .filter(|_| cfg.listserver_accounts);
        // PBKDF2 is slow on purpose (about 0.15 CPU-seconds), so a local
        // check waits for one of the `password_checks` slots: a burst of
        // logins queues instead of fanning out over the blocking pool. The
        // listserver answer only waits on the network, so it takes none.
        let slot = match link {
            Some(_) => None,
            None => Some(self.server.password_checks.slot().await),
        };
        // Both block: keep them off the network threads.
        tokio::task::spawn_blocking(move || {
            // Held until the hash is done, even if the connection goes.
            let _slot = slot;
            if let Some(link) = link {
                // The listserver owns the password; the local file keeps
                // the player's state and is made on first login.
                let verdict = link
                    .verify(&name, &password, account_type)
                    .recv_timeout(crate::listlink::VERIFY_TIMEOUT)
                    .unwrap_or(crate::listlink::Verdict::Unreachable);
                return match verdict {
                    crate::listlink::Verdict::Ok => match store.load(&name)? {
                        Some(a) => Ok(Ok(a)),
                        None if !create => Ok(Err(crate::rc::NOT_STAFF)),
                        None => {
                            let a = Account::new(&name, &cfg.start_level, cfg.start_x, cfg.start_y);
                            store.save(&a)?;
                            Ok(Ok(a))
                        }
                    },
                    crate::listlink::Verdict::Refused(_) => {
                        Ok(Err("Account name or password is wrong."))
                    }
                    crate::listlink::Verdict::Unreachable => Ok(Err(
                        "The account server cannot be reached; try again later.",
                    )),
                };
            }
            match store.load(&name)? {
                Some(a) if a.check_password(&password) => Ok(Ok(a)),
                Some(_) => Ok(Err("Account name or password is wrong.")),
                None if create && cfg.create_accounts && !password.is_empty() => {
                    let mut a = Account::new(&name, &cfg.start_level, cfg.start_x, cfg.start_y);
                    a.set_password(&password);
                    store.save(&a)?;
                    Ok(Ok(a))
                }
                None => Ok(Err("Account name or password is wrong.")),
            }
        })
        .await
        .map_err(io::Error::other)?
    }

    /// An RC login (`rc.rs`): the password, `STAFF` and the address are
    /// checked, then the RC gets its welcome and the link stays open until
    /// it closes. No account file is made for an RC.
    async fn run_rc(
        &mut self,
        login: &Login,
        mut codec: Codec,
        mut inbuf: InBuf,
        who: &str,
        after: &[u8],
    ) -> io::Result<()> {
        let account = match self.check_account(login, false).await? {
            Ok(a) => a,
            Err(why) => {
                self.server.log(&self.peer, &format!("RC {who}: {why}"));
                return self.refuse(&mut codec, why).await;
            }
        };
        if let Err(why) = crate::rc::admit(&account, self.peer.ip()) {
            self.server.log(&self.peer, &format!("RC {who}: {why}"));
            return self.refuse(&mut codec, why).await;
        }
        // A banned account is kept out of RC too (`config/bans.txt`).
        if let Some(text) = self.banned(&account.name) {
            self.server.log(&self.peer, &format!("RC {who}: banned"));
            return self.refuse(&mut codec, &text).await;
        }
        self.server.log(&self.peer, &format!("RC {who} logged in"));
        let welcome = crate::rc::welcome(&self.server.cfg.name, &account.name);
        self.send(&mut codec, &welcome).await?;
        let mut rc = crate::rc::RcSession::default();
        let mut early = rc.handle(&crate::rcfiles::split(after));
        // The nickname an RC sends straight after its login goes into its
        // first list entry.
        let nick = early.iter().rev().find_map(|e| match e {
            crate::rc::RcEvent::Nick(n) => Some(n.clone()),
            _ => None,
        });
        early.retain(|e| !matches!(e, crate::rc::RcEvent::Nick(_)));
        let server = self.server.clone();
        let peer = self.peer;
        let world = &server.world;
        let (outbox, mut from_world) = tokio::sync::mpsc::unbounded_channel();
        let Some(id) = world.rc_join(account.name.clone(), nick, outbox).await else {
            return self.refuse(&mut codec, "The server is full.").await;
        };
        // Hands the world what it serves; account and file requests come
        // back, for this connection to carry out.
        let tell = |events: Vec<crate::rc::RcEvent>| {
            let mut ops = Vec::new();
            for e in events {
                use crate::rc::RcEvent;
                let what = match &e {
                    RcEvent::Account(_)
                    | RcEvent::Files(_)
                    | RcEvent::Server(_)
                    | RcEvent::NpcServerQuery(_) => {
                        ops.push(e);
                        continue;
                    }
                    RcEvent::Nick(_) => None,
                    RcEvent::Chat(t) => Some(format!("chat: {}", crate::wire::text(t))),
                    RcEvent::Pm { to, text } => Some(format!(
                        "private message to {to:?}: {}",
                        crate::wire::text(text)
                    )),
                    RcEvent::AdminAll(t) => {
                        Some(format!("admin message to all: {}", crate::wire::text(t)))
                    }
                    RcEvent::AdminTo(id, t) => {
                        Some(format!("admin message to {id}: {}", crate::wire::text(t)))
                    }
                    RcEvent::Kick(id) => Some(format!("disconnects player {id}")),
                    RcEvent::Warp { id, level, x2, y2 } => Some(format!(
                        "warps player {id} to {} {} {}",
                        crate::wire::text(level),
                        f32::from(*x2) / 2.0,
                        f32::from(*y2) / 2.0
                    )),
                };
                if let Some(what) = what {
                    server.log(&peer, &format!("RC {} {what}", account.name));
                }
                world.rc_event(id, e);
            }
            ops
        };
        // Where this RC reached us: NC logins go to the same place.
        let here = self.stream.local_addr()?;
        let browser = Arc::new(Mutex::new(crate::rcfiles::Browser::new(
            &self.server.cfg.world,
            &account.folder_rights,
        )));
        // Account and file requests, in order, off the async threads (a
        // password set hashes, a file is read or written).
        let accounts = |ops: Vec<crate::rc::RcEvent>| {
            let server = server.clone();
            let name = account.name.clone();
            let browser = browser.clone();
            async move {
                if ops.is_empty() {
                    return Vec::new();
                }
                let done = tokio::task::spawn_blocking(move || {
                    let cfg = &server.cfg;
                    let start = (cfg.start_level.as_str(), cfg.start_x, cfg.start_y);
                    let online = server.online_accounts();
                    let mut replies = Vec::new();
                    for op in ops {
                        let (reply, log) = match op {
                            crate::rc::RcEvent::Account(op) => {
                                // A player id is the account online with it.
                                let op = match op {
                                    crate::rcaccounts::Op::PropsGetId(pid) => {
                                        match server.world.account_of(pid) {
                                            Some(account) => {
                                                crate::rcaccounts::Op::PropsGet { id: pid, account }
                                            }
                                            None => op,
                                        }
                                    }
                                    op => op,
                                };
                                let ctx = crate::rcaccounts::Ctx {
                                    store: &server.store,
                                    bans: &crate::bans::Bans::new(&cfg.world),
                                    online: &online,
                                    start,
                                    admin: &name,
                                    world: Some(&server.world),
                                };
                                let d = crate::rcaccounts::run(&ctx, op);
                                // Banned while online: off they go, as on an
                                // RC disconnect.
                                if let Some(pid) = d.kick.and_then(|a| server.world.id_of(&a)) {
                                    server.world.rc_event(id, crate::rc::RcEvent::Kick(pid));
                                }
                                (vec![d.reply], d.log)
                            }
                            crate::rc::RcEvent::Files(op) => {
                                let (r, l) = browser.lock().unwrap().run(op);
                                (r, Some(format!("files: {l}")))
                            }
                            crate::rc::RcEvent::NpcServerQuery(pid) => (
                                vec![crate::rc::npcserver_addr(
                                    pid,
                                    &here.ip().to_string(),
                                    here.port(),
                                )],
                                Some(format!("asks where NC logins go (player {pid})")),
                            ),
                            crate::rc::RcEvent::Server(op) => match op {
                                crate::rcserver::Op::FlagsGet => {
                                    let f = server.world.server_flags_blocking();
                                    (
                                        vec![crate::rcserver::flags_reply(&f)],
                                        Some("reads the server flags".to_string()),
                                    )
                                }
                                crate::rcserver::Op::FlagsSet(f) => {
                                    let n = f.len();
                                    server.world.set_server_flags_blocking(f);
                                    (Vec::new(), Some(format!("sets {n} server flag(s)")))
                                }
                                op => match crate::rcserver::Files::new(&cfg.world).run(&op) {
                                    Some(d) => (d.replies, Some(d.log)),
                                    None => continue,
                                },
                            },
                            _ => continue,
                        };
                        if let Some(l) = log {
                            server.log(&peer, &format!("RC {name} {l}"));
                        }
                        replies.extend(reply.into_iter().filter(|r| !r.is_empty()));
                    }
                    replies
                })
                .await;
                done.unwrap_or_default()
            }
        };
        for r in accounts(tell(early)).await {
            self.send(&mut codec, &r).await?;
        }
        let idle = self.server.cfg.idle_timeout;
        let mut last_heard = Instant::now();
        let result = loop {
            let idle_left = (!idle.is_zero()).then(|| idle.saturating_sub(last_heard.elapsed()));
            tokio::select! {
                r = fill(&mut self.stream, &mut inbuf) => {
                    if let Err(e) = r {
                        break Err(e);
                    }
                    last_heard = Instant::now();
                    let mut events = Vec::new();
                    while let Some(frame) = inbuf.take_frame()? {
                        let body = codec.decode(&frame)?;
                        events.extend(rc.handle(&crate::rcfiles::split(&body)));
                    }
                    let mut sent = Ok(());
                    for r in accounts(tell(events)).await {
                        sent = self.send(&mut codec, &r).await;
                        if sent.is_err() {
                            break;
                        }
                    }
                    if let Err(e) = sent {
                        break Err(e);
                    }
                }
                Some(frames) = from_world.recv() => {
                    let mut sent = Ok(());
                    for f in frames {
                        sent = self.send(&mut codec, &f).await;
                        if sent.is_err() {
                            break;
                        }
                    }
                    if let Err(e) = sent {
                        break Err(e);
                    }
                }
                _ = sleep_opt(idle_left) => {
                    self.server.log(&self.peer, "RC idle timeout");
                    let _ = self.refuse(&mut codec, "Disconnected for being idle.").await;
                    break Ok(());
                }
            }
        };
        world.rc_left(id);
        let unhandled: Vec<String> = rc
            .unhandled
            .iter()
            .map(|(id, n)| format!("{id} x{n}"))
            .collect();
        self.server.log(
            &self.peer,
            &format!(
                "RC {} left ({}){}",
                account.name,
                result
                    .as_ref()
                    .err()
                    .map_or("closed".to_string(), |e| e.to_string()),
                if unhandled.is_empty() {
                    String::new()
                } else {
                    format!(" (not answered yet: {})", unhandled.join(", "))
                }
            ),
        );
        // The leave is logged here; an error passed up would also log
        // "closed before login", as the lab run of 2026-11-12 saw.
        Ok(())
    }

    /// An NPC Control login (`nc.rs`): checked as an RC is, then served
    /// from the NPC-server's list (`npclink::Hub`) until it closes.
    async fn run_nc(
        &mut self,
        login: &Login,
        mut codec: Codec,
        mut inbuf: InBuf,
        who: &str,
        after: &[u8],
    ) -> io::Result<()> {
        let account = match self.check_account(login, false).await? {
            Ok(a) => a,
            Err(why) => {
                self.server.log(&self.peer, &format!("NC {who}: {why}"));
                return self.refuse(&mut codec, why).await;
            }
        };
        if let Err(why) = crate::rc::admit(&account, self.peer.ip()) {
            self.server.log(&self.peer, &format!("NC {who}: {why}"));
            return self.refuse(&mut codec, why).await;
        }
        if let Some(text) = self.banned(&account.name) {
            self.server.log(&self.peer, &format!("NC {who}: banned"));
            return self.refuse(&mut codec, &text).await;
        }
        self.server.log(&self.peer, &format!("NC {who} logged in"));
        let hub = self.server.npc.clone();
        let (list, mut changes) = hub.nc_watch();
        let mut first: Vec<u8> = Vec::new();
        for (id, n) in &list {
            first.extend(crate::nc::npc_add(*id, n));
        }
        if !first.is_empty() {
            self.send(&mut codec, &first).await?;
        }
        let mut unhandled: BTreeMap<u8, u32> = BTreeMap::new();
        let mut pending: Vec<Vec<u8>> = crate::rcfiles::split(after);
        let idle = self.server.cfg.idle_timeout;
        let mut last_heard = Instant::now();
        let result = loop {
            for m in std::mem::take(&mut pending) {
                let op = match crate::nc::parse(&m) {
                    Ok(Some(op)) => op,
                    Ok(None) => continue,
                    Err(id) => {
                        *unhandled.entry(id).or_default() += 1;
                        continue;
                    }
                };
                let reply = self.nc_op(&hub, &account.name, op).await;
                if let Some(r) = reply {
                    self.send(&mut codec, &r).await?;
                }
            }
            let idle_left = (!idle.is_zero()).then(|| idle.saturating_sub(last_heard.elapsed()));
            tokio::select! {
                r = fill(&mut self.stream, &mut inbuf) => {
                    if let Err(e) = r {
                        break Err(e);
                    }
                    last_heard = Instant::now();
                    while let Some(frame) = inbuf.take_frame()? {
                        let body = codec.decode(&frame)?;
                        pending.extend(crate::rcfiles::split(&body));
                    }
                }
                Some(change) = changes.recv() => {
                    let m = match change {
                        crate::npclink::NcChange::Added(id, n) => crate::nc::npc_add(id, &n),
                        crate::npclink::NcChange::Deleted(id) => crate::nc::npc_delete(id),
                    };
                    if let Err(e) = self.send(&mut codec, &m).await {
                        break Err(e);
                    }
                }
                _ = sleep_opt(idle_left) => {
                    self.server.log(&self.peer, "NC idle timeout");
                    let _ = self.refuse(&mut codec, "Disconnected for being idle.").await;
                    break Ok(());
                }
            }
        };
        let unhandled: Vec<String> = unhandled
            .iter()
            .map(|(id, n)| format!("{id} x{n}"))
            .collect();
        self.server.log(
            &self.peer,
            &format!(
                "NC {} left ({}){}",
                account.name,
                result
                    .as_ref()
                    .err()
                    .map_or("closed".to_string(), |e| e.to_string()),
                if unhandled.is_empty() {
                    String::new()
                } else {
                    format!(" (not answered: {})", unhandled.join(", "))
                }
            ),
        );
        Ok(())
    }

    /// One NC request: the reply, if it has one.
    async fn nc_op(
        &self,
        hub: &crate::npclink::Hub,
        account: &str,
        op: crate::nc::Op,
    ) -> Option<Vec<u8>> {
        use crate::nc::Op;
        use seihai_npcserver::link::{nc_get, Message};
        let log = |what: String| self.server.log(&self.peer, &format!("NC {account} {what}"));
        match op {
            Op::Attributes(id) => {
                let t = hub.nc_get(id, nc_get::ATTRIBUTES).await;
                Some(crate::nc::attributes(&t.unwrap_or_default()))
            }
            Op::Script(id) => {
                let t = hub.nc_get(id, nc_get::SCRIPT).await;
                Some(crate::nc::script(id, &t.unwrap_or_default()))
            }
            Op::SetScript(id, script) => {
                log(format!("sets the script of NPC {id}"));
                if !hub.nc_send(Message::NcScriptSet { npc_id: id, script }) {
                    log("no NPC-server connected".into());
                }
                None
            }
            Op::Delete(id) => {
                // The 159 comes when the NPC-server has taken it out.
                log(format!("deletes NPC {id}"));
                if !hub.nc_send(Message::NcNpcDelete { npc_id: id }) {
                    log("no NPC-server connected".into());
                }
                None
            }
            Op::WeaponList => Some(crate::nc::no_weapons()),
        }
    }

    /// The text for a banned account (`config/bans.txt`), or `None`. A
    /// ban file that cannot be read is logged and lets the login through.
    fn banned(&self, account: &str) -> Option<String> {
        match crate::bans::Bans::new(&self.server.cfg.world).find(account) {
            Ok(b) => b.map(|b| b.client_text()),
            Err(e) => {
                self.server
                    .log(&self.peer, &format!("bans.txt not read: {e}"));
                None
            }
        }
    }

    async fn send(&mut self, codec: &mut Codec, body: &[u8]) -> io::Result<()> {
        let frame = codec.encode(body)?;
        self.stream.write_all(&frame).await
    }

    /// Tell the client why and close.
    async fn refuse(&mut self, codec: &mut Codec, why: &str) -> io::Result<()> {
        let _ = self
            .send(codec, &msg(PLO_DISCMESSAGE, &crate::wire::latin1(why)))
            .await;
        let _ = self.stream.flush().await;
        let _ = self.stream.shutdown().await;
        // Read what the client still sends until it closes (at most 2 s).
        // Closing a socket with unread input makes Windows send a reset,
        // and a reset can drop the message above before the client reads
        // it (seen in CI: the secure-login refusal arrived empty).
        let drain = async {
            let mut buf = [0u8; 4096];
            while let Ok(n) = self.stream.read(&mut buf).await {
                if n == 0 {
                    break;
                }
            }
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), drain).await;
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            why.to_string(),
        ))
    }
}

/// What one logged-in client has said, for the log.
struct Session {
    id: u16,
    version: ClientVersion,
    stats: BTreeMap<u8, usize>,
    nickname: String,
    account: String,
    sword_power: u8,
    shield_power: u8,
    /// Messages in the last second, for the anti-cheat's rate check.
    rate: crate::anticheat::Rate,
    score: crate::anticheat::Score,
    /// A staff account, for the anti-cheat's `staff exempt`.
    staff: bool,
    /// The machine ids from the login line, for an anti-cheat ban.
    machines: Vec<String>,
    /// Over the rate now: whether to drop (one violation per run of
    /// messages over the limit, not one per message).
    flooding: Option<bool>,
    /// The anti-cheat says to close the connection.
    kicked: bool,
    /// Pings, for a build that reads `PLO_PING`.
    ping: Option<crate::ping::Pinger>,
}

impl Session {
    /// One more message against the rate limit: whether to drop it (and
    /// `kicked` when the policy says so).
    fn over_rate(&mut self, server: &Server) -> bool {
        let ac = &server.anticheat;
        let now = std::time::Instant::now();
        if !self.rate.over(ac.policy.rate, now) {
            self.flooding = None;
            return false;
        }
        if let Some(drop) = self.flooding {
            return drop;
        }
        let kind = crate::anticheat::Kind::Rate;
        let v = self.score.report(&ac.policy, kind, now, self.staff);
        if v.log {
            let what = format!("over {} a second{}", ac.policy.rate, v.suffix());
            ac.log(&self.account, self.version.label(), "", kind, &what);
        }
        let who = crate::anticheat::Offender {
            account: &self.account,
            machines: &self.machines,
            staff: self.staff,
        };
        self.kicked |= ac.enforce(&who, kind, &v);
        self.flooding = Some(v.correct);
        v.correct || v.kick
    }

    /// Handle the client's messages; returns the reply as frame bodies
    /// (none, one, or more when a file needs frames of its own).
    fn handle(&mut self, server: &Server, peer: &SocketAddr, msgs: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut reply = Vec::new();
        for m in msgs {
            let Some((&id, rest)) = m.split_first() else {
                continue;
            };
            let id = id.wrapping_sub(0x20);
            // File checks are left out: a client checks its whole cache at
            // login, hundreds at once, and each answer is cheap.
            if !matches!(id, PLI_WANTFILE | PLI_UPDATEFILE) && self.over_rate(server) {
                if self.kicked {
                    break;
                }
                continue;
            }
            let payload = rest.strip_suffix(b"\n").unwrap_or(rest);
            let n = self.stats.entry(id).or_insert(0);
            *n += 1;
            let first_time = *n == 1;
            match id {
                PLI_PONG => {
                    let rtt = self
                        .ping
                        .as_mut()
                        .and_then(|p| p.answer(payload, Instant::now()));
                    if let (Some(rtt), true) = (rtt, server.verbose) {
                        server.log(peer, &format!("ping {} ms", rtt.as_millis()));
                    }
                }
                // Files from the world folder (`files.rs`). A request for
                // a file the world does not have gets "failed"; a check of
                // a copy gets the file when the server's is newer, else
                // "up to date" (before 2.1, "failed"). 1.21 reads none of
                // the answers and gets none.
                PLI_WANTFILE | PLI_UPDATEFILE => {
                    let (since, raw) = if id == PLI_UPDATEFILE {
                        match payload.split_at_checked(5) {
                            Some((t, name)) => (Some(crate::wire::read_gint5(t)), name),
                            None => continue,
                        }
                    } else {
                        (None, payload)
                    };
                    let name = seihai_core::protocol::latin1(raw);
                    let found = server.files.find(&name);
                    let key = server.files.package_key(&name);
                    let newer = match (&found, since) {
                        (Some(f), Some(t)) => f.mod_time > t,
                        (found, None) => found.is_some(),
                        (None, Some(_)) => false,
                    };
                    let sent =
                        crate::files::offer(self.version, raw, &name, found.as_ref(), since, key);
                    match sent.len() {
                        1 => reply.extend_from_slice(&sent[0]),
                        n if n > 1 => {
                            if !reply.is_empty() {
                                frames.push(std::mem::take(&mut reply));
                            }
                            frames.extend(sent);
                        }
                        _ => {}
                    }
                    if server.verbose {
                        let what = if newer { "sent" } else { "not sent" };
                        server.log(peer, &format!("file {name}: {what}"));
                    }
                }
                PLI_NPCPROPS => server.world.npc_props(self.id, payload),
                PLI_WEAPONADD => server.world.weapon_add(self.id, payload),
                PLI_BADDYPROPS => server.world.baddy_props(self.id, payload),
                PLI_BADDYHURT => server.world.baddy_hurt(self.id, payload),
                PLI_HURTPLAYER | PLI_CLAIMPKER => {
                    if server.verbose {
                        let hex: Vec<String> = payload.iter().map(|b| format!("{b:02x}")).collect();
                        let what = if id == PLI_HURTPLAYER {
                            "hit"
                        } else {
                            "kill claim"
                        };
                        server.log(peer, &format!("{what}: {}", hex.join(" ")));
                    }
                    if id == PLI_HURTPLAYER {
                        server.world.hurt_player(self.id, payload)
                    } else {
                        server.world.claim_pker(self.id, payload)
                    }
                }
                PLI_BOMBADD => server.world.relay(self.id, PLO_BOMBADD, true, payload),
                PLI_BOMBDEL => server.world.relay(self.id, PLO_BOMBDEL, false, payload),
                PLI_ARROWADD => server.world.relay(self.id, PLO_ARROWADD, true, payload),
                PLI_FIRESPY => server.world.relay(self.id, PLO_FIRESPY, true, payload),
                PLI_EXPLOSION => server.world.relay(self.id, PLO_EXPLOSION, true, payload),
                PLI_PLAYERPROPS => {
                    let bow = crate::others::bow_prop10(self.version);
                    server.world.props(self.id, payload);
                    let props = parse_player_props_with(payload, bow);
                    if let Some(nick) = props.iter().find(|p| p.id == 0) {
                        let nick = nick
                            .raw
                            .get(1..)
                            .map(seihai_core::protocol::latin1)
                            .unwrap_or_default();
                        if nick != self.nickname {
                            if server.verbose {
                                server.log(peer, &format!("nickname {nick}"));
                            }
                            self.nickname = nick.clone();
                            if let Some(l) = &server.listlink {
                                l.set_nickname(self.id, &nick);
                            }
                        }
                    }
                }
                // Read and not answered in stage 1. See `stage1_quiet`.
                _ if stage1_quiet(id) => {}
                // Player flags: `name` or `name=value`; kept with the account.
                PLI_FLAGSET | PLI_FLAGDEL => {
                    let text = seihai_core::protocol::latin1(payload);
                    server.world.flag(self.id, text, id == PLI_FLAGSET);
                }
                // Items on the floor: `GCHAR x*2, GCHAR y*2[, GCHAR item]`.
                PLI_ITEMADD | PLI_ITEMDEL | PLI_ITEMTAKE => {
                    let op = match id {
                        PLI_ITEMADD => crate::world::ItemOp::Add,
                        PLI_ITEMDEL => crate::world::ItemOp::Del,
                        _ => crate::world::ItemOp::Take,
                    };
                    let g = |b: &u8| b.wrapping_sub(0x20);
                    match (op, payload) {
                        (crate::world::ItemOp::Del, [x, y, ..]) => {
                            server.world.item(self.id, op, g(x), g(y), 0)
                        }
                        (_, [x, y, item, ..]) => {
                            server.world.item(self.id, op, g(x), g(y), g(item))
                        }
                        _ => {}
                    }
                }
                // A chest the player opened: `GCHAR x, GCHAR y` in tiles.
                PLI_OPENCHEST => {
                    if let [x, y, ..] = payload {
                        server.world.open_chest(
                            self.id,
                            x.wrapping_sub(0x20),
                            y.wrapping_sub(0x20),
                        );
                    }
                }
                // A gmap neighbour: `GINT5 mod time, name`.
                PLI_ADJACENTLEVEL => {
                    if let Some((t, name)) = payload.split_at_checked(5) {
                        let name = seihai_core::protocol::latin1(name);
                        server
                            .world
                            .adjacent(self.id, name, crate::wire::read_gint5(t));
                    }
                }
                // A link the player walked into: `[GINT5 mod time,] GCHAR
                // x*2, GCHAR y*2, name`. The world answers with the level.
                PLI_LEVELWARP | PLI_LEVELWARPMOD => {
                    let (mod_time, rest) = if id == PLI_LEVELWARPMOD {
                        match payload.split_at_checked(5) {
                            Some((t, rest)) => (Some(crate::wire::read_gint5(t)), rest),
                            None => continue,
                        }
                    } else {
                        (None, payload)
                    };
                    let [x, y, name @ ..] = rest else {
                        continue;
                    };
                    server.world.warp(crate::world::Warp {
                        player: self.id,
                        level: seihai_core::protocol::latin1(name),
                        x: crate::others::classic_pos(*x),
                        y: crate::others::classic_pos(*y),
                        mod_time,
                    });
                }
                _ => {
                    if first_time {
                        server.note_unknown(id);
                        if server.verbose {
                            server.log(peer, &format!("message {id} not handled in stage 1"));
                        }
                    }
                }
            }
        }
        if !reply.is_empty() {
            frames.push(reply);
        }
        frames
    }

    /// Packets for a private message or a finished listserver file.
    fn link_out(&mut self, server: &Server) -> Vec<u8> {
        let Some(link) = &server.listlink else {
            return Vec::new();
        };
        let prof = self.version.profile();
        let mut out = Vec::new();
        for delivery in link.take(self.id) {
            match delivery {
                Delivery::Private {
                    from_id,
                    nick,
                    account,
                    text,
                } => {
                    // One quoted field, so a newline in the text stays
                    // one message. `""` is one quote.
                    out.extend(crate::others::private_message(
                        self.version,
                        from_id,
                        &latin1(&nick),
                        &latin1(&account),
                        quoted(&text).as_bytes(),
                    ));
                }
                Delivery::Image { kind, name } => {
                    server.remember_image(&self.account, kind, &name);
                    let Some((pid, val)) =
                        image_value(kind, &name, self.sword_power, self.shield_power)
                    else {
                        continue;
                    };
                    if prof.props.own.contains(pid) {
                        let mut p = vec![gchar(pid)];
                        p.extend(val);
                        out.extend(msg(PLO_PLAYERPROPS, &p));
                    }
                }
            }
        }
        out
    }
}

/// A quoted field: `"` doubled, nothing else escaped.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// The prop id and its value for an image the listserver just sent.
fn image_value(kind: u8, name: &str, sword: u8, shield: u8) -> Option<(u8, Vec<u8>)> {
    let raw = latin1(name);
    let raw = &raw[..raw.len().min(123)];
    match kind {
        0 => {
            let mut v = vec![gchar(100 + raw.len() as u8)];
            v.extend_from_slice(raw);
            Some((11, v))
        }
        1 => {
            let mut v = Vec::new();
            gstring(raw, &mut v);
            Some((35, v))
        }
        2 => Some((8, power_image(sword, 30, raw))),
        3 => Some((9, power_image(shield, 10, raw))),
        _ => None,
    }
}

fn power_image(power: u8, bias: u8, image: &[u8]) -> Vec<u8> {
    if image.is_empty() {
        return vec![gchar(power)];
    }
    let mut v = vec![gchar(power.saturating_add(bias).min(223))];
    gstring(image, &mut v);
    v
}

/// Wait until the list link has something for a player. Pending forever
/// when there is no link, so the `select` ignores it.
async fn list_ready(link: Option<&crate::listlink::ListLink>) {
    match link {
        Some(l) => l.notifier().notified().await,
        None => std::future::pending().await,
    }
}

/// More bytes for a session loop, unless a whole frame is already in: a
/// client may send its first messages in the same packet as its login (or
/// as the sealed login, `SEI00001`), and those must not wait for the
/// next packet.
async fn fill(stream: &mut TcpStream, inbuf: &mut InBuf) -> io::Result<()> {
    if inbuf.has_frame() {
        return Ok(());
    }
    read_more(stream, &mut inbuf.buf).await
}

async fn read_more(stream: &mut TcpStream, buf: &mut Vec<u8>) -> io::Result<()> {
    let mut chunk = [0u8; 8192];
    let n = stream.read(&mut chunk).await?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "client closed",
        ));
    }
    buf.extend_from_slice(&chunk[..n]);
    if buf.len() > 4 * MAX_IN_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "client sent too much at once",
        ));
    }
    Ok(())
}

async fn tick(t: Option<&mut tokio::time::Interval>) {
    match t {
        Some(t) => {
            t.tick().await;
        }
        None => std::future::pending().await,
    }
}

async fn sleep_opt(d: Option<Duration>) {
    match d {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}
