//! The NPC-server link: the gserver side of `PROTOCOL.md` (link version 1).
//!
//! `seihai-npcserver` connects to its own port (`npcserver_port`, loopback by
//! default), sends `HELLO` with the shared token, and from then on is told
//! about levels and players. The wire format is the NPC-server crate's
//! `link` module, so both sides share one codec. `PROTOCOL.md` is copied in
//! this repo; change it in both.
//!
//! - [`Hub`] is the shared state: who is online (with their props), the
//!   server flags, and the sender to the connected NPC-server, if any. The
//!   world task calls [`Hub::joined`], [`Hub::props`], [`Hub::level`],
//!   [`Hub::left`] and [`Hub::level_reloaded`]; each updates the state and
//!   forwards a message when an NPC-server is connected.
//! - The NPC-server's own messages (`0x40` to `0x4C`) go to the world task
//!   as [`WorldIn::FromNpc`], in the order they arrived, and the world
//!   applies them (`world.rs`, "The NPC-server"). When the link closes the
//!   world hears [`WorldIn::NpcGone`] and takes the server NPCs down.
//! - [`serve_link`] accepts NPC-server connections.
//! - [`supervise`] starts and restarts the NPC-server for `npcserver = auto`.
//!
//! Not sent yet: `PM_TO_NPC`, `TRIGGER_ACTION` (`0x24`, `0x26`), which wait
//! for the client messages they come from.

use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use seihai_core::protocol::parse_player_props_with;
use seihai_npcserver::link::{self, tokens_match, Message, LINK_VERSION, TOKEN_ENV};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use crate::world::WorldIn;

/// Time a new link connection gets to send `HELLO`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Send `PING` after this long without sending.
const PING_AFTER: Duration = Duration::from_secs(30);
/// Drop the link after this long without hearing from it.
const DEAD_AFTER: Duration = Duration::from_secs(90);
/// How long an NPC Control request waits for the NPC-server.
const NC_TIMEOUT: Duration = Duration::from_secs(10);

/// A player as the link describes it.
#[derive(Debug, Clone, Default)]
struct LinkPlayer {
    account: Vec<u8>,
    level: Vec<u8>,
    /// Latest value of each prop, in the 2.22 encoding.
    props: BTreeMap<u8, Vec<u8>>,
}

impl LinkPlayer {
    fn prop_stream(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (id, v) in &self.props {
            out.push(id + 0x20);
            out.extend_from_slice(v);
        }
        out
    }
}

#[derive(Default)]
struct HubState {
    players: BTreeMap<u16, LinkPlayer>,
    /// The connected NPC-server, and a number for that connection.
    link: Option<(u64, mpsc::UnboundedSender<Message>)>,
    next_link: u64,
    /// The server flags as the world holds them (`name=value`), for a
    /// `SERVER_FLAGS` when an NPC-server connects.
    server_flags: Vec<Vec<u8>>,
    /// NPC Control: the NPC-server's NPC list (`NC_NPC`), by id.
    nc_npcs: BTreeMap<u32, NcNpc>,
    /// NPC Control: `NC_GET`s waiting for their `NC_TEXT`, by request.
    nc_waiting: BTreeMap<u32, oneshot::Sender<Option<Vec<u8>>>>,
    nc_next: u32,
    /// NPC Control sessions that hear list changes.
    nc_watchers: Vec<mpsc::UnboundedSender<NcChange>>,
}

/// One NPC as NPC Control lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NcNpc {
    pub name: Vec<u8>,
    pub kind: Vec<u8>,
    pub level: Vec<u8>,
}

/// A change to the NPC Control list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NcChange {
    Added(u32, NcNpc),
    Deleted(u32),
}

/// Shared link state. One per server; cheap when no NPC-server connects.
pub struct Hub {
    state: Mutex<HubState>,
    token: Vec<u8>,
    server_name: String,
    levels_dir: PathBuf,
    started: Instant,
    /// The world task's inbox, for the NPC-server's messages. Weak, so the
    /// world task still ends when the server drops its handle.
    to_world: Mutex<Option<mpsc::WeakUnboundedSender<WorldIn>>>,
}

