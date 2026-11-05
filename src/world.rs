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
//! 3. Other players (`others.rs`): `PLO_OTHERPLPROPS` (8) to the players in
//!    the same level on join (with 50 = 1, both ways), on each prop change,
//!    and on logout (50 = 0, then 51). Only props in each receiver's
//!    `props.other` set.
//! 4. Links (`PLI_LEVELWARP` 0, `PLI_LEVELWARPMOD` 30): the player moves to
//!    the link's arrival in the new level, which goes out with
//!    `burst::level_answer` (no board, links, signs or mod time when the
//!    client's mod time is current). The old level sees the player leave
//!    (50 = 0) and the player sees its players leave; the new level meets
//!    the player as on a join. A level that is not there gets no answer.
//! 5. Gmaps (`gmaps.rs`): for a build that plays them, a level that is a
//!    cell of a `.gmap` in `levels/` is played as one. The login and links
//!    into it say `PLO_PLAYERWARP2` (49); `PLI_ADJACENTLEVEL` (35) gets the
//!    neighbour (`burst::gmap_level`); own props 43/44 with a new cell move
//!    the player into that cell's level, as a link does. Players see the
//!    players of their own cell only.
//! 6. Accounts, chests, flags and floor items: on logout the player's
//!    level, position, counted props, opened chests and flags go back to
//!    the account file (the [`Saver`]); a stop (Ctrl-C, SIGTERM) saves
//!    everyone online the same way ([`World::save_all`]). `PLI_OPENCHEST` (20) gives the
//!    chest's item and every level then shows it opened for that account;
//!    `PLI_FLAGSET`/`PLI_FLAGDEL` (18/19) keep the flags. `PLI_ITEMADD`
//!    (12) items lie in the level for the others and late arrivals,
//!    `PLI_ITEMTAKE` (32) gives one to the first taker only and
//!    `PLI_ITEMDEL` (13) clears it.
//!
//! Prop changes are batched on the [`TICK`] (2.8, issue #20): each
//! player's changes are merged until the tick, latest value per prop, and
//! each receiver then gets every change in its level in one body, so one
//! frame. Before a join or a leave the batch goes out first, so a client
//! never hears of a move after the mover left.
//!
//! To do: the anti-cheat checks of stage 7 that guard these (movement,
//! warps, items and counted props), see `docs/PLAN.md`.

use crate::anticheat::{AntiCheat, Kind, Score, Walk, LINK_SLACK};
use crate::baddies::Baddy;
use crate::gmaps::{plays_gmaps, Gmaps, Place, GMAPLEVELX, GMAPLEVELY};
use crate::levels::Level;
use crate::npcs::Npc;
use crate::others::{self, Change, Props};
use crate::wire::gchar;
use crate::wire::ids::{
    PLO_BADDYPROPS, PLO_ISLEADER, PLO_ITEMADD, PLO_ITEMDEL, PLO_SETACTIVELEVEL,
};
use seihai_core::protocol::{ClientVersion, EncryptionGen};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch};

/// `name=value` as name and value; a bare `name` has an empty value.
fn split_flag(text: &str) -> (String, String) {
    match text.split_once('=') {
        Some((n, v)) => (n.trim().to_string(), v.to_string()),
        None => (text.trim().to_string(), String::new()),
    }
}

/// The world tick. Real 2.x clients send walk props every 100 ms and expect
/// replies within a frame.
pub const TICK: Duration = Duration::from_millis(50);

/// One level's tick batch for one build: the body, and each mover's id
/// with where its message sits in the body.
type Batch = (Vec<u8>, Vec<(u16, usize, usize)>);

/// Player ids are GSHORTs, which hold 14 bits; 0 is not a player.
const ID_LIMIT: u16 = 1 << 14;

/// What a connection tells the world.
#[derive(Debug)]
pub enum WorldIn {
    /// A login passed the allow-list and the account check. The world
    /// answers with the player id or the reason to refuse.
    Joined {
        who: Joining,
        outbox: Outbox,
        reply: oneshot::Sender<Result<Joined, Refused>>,
    },
    /// `PLI_PLAYERPROPS` (2), as sent.
    Props { player: u16, raw: Vec<u8> },
    /// `PLI_LEVELWARP` (0) or `PLI_LEVELWARPMOD` (30).
    LevelWarp(Warp),
    /// An item on the floor: `PLI_ITEMADD` (12), `PLI_ITEMDEL` (13) or
    /// `PLI_ITEMTAKE` (32), with the position in half tiles.
    Item {
        player: u16,
        op: ItemOp,
        x2: u8,
        y2: u8,
        item: u8,
    },
    /// `PLI_OPENCHEST` (20): the chest at tile `x`, `y` of the player's level.
    OpenChest { player: u16, x: u8, y: u8 },
    /// `PLI_FLAGSET` (18, `name` or `name=value`) and `PLI_FLAGDEL` (19,
    /// `name`, `set` false).
    Flag {
        player: u16,
        text: String,
        set: bool,
    },
    /// `PLI_ADJACENTLEVEL` (35): a gmap neighbour the client wants, with
    /// the mod time of its copy (0 for none).
    Adjacent {
        player: u16,
        level: String,
        mod_time: u64,
    },
    /// The connection closed.
    Left { player: u16 },
    /// `PLI_NPCPROPS` (3): `GINT3 id`, then the props a client script
    /// changed.
    NpcProps { player: u16, raw: Vec<u8> },
    /// `PLI_BADDYPROPS` (15): a baddy id, then the props that changed.
    BaddyProps { player: u16, raw: Vec<u8> },
    /// `PLI_BADDYHURT` (16): a follower's hit on a baddy, for the leader.
    BaddyHurt { player: u16, raw: Vec<u8> },
    /// A message passed on to the others in the player's level as `out`,
    /// with the player's id first when `named` (bombs, shots, blasts).
    Relay {
        player: u16,
        out: u8,
        named: bool,
        raw: Vec<u8>,
    },
    /// The server is stopping: save everyone online, then answer.
    SaveAll { reply: oneshot::Sender<usize> },
}

/// Frame bodies for one player. Each item is a run of frames the
/// connection task writes back to back, with nothing of its own in
/// between: a gen-2 level answer is two frames (the raw-data header, then
/// the board), and a client takes whatever frame follows the header as the
/// board.
pub type Outbox = mpsc::UnboundedSender<Vec<Vec<u8>>>;

/// A link request: the player walked into a link to `level`.
#[derive(Debug, Clone)]
pub struct Warp {
    pub player: u16,
    pub level: String,
    /// The arrival, in tiles.
    pub x: f32,
    pub y: f32,
    /// `PLI_LEVELWARPMOD`: the mod time of the client's cached copy.
    pub mod_time: Option<u64>,
}

/// A login the world is asked to let in.
#[derive(Debug, Clone)]
pub struct Joining {
    pub account: String,
    /// The level the account was in, or the start level.
    pub level: String,
    /// The client's build: what it reads of other players.
    pub version: ClientVersion,
    /// The connection's generation, which shapes the level frames.
    pub gen: EncryptionGen,
    /// The player's props from the account, in the 2.22 encoding
    /// (`burst::own_props(ClientVersion::V222, ..)`).
    pub props: Vec<u8>,
    /// Chests the account opened: level (lower case), x, y.
    pub chests: Vec<(String, u8, u8)>,
    /// The account's player flags (`name` or `name=value`).
    pub flags: Vec<String>,
}

