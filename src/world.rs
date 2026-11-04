//! The world task (stage 2).
//!
//! One tokio task owns every level and player. Connection tasks send it
//! [`WorldIn`] events and get message bodies back through their [`Outbox`];
//! the world never sees a codec. See `docs/ARCHITECTURE.md`, "The world task".
//!
//! Done:
//! 1. The world owns who is online: [`World::join`] takes the account and
//!    returns the player id, or the refusal ("Account already in use.",
//!    "The server is full."). [`World::left`] frees the id.
//! 2. The world holds the levels: a join names the level the account is
//!    in, and the world answers with it, read from `<world>/levels/` the
//!    first time (`levels.rs`), or the stage 1 board when there is no such
//!    file. The connection builds the burst from it (`burst::login_burst`),
//!    since the burst's frames depend on the client's generation.
//!
//! To do, in order:
//! 3. Other players: `PLO_OTHERPLPROPS` (8) on join, prop changes, leave.
//!    Only props in each receiver's `props.other` set.
//! 4. `PLI_LEVELWARP` (0) and `PLI_LEVELWARPMOD` (30): load the level, send
//!    it (see Seihai `documentation/protocol.md`, "Level changes"); a current
//!    mod time gets no board.
//! 5. Gmaps: `PLI_ADJACENTLEVEL` (35), cell props 43/44 (`gmap-order.md`,
//!    `bigmap-neighbours.md` in the seihai repo).
//! 6. Flags (`PLI_FLAGSET`), chests, items on the ground.

use crate::levels::Level;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

/// The world tick. Real 2.x clients send walk props every 100 ms and expect
/// replies within a frame.
pub const TICK: Duration = Duration::from_millis(50);

/// Player ids are GSHORTs, which hold 14 bits; 0 is not a player.
const ID_LIMIT: u16 = 1 << 14;

/// What a connection tells the world.
#[derive(Debug)]
pub enum WorldIn {
    /// A login passed the allow-list and the account check. The world
    /// answers with the player id or the reason to refuse.
    Joined {
        account: String,
        /// The level the account was in, or the start level.
        level: String,
        outbox: Outbox,
        reply: oneshot::Sender<Result<Joined, Refused>>,
    },
    /// `PLI_PLAYERPROPS` (2), as sent.
    Props { player: u16, raw: Vec<u8> },
    /// `PLI_LEVELWARP` (0) or `PLI_LEVELWARPMOD` (30).
    LevelWarp {
        player: u16,
        level: String,
        x: f32,
        y: f32,
        mod_time: Option<u64>,
    },
    /// The connection closed.
    Left { player: u16 },
}

/// Message bodies for one player; the connection task frames and sends them.
pub type Outbox = mpsc::UnboundedSender<Vec<u8>>;

/// The world's answer to a login it lets in.
#[derive(Debug, Clone)]
pub struct Joined {
    pub id: u16,
    /// The level the player starts in.
    pub level: Arc<Level>,
}

/// Where the world finds its levels, and what it sends without one.
#[derive(Debug, Clone)]
pub struct LevelSource {
    /// `<world>/levels/`.
    pub dir: PathBuf,
    /// The tile of the stage 1 board.
    pub blank_tile: u16,
    /// The mod time the stage 1 board gets (the server's start).
    pub blank_mod_time: u64,
}

/// Why the world turned a login away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The account is online on another connection.
    InUse,
    /// `max_players` are online.
    Full,
}

impl Refused {
    /// The text for `PLO_DISCMESSAGE`.
    pub fn text(self) -> &'static str {
        match self {
            Refused::InUse => "Account already in use.",
            Refused::Full => "The server is full.",
        }
    }
}

/// The connection tasks' handle on the world.
pub struct World {
    tx: mpsc::UnboundedSender<WorldIn>,
    online: watch::Receiver<Vec<String>>,
    /// The task, until [`World::start`] spawns it.
    task: Mutex<Option<WorldTask>>,
}

impl World {
    /// A world for at most `max_players`. Nothing runs until
    /// [`World::start`], so this can be made outside a tokio runtime.
    pub fn new(max_players: usize, levels: LevelSource) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (online_tx, online) = watch::channel(Vec::new());
        World {
            tx,
            online,
            task: Mutex::new(Some(WorldTask {
                rx,
                online: online_tx,
                max_players,
                levels,
                loaded: HashMap::new(),
                players: BTreeMap::new(),
                by_account: HashMap::new(),
                last_id: 0,
            })),
        }
    }

    /// Spawn the world task on the current runtime. Later calls do nothing.
    pub fn start(&self) {
        if let Some(task) = self.task.lock().unwrap().take() {
            tokio::spawn(task.run());
        }
    }

    /// Register a logged-in account that starts in `level`. `None` when
    /// the world task is gone (the server is shutting down).
    pub async fn join(
        &self,
        account: &str,
        level: &str,
        outbox: Outbox,
    ) -> Option<Result<Joined, Refused>> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(WorldIn::Joined {
                account: account.to_string(),
                level: level.to_string(),
                outbox,
                reply,
            })
            .ok()?;
        answer.await.ok()
    }

    /// A player's prop update, as the client sent it.
    pub fn props(&self, player: u16, raw: &[u8]) {
        let _ = self.tx.send(WorldIn::Props {
            player,
            raw: raw.to_vec(),
        });
    }

    /// The player's connection closed.
    pub fn left(&self, player: u16) {
        let _ = self.tx.send(WorldIn::Left { player });
    }

    /// Accounts online now, lower-case and sorted. Updated by the world
    /// task after each join and leave.
    pub fn online_accounts(&self) -> Vec<String> {
        self.online.borrow().clone()
    }
}