impl Hub {
    /// `token` is what an NPC-server must send; an empty token accepts
    /// nobody (the link is off or misconfigured).
    pub fn new(token: Vec<u8>, server_name: &str, levels_dir: &Path) -> Self {
        Hub {
            state: Mutex::new(HubState::default()),
            token,
            server_name: server_name.to_string(),
            levels_dir: levels_dir.to_path_buf(),
            started: Instant::now(),
            to_world: Mutex::new(None),
        }
    }

    /// Where the NPC-server's messages go (`World::set_npc` calls this).
    pub fn attach_world(&self, tx: mpsc::WeakUnboundedSender<WorldIn>) {
        *self.to_world.lock().unwrap() = Some(tx);
    }

    /// Pass a message to the world task; false when there is none.
    fn to_world(&self, ev: WorldIn) -> bool {
        let tx = self
            .to_world
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|w| w.upgrade());
        tx.is_some_and(|tx| tx.send(ev).is_ok())
    }

    /// The token, for `supervise` to hand to the NPC-server it starts.
    pub fn token(&self) -> &[u8] {
        &self.token
    }

    /// True while an NPC-server is connected and authenticated.
    pub fn connected(&self) -> bool {
        self.state.lock().unwrap().link.is_some()
    }

    /// A player logged in. `props` is a full prop stream in the 2.22
    /// encoding (see `burst::own_props`).
    pub fn joined(&self, id: u16, account: &str, level: &str, props: &[u8]) {
        let mut p = LinkPlayer {
            account: account.as_bytes().to_vec(),
            level: level.as_bytes().to_vec(),
            props: BTreeMap::new(),
        };
        merge(&mut p.props, props, false);
        let mut st = self.state.lock().unwrap();
        let msg = Message::PlayerJoined {
            player_id: id,
            account: p.account.clone(),
            level: p.level.clone(),
            props: p.prop_stream(),
        };
        st.players.insert(id, p);
        send(&mut st, msg);
    }

    /// A player's props changed: `PLI_PLAYERPROPS` from the client, or a
    /// `SET_PLAYER_PROPS` the world applied (in the 2.22 encoding, so
    /// `bow_prop10` false). `bow_prop10` is the sender's profile setting:
    /// its prop 10 is the old bow slot, which is dropped (the link carries
    /// prop 10 as a gani).
    pub fn props(&self, id: u16, raw: &[u8], bow_prop10: bool) {
        let mut st = self.state.lock().unwrap();
        let Some(p) = st.players.get_mut(&id) else {
            return;
        };
        let changed = merge(&mut p.props, raw, bow_prop10);
        if changed.is_empty() {
            return;
        }
        send(
            &mut st,
            Message::PlayerProps {
                player_id: id,
                props: changed,
            },
        );
    }

    /// A player entered another level (a link, a gmap crossing or a warp).
    pub fn level(&self, id: u16, level: &str) {
        let mut st = self.state.lock().unwrap();
        let Some(p) = st.players.get_mut(&id) else {
            return;
        };
        let level = level.as_bytes().to_vec();
        if p.level == level {
            return;
        }
        p.level = level.clone();
        send(
            &mut st,
            Message::PlayerLevel {
                player_id: id,
                level,
            },
        );
    }

    /// A level file changed on disk and the world read it again.
    pub fn level_reloaded(&self, name: &str) {
        let mut st = self.state.lock().unwrap();
        send(
            &mut st,
            Message::LevelReloaded {
                name: name.as_bytes().to_vec(),
            },
        );
    }

    /// The server flag set changed (`name=value` entries). Kept for the
    /// next NPC-server to connect; the one connected set them itself.
    pub fn server_flags(&self, entries: Vec<Vec<u8>>) {
        self.state.lock().unwrap().server_flags = entries;
    }

    /// A player's connection closed.
    pub fn left(&self, id: u16) {
        let mut st = self.state.lock().unwrap();
        if st.players.remove(&id).is_some() {
            send(&mut st, Message::PlayerLeft { player_id: id });
        }
    }

    /// NPC Control: the NPC list now, and a receiver for its changes.
    pub fn nc_watch(&self) -> (Vec<(u32, NcNpc)>, mpsc::UnboundedReceiver<NcChange>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut st = self.state.lock().unwrap();
        st.nc_watchers.push(tx);
        let list = st.nc_npcs.iter().map(|(i, n)| (*i, n.clone())).collect();
        (list, rx)
    }

    /// NPC Control: ask the NPC-server for an NPC's script or attributes
    /// (`link::nc_get`). `None` when there is no NPC-server, no such NPC,
    /// or no answer.
    pub async fn nc_get(&self, npc_id: u32, what: u8) -> Option<Vec<u8>> {
        let rx = {
            let mut st = self.state.lock().unwrap();
            st.link.as_ref()?;
            st.nc_next = st.nc_next.wrapping_add(1);
            let request = st.nc_next;
            let (tx, rx) = oneshot::channel();
            st.nc_waiting.insert(request, tx);
            send(
                &mut st,
                Message::NcGet {
                    request,
                    npc_id,
                    what,
                },
            );
            rx
        };
        tokio::time::timeout(NC_TIMEOUT, rx).await.ok()?.ok()?
    }

    /// NPC Control: a new script, or a delete. False with no NPC-server.
    pub fn nc_send(&self, m: Message) -> bool {
        let mut st = self.state.lock().unwrap();
        if st.link.is_none() {
            return false;
        }
        send(&mut st, m);
        true
    }

    /// NPC Control messages from the NPC-server; `NPC_DELETED` comes here
    /// too (and still goes to the world).
    fn nc_from_npc(&self, m: &Message) {
        let mut st = self.state.lock().unwrap();
        let change = match m {
            Message::NcNpc {
                npc_id,
                name,
                kind,
                level,
            } => {
                let n = NcNpc {
                    name: name.clone(),
                    kind: kind.clone(),
                    level: level.clone(),
                };
                if st.nc_npcs.get(npc_id) == Some(&n) {
                    return;
                }
                st.nc_npcs.insert(*npc_id, n.clone());
                NcChange::Added(*npc_id, n)
            }
            Message::NpcDeleted { npc_id } => {
                if st.nc_npcs.remove(npc_id).is_none() {
                    return;
                }
                NcChange::Deleted(*npc_id)
            }
            Message::NcText {
                request,
                found,
                text,
            } => {
                if let Some(tx) = st.nc_waiting.remove(request) {
                    let _ = tx.send(found.then(|| text.clone()));
                }
                return;
            }
            _ => return,
        };
        st.nc_watchers.retain(|w| w.send(change.clone()).is_ok());
    }

    /// The NPC-server went: its NPCs leave the NPC Control list.
    fn nc_gone(st: &mut HubState) {
        let gone: Vec<u32> = std::mem::take(&mut st.nc_npcs).into_keys().collect();
        st.nc_waiting.clear();
        for id in gone {
            st.nc_watchers
                .retain(|w| w.send(NcChange::Deleted(id)).is_ok());
        }
    }

    fn log(&self, peer: &SocketAddr, text: &str) {
        eprintln!(
            "[{:>9.3}] npcserver {peer} {text}",
            self.started.elapsed().as_secs_f64()
        );
    }
}