/// The world's answer to a login it lets in.
#[derive(Debug, Clone)]
pub struct Joined {
    pub id: u16,
    /// The level the player starts in.
    pub level: Arc<Level>,
    /// The gmap that level is a cell of, for a build that plays gmaps.
    pub gmap: Option<Place>,
    /// The player leads the level: the burst says so (`PLO_ISLEADER`).
    pub leader: bool,
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
        let (gmaps, warnings) = Gmaps::load(&levels.dir);
        for w in warnings {
            eprintln!("levels: {w}");
        }
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
                gmaps,
                loaded: HashMap::new(),
                players: BTreeMap::new(),
                by_account: HashMap::new(),
                last_id: 0,
                saver: None,
                anticheat: None,
                floor: HashMap::new(),
                npcs: HashMap::new(),
                last_npc: 0,
                baddies: HashMap::new(),
                leaders: HashMap::new(),
                pending: BTreeMap::new(),
            })),
        }
    }

    /// Where logged-out players' state goes (the account files). Call
    /// before [`World::start`]; later calls do nothing.
    pub fn set_saver(&self, saver: Saver) {
        if let Some(t) = self.task.lock().unwrap().as_mut() {
            t.saver = Some(saver);
        }
    }

    /// `PLI_ITEMADD`, `PLI_ITEMDEL` or `PLI_ITEMTAKE` (`item` is 0 for a
    /// delete).
    pub fn item(&self, player: u16, op: ItemOp, x2: u8, y2: u8, item: u8) {
        let _ = self.tx.send(WorldIn::Item {
            player,
            op,
            x2,
            y2,
            item,
        });
    }

    /// The anti-cheat checks to run. Call before [`World::start`]; later
    /// calls do nothing.
    pub fn set_anticheat(&self, ac: Arc<AntiCheat>) {
        if let Some(t) = self.task.lock().unwrap().as_mut() {
            t.anticheat = Some(ac);
        }
    }

    /// `PLI_OPENCHEST`.
    pub fn open_chest(&self, player: u16, x: u8, y: u8) {
        let _ = self.tx.send(WorldIn::OpenChest { player, x, y });
    }

    /// `PLI_FLAGSET` (`set`) or `PLI_FLAGDEL`.
    pub fn flag(&self, player: u16, text: String, set: bool) {
        let _ = self.tx.send(WorldIn::Flag { player, text, set });
    }

    /// Spawn the world task on the current runtime. Later calls do nothing.
    pub fn start(&self) {
        if let Some(task) = self.task.lock().unwrap().take() {
            tokio::spawn(task.run());
        }
    }

    /// Register a logged-in account. `None` when the world task is gone
    /// (the server is shutting down).
    pub async fn join(&self, who: Joining, outbox: Outbox) -> Option<Result<Joined, Refused>> {
        let (reply, answer) = oneshot::channel();
        self.tx.send(WorldIn::Joined { who, outbox, reply }).ok()?;
        answer.await.ok()
    }

    /// A player's prop update, as the client sent it.
    pub fn props(&self, player: u16, raw: &[u8]) {
        let _ = self.tx.send(WorldIn::Props {
            player,
            raw: raw.to_vec(),
        });
    }

    /// `PLI_BADDYPROPS`, as sent.
    pub fn baddy_props(&self, player: u16, raw: &[u8]) {
        let _ = self.tx.send(WorldIn::BaddyProps {
            player,
            raw: raw.to_vec(),
        });
    }

    /// Pass `raw` on to the others in the player's level as `out`: a bomb
    /// (`PLI_BOMBADD` 4 to `PLO_BOMBADD` 11, `PLI_BOMBDEL` 5 to
    /// `PLO_BOMBDEL` 12), a shot (`PLI_ARROWADD` 9 to `PLO_ARROWADD` 19),
    /// fire (`PLI_FIRESPY` 10 to `PLO_FIRESPY` 20) or a blast
    /// (`PLI_EXPLOSION` 27 to `PLO_EXPLOSION` 36). Every one but the bomb
    /// removal goes with the sender's id first (`GSHORT`), as GServer-v2
    /// beta4 passes them on in the lab (2026-11-05).
    pub fn relay(&self, player: u16, out: u8, named: bool, raw: &[u8]) {
        let _ = self.tx.send(WorldIn::Relay {
            player,
            out,
            named,
            raw: raw.to_vec(),
        });
    }

    /// `PLI_BADDYHURT`, as sent.
    pub fn baddy_hurt(&self, player: u16, raw: &[u8]) {
        let _ = self.tx.send(WorldIn::BaddyHurt {
            player,
            raw: raw.to_vec(),
        });
    }

    /// `PLI_NPCPROPS`, as sent.
    pub fn npc_props(&self, player: u16, raw: &[u8]) {
        let _ = self.tx.send(WorldIn::NpcProps {
            player,
            raw: raw.to_vec(),
        });
    }

    /// A link request.
    pub fn warp(&self, w: Warp) {
        let _ = self.tx.send(WorldIn::LevelWarp(w));
    }

    /// A gmap neighbour request.
    pub fn adjacent(&self, player: u16, level: String, mod_time: u64) {
        let _ = self.tx.send(WorldIn::Adjacent {
            player,
            level,
            mod_time,
        });
    }

    /// The player's connection closed.
    pub fn left(&self, player: u16) {
        let _ = self.tx.send(WorldIn::Left { player });
    }

    /// Write every online player's state to their account, as a logout
    /// would, and return how many were saved. The players stay online;
    /// the server calls this when it stops, before the connections drop.
    pub async fn save_all(&self) -> usize {
        let (reply, answer) = oneshot::channel();
        if self.tx.send(WorldIn::SaveAll { reply }).is_err() {
            return 0;
        }
        answer.await.unwrap_or(0)
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
    version: ClientVersion,
    gen: EncryptionGen,
    /// Lower-case name of the level the player is in.
    level: String,
    /// The gmap the player plays that level as a cell of.
    gmap: Option<Place>,
    /// The levels (lower-case) this connection was sent in full as the
    /// player's own gmap level; walking back into one sends its name.
    entered: HashSet<String>,
    props: Props,
    /// Chests this account has opened: level (lower case), x, y.
    opened: HashSet<(String, u8, u8)>,
    /// Player flags by name; the value is empty for a bare flag.
    flags: BTreeMap<String, String>,
    /// The anti-cheat's view: the last accepted position in gmap
    /// coordinates, and recent violations.
    walk: Walk,
    score: Score,
    /// The spot the player warped from, until their first position after
    /// the warp: a queued props message may still name it.
    stale: Option<(f32, f32)>,
    outbox: Outbox,
}

impl Online {
    /// What goes back to the account file.
    fn leaving(&self) -> Leaving {
        let mut chests: Vec<_> = self.opened.iter().cloned().collect();
        chests.sort();
        Leaving {
            account: self.account.clone(),
            level: self.level.clone(),
            x: self.props.x(),
            y: self.props.y(),
            props: self.props.values().clone(),
            chests,
            flags: self
                .flags
                .iter()
                .map(|(n, v)| {
                    if v.is_empty() {
                        n.clone()
                    } else {
                        format!("{n}={v}")
                    }
                })
                .collect(),
        }
    }

    /// The position in gmap coordinates (the level's own off a gmap).
    fn global(&self) -> (f32, f32) {
        let (cx, cy) = self.gmap.as_ref().map_or((0, 0), |g| g.cell);
        (
            f32::from(cx) * 64.0 + self.props.x().unwrap_or(0.0),
            f32::from(cy) * 64.0 + self.props.y().unwrap_or(0.0),
        )
    }
}

/// What the world knows of a player who logged out, for the account file.
#[derive(Debug, Clone, Default)]
pub struct Leaving {
    /// Lower-case account name.
    pub account: String,
    /// The level the player was in (its name in lower case).
    pub level: String,
    pub x: Option<f32>,
    pub y: Option<f32>,
    /// Own props in the 2.22 encoding, by id (no position).
    pub props: BTreeMap<u8, Vec<u8>>,
    pub chests: Vec<(String, u8, u8)>,
    pub flags: Vec<String>,
}

impl Leaving {
    /// Put the player's state into their account: level, position, the
    /// counted props (hearts, rupees, arrows, bombs, glove), opened chests
    /// and flags. The password and the rest are left as they are.
    pub fn apply_to(&self, a: &mut crate::accounts::Account) {
        if !self.level.is_empty() {
            a.level = self.level.clone();
        }
        if let Some(x) = self.x {
            a.x = x;
        }
        if let Some(y) = self.y {
            a.y = y;
        }
        let byte = |id: u8| match self.props.get(&id).map(Vec::as_slice) {
            Some([v, ..]) => Some(v.wrapping_sub(0x20)),
            _ => None,
        };
        if let Some(v) = byte(1) {
            a.max_hp = v;
        }
        if let Some(v) = byte(2) {
            a.hp = f32::from(v) / 2.0;
        }
        if let Some(v) = self.props.get(&3) {
            a.rupees = seihai_core::protocol::read_gint3(v).map_or(0, |r| r as u32);
        }
        if let Some(v) = byte(4) {
            a.arrows = v;
        }
        if let Some(v) = byte(5) {
            a.bombs = v;
        }
        if let Some(v) = byte(6) {
            a.glove = v;
        }
        a.chests = self.chests.clone();
        a.flags = self.flags.clone();
    }
}

/// What a player did with an item on the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOp {
    /// `PLI_ITEMADD` (12): rolled from a bush or laid (1.38 on).
    Add,
    /// `PLI_ITEMDEL` (13): gone from the floor (taken by a build before
    /// 1.38, which gives itself the item).
    Del,
    /// `PLI_ITEMTAKE` (32): taken; the server gives the item.
    Take,
}

/// An item lying in a level.
#[derive(Debug, Clone)]
struct FloorItem {
    x2: u8,
    y2: u8,
    item: u8,
    /// When the clients have dropped it, with some slack for a late take.
    until: Instant,
}