/// One player as the world holds them.
struct Online {
    /// Lower-case account name.
    account: String,
    #[allow(dead_code)] // the world starts sending with item 3
    outbox: Outbox,
}

/// The world's own state, owned by its task.
pub struct WorldTask {
    rx: mpsc::UnboundedReceiver<WorldIn>,
    online: watch::Sender<Vec<String>>,
    max_players: usize,
    levels: LevelSource,
    /// Levels read so far, by lower-case name.
    loaded: HashMap<String, Arc<Level>>,
    players: BTreeMap<u16, Online>,
    /// Lower-case account name to player id.
    by_account: HashMap<String, u16>,
    last_id: u16,
}

impl WorldTask {
    /// Handle events until every [`World`] handle is gone.
    pub async fn run(mut self) {
        while let Some(ev) = self.rx.recv().await {
            self.handle(ev);
        }
    }

    fn handle(&mut self, ev: WorldIn) {
        match ev {
            WorldIn::Joined {
                account,
                level,
                outbox,
                reply,
            } => {
                let key = account.to_ascii_lowercase();
                let answer = self.join(&key, outbox).map(|id| Joined {
                    id,
                    level: self.level(&level),
                });
                // The connection may have closed while it waited; then the
                // id goes straight back.
                if let Err(Ok(j)) = reply.send(answer) {
                    self.leave(j.id);
                }
            }
            WorldIn::Left { player } => self.leave(player),
            // Read from item 3 (other players) and item 4 (warps) on.
            WorldIn::Props { .. } | WorldIn::LevelWarp { .. } => {}
        }
    }

    /// The level called `name`, read on first use. A name that does not
    /// pass the check, or a file that is missing or does not read, gets the
    /// stage 1 board under the name asked for; that is not kept, so the
    /// file is read once it is there.
    fn level(&mut self, name: &str) -> Arc<Level> {
        let key = name.to_ascii_lowercase();
        if let Some(l) = self.loaded.get(&key) {
            return l.clone();
        }
        match Level::load(&self.levels.dir, name) {
            Ok(l) => {
                let l = Arc::new(l);
                self.loaded.insert(key, l.clone());
                self.loaded.insert(l.name.to_ascii_lowercase(), l.clone());
                l
            }
            Err(e) => {
                eprintln!("level {name}: {e}; sending a blank board");
                Arc::new(Level::blank(
                    name,
                    self.levels.blank_tile,
                    self.levels.blank_mod_time,
                ))
            }
        }
    }

    fn join(&mut self, key: &str, outbox: Outbox) -> Result<u16, Refused> {
        if self.by_account.contains_key(key) {
            return Err(Refused::InUse);
        }
        if self.players.len() >= self.max_players {
            return Err(Refused::Full);
        }
        let id = self.take_id().ok_or(Refused::Full)?;
        self.by_account.insert(key.to_string(), id);
        self.players.insert(
            id,
            Online {
                account: key.to_string(),
                outbox,
            },
        );
        self.publish();
        Ok(id)
    }

    fn leave(&mut self, id: u16) {
        if let Some(p) = self.players.remove(&id) {
            self.by_account.remove(&p.account);
            self.publish();
        }
    }

    /// The next free id after the last one given out, so a freed id is not
    /// handed to someone else straight away.
    fn take_id(&mut self) -> Option<u16> {
        for _ in 1..ID_LIMIT {
            self.last_id = if self.last_id + 1 >= ID_LIMIT {
                1
            } else {
                self.last_id + 1
            };
            if !self.players.contains_key(&self.last_id) {
                return Some(self.last_id);
            }
        }
        None
    }