/// Send to the connected NPC-server, if any. A closed channel means the
/// link is going down; its task clears it.
fn send(st: &mut HubState, m: Message) {
    if let Some((_, tx)) = &st.link {
        let _ = tx.send(m);
    }
}

/// Merge a client prop stream into `props`; returns the props that were
/// taken, as a stream.
fn merge(props: &mut BTreeMap<u8, Vec<u8>>, raw: &[u8], bow_prop10: bool) -> Vec<u8> {
    let mut changed = Vec::new();
    for p in parse_player_props_with(raw, bow_prop10) {
        if bow_prop10 && p.id == 10 {
            continue;
        }
        changed.push(p.id + 0x20);
        changed.extend_from_slice(&p.raw);
        props.insert(p.id, p.raw);
    }
    changed
}

/// Level files under `dir`, relative, `/`-separated, sorted.
pub fn level_list(dir: &Path) -> Vec<Vec<u8>> {
    fn walk(base: &Path, dir: &Path, depth: usize, out: &mut Vec<Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && depth < 8 {
                walk(base, &path, depth + 1, out);
            } else if ft.is_file()
                && path.extension().and_then(|x| x.to_str()).is_some_and(|x| {
                    matches!(x.to_ascii_lowercase().as_str(), "nw" | "graal" | "zelda")
                })
            {
                if let Ok(rel) = path.strip_prefix(base) {
                    let parts: Vec<String> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect();
                    out.push(parts.join("/").into_bytes());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, 0, &mut out);
    out.sort();
    out
}

/// Accept NPC-server connections until the listener fails.
pub async fn serve_link(hub: Arc<Hub>, listener: TcpListener) -> io::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let _ = stream.set_nodelay(true);
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(&hub, stream, peer).await {
                hub.log(&peer, &format!("link closed: {e}"));
            }
        });
    }
}