/// How long an item lies: the clients' 164 ticks of 50 ms, and 2 s more
/// for a take that crosses the timeout on the wire.
fn item_life() -> Duration {
    TICK * seihai_core::items::ITEM_LIFE as u32 + Duration::from_secs(2)
}

/// Where the world hands a player's state when they log out.
pub type Saver = Box<dyn Fn(Leaving) + Send>;

/// The world's own state, owned by its task.
pub struct WorldTask {
    rx: mpsc::UnboundedReceiver<WorldIn>,
    online: watch::Sender<Vec<String>>,
    max_players: usize,
    levels: LevelSource,
    gmaps: Gmaps,
    /// Levels read so far, by lower-case name.
    loaded: HashMap<String, Arc<Level>>,
    players: BTreeMap<u16, Online>,
    /// Lower-case account name to player id.
    by_account: HashMap<String, u16>,
    last_id: u16,
    /// Prop changes since the last tick, merged per player.
    pending: BTreeMap<u16, Change>,
    saver: Option<Saver>,
    /// The anti-cheat policy and log; no checks without it.
    anticheat: Option<Arc<AntiCheat>>,
    /// Items lying in each level (lower-case name).
    floor: HashMap<String, Vec<FloorItem>>,
    /// The NPCs of each level read so far (lower-case name).
    npcs: HashMap<String, Vec<Npc>>,
    last_npc: u32,
    /// The baddies of each level read so far (lower-case name).
    baddies: HashMap<String, Vec<Baddy>>,
    /// Who leads each level (lower-case name): their client runs its
    /// baddies. An entry whose player has left the level is stale.
    leaders: HashMap<String, u16>,
}

