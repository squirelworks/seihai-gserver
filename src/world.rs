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
//!
//! To do, in order:
//! 2. Build the burst here with the level from `levels.rs` instead of the
//!    generated board (`burst::login_burst` takes the board as an argument).
//! 3. Other players: `PLO_OTHERPLPROPS` (8) on join, prop changes, leave.
//!    Only props in each receiver's `props.other` set.
//! 4. `PLI_LEVELWARP` (0) and `PLI_LEVELWARPMOD` (30): load the level, send
//!    it (see Seihai `documentation/protocol.md`, "Level changes"); a current
//!    mod time gets no board.
//! 5. Gmaps: `PLI_ADJACENTLEVEL` (35), cell props 43/44 (`gmap-order.md`,
//!    `bigmap-neighbours.md` in the seihai repo).
//! 6. Flags (`PLI_FLAGSET`), chests, items on the ground.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
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
        outbox: Outbox,
        reply: oneshot::Sender<Result<u16, Refused>>,
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
    pub fn new(max_players: usize) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (online_tx, online) = watch::channel(Vec::new());
        World {
            tx,
            online,
            task: Mutex::new(Some(WorldTask {
                rx,
                online: online_tx,
                max_players,
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

    /// Register a logged-in account. `None` when the world task is gone
    /// (the server is shutting down).
    pub async fn join(&self, account: &str, outbox: Outbox) -> Option<Result<u16, Refused>> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(WorldIn::Joined {
                account: account.to_string(),
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
                outbox,
                reply,
            } => {
                let key = account.to_ascii_lowercase();
                let answer = self.join(&key, outbox);
                // The connection may have closed while it waited; then the
                // id goes straight back.
                if reply.send(answer).is_err() {
                    if let Ok(id) = answer {
                        self.leave(id);
                    }
                }
            }
            WorldIn::Left { player } => self.leave(player),
            // Read from item 3 (other players) and item 4 (warps) on.
            WorldIn::Props { .. } | WorldIn::LevelWarp { .. } => {}
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

    #[tokio::test]
    async fn join_gives_ids_and_refuses_a_second_login() {
        let w = World::new(8);
        w.start();
        let (a, _ra) = outbox();
        let (b, _rb) = outbox();
        let (c, _rc) = outbox();
        assert_eq!(w.join("Alice", a).await, Some(Ok(1)));
        assert_eq!(w.join("bob", b).await, Some(Ok(2)));
        // Account names are compared without case.
        assert_eq!(w.join("ALICE", c).await, Some(Err(Refused::InUse)));
        assert_eq!(w.online_accounts(), ["alice", "bob"]);
        assert_eq!(Refused::InUse.text(), "Account already in use.");
    }

    #[tokio::test]
    async fn leaving_frees_the_account_but_not_the_id_at_once() {
        let w = World::new(8);
        w.start();
        let (a, _ra) = outbox();
        assert_eq!(w.join("alice", a).await, Some(Ok(1)));
        w.left(1);
        let (a2, _ra2) = outbox();
        // Events are handled in order, so the leave is in before this.
        assert_eq!(w.join("alice", a2).await, Some(Ok(2)));
        assert_eq!(w.online_accounts(), ["alice"]);
        w.left(2);
        // Leaving twice, or with an id nobody has, does nothing.
        w.left(2);
        w.left(77);
        let (b, _rb) = outbox();
        assert_eq!(w.join("bob", b).await, Some(Ok(3)));
        assert_eq!(w.online_accounts(), ["bob"]);
    }

    #[tokio::test]
    async fn full_world_refuses() {
        let w = World::new(2);
        w.start();
        for (i, name) in ["a", "b"].into_iter().enumerate() {
            let (o, _r) = outbox();
            assert_eq!(w.join(name, o).await, Some(Ok(i as u16 + 1)));
        }
        let (o, _r) = outbox();
        assert_eq!(w.join("c", o).await, Some(Err(Refused::Full)));
        w.left(1);
        let (o, _r) = outbox();
        assert_eq!(w.join("c", o).await, Some(Ok(3)));
    }

    #[test]
    fn ids_wrap_and_skip_taken_ones() {
        let w = World::new(usize::MAX);
        let mut t = w.task.lock().unwrap().take().unwrap();
        t.last_id = ID_LIMIT - 2;
        let (o, _r) = outbox();
        assert_eq!(t.join("a", o.clone()), Ok(ID_LIMIT - 1));
        // Past the 14-bit limit the ids start again at 1, skipping 0.
        assert_eq!(t.join("b", o.clone()), Ok(1));
        t.last_id = ID_LIMIT - 2;
        assert_eq!(t.join("c", o.clone()), Ok(2));
        // Every id taken: the world is full whatever max_players says.
        let mut full = World::new(usize::MAX).task.lock().unwrap().take().unwrap();
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
        let w = World::new(8);
        let mut t = w.task.lock().unwrap().take().unwrap();
        let (o, _r) = outbox();
        let (reply, answer) = oneshot::channel();
        drop(answer);
        t.handle(WorldIn::Joined {
            account: "alice".into(),
            outbox: o,
            reply,
        });
        assert!(t.players.is_empty());
        assert!(t.by_account.is_empty());
        assert!(w.online_accounts().is_empty());
    }
}