async fn handle(hub: &Hub, mut stream: TcpStream, peer: SocketAddr) -> io::Result<()> {
    let first = tokio::time::timeout(HELLO_TIMEOUT, read_frame(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no HELLO in time"))??;
    let (version, program) = match first.into_iter().next() {
        Some(Message::Hello {
            link_version,
            token,
            program,
        }) => {
            if link_version < 1 {
                return reject(
                    &mut stream,
                    &format!("link version {link_version} not supported"),
                )
                .await;
            }
            if hub.token.is_empty() || !tokens_match(&token, &hub.token) {
                hub.log(&peer, "refused: bad token");
                return reject(&mut stream, "bad token").await;
            }
            (link_version.min(LINK_VERSION), program)
        }
        _ => return reject(&mut stream, "expected HELLO").await,
    };

    // Register and queue the initial state under the lock, so no player
    // event can slip in between the snapshot and the live updates.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let me = {
        let mut st = hub.state.lock().unwrap();
        if st.link.is_some() {
            None
        } else {
            let _ = tx.send(Message::Welcome {
                link_version: version,
                server_name: hub.server_name.as_bytes().to_vec(),
                program: format!("seihai-gserver {}", env!("CARGO_PKG_VERSION")).into_bytes(),
            });
            let _ = tx.send(Message::LevelList {
                names: level_list(&hub.levels_dir),
            });
            for (id, p) in &st.players {
                let _ = tx.send(Message::PlayerJoined {
                    player_id: *id,
                    account: p.account.clone(),
                    level: p.level.clone(),
                    props: p.prop_stream(),
                });
            }
            if !st.server_flags.is_empty() {
                let _ = tx.send(Message::ServerFlags {
                    entries: st.server_flags.clone(),
                });
            }
            st.next_link += 1;
            let me = st.next_link;
            st.link = Some((me, tx.clone()));
            Some(me)
        }
    };
    let Some(me) = me else {
        hub.log(&peer, "refused: an NPC-server is already connected");
        return reject(&mut stream, "already connected").await;
    };
    hub.log(
        &peer,
        &format!("NPC-server connected: {}", link::show(&program)),
    );

    let (mut rd, mut wr) = stream.into_split();
    // Writer: everything queued goes out, batched per wake-up; PING when idle.
    let writer = tokio::spawn(async move {
        let mut nonce = 0u32;
        loop {
            let first = match tokio::time::timeout(PING_AFTER, rx.recv()).await {
                Ok(Some(m)) => m,
                Ok(None) => return Ok::<_, io::Error>(()),
                Err(_) => {
                    nonce = nonce.wrapping_add(1);
                    Message::Ping { nonce }
                }
            };
            let mut batch = vec![first];
            while let Ok(m) = rx.try_recv() {
                batch.push(m);
            }
            let bye = batch.iter().any(|m| matches!(m, Message::Bye { .. }));
            wr.write_all(&link::encode_frame(&batch)?).await?;
            if bye {
                let _ = wr.shutdown().await;
                return Ok(());
            }
        }
    });

    let mut seen_unknown = [false; 256];
    let mut world_gone = false;
    let result = loop {
        let msgs = match tokio::time::timeout(DEAD_AFTER, read_frame(&mut rd)).await {
            Err(_) => {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "nothing from the NPC-server for 90 s",
                ))
            }
            Ok(Err(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                break Ok("NPC-server closed the link".to_string())
            }
            Ok(Err(e)) => break Err(e),
            Ok(Ok(m)) => m,
        };
        let mut done = None;
        for m in msgs {
            match m {
                Message::Ping { nonce } => {
                    let _ = tx.send(Message::Pong { nonce });
                }
                Message::Pong { .. } => {}
                Message::Bye { reason } => done = Some(format!("bye: {}", link::show(&reason))),
                // NPC Control: kept here for NC logins.
                m @ (Message::NcNpc { .. } | Message::NcText { .. }) => hub.nc_from_npc(&m),
                // The NPC-server's own messages: the world applies them.
                m if (0x40..=0x4C).contains(&m.id()) && !matches!(m, Message::Unknown { .. }) => {
                    if matches!(m, Message::NpcDeleted { .. }) {
                        hub.nc_from_npc(&m);
                    }
                    if !hub.to_world(WorldIn::FromNpc(m)) && !world_gone {
                        world_gone = true;
                        hub.log(&peer, "no world task; NPC-server messages are dropped");
                    }
                }
                other => {
                    let id = other.id();
                    if !seen_unknown[id as usize] {
                        seen_unknown[id as usize] = true;
                        hub.log(
                            &peer,
                            &format!("message 0x{id:02x} not handled yet, skipping it from now on"),
                        );
                    }
                }
            }
        }
        if let Some(d) = done {
            break Ok(d);
        }
    };

    {
        let mut st = hub.state.lock().unwrap();
        if st.link.as_ref().is_some_and(|(n, _)| *n == me) {
            // Under the lock, so the world hears this before anything a
            // next NPC-server sends.
            hub.to_world(WorldIn::NpcGone);
            st.link = None;
            Hub::nc_gone(&mut st);
        }
    }
    drop(tx);
    let _ = writer.await;
    match result {
        Ok(why) => {
            hub.log(&peer, &format!("NPC-server gone: {why}"));
            Ok(())
        }
        Err(e) => Err(e),
    }
}

