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
use crate::wire::{gchar, gint4, gshort, gstring, ids::*, latin1, msg};
use seihai_core::protocol::{
    parse_player_props_with, zlib_decompress, ClientVersion, EncryptionGen,
};
use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
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
    /// The files the world serves (see `files`).
    pub files: crate::files::FileStore,
    /// The NPC-server link's shared state (see `npclink`).
    pub npc: Arc<crate::npclink::Hub>,
    started: Instant,
    pub verbose: bool,
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
        let login_rate = crate::ratelimit::LoginRate::new(cfg.login_rate, cfg.login_rate_loopback);
        let password_checks = crate::ratelimit::PasswordChecks::new(cfg.password_check_slots());
        Server {
            files: crate::files::FileStore::new(&cfg.world),
            npc,
            world,
            login_rate,
            password_checks,
            cfg,
            allow,
            extra_tokens,
            store,
            started: Instant::now(),
            verbose: false,
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
    server.world.start();
    loop {
        let (stream, peer) = listener.accept().await?;
        let _ = stream.set_nodelay(true);
        let s = server.clone();
        tokio::spawn(async move {
            let mut conn = Conn {
                server: s.clone(),
                stream,
                peer,
                account: None,
                player: None,
            };
            let result = conn.run().await;
            if let Some(id) = conn.player.take() {
                s.world.left(id);
                s.npc.left(id);
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
}

/// Bytes read but not yet a whole frame.
struct InBuf {
    buf: Vec<u8>,
    long: bool,
}

impl InBuf {
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
        let label = login.version.map_or("unknown build", |v| v.label());
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
        let account = match self.check_account(&login).await? {
            Ok(a) => a,
            Err(why) => {
                self.server.log(&self.peer, &format!("{who}: {why}"));
                return self.refuse(&mut codec, why).await;
            }
        };
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
                },
            ),
            chests: account.chests.clone(),
            flags: account.flags.clone(),
        };
        let (id, level, gmap) = match self.server.world.join(joining, outbox).await {
            Some(Ok(j)) => (j.id, j.level, j.gmap),
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
            // The link carries props in the 2.22 encoding.
            let props = burst::own_props(
                ClientVersion::V222,
                &Player {
                    id,
                    account: &account,
                },
            );
            self.server.npc.joined(id, &account.name, level, &props);
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
            },
            &level,
            gmap.as_ref(),
            self.server.world_time(),
        );
        for f in frames {
            self.send(&mut codec, &f).await?;
        }
        let mut session = Session {
            id,
            version,
            stats: BTreeMap::new(),
            nickname: account.nick.clone(),
            account: account.name.clone(),
            sword_power: account.sword.0,
            shield_power: account.shield.0,
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
        let listlink = self.server.listlink.clone();
        let mut last_heard = Instant::now();
        loop {
            let idle_left = (!cfg.idle_timeout.is_zero())
                .then(|| cfg.idle_timeout.saturating_sub(last_heard.elapsed()));
            tokio::select! {
                r = read_more(&mut self.stream, &mut inbuf.buf) => {
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
                    }
                    for f in reply {
                        self.send(&mut codec, &f).await?;
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
    async fn check_account(&self, login: &Login) -> io::Result<Result<Account, &'static str>> {
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
                None if cfg.create_accounts && !password.is_empty() => {
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
}

impl Session {
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
            let payload = rest.strip_suffix(b"\n").unwrap_or(rest);
            let n = self.stats.entry(id).or_insert(0);
            *n += 1;
            let first_time = *n == 1;
            match id {
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
                    let newer = match (&found, since) {
                        (Some(f), Some(t)) => f.mod_time > t,
                        (found, None) => found.is_some(),
                        (None, Some(_)) => false,
                    };
                    let sent = found
                        .filter(|_| newer)
                        .and_then(|f| crate::files::send_file(self.version, &f));
                    match sent {
                        Some(mut out) if out.len() == 1 => reply.append(&mut out[0]),
                        Some(out) => {
                            if !reply.is_empty() {
                                frames.push(std::mem::take(&mut reply));
                            }
                            frames.extend(out);
                        }
                        None if since.is_some() && !newer => {
                            reply.extend(crate::files::up_to_date(self.version, raw));
                        }
                        None => reply.extend(crate::files::failed(self.version, raw)),
                    }
                    if server.verbose {
                        let what = if newer { "sent" } else { "not sent" };
                        server.log(peer, &format!("file {name}: {what}"));
                    }
                }
                PLI_PLAYERPROPS => {
                    let bow = self.version.profile().props.bow_prop10;
                    server.world.props(self.id, payload);
                    server.npc.props(self.id, payload, bow);
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
                // Read and not answered in stage 1: the language, the
                // packet count (1.33 to 1.39, every 10 s; not a login),
                // the 1.33 to 1.37 weapon upload, the tamper check (never
                // required), the process list, image updates,
                // level entry and walking off a level.
                PLI_LANGUAGE | PLI_PACKETCOUNT | PLI_WEAPONADD | PLI_TAMPERCHECK
                | PLI_PROCESSLIST | PLI_SHOWIMG | PLI_ENTERLEVEL => {}
                // Player flags: `name` or `name=value`; kept with the account.
                PLI_FLAGSET | PLI_FLAGDEL => {
                    let text = seihai_core::protocol::latin1(payload);
                    server.world.flag(self.id, text, id == PLI_FLAGSET);
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
                        x: x.wrapping_sub(0x20) as f32 / 2.0,
                        y: y.wrapping_sub(0x20) as f32 / 2.0,
                        mod_time,
                    });
                }
                _ => {
                    if first_time && server.verbose {
                        server.log(peer, &format!("message {id} not handled in stage 1"));
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
        let plo = &prof.protocol.plo;
        let mut out = Vec::new();
        for delivery in link.take(self.id) {
            match delivery {
                Delivery::Private {
                    from_id,
                    nick,
                    account,
                    text,
                } => {
                    if plo.contains(PLO_OTHERPLPROPS) {
                        let mut p = Vec::new();
                        gshort(from_id, &mut p);
                        if prof.props.other.contains(0) {
                            p.push(gchar(0));
                            gstring(&latin1(&nick), &mut p);
                        }
                        if prof.props.other.contains(34) {
                            p.push(gchar(34));
                            gstring(&latin1(&account), &mut p);
                        }
                        out.extend(msg(PLO_OTHERPLPROPS, &p));
                    }
                    if plo.contains(PLO_PRIVATEMESSAGE) {
                        let mut p = Vec::new();
                        gshort(from_id, &mut p);
                        // One quoted field, so a newline in the text stays
                        // one message. `""` is one quote.
                        let body = format!(
                            "{},{},{}",
                            quoted(""),
                            quoted("Private message:"),
                            quoted(&text)
                        );
                        p.extend(body.into_bytes());
                        out.extend(msg(PLO_PRIVATEMESSAGE, &p));
                    }
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