impl WorldTask {
    /// Handle events until every [`World`] handle is gone.
    pub async fn run(mut self) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                ev = self.rx.recv() => match ev {
                    Some(ev) => self.handle(ev),
                    None => break,
                },
                _ = tick.tick() => {
                    self.flush();
                    self.baddy_timers(Instant::now());
                }
            }
        }
    }

    fn handle(&mut self, ev: WorldIn) {
        // Who is in which level changes below: what moved so far goes out
        // to the receivers it was meant for, first.
        if !matches!(ev, WorldIn::Props { .. }) {
            self.flush();
        }
        match ev {
            WorldIn::Joined { who, outbox, reply } => {
                let key = who.account.to_ascii_lowercase();
                let answer = self.join(&key, who.version, who.gen, outbox).map(|id| {
                    let level = self.level(&who.level);
                    let gmap = self.gmap_of(who.version, &level.name);
                    if let Some(p) = self.players.get_mut(&id) {
                        p.level = level.name.to_ascii_lowercase();
                        p.props.apply(&who.props, ClientVersion::V222);
                        p.gmap = gmap.clone();
                        if gmap.is_some() {
                            p.entered.insert(p.level.clone());
                        }
                        p.opened = who.chests.iter().cloned().collect();
                        p.flags = who.flags.iter().map(|f| split_flag(f)).collect();
                        p.walk = Walk::at(p.global().0, p.global().1, Instant::now());
                    }
                    let key = level.name.to_ascii_lowercase();
                    let leader = self.leader_of(&key).is_none_or(|l| l == id);
                    if leader {
                        self.leaders.insert(key, id);
                    }
                    let level = self.view(id, &level);
                    Joined {
                        id,
                        level,
                        gmap,
                        leader,
                    }
                });
                let new_id = answer.as_ref().ok().map(|j| j.id);
                match (reply.send(answer), new_id) {
                    (Ok(()), Some(id)) => self.meet(id),
                    // The connection closed while it waited: the id goes
                    // straight back, before anyone heard of it.
                    (Err(_), Some(id)) => {
                        if let Some(p) = self.players.remove(&id) {
                            self.by_account.remove(&p.account);
                            self.publish();
                        }
                    }
                    _ => {}
                }
            }
            WorldIn::Left { player } => self.leave(player),
            WorldIn::SaveAll { reply } => {
                let mut n = 0;
                if let Some(save) = &self.saver {
                    for p in self.players.values() {
                        save(p.leaving());
                        n += 1;
                    }
                }
                let _ = reply.send(n);
            }
            WorldIn::Props { player, raw } => self.props(player, &raw),
            WorldIn::NpcProps { player, raw } => self.npc_props(player, &raw),
            WorldIn::BaddyProps { player, raw } => self.baddy_props(player, &raw),
            WorldIn::BaddyHurt { player, raw } => self.baddy_hurt(player, &raw),
            WorldIn::Relay {
                player,
                out,
                named,
                raw,
            } => self.relay(player, out, named, &raw),
            WorldIn::LevelWarp(w) => self.warp(w),
            WorldIn::OpenChest { player, x, y } => self.open_chest(player, x, y),
            WorldIn::Item {
                player,
                op,
                x2,
                y2,
                item,
            } => self.item(player, op, x2, y2, item),
            WorldIn::Flag { player, text, set } => {
                if let Some(p) = self.players.get_mut(&player) {
                    let (name, value) = split_flag(&text);
                    if !name.is_empty() {
                        if set {
                            p.flags.insert(name, value);
                        } else {
                            p.flags.remove(&name);
                        }
                    }
                }
            }
            WorldIn::Adjacent {
                player,
                level,
                mod_time,
            } => self.adjacent(player, &level, mod_time),
        }
    }

    /// `level` as player `id` sees it: the chests the account opened go as
    /// opened.
    fn view(&self, id: u16, level: &Arc<Level>) -> Arc<Level> {
        let Some(p) = self.players.get(&id) else {
            return level.clone();
        };
        let key = level.name.to_ascii_lowercase();
        let is_open = |c: &seihai_core::assets::Chest| {
            let (x, y) = (c.x.clamp(0, 63) as u8, c.y.clamp(0, 63) as u8);
            p.opened.contains(&(key.clone(), x, y))
        };
        if !level.chests.iter().any(is_open) {
            return level.clone();
        }
        let mut l = (**level).clone();
        for c in &mut l.chests {
            c.opened = is_open(c);
        }
        Arc::new(l)
    }

    /// A chest opened (`PLI_OPENCHEST`): the account keeps it as opened,
    /// and its item counts where this server has the prop for it (rupees,
    /// bombs, darts, hearts). The client draws the chest open itself; the
    /// player gets their new props.
    fn open_chest(&mut self, id: u16, x: u8, y: u8) {
        let Some(p) = self.players.get(&id) else {
            return;
        };
        let Some(level) = self.loaded.get(&p.level).cloned() else {
            return;
        };
        let Some(chest) = level
            .chests
            .iter()
            .find(|c| c.x == i32::from(x) && c.y == i32::from(y))
        else {
            return;
        };
        let Some(item) = crate::levels::chest_item(chest) else {
            return;
        };
        let Some(p) = self.players.get_mut(&id) else {
            return;
        };
        if !p.opened.insert((p.level.clone(), x, y)) {
            return;
        }
        let changed = others::give_item(&mut p.props, item);
        if let Some(m) = others::own_message(p.version, &changed) {
            let _ = p.outbox.send(vec![m]);
        }
    }

    /// An item put down, removed or taken in the player's level. Adds and
    /// removals go to the others in the level (`PLO_ITEMADD` 22,
    /// `PLO_ITEMDEL` 23); a take of an item that is still there gives it to
    /// the taker, and a take of one that is gone (someone else was first,
    /// or it timed out) gives nothing.
    fn item(&mut self, id: u16, op: ItemOp, x2: u8, y2: u8, item: u8) {
        let Some(level) = self.players.get(&id).map(|p| p.level.clone()) else {
            return;
        };
        let now = Instant::now();
        let items = self.floor.entry(level.clone()).or_default();
        items.retain(|i| i.until > now);
        let at = |i: &FloorItem| i.x2 == x2 && i.y2 == y2;
        let tell = match op {
            ItemOp::Add => {
                if !seihai_core::items::is_floor_item(item) {
                    return;
                }
                items.retain(|i| !at(i));
                items.push(FloorItem {
                    x2,
                    y2,
                    item,
                    until: now + item_life(),
                });
                crate::wire::msg(PLO_ITEMADD, &[gchar(x2), gchar(y2), gchar(item)])
            }
            ItemOp::Del | ItemOp::Take => {
                let Some(n) = items
                    .iter()
                    .position(|i| at(i) && (op == ItemOp::Del || i.item == item))
                else {
                    return;
                };
                items.remove(n);
                if op == ItemOp::Take {
                    if let Some(p) = self.players.get_mut(&id) {
                        let changed = others::give_item(&mut p.props, item);
                        if let Some(m) = others::own_message(p.version, &changed) {
                            let _ = p.outbox.send(vec![m]);
                        }
                    }
                }
                crate::wire::msg(PLO_ITEMDEL, &[gchar(x2), gchar(y2)])
            }
        };
        if self.floor.get(&level).is_some_and(Vec::is_empty) {
            self.floor.remove(&level);
        }
        let id_of = if op == ItemOp::Add {
            PLO_ITEMADD
        } else {
            PLO_ITEMDEL
        };
        for (&other, p) in &self.players {
            if other != id && p.level == level && p.version.profile().protocol.plo.contains(id_of) {
                let _ = p.outbox.send(vec![tell.clone()]);
            }
        }
    }

    /// Where `level` is on a gmap, for a build that plays gmaps.
    fn gmap_of(&self, version: ClientVersion, level: &str) -> Option<Place> {
        plays_gmaps(version)
            .then(|| self.gmaps.find(level))
            .flatten()
    }

    /// A gmap neighbour the client asked for. It must be a cell of the gmap
    /// the player is on and its file must be there; otherwise there is no
    /// answer. A copy with the level's mod time gets the name alone.
    fn adjacent(&mut self, id: u16, name: &str, mod_time: u64) {
        let Some(gmap) = self.players.get(&id).and_then(|p| p.gmap.clone()) else {
            return;
        };
        if self.gmaps.find(name).is_none_or(|g| g.gmap != gmap.gmap) {
            return;
        }
        let level = match self.find_level(name) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("player {id} neighbour {name}: {e}; not answered");
                return;
            }
        };
        if let Some(p) = self.players.get(&id) {
            let full = mod_time != level.mod_time;
            let level = self.view(id, &level);
            let f = crate::burst::gmap_level(p.version, &level, &gmap.gmap, full);
            let _ = p.outbox.send(vec![f]);
        }
    }

    /// The player walked across a seam: their props named a new cell. The
    /// level there becomes theirs; a cell with no level leaves them where
    /// they are.
    fn cross(&mut self, id: u16, cell: (u8, u8)) {
        let Some(p) = self.players.get(&id) else {
            return;
        };
        let Some(gmap) = p.gmap.clone() else {
            return;
        };
        if cell == gmap.cell {
            return;
        }
        let Some(name) = self.gmaps.level_at(&gmap.gmap, cell).map(str::to_string) else {
            return;
        };
        let level = self.level(&name);
        let key = level.name.to_ascii_lowercase();
        let Some(p) = self.players.get_mut(&id) else {
            return;
        };
        let old = std::mem::replace(&mut p.level, key.clone());
        p.gmap = Some(Place {
            gmap: gmap.gmap.clone(),
            cell,
        });
        let full = p.entered.insert(key.clone());
        if old == key {
            return;
        }
        self.part(id, &old);
        if let Some(p) = self.players.get(&id) {
            let level = self.view(id, &level);
            let f = crate::burst::gmap_level(p.version, &level, &gmap.gmap, full);
            let _ = p.outbox.send(vec![f]);
        }
        self.tell_level(&old, id, &Change::default(), &others::LEFT_LEVEL);
        self.hand_off(&old);
        self.meet(id);
    }

    /// The level called `name`, read on first use. A name that does not
    /// pass the check, or a file that is missing or does not read, gets the
    /// stage 1 board under the name asked for; that is not kept, so the
    /// file is read once it is there.
    fn level(&mut self, name: &str) -> Arc<Level> {
        match self.find_level(name) {
            Ok(l) => l,
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

    /// The level called `name`, read on first use and kept.
    fn find_level(&mut self, name: &str) -> std::io::Result<Arc<Level>> {
        let key = name.to_ascii_lowercase();
        if let Some(l) = self.loaded.get(&key) {
            return Ok(l.clone());
        }
        let l = Arc::new(Level::load(&self.levels.dir, name)?);
        let npcs = l
            .npcs
            .iter()
            .map(|n| {
                self.last_npc += 1;
                Npc::from_level(self.last_npc, n)
            })
            .collect();
        self.npcs.insert(l.name.to_ascii_lowercase(), npcs);
        let baddies = l
            .baddies
            .iter()
            .filter_map({
                let mut n = 0u8;
                move |b| {
                    n = n.checked_add(1)?;
                    Baddy::from_level(n, b)
                }
            })
            .collect();
        self.baddies.insert(l.name.to_ascii_lowercase(), baddies);
        self.loaded.insert(key, l.clone());
        self.loaded.insert(l.name.to_ascii_lowercase(), l.clone());
        Ok(l)
    }

    /// A link request. The level must exist: a link to a missing level
    /// gets no answer, so the client gives up its warp and stays where it
    /// is. Otherwise the player moves to the arrival, the others in the old
    /// level see them leave (50 = 0), and the level goes out
    /// ([`crate::burst::level_answer`]); then the players there and the
    /// newcomer see each other.
    fn warp(&mut self, w: Warp) {
        if !self.players.contains_key(&w.player) {
            return;
        }
        if let Some(what) = self.unlinked(&w) {
            if self.report(w.player, Kind::Warp, what) {
                return;
            }
        }
        let level = match self.find_level(&w.level) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("player {} warp to {}: {e}; not answered", w.player, w.level);
                return;
            }
        };
        let key = level.name.to_ascii_lowercase();
        let Some(p) = self.players.get_mut(&w.player) else {
            return;
        };
        let gmap = plays_gmaps(p.version)
            .then(|| self.gmaps.find(&level.name))
            .flatten();
        let old = std::mem::replace(&mut p.level, key.clone());
        // The client may still send the spot it warped from once, with
        // props it had queued: that position is not a step in the new level.
        p.stale = p.props.x().zip(p.props.y());
        let moved = p.props.moved(w.x, w.y);
        let (cx, cy) = gmap.as_ref().map_or((0, 0), |g| g.cell);
        p.walk = Walk::at(
            f32::from(cx) * 64.0 + w.x,
            f32::from(cy) * 64.0 + w.y,
            Instant::now(),
        );
        let cached = w.mod_time == Some(level.mod_time);
        p.gmap = gmap.clone();
        if gmap.is_some() {
            p.entered.insert(key.clone());
        }
        if old != key {
            self.part(w.player, &old);
        }
        let level = self.view(w.player, &level);
        let Some(p) = self.players.get(&w.player) else {
            return;
        };
        match &gmap {
            // A link into a gmap cell says where on the gmap, then the
            // level as a gmap level (Seihai's beta4 lab notes, "Warps").
            Some(g) => {
                let mut f = crate::wire::msg(
                    crate::wire::ids::PLO_PLAYERWARP2,
                    &crate::burst::warp2(w.x, w.y, g),
                );
                f.extend(crate::burst::gmap_level(
                    p.version, &level, &g.gmap, !cached,
                ));
                let _ = p.outbox.send(vec![f]);
            }
            None => {
                let answer = crate::burst::level_answer(p.version, p.gen, &level, cached);
                let _ = p.outbox.send(answer);
            }
        }
        if old == key {
            self.tell_level(&key, w.player, &moved, &[]);
        } else {
            self.tell_level(&old, w.player, &Change::default(), &others::LEFT_LEVEL);
            self.hand_off(&old);
            self.meet(w.player);
        }
    }

    /// Why a warp is not through a link the player stands on, if it is not:
    /// the level they are in must have a link to `w.level`, the player
    /// must be on it (give or take [`LINK_SLACK`] tiles) and the arrival
    /// must be the link's.
    fn unlinked(&self, w: &Warp) -> Option<String> {
        let p = self.players.get(&w.player)?;
        let (px, py) = (p.props.x().unwrap_or(0.0), p.props.y().unwrap_or(0.0));
        let what = || format!("to {} {} {} from {px} {py}", w.level, w.x, w.y);
        let Some(level) = self.loaded.get(&p.level) else {
            return Some(what());
        };
        // The player's middle, as the clients take links and items.
        let (mx, my) = (px + 1.5, py + 2.0);
        let near = |at: i32, len: i32, m: f32| {
            m >= at as f32 - LINK_SLACK && m <= (at + len) as f32 + LINK_SLACK
        };
        let arrives = |want: Option<f32>, got: f32| want.is_none_or(|v| (v - got).abs() <= 1.0);
        let ok = level.links.iter().any(|l| {
            l.dest.trim().eq_ignore_ascii_case(w.level.trim())
                && near(l.x, l.width, mx)
                && near(l.y, l.height, my)
                && arrives(l.new_x, w.x)
                && arrives(l.new_y, w.y)
        });
        (!ok).then(what)
    }

    /// A violation by player `id`: logged, and the player kicked, as the
    /// policy says. Returns whether to correct it.
    fn report(&mut self, id: u16, kind: Kind, what: String) -> bool {
        let Some(ac) = self.anticheat.clone() else {
            return false;
        };
        let Some(p) = self.players.get_mut(&id) else {
            return false;
        };
        let v = p.score.report(&ac.policy, kind, Instant::now());
        if v.log {
            let what = if v.kick {
                format!("{what}; kicked")
            } else {
                what
            };
            ac.log(&p.account, p.version.label(), &p.level, kind, &what);
        }
        if v.kick {
            // An empty item: the connection closes.
            let _ = p.outbox.send(Vec::new());
        }
        v.correct
    }

    /// Player `id` is leaving `old`: everyone still there leaves the
    /// player's view (`JOINLEAVELVL` 0), before the new level comes.
    fn part(&self, id: u16, old: &str) {
        let Some(me) = self.players.get(&id) else {
            return;
        };
        for (&other, p) in &self.players {
            if other != id && p.level == old {
                let none = Change::default();
                if let Some(m) = others::message(me.version, other, &none, &others::LEFT_LEVEL) {
                    let _ = me.outbox.send(vec![m]);
                }
            }
        }
    }

    fn join(
        &mut self,
        key: &str,
        version: ClientVersion,
        gen: EncryptionGen,
        outbox: Outbox,
    ) -> Result<u16, Refused> {
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
                version,
                gen,
                level: String::new(),
                gmap: None,
                entered: HashSet::new(),
                props: Props::default(),
                opened: HashSet::new(),
                flags: BTreeMap::new(),
                walk: Walk::at(0.0, 0.0, Instant::now()),
                score: Score::default(),
                stale: None,
                outbox,
            },
        );
        self.publish();
        Ok(id)
    }

    fn leave(&mut self, id: u16) {
        if let Some(p) = self.players.remove(&id) {
            if let Some(save) = &self.saver {
                save(p.leaving());
            }
            self.by_account.remove(&p.account);
            self.publish();
            self.tell_level(&p.level, id, &Change::default(), &others::LOGGED_OUT);
            self.hand_off(&p.level);
        }
    }

    /// A player is in: they and the others in their level see each other.
    fn meet(&mut self, id: u16) {
        let Some(me) = self.players.get(&id) else {
            return;
        };
        let mine = me.props.all();
        // The level's NPCs, as they are now. A gmap client takes them
        // under the gmap's name, with the level's cell (beta4 lab
        // capture, 2026-11-05); the level is made active again after.
        let npcs: Vec<_> = self
            .npcs
            .get(&me.level)
            .into_iter()
            .flatten()
            .filter_map(|n| n.message_on(me.version, me.gmap.as_ref().map(|g| g.cell)))
            .collect();
        if !npcs.is_empty() {
            let name = self.loaded.get(&me.level).map_or(&me.level, |l| &l.name);
            for m in under_gmap(me.gmap.as_ref(), name, npcs) {
                let _ = me.outbox.send(vec![m]);
            }
        }
        // The level's leader, if it has none, and its baddies.
        if self.leader_of(&me.level).is_none() {
            self.leaders.insert(me.level.clone(), id);
            let _ = me.outbox.send(vec![crate::wire::msg(PLO_ISLEADER, b"")]);
        }
        if me.version.profile().protocol.plo.contains(PLO_BADDYPROPS) {
            let msgs: Vec<_> = self
                .baddies
                .get(&me.level)
                .into_iter()
                .flatten()
                .map(|b| b.message(me.version))
                .collect();
            if !msgs.is_empty() {
                let name = self.loaded.get(&me.level).map_or(&me.level, |l| &l.name);
                for m in under_gmap(me.gmap.as_ref(), name, msgs) {
                    let _ = me.outbox.send(vec![m]);
                }
            }
        }
        // The items lying there.
        if me.version.profile().protocol.plo.contains(PLO_ITEMADD) {
            let now = Instant::now();
            for i in self.floor.get(&me.level).into_iter().flatten() {
                if i.until > now {
                    let m =
                        crate::wire::msg(PLO_ITEMADD, &[gchar(i.x2), gchar(i.y2), gchar(i.item)]);
                    let _ = me.outbox.send(vec![m]);
                }
            }
        }
        for (&other, p) in &self.players {
            if other == id || p.level != me.level {
                continue;
            }
            if let Some(m) = others::message(me.version, other, &p.props.all(), &others::JOINED) {
                let _ = me.outbox.send(vec![m]);
            }
            if let Some(m) = others::message(p.version, id, &mine, &others::JOINED) {
                let _ = p.outbox.send(vec![m]);
            }
        }
    }

    /// Who leads `level`, if they are still in it.
    fn leader_of(&self, level: &str) -> Option<u16> {
        let id = *self.leaders.get(level)?;
        self.players
            .get(&id)
            .is_some_and(|p| p.level == level)
            .then_some(id)
    }

    /// The leader may have left `level`: the next player still there (the
    /// lowest id, one off a gmap first) leads it now and is told so.
    fn hand_off(&mut self, level: &str) {
        if self.leader_of(level).is_some() {
            return;
        }
        self.leaders.remove(level);
        // A player who plays the level as a gmap cell comes last: real
        // 2.22 on a gmap draws the baddies and runs them, but sends none
        // of their changes (Wine lab, 2026-11-05).
        let next = self
            .players
            .iter()
            .filter(|(_, p)| p.level == level)
            .min_by_key(|(&id, p)| (p.gmap.is_some(), id))
            .map(|(&id, p)| (id, p.outbox.clone()));
        if let Some((id, outbox)) = next {
            self.leaders.insert(level.to_string(), id);
            let _ = outbox.send(vec![crate::wire::msg(PLO_ISLEADER, b"")]);
        }
    }

    /// A baddy change from a client (the leader's AI, or a follower's hit):
    /// kept, and passed on to the others in the level.
    fn baddy_props(&mut self, id: u16, raw: &[u8]) {
        let Some(p) = self.players.get(&id) else {
            return;
        };
        let Some((bid, rest)) = crate::baddies::id_of(raw) else {
            return;
        };
        let Some(b) = self
            .baddies
            .get_mut(&p.level)
            .and_then(|l| l.iter_mut().find(|b| b.id == bid))
        else {
            return;
        };
        let changed = b.update(rest, Instant::now());
        if changed.is_empty() {
            return;
        }
        for (&other, o) in &self.players {
            if other != id
                && o.level == p.level
                && o.version.profile().protocol.plo.contains(PLO_BADDYPROPS)
            {
                let _ = o
                    .outbox
                    .send(vec![crate::baddies::message(bid, &changed, o.version)]);
            }
        }
    }

    /// A hit on a baddy of the player's level, for the leader to apply.
    fn baddy_hurt(&mut self, id: u16, raw: &[u8]) {
        let Some(p) = self.players.get(&id) else {
            return;
        };
        let Some((bid, _)) = crate::baddies::id_of(raw) else {
            return;
        };
        let known = self
            .baddies
            .get(&p.level)
            .is_some_and(|l| l.iter().any(|b| b.id == bid));
        let leader = self.leader_of(&p.level).and_then(|l| self.players.get(&l));
        if let (true, Some(l), Some(m)) = (known, leader, crate::baddies::hurt_message(raw)) {
            let _ = l.outbox.send(vec![m]);
        }
    }

    /// A bomb, shot or blast, passed on to the others in the level whose
    /// build reads `out`.
    fn relay(&self, id: u16, out: u8, named: bool, raw: &[u8]) {
        let Some(p) = self.players.get(&id) else {
            return;
        };
        let mut payload = Vec::with_capacity(raw.len() + 2);
        if named {
            crate::wire::gshort(id, &mut payload);
        }
        payload.extend_from_slice(raw);
        let m = crate::wire::msg(out, &payload);
        for (&other, o) in &self.players {
            if other != id && o.level == p.level && o.version.profile().protocol.plo.contains(out) {
                let _ = o.outbox.send(vec![m.clone()]);
            }
        }
    }

    /// Deaths that are over, and dead baddies that are back.
    fn baddy_timers(&mut self, now: Instant) {
        // (level, baddy, the props that changed, or none for the whole baddy)
        type Due = (String, u8, Option<Vec<(u8, Vec<u8>)>>);
        let mut out: Vec<Due> = Vec::new();
        for (level, list) in self.baddies.iter_mut() {
            for b in list.iter_mut() {
                if b.dead_at.is_some_and(|t| t <= now) {
                    out.push((level.clone(), b.id, Some(b.die(now))));
                } else if b.back_at.is_some_and(|t| t <= now) {
                    b.respawn();
                    out.push((level.clone(), b.id, None));
                }
            }
        }
        for (level, bid, changed) in out {
            let Some(b) = self
                .baddies
                .get(&level)
                .and_then(|l| l.iter().find(|b| b.id == bid))
            else {
                continue;
            };
            for o in self.players.values() {
                if o.level == level && o.version.profile().protocol.plo.contains(PLO_BADDYPROPS) {
                    let m = match &changed {
                        Some(c) => crate::baddies::message(bid, c, o.version),
                        None => b.message(o.version),
                    };
                    let _ = o.outbox.send(vec![m]);
                }
            }
        }
    }

    /// A client's update of an NPC in its level: kept, and passed on to the
    /// others there. An NPC of another level, or none, is ignored.
    fn npc_props(&mut self, id: u16, raw: &[u8]) {
        let Some(p) = self.players.get(&id) else {
            return;
        };
        let (Some(head), Some(rest)) = (raw.get(..3), raw.get(3..)) else {
            return;
        };
        let npc_id = head
            .iter()
            .fold(0u32, |a, b| a << 7 | u32::from(b.wrapping_sub(0x20) & 0x7F));
        let Some(npc) = self
            .npcs
            .get_mut(&p.level)
            .and_then(|l| l.iter_mut().find(|n| n.id == npc_id))
        else {
            return;
        };
        let changed = npc.update(rest, p.version);
        if changed.is_empty() {
            return;
        }
        let name = self.loaded.get(&p.level).map_or(&p.level, |l| &l.name);
        for (&other, o) in &self.players {
            if other != id && o.level == p.level {
                if let Some(m) = crate::npcs::message(npc_id, &changed, o.version) {
                    for m in under_gmap(o.gmap.as_ref(), name, vec![m]) {
                        let _ = o.outbox.send(vec![m]);
                    }
                }
            }
        }
    }

    /// A player's own prop update, passed on to the others in the level.
    fn props(&mut self, id: u16, raw: &[u8]) {
        let Some(p) = self.players.get_mut(&id) else {
            return;
        };
        let before: Vec<(u8, Option<Vec<u8>>)> = [GMAPLEVELX, GMAPLEVELY]
            .iter()
            .map(|&i| (i, p.props.values().get(&i).cloned()))
            .collect();
        let (was_cell, (ox, oy)) = (
            p.gmap.as_ref().map(|g| g.cell),
            (p.props.x().unwrap_or(0.0), p.props.y().unwrap_or(0.0)),
        );
        let mut change = p.props.apply(raw, p.version);
        let stale = std::mem::take(&mut p.stale).filter(|&(sx, sy)| {
            let (x, y) = (p.props.x().unwrap_or(0.0), p.props.y().unwrap_or(0.0));
            Props::moves(&change) && (x - sx).abs() <= 0.5 && (y - sy).abs() <= 0.5
        });
        if stale.is_some() {
            // Not passed on, not checked; the next position is.
            p.props.put_back(&mut change, ox, oy, &[]);
            p.stale = None;
        } else if !Props::moves(&change) {
            p.stale = stale;
        }
        if Props::moves(&change) && self.anticheat.is_some() {
            let cell = was_cell.map_or((0, 0), |c| {
                (
                    change.byte(GMAPLEVELX).unwrap_or(c.0),
                    change.byte(GMAPLEVELY).unwrap_or(c.1),
                )
            });
            let (x, y) = (p.props.x().unwrap_or(0.0), p.props.y().unwrap_or(0.0));
            let (gx, gy) = (f32::from(cell.0) * 64.0 + x, f32::from(cell.1) * 64.0 + y);
            if let Some((kind, d)) = p.walk.step(gx, gy, Instant::now()) {
                let what = format!("{d:.1} tiles to {gx} {gy}");
                if self.report(id, kind, what) {
                    let Some(p) = self.players.get_mut(&id) else {
                        return;
                    };
                    p.props.put_back(&mut change, ox, oy, &before);
                    let mut back = others::own_position(ox, oy);
                    for (i, v) in &before {
                        if let Some(v) = v {
                            back.push((*i, v.clone()));
                        }
                    }
                    if let Some(m) = others::own_message(p.version, &back) {
                        let _ = p.outbox.send(vec![m]);
                    }
                } else if let Some(p) = self.players.get_mut(&id) {
                    p.walk.accept(gx, gy);
                }
            }
        }
        let Some(p) = self.players.get_mut(&id) else {
            return;
        };
        let cell = p.gmap.as_ref().map(|g| {
            (
                g.cell,
                (
                    change.byte(GMAPLEVELX).unwrap_or(g.cell.0),
                    change.byte(GMAPLEVELY).unwrap_or(g.cell.1),
                ),
            )
        });
        if let Some((_, now)) = cell.filter(|(was, now)| was != now) {
            let before = p.level.clone();
            // The player changes level: what moved so far goes out to the
            // level it was meant for, first.
            self.flush();
            self.cross(id, now);
            // Into a new level: the players there met the player with
            // every prop, so this change is not said again.
            if self.players.get(&id).is_some_and(|p| p.level != before) {
                return;
            }
        }
        if !self.players.contains_key(&id) {
            return;
        }
        if !change.is_empty() {
            self.pending.entry(id).or_default().merge(change);
        }
    }

    /// The tick: every receiver gets the batched changes of the others in
    /// its level, in one body. The messages depend only on the receiver's
    /// build, so each level's batch is written once per build there and
    /// each receiver gets a copy without its own message.
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        let mut by_level: HashMap<&str, Vec<(u16, &Change)>> = HashMap::new();
        for (id, change) in &pending {
            if let Some(p) = self.players.get(id) {
                by_level.entry(&p.level).or_default().push((*id, change));
            }
        }
        // (level, build) to the batch body and where each mover's message
        // sits in it.
        let mut written: HashMap<(&str, ClientVersion), Batch> = HashMap::new();
        for (&rid, r) in &self.players {
            let Some(moved) = by_level.get(r.level.as_str()) else {
                continue;
            };
            let (all, spans) = written
                .entry((r.level.as_str(), r.version))
                .or_insert_with(|| {
                    let mut body = Vec::new();
                    let mut spans = Vec::new();
                    for &(id, change) in moved {
                        if let Some(m) = others::message(r.version, id, change, &[]) {
                            spans.push((id, body.len(), body.len() + m.len()));
                            body.extend(m);
                        }
                    }
                    (body, spans)
                });
            let body = match spans.iter().find(|s| s.0 == rid) {
                Some(&(_, from, to)) => [&all[..from], &all[to..]].concat(),
                None => all.clone(),
            };
            if !body.is_empty() {
                let _ = r.outbox.send(vec![body]);
            }
        }
    }

    /// `change` about player `id` to everyone else in `level`.
    fn tell_level(&self, level: &str, id: u16, change: &Change, tail: &[(u8, &[u8])]) {
        for (&other, p) in &self.players {
            if other != id && p.level == level {
                if let Some(m) = others::message(p.version, id, change, tail) {
                    let _ = p.outbox.send(vec![m]);
                }
            }
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
    use seihai_core::protocol::SessionState;

    fn outbox() -> (Outbox, mpsc::UnboundedReceiver<Vec<Vec<u8>>>) {
        mpsc::unbounded_channel()
    }

    fn blank() -> LevelSource {
        LevelSource {
            dir: PathBuf::from("no-such-world/levels"),
            blank_tile: 0x1FF,
            blank_mod_time: 5,
        }
    }

    fn who(account: &str, level: &str) -> Joining {
        Joining {
            account: account.into(),
            level: level.into(),
            version: ClientVersion::V222,
            gen: EncryptionGen::Gen5,
            props: Vec::new(),
            chests: Vec::new(),
            flags: Vec::new(),
        }
    }

    fn joining(account: &str, level: &str, v: ClientVersion) -> Joining {
        let a = crate::accounts::Account::new(account, level, 30.0, 30.0);
        Joining {
            version: v,
            gen: v.encryption(),
            props: crate::burst::own_props(
                ClientVersion::V222,
                &crate::burst::Player {
                    id: 0,
                    account: &a,
                    leader: false,
                },
            ),
            ..who(account, level)
        }
    }

    /// What a client of build `v` makes of everything in its outbox.
    fn seen(v: ClientVersion, rx: &mut mpsc::UnboundedReceiver<Vec<Vec<u8>>>) -> SessionState {
        let mut st = SessionState::for_version(v);
        while let Ok(m) = rx.try_recv() {
            m.iter().for_each(|f| st.apply_body(f));
        }
        assert!(st.ignored.is_empty(), "{v:?}");
        st
    }

    #[test]
    fn leaving_goes_into_the_account() {
        let mut was = crate::accounts::Account::new("cora", "start.nw", 30.0, 30.0);
        was.rupees = 7;
        let mut p = Props::default();
        p.apply(
            &crate::burst::own_props(
                ClientVersion::V222,
                &crate::burst::Player {
                    id: 1,
                    account: &was,
                    leader: false,
                },
            ),
            ClientVersion::V222,
        );
        let mut changed = others::give_item(&mut p, 2);
        changed.extend(others::give_item(&mut p, 11));
        assert_eq!(changed.len(), 3);
        p.moved(12.5, 40.0);
        let l = Leaving {
            account: "cora".into(),
            level: "house.nw".into(),
            x: p.x(),
            y: p.y(),
            props: p.values().clone(),
            chests: vec![("house.nw".into(), 1, 2)],
            flags: vec!["quest=2".into()],
        };
        let mut a = was.clone();
        l.apply_to(&mut a);
        assert_eq!((a.level.as_str(), a.x, a.y), ("house.nw", 12.5, 40.0));
        assert_eq!(a.rupees, 37);
        assert_eq!(
            (a.max_hp, a.hp),
            (was.max_hp + 1, f32::from(was.max_hp + 1))
        );
        assert_eq!(
            (a.arrows, a.bombs, a.glove),
            (was.arrows, was.bombs, was.glove)
        );
        assert_eq!(a.chests, l.chests);
        assert_eq!(a.flags, l.flags);
        assert_eq!(a.password, was.password);
        assert_eq!(split_flag("a=b=c"), ("a".into(), "b=c".into()));
        assert_eq!(split_flag("door"), ("door".into(), String::new()));
    }

    #[tokio::test]
    async fn players_in_a_level_see_each_other_come_move_and_go() {
        let w = World::new(8, blank());
        w.start();
        let (a, mut ra) = outbox();
        let (b, mut rb) = outbox();
        let (c, mut rc) = outbox();
        let alice = w
            .join(joining("alice", "one.nw", ClientVersion::V205), a)
            .await
            .unwrap()
            .unwrap()
            .id;
        let bob = w
            .join(joining("bob", "ONE.nw", ClientVersion::V231), b)
            .await
            .unwrap()
            .unwrap()
            .id;
        // In another level: sees nobody, and nobody sees them.
        let carol = w
            .join(joining("carol", "two.nw", ClientVersion::V222), c)
            .await
            .unwrap()
            .unwrap()
            .id;
        let st = seen(ClientVersion::V205, &mut ra);
        assert_eq!(st.others.len(), 1);
        assert_eq!(st.others[0].id, bob);
        assert_eq!(st.others[0].nick.as_deref(), Some("bob"));
        let st = seen(ClientVersion::V231, &mut rb);
        assert_eq!(st.others.iter().map(|o| o.id).collect::<Vec<_>>(), [alice]);
        assert!(seen(ClientVersion::V222, &mut rc).others.is_empty());

        // Bob (2.31) walks with X2/Y2; Alice (2.05) sees it.
        let mut raw = vec![crate::wire::gchar(78)];
        crate::wire::gshort(seihai_core::protocol::encode_pos_precise(12.5), &mut raw);
        w.props(bob, &raw);
        w.left(carol);
        // Bob logs out: Alice sees him go.
        w.left(bob);
        let (d, _rd) = outbox();
        // A round trip, so the events above are handled.
        w.join(who("dave", "x.nw"), d).await.unwrap().unwrap();
        let mut st = SessionState::for_version(ClientVersion::V205);
        let mut moved = false;
        while let Ok(m) = ra.try_recv() {
            m.iter().for_each(|f| st.apply_body(f));
            if st.others.first().is_some_and(|o| o.x == 12.5) {
                moved = true;
            }
        }
        assert!(moved);
        assert!(st.others.is_empty(), "bob left");
        assert!(rb.try_recv().is_err(), "carol was in another level");
        let _ = carol;
    }

    /// Many moves in one tick reach each receiver as one body, with only the
    /// latest position of each mover.
    #[tokio::test(start_paused = true)]
    async fn moves_in_a_tick_go_out_as_one_body() {
        let w = World::new(8, blank());
        w.start();
        let mut ids = Vec::new();
        let mut rxs = Vec::new();
        for name in ["amy", "ben", "cat"] {
            let (o, r) = outbox();
            let id = w
                .join(joining(name, "one.nw", ClientVersion::V231), o)
                .await
                .unwrap()
                .unwrap()
                .id;
            ids.push(id);
            rxs.push(r);
        }
        // Let the joins go out, then drop what they sent. Half a tick
        // off, so the moves below are all in before the next tick is due.
        tokio::time::sleep(TICK * 2 + TICK / 2).await;
        for r in &mut rxs {
            while r.try_recv().is_ok() {}
        }
        let walk = |x: f32| {
            let mut raw = vec![crate::wire::gchar(78)];
            crate::wire::gshort(seihai_core::protocol::encode_pos_precise(x), &mut raw);
            raw
        };
        // Ben and Cat each walk five steps within one tick.
        for step in 1..=5 {
            w.props(ids[1], &walk(10.0 + step as f32));
            w.props(ids[2], &walk(20.0 + step as f32));
        }
        tokio::time::sleep(TICK * 2).await;
        // Amy gets one body: one message per mover, at the last step.
        let item = rxs[0].try_recv().expect("a batch");
        assert!(rxs[0].try_recv().is_err(), "one body per tick");
        assert_eq!(item.len(), 1, "one frame");
        let body = &item[0];
        assert_eq!(body.iter().filter(|&&b| b == b'\n').count(), 2);
        let mut st = SessionState::for_version(ClientVersion::V231);
        st.apply_body(body);
        let x = |id| st.others.iter().find(|o| o.id == id).map(|o| o.x);
        assert_eq!((x(ids[1]), x(ids[2])), (Some(15.0), Some(25.0)));
        // Ben hears only of Cat, not of himself.
        let body = rxs[1].try_recv().expect("a batch").concat();
        assert_eq!(body.iter().filter(|&&b| b == b'\n').count(), 1);
        // A quiet tick sends nothing.
        tokio::time::sleep(TICK * 2).await;
        assert!(rxs[0].try_recv().is_err());
    }

    #[tokio::test]
    async fn a_link_moves_the_player_and_the_level_goes_out() {
        let dir = std::env::temp_dir().join(format!("seihai-gserver-warp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("one.nw"), "GLEVNW01\nLINK two.nw 0 0 1 1 30 30\n").unwrap();
        std::fs::write(dir.join("two.nw"), "GLEVNW01\nLINK one.nw 63 0 1 1 1 30\n").unwrap();
        let w = World::new(
            8,
            LevelSource {
                dir: dir.clone(),
                ..blank()
            },
        );
        w.start();
        let v = ClientVersion::V222;
        let (a, mut ra) = outbox();
        let (b, mut rb) = outbox();
        let (c, mut rc) = outbox();
        let amy = w
            .join(joining("amy", "one.nw", v), a)
            .await
            .unwrap()
            .unwrap()
            .id;
        let ben = w
            .join(joining("ben", "one.nw", v), b)
            .await
            .unwrap()
            .unwrap()
            .id;
        let cal = w
            .join(joining("cal", "two.nw", v), c)
            .await
            .unwrap()
            .unwrap()
            .id;
        let _ = (seen(v, &mut ra), seen(v, &mut rc));
        let mut sb = seen(v, &mut rb);
        assert_eq!(sb.others.iter().map(|o| o.id).collect::<Vec<_>>(), [amy]);

        // Amy walks into the link to two.nw; then asks for a missing level.
        w.warp(Warp {
            player: amy,
            level: "TWO.nw".into(),
            x: 30.0,
            y: 30.0,
            mod_time: None,
        });
        w.warp(Warp {
            player: amy,
            level: "gone.nw".into(),
            x: 1.0,
            y: 1.0,
            mod_time: None,
        });
        // A round trip so the warps are handled.
        let (d, _rd) = outbox();
        w.join(who("dave", "x.nw"), d).await.unwrap().unwrap();

        let st = seen(v, &mut ra);
        assert_eq!(st.current_level_name().as_deref(), Some("two.nw"));
        assert_eq!(st.links, ["one.nw 63 0 1 1 1 30"]);
        assert_eq!(st.others.iter().map(|o| o.id).collect::<Vec<_>>(), [cal]);
        assert_eq!((st.others[0].x, st.others[0].y), (30.0, 30.0));
        // Ben, left behind, saw Amy go; Cal saw her arrive at the link's
        // arrival.
        while let Ok(m) = rb.try_recv() {
            m.iter().for_each(|f| sb.apply_body(f));
        }
        assert!(sb.others.is_empty(), "amy left ben's level");
        let sc = seen(v, &mut rc);
        assert_eq!(sc.others.iter().map(|o| o.id).collect::<Vec<_>>(), [amy]);
        assert_eq!((sc.others[0].x, sc.others[0].y), (30.0, 30.0));
        let _ = ben;

        // Back to one.nw with its current mod time: no board this time.
        let one = Level::load(&dir, "one.nw").unwrap();
        w.warp(Warp {
            player: amy,
            level: "one.nw".into(),
            x: 1.0,
            y: 30.0,
            mod_time: Some(one.mod_time),
        });
        let (e, _re) = outbox();
        w.join(who("eve", "x.nw"), e).await.unwrap().unwrap();
        let mut ids = Vec::new();
        while let Ok(m) = ra.try_recv() {
            ids.extend(
                seihai_core::protocol::split_messages(&m.concat())
                    .iter()
                    .map(|m| m.id),
            );
        }
        assert!(ids.contains(&6) && ids.contains(&156));
        assert!(!ids.contains(&101), "{ids:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A gen-2 level answer is two frames (the raw-data header, then the
    /// board) and goes to the connection as one item, so nothing the
    /// connection sends itself can land between them (suite stress run,
    /// 1.32 and 1.39 lost a `WANTFILE` answer to the board slot).
    #[tokio::test]
    async fn a_gen2_level_answer_is_one_item() {
        let dir = std::env::temp_dir().join(format!("seihai-gserver-gen2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("two.nw"), "GLEVNW01\n").unwrap();
        let w = World::new(
            8,
            LevelSource {
                dir: dir.clone(),
                ..blank()
            },
        );
        w.start();
        let v = ClientVersion::V139;
        let (a, mut ra) = outbox();
        let mut j = joining("old", "one.nw", v);
        j.gen = EncryptionGen::Gen2;
        let id = w.join(j, a).await.unwrap().unwrap().id;
        w.warp(Warp {
            player: id,
            level: "two.nw".into(),
            x: 30.0,
            y: 30.0,
            mod_time: None,
        });
        let (b, _rb) = outbox();
        w.join(who("rt", "x.nw"), b).await.unwrap().unwrap();
        let item = ra.try_recv().expect("the level answer");
        assert!(item.len() >= 2, "header and board together");
        assert!(item[0].ends_with(b"\n"));
        let mut st = SessionState::for_version(v);
        item.iter().for_each(|f| st.apply_body(f));
        assert_eq!(st.current_level_name().as_deref(), Some("two.nw"));
        assert!(st.board.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `join` for tests that only look at the id.
    async fn id(w: &World, account: &str, o: Outbox) -> Option<Result<u16, Refused>> {
        w.join(who(account, "start.nw"), o)
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
        let j = w.join(who("alice", "lab.nw"), a).await.unwrap().unwrap();
        assert_eq!(j.level.name, "Lab.nw");
        assert_eq!(j.level.links.len(), 1);
        // Read once: the second player gets the same level.
        let (b, _rb) = outbox();
        let k = w.join(who("bob", "LAB.NW"), b).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&j.level, &k.level));
        // No file, or a name that may not name one: the stage 1 board.
        for name in ["gone.nw", "../Lab.nw"] {
            let (c, _rc) = outbox();
            let l = w.join(who(name, name), c).await.unwrap().unwrap().level;
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
        assert_eq!(
            t.join("a", ClientVersion::V222, EncryptionGen::Gen5, o.clone()),
            Ok(ID_LIMIT - 1)
        );
        // Past the 14-bit limit the ids start again at 1, skipping 0.
        assert_eq!(
            t.join("b", ClientVersion::V222, EncryptionGen::Gen5, o.clone()),
            Ok(1)
        );
        t.last_id = ID_LIMIT - 2;
        assert_eq!(
            t.join("c", ClientVersion::V222, EncryptionGen::Gen5, o.clone()),
            Ok(2)
        );
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
                    version: ClientVersion::V222,
                    gen: EncryptionGen::Gen5,
                    level: String::new(),
                    gmap: None,
                    entered: HashSet::new(),
                    props: Props::default(),
                    opened: HashSet::new(),
                    flags: BTreeMap::new(),
                    walk: Walk::at(0.0, 0.0, Instant::now()),
                    score: Score::default(),
                    stale: None,
                    outbox: o.clone(),
                },
            );
        }
        assert_eq!(
            full.join("z", ClientVersion::V222, EncryptionGen::Gen5, o),
            Err(Refused::Full)
        );
    }

    #[tokio::test]
    async fn a_join_nobody_waits_for_is_undone() {
        let w = World::new(8, blank());
        let mut t = w.task.lock().unwrap().take().unwrap();
        let (o, _r) = outbox();
        let (reply, answer) = oneshot::channel();
        drop(answer);
        t.handle(WorldIn::Joined {
            who: who("alice", "start.nw"),
            outbox: o,
            reply,
        });
        assert!(t.players.is_empty());
        assert!(t.by_account.is_empty());
        assert!(w.online_accounts().is_empty());
    }
}

/// NPC messages for a player in `level`. A gmap client keeps NPCs under
/// the gmap's name, so for one the messages go between a
/// `PLO_SETACTIVELEVEL` naming the gmap and one naming the level again (the
/// level is what the login burst made active). Real 2.22 neither draws nor
/// moves a gmap level's NPC sent under the level's own name (Wine lab,
/// 2026-11-05).
fn under_gmap(gmap: Option<&Place>, level: &str, msgs: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let Some(g) = gmap else {
        return msgs;
    };
    let active = |name: &str| crate::wire::msg(PLO_SETACTIVELEVEL, &crate::wire::latin1(name));
    let mut out = vec![active(&g.gmap)];
    out.extend(msgs);
    out.push(active(level));
    out
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::codec::Codec;
    use seihai_core::protocol::EncryptionGen;
    use std::time::Instant;

    /// One level, everyone walking, every build Seihai knows; how long the
    /// tick's fan-out and the connections' frame encoding take.
    /// `SEIHAI_BENCH_PLAYERS=1000 cargo test --release --lib bench_fanout -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_fanout() {
        let n: usize = std::env::var("SEIHAI_BENCH_PLAYERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300);
        let versions = ClientVersion::all_versions();
        let mut w = World::new(
            n,
            LevelSource {
                dir: PathBuf::from("no-such"),
                blank_tile: 0,
                blank_mod_time: 0,
            },
        )
        .task
        .lock()
        .unwrap()
        .take()
        .unwrap();
        let mut rxs = Vec::new();
        for i in 0..n {
            let (o, r) = mpsc::unbounded_channel();
            rxs.push(r);
            let v = versions[i % versions.len()];
            let id = w.join(&format!("p{i}"), v, v.encryption(), o).unwrap();
            let a = crate::accounts::Account::new(&format!("p{i}"), "one.nw", 30.0, 30.0);
            let p = w.players.get_mut(&id).unwrap();
            p.level = "one.nw".into();
            p.props.apply(
                &crate::burst::own_props(
                    ClientVersion::V222,
                    &crate::burst::Player {
                        id,
                        account: &a,
                        leader: false,
                    },
                ),
                ClientVersion::V222,
            );
        }
        let ids: Vec<u16> = w.players.keys().copied().collect();
        let ticks = 40;
        let mut flush_t = std::time::Duration::ZERO;
        let mut enc_t = std::time::Duration::ZERO;
        let mut bytes = 0usize;
        let mut out_bytes = 0usize;
        let mut codecs: Vec<Codec> = (0..n)
            .map(|i| {
                let v = versions[i % versions.len()];
                Codec::new(v.encryption(), 0x33, Some(v))
            })
            .collect();
        for t in 0..ticks {
            // Half the players step each tick (walking every 100 ms).
            for (k, &id) in ids.iter().enumerate() {
                if (k + t) % 2 == 0 {
                    let mut raw = vec![crate::wire::gchar(78)];
                    crate::wire::gshort(
                        seihai_core::protocol::encode_pos_precise(10.0 + (t as f32) * 0.5),
                        &mut raw,
                    );
                    raw.push(crate::wire::gchar(79));
                    crate::wire::gshort(seihai_core::protocol::encode_pos_precise(20.0), &mut raw);
                    w.props(id, &raw);
                }
            }
            let s = Instant::now();
            w.flush();
            flush_t += s.elapsed();
            let s = Instant::now();
            for (i, r) in rxs.iter_mut().enumerate() {
                while let Ok(item) = r.try_recv() {
                    let b = item.concat();
                    bytes += b.len();
                    let f = codecs[i].encode(&b).unwrap();
                    out_bytes += f.len();
                    std::hint::black_box(f);
                }
            }
            enc_t += s.elapsed();
        }
        let _ = EncryptionGen::Gen5;
        eprintln!(
            "{n} players, {ticks} ticks: flush {:?}/tick, encode {:?}/tick, {} bytes/tick ({} on the wire); at 20 ticks/s: flush {:.0}% + encode {:.0}% of a core",
            flush_t / ticks as u32,
            enc_t / ticks as u32,
            bytes / ticks,
            out_bytes / ticks,
            flush_t.as_secs_f64() / ticks as f64 * 20.0 * 100.0,
            enc_t.as_secs_f64() / ticks as f64 * 20.0 * 100.0,
        );
    }
}
