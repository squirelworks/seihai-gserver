//! Listening, login and the idle session.
//!
//! One tokio task per connection. Stage 1 keeps the shared state small: who
//! is online and which player ids are taken. The world task with levels and
//! the 50 ms tick comes with stage 2.

use crate::accounts::{Account, Store};
use crate::allow::AllowList;
use crate::burst::{self, Player};
use crate::codec::{frame_header, Codec, MAX_IN_FRAME};
use crate::config::{Config, NpcMode};
use crate::login::{self, Login};
use crate::secure;
use crate::wire::{gint4, ids::*, msg};
use seihai_core::protocol::{
    parse_player_props_with, zlib_decompress, ClientVersion, EncryptionGen,
};
use std::collections::{BTreeMap, HashMap};
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
    online: Mutex<Online>,
    /// The NPC-server link's shared state (see `npclink`).
    pub npc: Arc<crate::npclink::Hub>,
    started: Instant,
    started_unix: u64,
    pub verbose: bool,
    /// The secure-login key (`SEI00001`); `None` when secure logins are off.
    pub identity: Option<seihai_core::protocol::seclogin::ServerIdentity>,
    /// The listserver link (see `listlink`); `None` when `listserver` is
    /// empty. Set by `run`.
    pub listlink: Option<Arc<crate::listlink::ListLink>>,
}

#[derive(Default)]
struct Online {
    /// Lower-case account name to player id.
    by_account: HashMap<String, u16>,
    next_id: u16,
}

impl Online {
    fn take_id(&mut self) -> u16 {
        let used: std::collections::HashSet<u16> = self.by_account.values().copied().collect();
        loop {
            self.next_id = self.next_id.wrapping_add(1);
            // GSHORT holds 14 bits; 0 is not a player.
            if self.next_id == 0 || self.next_id >= 1 << 14 {
                self.next_id = 1;
            }
            if !used.contains(&self.next_id) {
                return self.next_id;
            }
        }
    }
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
        Server {
            npc,
            cfg,
            allow,
            extra_tokens,
            store,
            online: Mutex::new(Online::default()),
            started: Instant::now(),
            started_unix,
            verbose: false,
            identity: None,
            listlink: None,
        }
    }

    /// Accounts online now, lower-case.
    pub fn online_accounts(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .online
            .lock()
            .unwrap()
            .by_account
            .keys()
            .cloned()
            .collect();
        v.sort();
        v
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
}