    fn publish(&self) {
        let mut v: Vec<String> = self.by_account.keys().cloned().collect();
        v.sort();
        self.online.send_replace(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outbox() -> (Outbox, mpsc::UnboundedReceiver<Vec<u8>>) {
        mpsc::unbounded_channel()
    }

    fn blank() -> LevelSource {
        LevelSource {
            dir: PathBuf::from("no-such-world/levels"),
            blank_tile: 0x1FF,
            blank_mod_time: 5,
        }
    }

    /// `join` for tests that only look at the id.
    async fn id(w: &World, account: &str, o: Outbox) -> Option<Result<u16, Refused>> {
        w.join(account, "start.nw", o)
            .await
            .map(|r| r.map(|j| j.id))
    }

    #[tokio::test]
    async fn join_hands_out_the_level_from_the_folder() {
        let dir = std::env::temp_dir().join(format!("seihai-gserver-world-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Lab.nw"), "GLEVNW01\nLINK b.nw 0 0 1 1 30 30\n").unwrap();
        let w = World::new(
            8,
            LevelSource {
                dir: dir.clone(),
                ..blank()
            },
        );
        w.start();
        let (a, _ra) = outbox();
        let j = w.join("alice", "lab.nw", a).await.unwrap().unwrap();
        assert_eq!(j.level.name, "Lab.nw");
        assert_eq!(j.level.links.len(), 1);
        // Read once: the second player gets the same level.
        let (b, _rb) = outbox();
        let k = w.join("bob", "LAB.NW", b).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&j.level, &k.level));
        // No file, or a name that may not name one: the stage 1 board.
        for name in ["gone.nw", "../Lab.nw"] {
            let (c, _rc) = outbox();
            let l = w.join(name, name, c).await.unwrap().unwrap().level;
            assert_eq!(l.name, name);
            assert!(l.board.iter().all(|&t| t == 0x1FF));
            assert_eq!(l.mod_time, 5);
            assert!(l.links.is_empty());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn join_gives_ids_and_refuses_a_second_login() {
        let w = World::new(8, blank());
        w.start();
        let (a, _ra) = outbox();
        let (b, _rb) = outbox();
        let (c, _rc) = outbox();
        assert_eq!(id(&w, "Alice", a).await, Some(Ok(1)));
        assert_eq!(id(&w, "bob", b).await, Some(Ok(2)));
        // Account names are compared without case.
        assert_eq!(id(&w, "ALICE", c).await, Some(Err(Refused::InUse)));
        assert_eq!(w.online_accounts(), ["alice", "bob"]);
        assert_eq!(Refused::InUse.text(), "Account already in use.");
    }

    #[tokio::test]
    async fn leaving_frees_the_account_but_not_the_id_at_once() {
        let w = World::new(8, blank());
        w.start();
        let (a, _ra) = outbox();
        assert_eq!(id(&w, "alice", a).await, Some(Ok(1)));
        w.left(1);
        let (a2, _ra2) = outbox();
        // Events are handled in order, so the leave is in before this.
        assert_eq!(id(&w, "alice", a2).await, Some(Ok(2)));
        assert_eq!(w.online_accounts(), ["alice"]);
        w.left(2);
        // Leaving twice, or with an id nobody has, does nothing.
        w.left(2);
        w.left(77);
        let (b, _rb) = outbox();
        assert_eq!(id(&w, "bob", b).await, Some(Ok(3)));
        assert_eq!(w.online_accounts(), ["bob"]);
    }

    #[tokio::test]
    async fn full_world_refuses() {
        let w = World::new(2, blank());
        w.start();
        for (i, name) in ["a", "b"].into_iter().enumerate() {
            let (o, _r) = outbox();
            assert_eq!(id(&w, name, o).await, Some(Ok(i as u16 + 1)));
        }
        let (o, _r) = outbox();
        assert_eq!(id(&w, "c", o).await, Some(Err(Refused::Full)));
        w.left(1);
        let (o, _r) = outbox();
        assert_eq!(id(&w, "c", o).await, Some(Ok(3)));
    }

    #[test]
    fn ids_wrap_and_skip_taken_ones() {
        let w = World::new(usize::MAX, blank());
        let mut t = w.task.lock().unwrap().take().unwrap();
        t.last_id = ID_LIMIT - 2;
        let (o, _r) = outbox();
        assert_eq!(t.join("a", o.clone()), Ok(ID_LIMIT - 1));
        // Past the 14-bit limit the ids start again at 1, skipping 0.
        assert_eq!(t.join("b", o.clone()), Ok(1));
        t.last_id = ID_LIMIT - 2;
        assert_eq!(t.join("c", o.clone()), Ok(2));
        // Every id taken: the world is full whatever max_players says.
        let mut full = World::new(usize::MAX, blank())
            .task
            .lock()
            .unwrap()
            .take()
            .unwrap();
        for id in 1..ID_LIMIT {
            full.players.insert(
                id,
                Online {
                    account: format!("p{id}"),
                    outbox: o.clone(),
                },
            );
        }
        assert_eq!(full.join("z", o), Err(Refused::Full));
    }

    #[tokio::test]
    async fn a_join_nobody_waits_for_is_undone() {
        let w = World::new(8, blank());
        let mut t = w.task.lock().unwrap().take().unwrap();
        let (o, _r) = outbox();
        let (reply, answer) = oneshot::channel();
        drop(answer);
        t.handle(WorldIn::Joined {
            account: "alice".into(),
            level: "start.nw".into(),
            outbox: o,
            reply,
        });
        assert!(t.players.is_empty());
        assert!(t.by_account.is_empty());
        assert!(w.online_accounts().is_empty());
    }
}