async fn reject(stream: &mut TcpStream, reason: &str) -> io::Result<()> {
    let frame = link::encode_frame(&[Message::Reject {
        reason: reason.as_bytes().to_vec(),
    }])?;
    let _ = stream.write_all(&frame).await;
    let _ = stream.shutdown().await;
    Ok(())
}

/// Read one link frame and decode it with the shared codec.
async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Vec<Message>> {
    let mut head = [0u8; 6];
    r.read_exact(&mut head[..2]).await?;
    let (n, len) = if head[..2] == [0xFF, 0xFF] {
        r.read_exact(&mut head[2..]).await?;
        (
            6,
            u32::from_be_bytes([head[2], head[3], head[4], head[5]]) as usize,
        )
    } else {
        (2, u16::from_be_bytes([head[0], head[1]]) as usize)
    };
    if len > link::MAX_FRAME + 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "link frame over 16 MiB",
        ));
    }
    let mut buf = head[..n].to_vec();
    buf.resize(n + len, 0);
    r.read_exact(&mut buf[n..]).await?;
    link::read_frame(&mut &buf[..])
}

/// A fresh token for `npcserver = auto`: 32 random bytes as hex.
pub fn fresh_token() -> io::Result<Vec<u8>> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<String>()
        .into_bytes())
}