/// Accept connections until the listener fails.
pub async fn serve(server: Arc<Server>, listener: TcpListener) -> io::Result<()> {
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
                s.npc.left(id);
                if let Some(l) = &s.listlink {
                    l.player_removed(id);
                }
            }
            if let Some(name) = conn.account.take() {
                s.online.lock().unwrap().by_account.remove(&name);
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
    /// Lower-case account name, once it is registered online.
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
                // The generation is not known: a plain zlib frame is the
                // best guess (it is what gen 2 and gen 3 read).
                let mut c = Codec::new(EncryptionGen::Gen2, 0, None);
                return self
                    .refuse(&mut c, "This server cannot read that login.")
                    .await;
            }
        };
        let version = burst::shape_version(login.version, login.gen);
        let mut codec = Codec::new(login.gen, login.key, login.version);
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
        let id = {
            let mut on = self.server.online.lock().unwrap();
            if on.by_account.contains_key(&key) {
                None
            } else if on.by_account.len() >= cfg.max_players {
                Some(None)
            } else {
                let id = on.take_id();
                on.by_account.insert(key.clone(), id);
                Some(Some(id))
            }
        };
        let id = match id {
            None => {
                self.server
                    .log(&self.peer, &format!("{who}: already online"));
                return self.refuse(&mut codec, "Account already in use.").await;
            }
            Some(None) => return self.refuse(&mut codec, "The server is full.").await,
            Some(Some(id)) => id,
        };
        self.account = Some(key);
        self.player = Some(id);
        {
            let level = if account.level.is_empty() {
                cfg.start_level.as_str()
            } else {
                account.level.as_str()
            };
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
                l.player_added(id);
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
            self.server.started_unix,
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
        };
        let reply = session.handle(&self.server, &self.peer, &early);
        if !reply.is_empty() {
            self.send(&mut codec, &reply).await?;
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
        let mut last_heard = Instant::now();
        loop {
            let idle_left = (!cfg.idle_timeout.is_zero())
                .then(|| cfg.idle_timeout.saturating_sub(last_heard.elapsed()));
            tokio::select! {
                r = read_more(&mut self.stream, &mut inbuf.buf) => {
                    r?;
                    last_heard = Instant::now();
                    let mut reply = Vec::new();
                    while let Some(frame) = inbuf.take_frame()? {
                        let body = codec.decode(&frame)?;
                        let msgs = codec.messages(&body);
                        if codec.bad_in_a_row >= MAX_BAD_IN_A_ROW {
                            self.server.log(&self.peer, "gen-3 bytes out of step; dropping the client");
                            return self.refuse(&mut codec, "Disconnected for sending invalid packets.").await;
                        }
                        reply.extend(session.handle(&self.server, &self.peer, &msgs));
                    }
                    if !reply.is_empty() {
                        self.send(&mut codec, &reply).await?;
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
        let link = self
            .server
            .listlink
            .clone()
            .filter(|_| cfg.listserver_accounts);
        // PBKDF2 is slow on purpose, and the listserver answer blocks:
        // keep both off the network threads.
        tokio::task::spawn_blocking(move || {
            if let Some(link) = link {
                // The listserver owns the password; the local file keeps
                // the player's state and is made on first login.
                let verdict = link
                    .verify(&name, &password)
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
}

impl Session {
    /// Handle the client's messages; returns the reply body (maybe empty).
    fn handle(&mut self, server: &Server, peer: &SocketAddr, msgs: &[Vec<u8>]) -> Vec<u8> {
        let plo = &self.version.profile().protocol.plo;
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
                // Stage 1 serves no files: a request gets "failed", a check
                // of a file the client has gets "up to date" where the
                // build reads that (2.1 on), else "failed".
                // 1.21 reads neither answer; it gets none.
                PLI_WANTFILE => {
                    if plo.contains(PLO_FILESENDFAILED) {
                        reply.extend(msg(PLO_FILESENDFAILED, payload));
                    }
                }
                PLI_UPDATEFILE => {
                    let name = payload.get(5..).unwrap_or_default();
                    let answer = [PLO_FILEUPTODATE, PLO_FILESENDFAILED]
                        .into_iter()
                        .find(|&id| plo.contains(id));
                    if let Some(answer) = answer {
                        reply.extend(msg(answer, name));
                    }
                }
                PLI_PLAYERPROPS => {
                    let bow = self.version.profile().props.bow_prop10;
                    server.npc.props(self.id, payload, bow);
                    let props = parse_player_props_with(payload, bow);
                    if let Some(nick) = props.iter().find(|p| p.id == 0) {
                        let nick = nick
                            .raw
                            .get(1..)
                            .map(seihai_core::protocol::latin1)
                            .unwrap_or_default();
                        if nick != self.nickname && server.verbose {
                            server.log(peer, &format!("nickname {nick}"));
                        }
                        self.nickname = nick;
                    }
                }
                // Read and not answered in stage 1: the language, the
                // packet count (1.33 to 1.39, every 10 s; not a login),
                // the 1.33 to 1.37 weapon upload, the tamper check (never
                // required), the process list, image and flag updates,
                // level entry, neighbour requests and walking off a level.
                PLI_LANGUAGE | PLI_PACKETCOUNT | PLI_WEAPONADD | PLI_TAMPERCHECK
                | PLI_PROCESSLIST | PLI_SHOWIMG | PLI_FLAGSET | PLI_ENTERLEVEL
                | PLI_ADJACENTLEVEL | PLI_LEVELWARP | PLI_LEVELWARPMOD => {}
                _ => {
                    if first_time && server.verbose {
                        server.log(peer, &format!("message {id} not handled in stage 1"));
                    }
                }
            }
        }
        reply
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