/// The NPC-server binary for `auto`: `npcserver_path`, or the one next to
/// this binary.
pub fn npcserver_binary(configured: &str) -> PathBuf {
    if !configured.is_empty() {
        return PathBuf::from(configured);
    }
    let name = format!("seihai-npcserver{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(&name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

/// `npcserver = auto`: start the NPC-server, and start it again with a
/// growing delay (1 s up to 30 s) whenever it exits. The child is killed
/// when this task is dropped (the gserver stopping). Returns only if the
/// binary cannot be started at all.
pub async fn supervise(binary: PathBuf, link_addr: SocketAddr, world: PathBuf, token: Vec<u8>) {
    let mut delay = Duration::from_secs(1);
    loop {
        let mut cmd = tokio::process::Command::new(&binary);
        cmd.arg("--gserver")
            .arg(link_addr.to_string())
            .arg("--world")
            .arg(&world)
            .arg("--once")
            .env(TOKEN_ENV, String::from_utf8_lossy(&token).as_ref())
            .kill_on_drop(true);
        let began = Instant::now();
        match cmd.spawn() {
            Ok(mut child) => {
                eprintln!(
                    "npcserver: started {} (pid {})",
                    binary.display(),
                    child.id().unwrap_or(0)
                );
                let status = child.wait().await;
                eprintln!("npcserver: exited ({status:?})");
            }
            Err(e) => {
                eprintln!(
                    "npcserver: cannot start {}: {e}. Put seihai-npcserver next to seihai-gserver, set npcserver_path, or set npcserver = off.",
                    binary.display()
                );
                return;
            }
        }
        if began.elapsed() > Duration::from_secs(60) {
            delay = Duration::from_secs(1);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_drops_the_old_bow_slot() {
        let mut props = BTreeMap::new();
        // Prop 10 as bow power 0, then X = 30.
        let changed = merge(&mut props, &[0x2a, 0x20, 0x2f, 0x20 + 60], true);
        assert_eq!(changed, vec![0x2f, 0x20 + 60]);
        assert!(!props.contains_key(&10));
    }

    #[test]
    fn levels_are_listed_relative_and_sorted() {
        let dir =
            std::env::temp_dir().join(format!("seihai-gserver-levels-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("gmaps")).unwrap();
        for f in ["b.nw", "a.graal", "gmaps/c.nw", "notes.txt"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let list = level_list(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            list,
            vec![
                b"a.graal".to_vec(),
                b"b.nw".to_vec(),
                b"gmaps/c.nw".to_vec()
            ]
        );
        assert!(level_list(Path::new("/nonexistent-seihai-levels")).is_empty());
    }

    #[test]
    fn tokens_are_64_hex_chars() {
        let t = fresh_token().unwrap();
        assert_eq!(t.len(), 64);
        assert!(t.iter().all(u8::is_ascii_hexdigit));
        assert_ne!(t, fresh_token().unwrap());
    }

    #[test]
    fn events_without_a_link_only_update_state() {
        let hub = Hub::new(
            b"0123456789abcdef".to_vec(),
            "Lab",
            Path::new("/nonexistent"),
        );
        hub.joined(
            1,
            "test1",
            "a.nw",
            &[0x20, 0x25, b't', b'e', b's', b't', b'1'],
        );
        hub.props(1, &[0x2f, 0x20 + 60], false);
        assert_eq!(
            hub.state.lock().unwrap().players[&1].props[&15],
            vec![0x20 + 60]
        );
        hub.left(1);
        assert!(hub.state.lock().unwrap().players.is_empty());
        assert!(!hub.connected());
    }
}
