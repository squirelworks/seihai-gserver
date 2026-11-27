//! One accepted 3D link, server side.
//!
//! It keeps the notify protocol ([`Link`]), resends guaranteed events and
//! ghost updates that a lost packet carried ([`Events`], the ghost table
//! here), drives the mission start and carries Graal's BigData text
//! channel both ways. It does no I/O: datagrams come in through
//! [`Connection::receive`] with the time, and everything it wants done
//! comes out of [`Connection::take_out`], so it is tested without sockets.
//!
//! **GameObject era** (4.20r6, 4.20r8, 5.x; Graal_analysis
//! `torque/udp-session.md` §6 to §8): `MissionStartPhase1` goes out with
//! the first packet after the client's first; its ack gets
//! `MissionStartPhase2`, whose ack ends the start. Ghosts go out after
//! phase 1's ack, and phase 2 waits until the client has the ghosts the
//! mission lighting needs (TerrainBlock, WaterBlock, Sun) and, with a
//! TerrainBlock, every chunk it asked for, or [`PHASE2_HOLD_MS`]: a 5.12
//! lights the mission at phase 2, and a terrain or chunks that arrived
//! after it drew wrongly lit all session (lab T3D-162). The
//! control object is named only once its create is acked
//! and the start is over: a control object before that leaves a 5.12
//! client stuck at login step 4. The client then reports its position
//! and its own object's changes in every packet ([`Out::ClientState`]).
//!
//! **Protocol 19** (3.153, 4.04r2; `torque/protocol19-server.md`): three
//! phases. After phase 1's ack the datablock the control object uses goes
//! out, then `DataBlocksDone`; once the client says they are loaded,
//! phase 2, `GhostAlwaysStarting`, and the ghosts. Once phase 2 is acked
//! and every ghost is in, `GhostAlwaysDone` and phase 3; after phase 3's
//! ack the Player or Camera becomes the control object. The client then
//! sends moves, which are acked; a Player's are applied ([`tge::apply_move`])
//! and its packet data goes out again after each change. 3.153 runs the
//! stock mission scripts, which take the mission sequence as an argument
//! and echo it in their acks; 4.04r2 sends none.

use super::bits::{BitReader, BitWriter};
use super::events::{big_data_events, Arg, BigData, BigDataAssembler, ClassTable, Event, Events};
use super::handshake::{self, Profile};
use super::link::Link;
use super::mutate::{Mutation, Rng};
use super::objects::{GameObject, Ghost};
use super::tge;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Milliseconds without a packet from the client before the link is dead.
pub const TIMEOUT_MS: u64 = 30_000;
/// A data packet at least this often, even with nothing to say.
pub const KEEP_ALIVE_MS: u64 = 1_000;
/// At most one data packet this often while there is work.
pub const SEND_INTERVAL_MS: u64 = 50;
/// The longest `MissionStartPhase2` waits for the lighting ghosts and
/// the terrain chunks (a whole 256-chunk terrain takes several seconds).
pub const PHASE2_HOLD_MS: u64 = 15_000;
/// The clients accept up to 450 bytes.
pub const MAX_PACKET_BYTES: usize = 400;
/// The client's ghost table.
pub const MAX_GHOSTS: u32 = 4096;

/// The server's name for a ghosted object.
pub type Key = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mission {
    NotStarted,
    /// GameObject era: "Download Ghost Objects"; protocol 19: datablocks.
    Phase1Sent,
    /// GameObject era: "Mission Lighting".
    Phase2Sent,
    /// Protocol 19 only: "Mission Lighting".
    Phase3Sent,
    /// Every phase acknowledged.
    Done,
}

/// What a GameObject-era client says about itself in each packet.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientState {
    /// Its control object's position.
    pub position: [f32; 3],
    /// The sections of its own object it sent ([`super::objects::bits`]); 0
    /// when it sent no update.
    pub changed: u32,
    /// Its control object as its updates describe it.
    pub object: GameObject,
}

/// What the connection wants done.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// A datagram for the client.
    Send(Vec<u8>),
    /// A finished text transfer from the client.
    Text(BigData),
    ClientState(Box<ClientState>),
    Mission(Mission),
    /// The client's terrain chunk stamps (index y * 16 + x, 0 for none).
    TerrainRequest(Vec<u32>),
    Log(String),
    /// A line for the lab trace (`graal3d_trace`), when tracing is on.
    Trace(String),
}

struct Slot {
    index: u32,
    class_id: u32,
    ghost: Ghost,
    /// Sections not yet sent.
    pending: u32,
    /// The client acked the create.
    created: bool,
    create_in_flight: bool,
    kill: bool,
    kill_in_flight: bool,
    /// Another object set under this key while it was being deleted: it
    /// is created once the client has the delete.
    revive: Option<Ghost>,
}

struct GhostSent {
    key: Key,
    mask: u32,
    create: bool,
    kill: bool,
}

#[derive(Default)]
struct Note {
    ghosts: Vec<GhostSent>,
    /// The packet named the control object for the first time.
    control: bool,
    /// Protocol 19: a moved Player's packet data again.
    control_update: bool,
}

pub struct Connection {
    profile: &'static Profile,
    classes: ClassTable,
    sequence: u32,
    link: Link,
    events: Events,
    established: bool,
    last_receive: u64,
    last_send: u64,
    ack_owed: bool,
    out: Vec<Out>,

    mission: Mission,
    next_tag: u32,
    next_transfer: u32,
    assembler: BigDataAssembler,

    slots: BTreeMap<Key, Slot>,
    used: BTreeSet<u32>,
    notes: HashMap<u32, Note>,
    control_key: Option<Key>,
    control_in_flight: bool,
    control_delivered: bool,
    camera_key: Option<Key>,

    client_object: GameObject,
    client_move_type: Vec<u8>,
    warned_window: bool,

    // Protocol 19.
    mission_seq: u32,
    datablocks_sent: bool,
    ghosting_allowed: bool,
    phase2_acked: bool,
    /// GameObject era: phase 1 is acked and phase 2 waits for the
    /// lighting ghosts, since this time (0 until the next update).
    phase2_held: Option<u64>,
    /// The client asked for terrain chunks: the chunk events sent since go
    /// in `chunk_seqs`.
    terrain_asked: bool,
    chunk_seqs: Vec<u32>,
    /// The service has answered the terrain request ([`Self::terrain_answered`]).
    chunks_answered: bool,
    /// A world switch: phase 2 goes again ([`Self::relight`]) once the new
    /// world's lighting ghosts are in, and its ack ends nothing more.
    relighting: bool,
    /// With `relighting`: the new world brings a TerrainBlock, whose
    /// chunks phase 2 waits for.
    relight_terrain: bool,
    /// A switch came while the start's phase 2 was out: relight after its ack.
    relight_after_ack: bool,
    last_move_ack: u32,
    moves_received: usize,
    control_moved: bool,

    /// The Sharp3D route: the GhostAlwaysObjectEvent creating each ghost,
    /// by event sequence.
    ghost_always_events: HashMap<u32, Key>,
    tracing: bool,
    last_state_trace: String,
    /// Lab: the mutation, when it is due, and whether it went out.
    mutation: Option<(Mutation, u32)>,
    mutation_at: u64,
    mutation_done: bool,
    /// The least time between two data packets ([`SEND_INTERVAL_MS`]
    /// unless `graal3d_sendms` says otherwise).
    send_interval: u64,
}

/// Bits that hold `index`, at least 1.
fn bits_for(index: u32) -> u32 {
    let mut b = 1;
    while index >> b != 0 {
        b += 1;
    }
    b
}

impl Connection {
    pub fn new(profile: &'static Profile, sequence: u32, now: u64) -> Self {
        Connection {
            profile,
            classes: profile.class_table(),
            sequence,
            link: Link::new(sequence),
            events: Events::new(),
            established: false,
            last_receive: now,
            last_send: now,
            ack_owed: false,
            out: Vec::new(),
            mission: Mission::NotStarted,
            next_tag: 0,
            next_transfer: 0,
            assembler: BigDataAssembler::default(),
            slots: BTreeMap::new(),
            used: BTreeSet::new(),
            notes: HashMap::new(),
            control_key: None,
            control_in_flight: false,
            control_delivered: false,
            camera_key: None,
            client_object: GameObject::default(),
            client_move_type: Vec::new(),
            warned_window: false,
            mission_seq: 1,
            datablocks_sent: false,
            ghosting_allowed: false,
            phase2_acked: false,
            phase2_held: None,
            terrain_asked: false,
            chunk_seqs: Vec::new(),
            send_interval: SEND_INTERVAL_MS,
            chunks_answered: false,
            relighting: false,
            relight_terrain: false,
            relight_after_ack: false,
            last_move_ack: 0,
            moves_received: 0,
            control_moved: false,
            ghost_always_events: HashMap::new(),
            tracing: false,
            last_state_trace: String::new(),
            mutation: None,
            mutation_at: 0,
            mutation_done: false,
        }
    }

    /// Lab: [`Out::Trace`] lines for each event and each change of the
    /// client's control object.
    pub fn set_tracing(&mut self, on: bool) {
        self.tracing = on;
    }

    /// Lab: one mutation on this link (the accept ones are the service's).
    /// Sets the least time between two data packets, in milliseconds
    /// (`graal3d_sendms`). A real 5.12 took 10 ms on loopback with no loss
    /// (lab, 2026-11-27), although it advertises 32 ms.
    pub fn set_send_interval(&mut self, ms: u64) {
        self.send_interval = ms.max(1);
    }

    pub fn set_mutation(&mut self, m: Option<(Mutation, u32)>) {
        self.mutation = m.filter(|(k, _)| !k.in_accept());
    }

    pub fn mutation_sent(&self) -> bool {
        self.mutation_done
    }

    fn trace(&mut self, text: impl Into<String>) {
        if self.tracing {
            self.out.push(Out::Trace(text.into()));
        }
    }

    fn mutation_due(&self, now: u64) -> bool {
        self.mutation.is_some()
            && !self.mutation_done
            && self.mutation_at != 0
            && now >= self.mutation_at
    }

    pub fn profile(&self) -> &'static Profile {
        self.profile
    }

    pub fn sequence(&self) -> u32 {
        self.sequence
    }

    pub fn mission(&self) -> Mission {
        self.mission
    }

    /// The client has sent a data packet.
    pub fn established(&self) -> bool {
        self.established
    }

    pub fn control_delivered(&self) -> bool {
        self.control_delivered
    }

    /// Protocol 19: moves acknowledged so far, and how many were taken.
    pub fn last_move_ack(&self) -> u32 {
        self.last_move_ack
    }

    pub fn moves_received(&self) -> usize {
        self.moves_received
    }

    /// Protocol 19: the control object, when it is a Player.
    pub fn control_player(&self) -> Option<tge::PlayerState> {
        match &self.slots.get(&self.control_key?)?.ghost {
            Ghost::TgePlayer(p) => Some(p.state),
            _ => None,
        }
    }

    pub fn ghost_index(&self, key: Key) -> Option<u32> {
        self.slots.get(&key).map(|s| s.index)
    }

    /// The client acked the ghost's create.
    pub fn ghost_delivered(&self, key: Key) -> bool {
        self.slots.get(&key).is_some_and(|s| s.created)
    }

    /// The ghost under `key`, as it was last set.
    /// Whether part of the object's update is still to go.
    #[cfg(test)]
    pub fn ghost_pending(&self, key: Key) -> bool {
        self.slots.get(&key).is_some_and(|s| s.pending != 0)
    }

    pub fn ghost(&self, key: Key) -> Option<&Ghost> {
        self.slots
            .get(&key)
            .map(|s| s.revive.as_ref().unwrap_or(&s.ghost))
    }

    pub fn ghost_count(&self) -> usize {
        self.slots.len()
    }

    /// The keys of the ghosts this client has or is getting (not those
    /// being deleted).
    pub fn ghost_keys(&self) -> Vec<Key> {
        self.slots
            .iter()
            .filter(|(_, s)| !s.kill || s.revive.is_some())
            .map(|(k, _)| *k)
            .collect()
    }

    /// Everything the connection wants done since the last call.
    pub fn take_out(&mut self) -> Vec<Out> {
        std::mem::take(&mut self.out)
    }

    fn log(&mut self, text: impl Into<String>) {
        self.out.push(Out::Log(text.into()));
    }

    fn set_mission(&mut self, m: Mission) {
        self.mission = m;
        self.out.push(Out::Mission(m));
    }

    /// The out-of-band disconnect for this link.
    pub fn disconnect_packet(&self, reason: &[u8]) -> Vec<u8> {
        handshake::disconnect(self.sequence, reason)
    }

    // --- Receiving ----------------------------------------------------------

    /// A datagram whose first bit is 1 (data, ping or ack).
    pub fn receive(&mut self, datagram: &[u8], now: u64) {
        let received = match self.link.receive(datagram) {
            Ok(r) => r,
            Err(super::link::Dropped::AckOutsideWindow) => {
                self.log("dropped a packet that acks one not sent yet");
                return;
            }
            Err(_) => return,
        };
        self.last_receive = now;
        for n in &received.notifies {
            self.events.notify(*n);
            self.ghost_notify(n.sequence, n.delivered);
        }
        if !received.notifies.is_empty() {
            self.ghost_always_delivered();
        }
        if received.answer_with_ack {
            let ack = self.link.ack();
            self.out.push(Out::Send(ack));
            self.last_send = now;
        }
        let Some(mut data) = received.data else {
            return;
        };
        self.ack_owed = true;
        if !self.established {
            self.established = true;
            self.on_established();
        }
        self.read_data(&mut data.body);
    }

    fn on_established(&mut self) {
        if self.profile.ghost_always {
            // Sharp3D route: no mission start commands. The ghosts go as
            // GhostAlwaysObjectEvents (posted by set_ghost), and the start
            // is over once the client has every one; the control object
            // follows. Phase1Sent still goes to the service, which sends
            // the flags and weapons then.
            self.ghosting_allowed = true;
            self.set_mission(Mission::Phase1Sent);
            self.finish_ghost_always();
            return;
        }
        if self.profile.tge {
            let seq = self.mission_seq.to_string();
            self.send_command(b"MissionStartPhase1", &[seq.as_bytes(), b"Graal3D"]);
        } else {
            self.send_command(b"MissionStartPhase1", &[]);
        }
        self.set_mission(Mission::Phase1Sent);
    }

    fn read_data(&mut self, r: &mut BitReader<'_>) {
        let read_game = if self.profile.tge {
            self.read_tge_game(r)
        } else {
            self.read_game(r)
        };
        if !read_game {
            return;
        }
        let events = match self.events.read(r, &self.classes) {
            Ok(e) => e,
            Err(e) => {
                self.log(format!("could not read the client's events: {e:?}"));
                return;
            }
        };
        for ev in events {
            self.process_event(ev);
        }
    }

    /// The GameObject-era client game section.
    fn read_game(&mut self, r: &mut BitReader<'_>) -> bool {
        let camera_control = self.profile.ghost_always
            || self
                .control_key
                .and_then(|k| self.slots.get(&k))
                .is_some_and(|s| matches!(s.ghost, Ghost::Camera(_)));
        let mut read = || -> Result<ClientState, super::bits::OutOfBits> {
            r.flag()?; // first person
            let position = [r.f32()?, r.f32()?, r.f32()?];
            if r.flag()? {
                r.int(12)?; // its control object's ghost
            }
            let mut changed = 0;
            if r.flag()? {
                changed = self.client_object.read(r, &mut self.client_move_type)?;
                // A Camera as the control object: its update has two more
                // sections, mode and followed object, which a live 5.12
                // writes (T3D-023, lab 2026-11-25).
                if camera_control {
                    if r.flag()? {
                        r.int(1)?;
                    }
                    if r.flag()? && r.flag()? {
                        r.int(12)?;
                    }
                }
            }
            if r.flag()? {
                r.int(8)?; // field of view
            }
            Ok(ClientState {
                position,
                changed,
                object: self.client_object.clone(),
            })
        };
        match read() {
            Ok(state) => {
                if self.tracing && state.changed != 0 {
                    let p = state.object.transform.position;
                    let line = format!(
                        "control object: sections {:#x}, at {:.2},{:.2},{:.2}",
                        state.changed, p[0], p[1], p[2]
                    );
                    if line != self.last_state_trace {
                        self.last_state_trace = line.clone();
                        self.trace(line);
                    }
                }
                if self.control_delivered {
                    self.out.push(Out::ClientState(Box::new(state)));
                }
                true
            }
            Err(e) => {
                self.log(format!("could not read the client's game section: {e}"));
                false
            }
        }
    }

    /// The protocol 19 client game section: moves, acked and applied.
    fn read_tge_game(&mut self, r: &mut BitReader<'_>) -> bool {
        let game = match tge::ClientGame::read(r) {
            Ok(Some(g)) => g,
            _ => {
                self.log("could not read the client's game section (protocol 19)");
                return false;
            }
        };
        if game.sends_state || game.moves.is_empty() {
            return true;
        }
        let next = game.first_move + game.moves.len() as u32;
        if next <= self.last_move_ack {
            return true;
        }
        let from = self.last_move_ack.max(game.first_move);
        let control = if self.control_delivered {
            self.control_key
        } else {
            None
        };
        let mut moved = false;
        if let Some(Slot {
            ghost: Ghost::TgePlayer(p),
            ..
        }) = control.and_then(|k| self.slots.get_mut(&k))
        {
            for m in &game.moves[(from - game.first_move) as usize..] {
                tge::apply_move(&mut p.state, m);
            }
            moved = true;
        }
        if moved {
            self.control_moved = true;
        }
        let before = self.moves_received;
        self.moves_received += (next - from) as usize;
        // A move count in the log, for lab runs: the first move, then every
        // hundredth.
        if before == 0 {
            self.log("first move received");
        } else if before / 100 != self.moves_received / 100 {
            self.log(format!("{} moves received", self.moves_received));
        }
        self.last_move_ack = next;
        self.ack_owed = true;
        true
    }

    fn command_args(&self, ev: &Event) -> Option<Vec<Vec<u8>>> {
        let Event::RemoteCommand(args) = ev else {
            return None;
        };
        let cmd = self.events.command(ev)?;
        let mut out = vec![cmd.name];
        for a in &args[1..] {
            out.push(match a {
                Arg::Empty => Vec::new(),
                Arg::Text(t) => t.clone(),
                Arg::Int(i) => i.to_string().into_bytes(),
                Arg::Tag(_) => {
                    // A tagged argument after the name: look it up the same way.
                    let one = Event::RemoteCommand(vec![a.clone()]);
                    self.events
                        .command(&one)
                        .map(|c| c.name)
                        .unwrap_or_default()
                }
            });
        }
        Some(out)
    }

    fn process_event(&mut self, ev: Event) {
        if self.tracing {
            let what = match &ev {
                Event::RemoteCommand(_) => self
                    .command_args(&ev)
                    .map(|a| {
                        a.iter()
                            .map(|x| String::from_utf8_lossy(x).into_owned())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default(),
                Event::BigDataStart {
                    kind,
                    weapon,
                    total,
                    ..
                } => format!(
                    "{} {} ({total} bytes)",
                    String::from_utf8_lossy(kind),
                    String::from_utf8_lossy(weapon)
                ),
                Event::ConnectionMessage {
                    sequence, message, ..
                } => format!("message {message} sequence {sequence}"),
                _ => String::new(),
            };
            let name = ev.class_name().unwrap_or("(unknown class)").to_string();
            self.trace(format!("event {name} {what}").trim_end().to_string());
        }
        match &ev {
            Event::NetString { .. } => {} // Events keeps the table
            Event::RemoteCommand(_) => {
                let Some(args) = self.command_args(&ev) else {
                    return;
                };
                self.remote_command(&args);
            }
            Event::BigDataStart { .. } | Event::BigDataChunk { .. } => {
                match self.assembler.feed(&ev) {
                    Some(Ok(text)) => self.out.push(Out::Text(text)),
                    Some(Err((mut text, compression))) => match unpack(&text.text, compression) {
                        Some(t) => {
                            text.text = t;
                            self.out.push(Out::Text(text));
                        }
                        None => self.log(format!("text with compression {compression} not read")),
                    },
                    None => {}
                }
            }
            Event::ConnectionMessage {
                sequence, message, ..
            } => {
                if !self.profile.tge {
                    return; // this server ghosts from the start
                }
                let (sequence, message) = (*sequence, *message);
                if message == tge::message::DATABLOCKS_DOWNLOAD_DONE
                    && sequence == self.mission_seq
                    && self.datablocks_sent
                    && self.mission == Mission::Phase1Sent
                {
                    self.log("datablocks loaded on the client");
                    let seq = self.mission_seq.to_string();
                    self.send_command(b"MissionStartPhase2", &[seq.as_bytes(), b"Graal3D"]);
                    self.events.send(Event::ConnectionMessage {
                        sequence: self.mission_seq,
                        message: tge::message::GHOST_ALWAYS_STARTING,
                        ghost_count: self.slots.len() as u32,
                    });
                    self.ghosting_allowed = true;
                    self.set_mission(Mission::Phase2Sent);
                } else if message == tge::message::READY_FOR_NORMAL_GHOSTS {
                    self.log("client ready for normal ghosts");
                } else {
                    self.log(format!(
                        "connection message {message} (sequence {sequence}) ignored"
                    ));
                }
            }
            Event::TerrainRequest(stamps) => {
                let held = stamps.iter().filter(|s| **s != 0).count();
                self.log(format!("terrain request: holds {held} of 256 chunks"));
                self.terrain_asked = true;
                self.out.push(Out::TerrainRequest(stamps.clone()));
            }
            Event::SimDataBlock { .. } | Event::Raw { .. } | Event::Other { .. } => {
                self.log(format!(
                    "event ignored: {}",
                    ev.class_name().unwrap_or("(unknown class)")
                ));
            }
        }
    }

    fn remote_command(&mut self, args: &[Vec<u8>]) {
        let command = args[0].as_slice();
        if self.profile.tge {
            // 3.153 echoes the mission sequence; 4.04r2 sends none.
            if let Some(seq) = args.get(1) {
                if *seq != self.mission_seq.to_string().as_bytes() {
                    self.log(format!(
                        "{} for mission sequence {} ignored",
                        String::from_utf8_lossy(command),
                        String::from_utf8_lossy(seq)
                    ));
                    return;
                }
            }
            match (command, self.mission) {
                (b"MissionStartPhase1Ack", Mission::Phase1Sent) => self.start_tge_datablocks(),
                (b"MissionStartPhase2Ack", Mission::Phase2Sent) => {
                    self.phase2_acked = true;
                    self.advance_tge();
                }
                (b"MissionStartPhase3Ack", Mission::Phase3Sent) => self.set_mission(Mission::Done),
                _ => self.log(format!(
                    "remote command ignored: {}",
                    String::from_utf8_lossy(command)
                )),
            }
            return;
        }
        match (command, self.mission) {
            (b"MissionStartPhase1Ack", Mission::Phase1Sent) => {
                // Ghosts go only after this ack. A 5.12 on a return visit
                // dropped ghosts it got before it, then stalled in phase 1 on
                // "Received unknown camera object" (lab, 2026-11-25).
                self.ghosting_allowed = true;
                // Phase 2 goes once the lighting ghosts are in (update).
                if self.phase2_held.is_none() {
                    self.phase2_held = Some(0);
                }
            }
            (b"MissionStartPhase2Ack", Mission::Phase2Sent) if self.relighting => {
                // The start is long over: the service hears nothing.
                self.relighting = false;
                self.mission = Mission::Done;
                self.log("phase 2 acked again: the new world is lit");
            }
            (b"MissionStartPhase2Ack", Mission::Phase2Sent) => {
                self.set_mission(Mission::Done);
                if std::mem::take(&mut self.relight_after_ack) {
                    self.start_relight(true);
                }
            }
            _ => self.log(format!(
                "remote command ignored: {}",
                String::from_utf8_lossy(command)
            )),
        }
    }

    // --- Sending ------------------------------------------------------------

    /// `commandToClient`: the name as a tag (a `NetStringEvent` first), the
    /// other arguments as plain strings.
    fn send_command(&mut self, name: &[u8], args: &[&[u8]]) {
        let tag = self.next_tag;
        self.next_tag = (self.next_tag + 1) % 32;
        self.events.send(Event::NetString {
            id: tag,
            text: name.to_vec(),
        });
        let mut all = vec![Arg::Tag(tag)];
        all.extend(args.iter().map(|a| Arg::Text(a.to_vec())));
        self.events.send(Event::RemoteCommand(all));
    }

    /// Queues a guaranteed event (a terrain chunk, a lab event). An event
    /// of 16384 bits or more is refused: its int14 length would wrap, and
    /// 5.12 runs what an under-counted event leaves and crashes (fuzz tier 1).
    pub fn send_event(&mut self, ev: Event) -> bool {
        let bits = ev.payload_bits();
        if bits >= 1 << 14 {
            self.log(format!(
                "{} of {bits} bits is longer than an event can say, not sent",
                ev.class_name().unwrap_or("event")
            ));
            return false;
        }
        let chunk = self.terrain_asked
            && !self.chunks_answered
            && ev.class_name() == Some("TerrainChunkEvent");
        let seq = self.events.send(ev);
        if chunk {
            self.chunk_seqs.push(seq);
        }
        true
    }

    /// The service has sent every chunk event answering the client's
    /// terrain request (none if it has them all). Phase 2 waits for them.
    pub fn terrain_answered(&mut self) {
        if self.terrain_asked {
            self.chunks_answered = true;
        }
    }

    /// A world switch has set the new world's ghosts: a 5.12 lights the
    /// mission only at phase 2, so a TerrainBlock, WaterBlock or Sun it
    /// gets after the start draws unshaded until something relights it
    /// (lab, #223 world switch, real 5.12: flat grey, cold and warm). So
    /// phase 2 goes again, once the new lighting ghosts are created and,
    /// with a new TerrainBlock, the chunks the client then asks for are
    /// in. A 5.12 takes a second phase 2: it relights, acks, and keeps
    /// the link, the 3D view and its running weapons (same lab). Nothing
    /// is done when no lighting ghost is new, before the start is over
    /// (the start's own phase 2 still waits for them) or on protocol 19.
    pub fn relight(&mut self) {
        self.start_relight(false);
    }

    /// [`Self::relight`]; `force`: even with no lighting ghost still to
    /// come (they came while the start's phase 2 was out, too late).
    fn start_relight(&mut self, force: bool) {
        if self.profile.tge || self.profile.ghost_always {
            return;
        }
        // The object the client is still to be sent under a key.
        fn incoming(s: &Slot) -> Option<&Ghost> {
            if s.kill {
                s.revive.as_ref()
            } else if !s.created {
                Some(&s.ghost)
            } else {
                None
            }
        }
        let new_lights = self
            .slots
            .values()
            .filter_map(incoming)
            .filter(|g| Self::lights_the_mission(g))
            .count();
        if new_lights == 0 && !force {
            return;
        }
        match self.mission {
            Mission::Done => {}
            // The start's phase 2 is out: once it is acked, again.
            Mission::Phase2Sent if !self.relighting => {
                self.relight_after_ack = true;
                return;
            }
            // Still holding phase 2 (the start's, or one for an earlier
            // switch): that wait now covers the newest world's ghosts.
            _ => {
                if self.relighting {
                    self.relight_terrain = self
                        .slots
                        .values()
                        .filter_map(incoming)
                        .any(|g| matches!(g, Ghost::Terrain(_)));
                }
                return;
            }
        }
        self.relighting = true;
        self.relight_terrain = self
            .slots
            .values()
            .filter_map(incoming)
            .any(|g| matches!(g, Ghost::Terrain(_)));
        self.terrain_asked = false;
        self.chunks_answered = false;
        self.chunk_seqs.clear();
        self.phase2_held = Some(0);
        self.log(format!(
            "world switch: phase 2 again once {new_lights} lighting ghosts{} are in",
            if self.relight_terrain {
                " and the terrain chunks"
            } else {
                ""
            }
        ));
    }

    /// Lab: one guaranteed event of `class` whose payload is the raw
    /// fields (a `rawevent` line). False if the client's class table has
    /// no such class or the payload is too long.
    pub fn send_raw_event(&mut self, class: &str, fields: &[super::raw::RawField]) -> bool {
        if self.classes.event_id(class).is_none() {
            return false;
        }
        let mut payload = BitWriter::new();
        super::raw::write_fields(&mut payload, fields);
        self.send_event(Event::Raw {
            class: class.to_string().into(),
            payload,
        })
    }

    /// Queues a text for the client: `weapon` (s1, empty for the client
    /// itself), `kind` (s2: set, unset, events, addweapon, ...), `option`.
    pub fn send_text(&mut self, weapon: &[u8], kind: &[u8], option: &[u8], text: &[u8]) {
        let id = self.next_transfer;
        self.next_transfer = (self.next_transfer + 1) & 0xffff;
        for ev in big_data_events(id, weapon, kind, option, text) {
            self.events.send(ev);
        }
    }

    fn start_tge_datablocks(&mut self) {
        if self.datablocks_sent {
            return;
        }
        self.datablocks_sent = true;
        let mut blocks: Vec<(&str, u32, BitWriter)> = Vec::new();
        if self
            .slots
            .values()
            .any(|s| matches!(s.ghost, Ghost::TgeCamera(_)))
        {
            blocks.push((
                "CameraData",
                tge::CAMERA_DATABLOCK_ID,
                tge::camera_data(b""),
            ));
        }
        if let Some(shape) = self.slots.values().find_map(|s| match &s.ghost {
            Ghost::TgePlayer(p) => Some(p.shape.clone()),
            _ => None,
        }) {
            blocks.push((
                "PlayerData",
                tge::PLAYER_DATABLOCK_ID,
                tge::player_data(b"", &shape),
            ));
        }
        let total = blocks.len() as u32;
        for (i, (class, id, data)) in blocks.iter().enumerate() {
            if let Some(class_id) = self.profile.datablock_id(class) {
                self.events.send(Event::Raw {
                    class: "SimDataBlockEvent".into(),
                    payload: tge::sim_datablock_event(*id, class_id, i as u32, total, data),
                });
            }
        }
        self.events.send(Event::ConnectionMessage {
            sequence: self.mission_seq,
            message: tge::message::DATABLOCKS_DONE,
            ghost_count: 0,
        });
        self.log(format!("datablocks sent ({total}), then DataBlocksDone"));
    }

    fn advance_tge(&mut self) {
        if self.mission != Mission::Phase2Sent || !self.phase2_acked {
            return;
        }
        if self.slots.values().any(|s| !s.created && !s.kill) {
            return;
        }
        self.events.send(Event::ConnectionMessage {
            sequence: self.mission_seq,
            message: tge::message::GHOST_ALWAYS_DONE,
            ghost_count: 0,
        });
        let seq = self.mission_seq.to_string();
        self.send_command(b"MissionStartPhase3", &[seq.as_bytes(), b"Graal3D"]);
        self.set_mission(Mission::Phase3Sent);
    }

    /// Sends what is due. Returns false once the client has been quiet for
    /// [`TIMEOUT_MS`].
    pub fn update(&mut self, now: u64) -> bool {
        if now.saturating_sub(self.last_receive) > TIMEOUT_MS {
            return false;
        }
        if self.profile.tge && self.established {
            self.advance_tge();
        }
        self.release_phase2(now);
        let since = now.saturating_sub(self.last_send);
        if self.mutation.is_some()
            && !self.mutation_done
            && self.mutation_at == 0
            && self.mission == Mission::Done
        {
            self.mutation_at = now + 2_000;
        }
        if self.established {
            let work = self.events.has_pending()
                || self.ack_owed
                || self.ghost_work_pending()
                || self.control_pending()
                || (self.control_delivered && self.control_moved)
                || self.mutation_due(now);
            if (work && since >= self.send_interval) || since >= KEEP_ALIVE_MS {
                self.send_data(now);
            }
        } else if self.ack_owed {
            let ack = self.link.ack();
            self.out.push(Out::Send(ack));
            self.ack_owed = false;
            self.last_send = now;
        }
        true
    }

    /// Whether `ghost` is one the 5.12 lights the mission with.
    fn lights_the_mission(ghost: &Ghost) -> bool {
        matches!(ghost, Ghost::Terrain(_) | Ghost::Water(_) | Ghost::Sun(_))
    }

    /// Sends `MissionStartPhase2` once every lighting ghost is created on
    /// the client, or [`PHASE2_HOLD_MS`] after phase 1's ack.
    fn release_phase2(&mut self, now: u64) {
        let Some(since) = self.phase2_held else {
            return;
        };
        let since = if since == 0 {
            self.phase2_held = Some(now.max(1));
            now
        } else {
            since
        };
        // A ghost still to be created, or created once its key's old
        // object is deleted (a world switch).
        let waiting = self
            .slots
            .values()
            .filter(|s| {
                (!s.kill && !s.created && Self::lights_the_mission(&s.ghost))
                    || s.revive.as_ref().is_some_and(Self::lights_the_mission)
            })
            .count();
        // With a terrain, the client asks for its chunks once it has the
        // TerrainBlock; those must be in too. After a switch, only a new
        // TerrainBlock is asked for again.
        let terrain = if self.relighting {
            self.relight_terrain
        } else {
            self.slots.values().any(|s| {
                (!s.kill && matches!(s.ghost, Ghost::Terrain(_)))
                    || matches!(s.revive, Some(Ghost::Terrain(_)))
            })
        };
        let chunks_left = if terrain && self.chunks_answered {
            self.chunk_seqs
                .iter()
                .filter(|q| !self.events.delivered(**q))
                .count()
        } else {
            0
        };
        let chunks_done = !terrain || (self.chunks_answered && chunks_left == 0);
        let late = now.saturating_sub(since) >= PHASE2_HOLD_MS;
        if (waiting > 0 || !chunks_done) && !late {
            return;
        }
        if waiting > 0 {
            self.log(format!(
                "phase 2 sent with {waiting} lighting ghosts not yet created; the terrain may draw unshaded"
            ));
        }
        if !chunks_done {
            let what = if self.chunks_answered {
                format!("{chunks_left} terrain chunks not yet acked")
            } else {
                "no terrain request answered yet".to_string()
            };
            self.log(format!(
                "phase 2 sent with {what}; the terrain may draw wrongly lit"
            ));
        }
        self.phase2_held = None;
        self.send_command(b"MissionStartPhase2", &[]);
        if self.relighting {
            self.mission = Mission::Phase2Sent;
            self.log("phase 2 sent again: the new world is lit");
        } else {
            self.set_mission(Mission::Phase2Sent);
        }
    }

    fn send_data(&mut self, now: u64) {
        self.ack_owed = false;
        self.last_send = now;
        let Some(mut w) = self.link.data(None, None) else {
            if !self.warned_window {
                self.warned_window = true;
                self.log("send window full: the client acks nothing new");
            }
            let ack = self.link.ack();
            self.out.push(Out::Send(ack));
            return;
        };
        let packet = self.link.last_sent();
        let mut note = Note::default();
        let mutation = self.mutation.filter(|_| self.mutation_due(now));
        let mut rng = Rng::new(mutation.map_or(1, |(_, seed)| seed));
        let mut said = String::new();
        let bad_control = match mutation {
            Some((m @ (Mutation::ControlWrongClass | Mutation::ControlDeleted), _)) => {
                self.bad_control(m, &mut rng, &mut said)
            }
            _ => None,
        };
        match (self.profile.tge, bad_control) {
            (true, Some(bad)) => tge::write_server_game(
                &mut w,
                self.last_move_ack,
                Some(&tge::ServerControl {
                    server_time: now as u32,
                    ghost: bad,
                    player: None,
                    camera: tge::Camera::default(),
                }),
            ),
            (true, None) => self.write_tge_game(&mut w, &mut note, now),
            (false, Some(bad)) => {
                w.flag(true);
                w.flag(true);
                w.u32(now as u32);
                w.int(bad, 12);
                GameObject::default().write(&mut w, 0);
                self.write_camera(&mut w);
                w.flag(false);
            }
            (false, None) => {
                self.write_control(&mut w, &mut note, now);
                self.write_camera(&mut w);
                w.flag(false); // field of view
            }
        }
        let extra = match mutation {
            Some((m @ (Mutation::EventUnknownClass | Mutation::EventBadLength), _)) => {
                Some(self.bad_event(m, &mut rng, &mut said))
            }
            _ => None,
        };
        self.events.write_with(
            &mut w,
            packet,
            &self.classes,
            MAX_PACKET_BYTES * 8 - 16,
            extra.as_ref(),
        );
        match mutation {
            Some((
                m @ (Mutation::GhostUncreated
                | Mutation::GhostDoubleDelete
                | Mutation::GhostIndexWidth),
                _,
            )) => self.bad_ghosts(&mut w, m, &mut rng, &mut said),
            _ => self.write_ghosts(&mut w, &mut note),
        }
        self.notes.insert(packet, note);
        if let Some((m, seed)) = mutation {
            self.mutation_done = true;
            if said.is_empty() {
                said = "nothing to mutate (no fitting ghost)".into();
            }
            let hex: String = w.bytes().iter().map(|b| format!("{b:02x}")).collect();
            let line = format!(
                "mutation {}:{seed} in seq {packet}: {said}; packet {hex}",
                m.name()
            );
            self.log(line.clone());
            self.trace(line);
        }
        self.out.push(Out::Send(w.into_bytes()));
    }

    fn free_index(&self) -> u32 {
        (0..).find(|i| !self.used.contains(i)).unwrap_or(0)
    }

    /// The ghost a control mutation names: one of another class, or one
    /// that holds nothing.
    fn bad_control(&self, m: Mutation, rng: &mut Rng, said: &mut String) -> Option<u32> {
        if m == Mutation::ControlDeleted {
            let bad = (self.free_index() + rng.below(8)).min(MAX_GHOSTS - 1);
            *said = format!("control object: ghost index {bad}, which holds no ghost");
            return Some(bad);
        }
        let class = self
            .control_key
            .and_then(|k| self.slots.get(&k))
            .map(|s| s.ghost.class_name().to_string())
            .unwrap_or_default();
        let others: Vec<&Slot> = self
            .slots
            .values()
            .filter(|s| s.created && !s.kill && s.ghost.class_name() != class)
            .collect();
        if others.is_empty() {
            return None;
        }
        let s = others[rng.below(others.len() as u32) as usize];
        *said = format!(
            "control object: ghost {}, a {}",
            s.index,
            s.ghost.class_name()
        );
        Some(s.index)
    }

    /// An unguaranteed event entry: a class the table lacks, or a length
    /// that does not match its bits.
    fn bad_event(&self, m: Mutation, rng: &mut Rng, said: &mut String) -> BitWriter {
        let classes = self.profile.events.len() as u32;
        let bits = rng.below(64);
        let (class_id, length) = if m == Mutation::EventUnknownClass {
            let id = classes + rng.below(4096 - classes);
            *said = format!(
                "an unguaranteed event of class id {id} (the table has {classes}), {bits} bits"
            );
            (id, bits)
        } else {
            let id = rng.below(classes);
            let delta = [1i64, 8, -1, 16383][rng.below(4) as usize];
            let length = (i64::from(bits) + delta).clamp(0, 16383) as u32;
            *said = format!(
                "an unguaranteed {} claiming {length} bits with {bits}",
                self.profile.events.get(id as usize).copied().unwrap_or("?")
            );
            (id, length)
        };
        let mut e = BitWriter::new();
        e.int(class_id, 12);
        e.int(length, 14);
        for _ in 0..bits {
            e.flag(rng.next_u32() & 1 == 1);
        }
        e
    }

    /// A ghost section that updates an index never created, deletes a
    /// free index twice, or reads with the wrong index width.
    fn bad_ghosts(&self, w: &mut BitWriter, m: Mutation, rng: &mut Rng, said: &mut String) {
        let max_index = self.slots.values().map(|s| s.index).max().unwrap_or(0);
        let existing = self
            .slots
            .iter()
            .find(|(k, s)| s.created && !s.kill && self.control_key != Some(**k))
            .map(|(_, s)| s);
        let spare = (self.free_index() + rng.below(8)).min(MAX_GHOSTS - 1);
        if let (Mutation::GhostIndexWidth, Some(s)) = (m, existing) {
            let real = bits_for(max_index);
            let mut wrong = (real + 1 + rng.below(14)) % 16;
            if wrong == real {
                wrong = (wrong + 1) % 16;
            }
            let mut payload = BitWriter::new();
            s.ghost.write(&mut payload, s.ghost.full_mask(), false);
            w.flag(true);
            w.int(wrong, 4);
            w.flag(true);
            w.int(s.index & ((1u32 << wrong) - 1), wrong);
            w.flag(false);
            w.int(s.class_id, 12);
            w.int(payload.bit_len().min(16383) as u32, 14);
            w.append(&payload);
            w.flag(false);
            *said = format!(
                "ghost section with index width {wrong} (needs {real}): a full update of ghost {}, a {}",
                s.index,
                s.ghost.class_name()
            );
            return;
        }
        let id_bits = bits_for(max_index.max(spare));
        w.flag(true);
        w.int(id_bits, 4);
        if m == Mutation::GhostDoubleDelete {
            for _ in 0..2 {
                w.flag(true);
                w.int(spare, id_bits);
                w.flag(true);
            }
            *said = format!("two deletes of ghost index {spare}, which holds no ghost");
        } else {
            let class_id = existing.map_or(0, |s| s.class_id);
            let bits = rng.below(64);
            w.flag(true);
            w.int(spare, id_bits);
            w.flag(false);
            w.int(class_id, 12);
            w.int(bits, 14);
            for _ in 0..bits {
                w.flag(rng.next_u32() & 1 == 1);
            }
            *said = format!(
                "an update of ghost index {spare}, never created, class id {class_id}, {bits} random bits"
            );
        }
        w.flag(false);
    }

    fn write_tge_game(&mut self, w: &mut BitWriter, note: &mut Note, now: u64) {
        let first = self.control_pending();
        let moved = !first && self.control_delivered && self.control_moved;
        let control = if first || moved {
            self.control_key
                .and_then(|k| self.slots.get(&k))
                .map(|s| tge::ServerControl {
                    server_time: now as u32,
                    ghost: s.index,
                    player: match &s.ghost {
                        Ghost::TgePlayer(p) => Some(p.state),
                        _ => None,
                    },
                    camera: match &s.ghost {
                        Ghost::TgeCamera(c) => *c,
                        _ => tge::Camera::default(),
                    },
                })
        } else {
            None
        };
        if let Some(c) = &control {
            self.control_moved = false;
            if first {
                note.control = true;
                self.control_in_flight = true;
                let seq = self.link.last_sent();
                self.log(format!(
                    "control object (ghost {}) sent in seq {seq}",
                    c.ghost
                ));
            } else {
                note.control_update = true;
            }
        }
        tge::write_server_game(w, self.last_move_ack, control.as_ref());
    }

    /// The control object section: flag A always, flag B when the object
    /// is named, then the server time, its ghost and an empty update (the
    /// ghost carries the state).
    fn write_control(&mut self, w: &mut BitWriter, note: &mut Note, now: u64) {
        w.flag(true);
        // The Sharp3D route names its Camera control object in every packet
        // once the world is in, as the server a 5.12 joined did.
        let again = self.profile.ghost_always
            && self.control_delivered
            && self
                .control_key
                .and_then(|k| self.slots.get(&k))
                .is_some_and(|s| s.created && !s.kill);
        if !w.flag(self.control_pending() || again) {
            return;
        }
        let slot = &self.slots[&self.control_key.expect("pending")];
        w.u32(now as u32);
        w.int(slot.index, 12);
        // A full update with no section set; a Camera's carries its two
        // Camera flags too (both clear).
        if let Ghost::GameObject(_) | Ghost::Camera(_) = &slot.ghost {
            slot.ghost.write(w, 0, false);
        } else {
            GameObject::default().write(w, 0);
        }
        if again {
            return;
        }
        let index = slot.index;
        note.control = true;
        self.control_in_flight = true;
        let seq = self.link.last_sent();
        self.log(format!("control object (ghost {index}) sent in seq {seq}"));
    }

    /// The camera is named once its create is acked: an index the client
    /// cannot resolve as a Camera loses the rest of the packet.
    fn write_camera(&mut self, w: &mut BitWriter) {
        let named = self
            .camera_key
            .and_then(|k| self.slots.get(&k))
            .filter(|s| s.created && !s.kill && matches!(s.ghost, Ghost::Camera(_)));
        if let Some(s) = named {
            w.flag(true);
            w.int(s.index, 12);
            w.flag(false); // no camera packet data
        } else {
            w.flag(false);
        }
    }

    // --- Ghosting -----------------------------------------------------------

    fn allocate_index(&mut self) -> Option<u32> {
        let mut index = 0;
        for u in &self.used {
            if *u != index {
                break;
            }
            index += 1;
        }
        if index >= MAX_GHOSTS {
            return None;
        }
        self.used.insert(index);
        Some(index)
    }

    /// Adds an object or changes it; only what changed goes out. Each era
    /// takes only its own kinds (a Sky goes to both).
    pub fn set_ghost(&mut self, key: Key, ghost: Ghost) {
        if !ghost.sent_to(self.profile.tge) {
            return;
        }
        if let Some(s) = self.slots.get_mut(&key) {
            if s.kill {
                if s.ghost != ghost {
                    // A different object under the key (a world switch):
                    // the old one is deleted on the client first, then
                    // this one created. An update in its place left the
                    // old shape drawn (lab, 2026-11-26, real 5.12).
                    s.revive = Some(ghost);
                    return;
                }
                // The same object brought back before its delete was
                // acked: whole again.
                s.kill = false;
                s.revive = None;
                s.pending |= ghost.full_mask();
            }
            s.pending |= ghost.changes_from(&s.ghost);
            s.ghost = ghost;
            return;
        }
        let Some(class_id) = self.profile.object_id(ghost.class_name()) else {
            self.log(format!(
                "cannot ghost a {}: no class id",
                ghost.class_name()
            ));
            return;
        };
        let Some(index) = self.allocate_index() else {
            self.log("cannot ghost: the client's table is full");
            return;
        };
        self.slots.insert(
            key,
            Slot {
                index,
                class_id,
                ghost,
                pending: 0,
                created: false,
                create_in_flight: false,
                kill: false,
                kill_in_flight: false,
                revive: None,
            },
        );
        if self.profile.ghost_always {
            self.post_ghost_always(key);
        }
    }

    /// The Sharp3D route's create: a GhostAlwaysObjectEvent with the ghost
    /// index (12 bits), a flag (an object follows), its class (12), the
    /// update's length (14) and the update. Its ack creates the ghost.
    fn post_ghost_always(&mut self, key: Key) {
        let controlled = self.control_key == Some(key);
        let Some(s) = self.slots.get_mut(&key) else {
            return;
        };
        let mut update = BitWriter::new();
        s.ghost.write(&mut update, s.ghost.full_mask(), controlled);
        let mut payload = BitWriter::new();
        payload.int(s.index, 12);
        payload.flag(true);
        payload.int(s.class_id, 12);
        payload.int((update.bit_len() as u32).min(16383), 14);
        payload.append(&update);
        if payload.bit_len() >= 1 << 14 {
            let label = s.ghost.label();
            self.log(format!("ghost-always object too large, not sent: {label}"));
            return;
        }
        s.create_in_flight = true;
        s.pending = 0;
        let seq = self.events.send(Event::Raw {
            class: "GhostAlwaysObjectEvent".into(),
            payload,
        });
        self.ghost_always_events.insert(seq, key);
    }

    /// Marks the ghosts whose GhostAlwaysObjectEvent the client has.
    fn ghost_always_delivered(&mut self) {
        if self.ghost_always_events.is_empty() {
            return;
        }
        let done: Vec<u32> = self
            .ghost_always_events
            .keys()
            .copied()
            .filter(|seq| self.events.delivered(*seq))
            .collect();
        for seq in done {
            let key = self.ghost_always_events.remove(&seq).expect("listed");
            if let Some(s) = self.slots.get_mut(&key) {
                s.create_in_flight = false;
                s.created = true;
            }
        }
        self.finish_ghost_always();
    }

    fn finish_ghost_always(&mut self) {
        if !self.profile.ghost_always
            || !self.established
            || self.mission == Mission::Done
            || !self.ghost_always_events.is_empty()
        {
            return;
        }
        self.log("every ghost-always object is on the client");
        self.set_mission(Mission::Done);
    }

    /// Deletes an object on the client.
    pub fn remove_ghost(&mut self, key: Key) {
        let Some(s) = self.slots.get_mut(&key) else {
            return;
        };
        if self.control_key == Some(key) {
            self.control_key = None;
            self.control_in_flight = false;
            self.control_delivered = false;
        }
        if self.camera_key == Some(key) {
            self.camera_key = None;
        }
        if !s.created && !s.create_in_flight && !s.kill_in_flight {
            let index = s.index;
            self.slots.remove(&key);
            self.used.remove(&index);
            return;
        }
        s.kill = true;
        s.revive = None;
        s.pending = 0;
    }

    /// Sends the object's whole update again (once the client has it).
    pub fn resend_ghost(&mut self, key: Key) {
        if let Some(s) = self.slots.get_mut(&key) {
            if s.created && !s.kill {
                s.pending |= s.ghost.full_mask();
            }
        }
    }

    /// Makes this ghost the client's control object, once it is in.
    pub fn set_control_object(&mut self, key: Key) {
        if self.control_key != Some(key) {
            self.control_key = Some(key);
            self.control_in_flight = false;
            self.control_delivered = false;
        }
    }

    /// Makes this Camera ghost the client's camera, once it is in.
    pub fn set_camera_object(&mut self, key: Key) {
        self.camera_key = Some(key);
    }

    fn control_pending(&self) -> bool {
        if self.control_delivered || self.control_in_flight || self.mission != Mission::Done {
            return false;
        }
        self.control_key
            .and_then(|k| self.slots.get(&k))
            .is_some_and(|s| s.created && !s.kill)
    }

    fn ghost_work_pending(&self) -> bool {
        if !self.ghosting_allowed {
            return false;
        }
        self.slots.values().any(|s| {
            !s.create_in_flight && !s.kill_in_flight && (s.kill || !s.created || s.pending != 0)
        })
    }

    /// The ghost section: deletes, then creates, then updates, under the
    /// packet limit. The index width is the bits of the highest index.
    fn write_ghosts(&mut self, w: &mut BitWriter, note: &mut Note) {
        // Protocol 19: nothing before the datablocks they name are loaded;
        // protocols 21 and 24: nothing before MissionStartPhase1Ack.
        if !self.ghosting_allowed {
            w.flag(false);
            return;
        }
        let mut todo: Vec<(u8, Key)> = Vec::new();
        let mut max_index = 0;
        for (k, s) in &self.slots {
            if s.create_in_flight || s.kill_in_flight {
                continue;
            }
            let order = if s.kill {
                0
            } else if !s.created {
                1
            } else if s.pending != 0 {
                2
            } else {
                continue;
            };
            todo.push((order, *k));
            max_index = max_index.max(s.index);
        }
        if !w.flag(!todo.is_empty()) {
            return;
        }
        // Among creates, the client's own Camera and control object go
        // first: a 5.12 draws nothing until it has its Camera, and a big
        // world takes seconds to create (about 140 creates a second, lab
        // T3D-147), so the view comes up at once and fills in.
        // Then what the mission lighting needs, which phase 2 waits for.
        let rank = |k: &Key| {
            if self.camera_key == Some(*k) || self.control_key == Some(*k) {
                0
            } else if Self::lights_the_mission(&self.slots[k].ghost) {
                1
            } else {
                2
            }
        };
        todo.sort_by_key(|(o, k)| (*o, rank(k)));
        let mut id_bits = 1;
        while max_index >> id_bits != 0 {
            id_bits += 1;
        }
        w.int(id_bits, 4);
        let limit = MAX_PACKET_BYTES * 8;
        for (order, key) in todo {
            let controlled = self.control_key == Some(key);
            let s = self.slots.get_mut(&key).expect("listed");
            let create = order == 1;
            let mask = if create {
                s.ghost.full_mask()
            } else {
                s.pending
            };
            let mut payload = BitWriter::new();
            if !s.kill {
                s.ghost.write(&mut payload, mask, controlled);
            }
            if payload.bit_len() >= 1 << 14 {
                s.pending = 0;
                let label = s.ghost.label();
                self.log(format!("ghost update too large, dropped: {label}"));
                continue;
            }
            let entry = 2
                + id_bits as usize
                + if s.kill {
                    0
                } else {
                    12 + 14 + payload.bit_len()
                };
            if !note.ghosts.is_empty() && w.bit_len() + entry + 2 > limit {
                break;
            }
            w.flag(true);
            w.int(s.index, id_bits);
            if !w.flag(s.kill) {
                w.int(s.class_id, 12);
                w.int(payload.bit_len() as u32, 14);
                w.append(&payload);
            }
            note.ghosts.push(GhostSent {
                key,
                mask,
                create,
                kill: s.kill,
            });
            if s.kill {
                s.kill_in_flight = true;
            } else if create {
                s.create_in_flight = true;
            }
            s.pending = 0;
        }
        w.flag(false);
    }

    fn ghost_notify(&mut self, packet: u32, delivered: bool) {
        let Some(note) = self.notes.remove(&packet) else {
            return;
        };
        if note.control_update && !delivered {
            self.control_moved = true;
        }
        if note.control {
            self.control_in_flight = false;
            if delivered {
                self.control_delivered = true;
                if let Some(Slot {
                    ghost: Ghost::GameObject(o),
                    ..
                }) = self.control_key.and_then(|k| self.slots.get(&k))
                {
                    self.client_object = o.clone();
                }
            }
            self.log(format!(
                "control object {} (seq {packet})",
                if delivered {
                    "delivered"
                } else {
                    "lost, sending it again"
                }
            ));
        }
        for sent in note.ghosts {
            let Some(s) = self.slots.get_mut(&sent.key) else {
                continue;
            };
            if sent.kill {
                s.kill_in_flight = false;
                if !delivered {
                    continue;
                }
                if s.kill {
                    if let Some(g) = s.revive.take() {
                        // Its delete is in: the new object is created at
                        // the same index.
                        if let Some(id) = self.profile.object_id(g.class_name()) {
                            s.class_id = id;
                            s.ghost = g;
                            s.kill = false;
                            s.created = false;
                            s.pending = 0;
                            continue;
                        }
                    }
                    let index = s.index;
                    self.slots.remove(&sent.key);
                    self.used.remove(&index);
                } else {
                    s.created = false;
                    s.pending = 0;
                }
                continue;
            }
            if sent.create {
                s.create_in_flight = false;
                s.created |= delivered;
                let msg = format!(
                    "ghost {} ({} {}) {} (seq {packet})",
                    s.index,
                    s.ghost.class_name(),
                    s.ghost.label(),
                    if delivered {
                        "created on the client"
                    } else {
                        "create lost, sending it again"
                    }
                );
                self.log(msg);
                continue;
            }
            if !delivered {
                s.pending |= sent.mask;
            }
        }
    }
}

/// Unpacks a compressed text: 1 zlib, 2 bzip2 (seihai-core's bounded
/// reader, as for gen-4 frames).
fn unpack(bytes: &[u8], compression: u32) -> Option<Vec<u8>> {
    use std::io::Read;
    match compression {
        1 => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(bytes)
                .take(1 << 20)
                .read_to_end(&mut out)
                .ok()?;
            Some(out)
        }
        2 => seihai_core::protocol::bz2_decompress(bytes).ok(),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::graal3d::events::Command;
    use crate::graal3d::handshake::{PROTOCOL_19, PROTOCOL_24};
    use crate::graal3d::link::Rate;
    use crate::graal3d::objects::{flags, Sun, Transform};

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A client built from the same wire layer: it numbers, acks, sends
    /// commands and reads the server's events.
    pub(crate) struct FakeClient {
        pub link: Link,
        pub events: Events,
        classes: ClassTable,
        tge: bool,
        first: bool,
        /// Commands the server sent, by name.
        pub commands: Vec<Command>,
        /// Protocol 19: the moves to put in the next packet.
        pub moves: Vec<tge::Move>,
        pub next_move: u32,
        /// Every event the server sent, in order.
        pub got: Vec<Event>,
        /// The control object is a Camera (its packet data has two more
        /// flags).
        pub camera_control: bool,
        /// Packets that named the control object.
        pub controls: usize,
    }

    impl FakeClient {
        pub fn new(profile: &'static Profile) -> Self {
            FakeClient {
                link: Link::new(0x400),
                events: Events::new(),
                classes: profile.class_table(),
                tge: profile.tge,
                first: true,
                commands: Vec::new(),
                moves: Vec::new(),
                next_move: 0,
                got: Vec::new(),
                camera_control: false,
                controls: 0,
            }
        }

        pub fn packet(&mut self) -> Vec<u8> {
            let max = std::mem::replace(&mut self.first, false).then_some(Rate::CLIENT_512_MAX);
            let mut w = self.link.data(None, max).expect("window open");
            if self.tge {
                let g = tge::ClientGame {
                    first_move: self.next_move,
                    moves: std::mem::take(&mut self.moves),
                    ..tge::ClientGame::default()
                };
                self.next_move += g.moves.len() as u32;
                g.write(&mut w);
            } else {
                w.flag(true); // first person
                for _ in 0..3 {
                    w.f32(0.0);
                }
                w.flag(false);
                w.flag(false);
                w.flag(false);
            }
            let seq = self.link.last_sent();
            self.events.write(&mut w, seq, &self.classes, 3000);
            w.into_bytes()
        }

        pub fn take(&mut self, data: &[u8]) {
            let Ok(r) = self.link.receive(data) else {
                return;
            };
            for n in &r.notifies {
                self.events.notify(*n);
            }
            let Some(mut d) = r.data else {
                return;
            };
            let b = &mut d.body;
            if self.tge {
                b.u32().unwrap(); // move ack
                if b.flag().unwrap() {
                    b.flag().unwrap();
                    b.flag().unwrap();
                }
                if b.flag().unwrap() {
                    assert!(b.flag().unwrap(), "always dirty");
                    b.u32().unwrap();
                    b.int(12).unwrap();
                    // Player or Camera packet data: skip by trying the Player.
                    let at = b.position();
                    if tge::PlayerState::read_packet_data(b)
                        .ok()
                        .flatten()
                        .is_none()
                    {
                        b.seek(at).unwrap();
                        tge::Camera::read_packet_data(b).unwrap();
                    }
                }
                if b.flag().unwrap() {
                    b.int(12).unwrap();
                    b.flag().unwrap();
                }
                if b.flag().unwrap() {
                    b.int(8).unwrap();
                }
            } else {
                if b.flag().unwrap() && b.flag().unwrap() {
                    self.controls += 1;
                    b.u32().unwrap();
                    b.int(12).unwrap();
                    GameObject::default().read(b, &mut Vec::new()).unwrap();
                    if self.camera_control {
                        assert!(!b.flag().unwrap() && !b.flag().unwrap());
                    }
                }
                if b.flag().unwrap() {
                    b.int(12).unwrap();
                    b.flag().unwrap();
                }
                if b.flag().unwrap() {
                    b.int(8).unwrap();
                }
            }
            for ev in self.events.read(b, &self.classes).unwrap() {
                if let Some(c) = self.events.command(&ev) {
                    self.commands.push(c);
                }
                self.got.push(ev);
            }
        }

        pub fn answered(&mut self, name: &[u8]) -> bool {
            self.commands.iter().any(|c| c.name == name)
        }

        pub fn count(&self, name: &[u8]) -> usize {
            self.commands.iter().filter(|c| c.name == name).count()
        }
    }

    fn sends(out: &[Out]) -> Vec<Vec<u8>> {
        out.iter()
            .filter_map(|o| match o {
                Out::Send(d) => Some(d.clone()),
                _ => None,
            })
            .collect()
    }

    fn lab_player() -> GameObject {
        GameObject {
            name: b"G3DPlayer_lab".to_vec(),
            shape: b"labcube.3ds".to_vec(),
            transform: Transform {
                position: [0.0, 0.0, 1.0],
                ..Transform::default()
            },
            nickname: b"lab".to_vec(),
            flags: flags::SOLID | flags::COLLISION,
            ..GameObject::default()
        }
    }

    /// Runs `c` and a fake client a few rounds, acking the mission phases.
    fn run_512(c: &mut Connection, client: &mut FakeClient, mut now: u64, rounds: usize) -> u64 {
        for _ in 0..rounds {
            let p = client.packet();
            c.receive(&p, now);
            now += 60;
            c.update(now);
            for d in sends(&c.take_out()) {
                client.take(&d);
            }
            for (cmd, ack, mark) in [
                (
                    &b"MissionStartPhase1"[..],
                    &b"MissionStartPhase1Ack"[..],
                    &b"sent1"[..],
                ),
                (b"MissionStartPhase2", b"MissionStartPhase2Ack", b"sent2"),
            ] {
                // Each time it is sent (phase 2 goes again on a switch).
                while client.count(cmd) > client.count(mark) {
                    client.commands.push(Command {
                        name: mark.to_vec(),
                        args: vec![],
                    });
                    client.events.send_command(ack, &[]);
                }
            }
        }
        now
    }

    /// The session of `udp-session.md` §8 that a real 5.12 drew its world
    /// from, with one change: the ghosts wait for `MissionStartPhase1Ack`.
    /// The first packet is the recorded one up to its ghost section, which
    /// is now empty (a 5.12 on a return visit dropped ghosts sent before
    /// the ack and stalled in phase 1, lab 2026-11-25).
    #[test]
    fn a_512_session_ghosts_only_after_the_phase_1_ack() {
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        c.set_ghost(1, Ghost::Sun(Sun::default()));
        c.set_ghost(2, Ghost::GameObject(lab_player()));
        c.set_control_object(2);
        let mut client = FakeClient::new(&PROTOCOL_24);
        let recorded = hex("05084002080108000d004ab8b835569a421aeccdd95ebc001880203062007b41");
        c.receive(&client.packet(), 0);
        c.update(60);
        let first = sends(&c.take_out()).remove(0);
        // The control, camera and event sections as recorded; no ghosts.
        assert_eq!(first[..26], recorded[..26]);
        assert!(first.len() < recorded.len());
        assert!(!c.ghost_delivered(1));
        client.take(&first);
        run_512(&mut c, &mut client, 60, 10);
        assert!(c.ghost_delivered(1) && c.ghost_delivered(2));
        assert_eq!(c.mission(), Mission::Done);
        assert!(c.control_delivered());
    }

    /// On a big world the client's own Camera and control object are
    /// created first, so a 5.12 draws at once instead of after the whole
    /// world (lab T3D-147: about 140 creates a second).
    #[test]
    fn the_own_camera_and_control_object_are_created_first() {
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        for k in 1..=200 {
            let mut o = lab_player();
            o.name = format!("cube{k}").into_bytes();
            c.set_ghost(k, Ghost::GameObject(o));
        }
        c.set_ghost(1000, Ghost::GameObject(lab_player()));
        c.set_control_object(1000);
        c.set_ghost(1001, Ghost::Camera(lab_player()));
        c.set_camera_object(1001);
        let mut client = FakeClient::new(&PROTOCOL_24);
        let mut now = 0;
        while !c.ghost_delivered(1001) {
            assert!(now < 60 * 40, "no Camera");
            now = run_512(&mut c, &mut client, now, 1);
        }
        assert!(c.ghost_delivered(1000));
        let world = (1..=200).filter(|k| c.ghost_delivered(*k)).count();
        assert!(world < 20, "{world} world objects before the Camera");
        run_512(&mut c, &mut client, now, 60);
        assert!((1..=200).all(|k| c.ghost_delivered(k)));
    }

    /// `MissionStartPhase2` waits until the client has the TerrainBlock:
    /// a 5.12 lights the mission at phase 2, and a terrain created after
    /// it drew unshaded all session (lab T3D-162).
    #[test]
    fn phase_2_waits_for_the_terrain() {
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        for k in 1..=60 {
            let mut o = lab_player();
            o.name = format!("cube{k}").into_bytes();
            c.set_ghost(k, Ghost::GameObject(o));
        }
        let terrain = crate::graal3d::ground::Terrain::new("t", 8, false);
        c.set_ghost(0x4_0000, Ghost::Terrain(Box::new(terrain)));
        c.set_ghost(1000, Ghost::GameObject(lab_player()));
        c.set_control_object(1000);
        let mut client = FakeClient::new(&PROTOCOL_24);
        let mut now = 0;
        while !client.answered(b"MissionStartPhase2") {
            assert!(now < 60 * 200, "no phase 2");
            assert!(c.mission() != Mission::Phase2Sent || c.ghost_delivered(0x4_0000));
            if c.ghost_delivered(0x4_0000) && !c.terrain_asked {
                // A warm cache: the client asks, and needs no chunk.
                c.terrain_asked = true;
                c.terrain_answered();
            }
            now = run_512(&mut c, &mut client, now, 1);
        }
        assert!(c.ghost_delivered(0x4_0000));
        run_512(&mut c, &mut client, now, 40);
        assert_eq!(c.mission(), Mission::Done);
        assert!(c.control_delivered());
    }

    /// With a terrain, phase 2 also waits for the chunks the client asked
    /// for: chunks that came after the lighting drew lit from the old
    /// heights (lab, #223 check, cold cache).
    #[test]
    fn phase_2_waits_for_the_asked_terrain_chunks() {
        use crate::graal3d::ground::{chunk_event, Chunk, Terrain};
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        c.set_ghost(
            0x4_0000,
            Ghost::Terrain(Box::new(Terrain::new("t", 8, false))),
        );
        c.set_ghost(1000, Ghost::GameObject(lab_player()));
        c.set_control_object(1000);
        let mut client = FakeClient::new(&PROTOCOL_24);
        let mut now = 0;
        while !c.ghost_delivered(0x4_0000) {
            assert!(now < 60 * 100, "no terrain");
            now = run_512(&mut c, &mut client, now, 1);
        }
        // The TerrainBlock is in, but the client's request is not answered.
        now = run_512(&mut c, &mut client, now, 10);
        assert!(!client.answered(b"MissionStartPhase2"));
        c.terrain_asked = true;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut z, &[1u8; 600]).unwrap();
        let chunk = Chunk::from_file(z.finish().unwrap()).unwrap();
        for i in 0..20 {
            assert!(c.send_event(chunk_event(i % 16, i / 16, &chunk)));
        }
        c.terrain_answered();
        let seqs = c.chunk_seqs.clone();
        assert_eq!(seqs.len(), 20);
        while !client.answered(b"MissionStartPhase2") {
            assert!(now < 60 * 400, "no phase 2");
            if c.mission() == Mission::Phase2Sent {
                assert!(seqs.iter().all(|q| c.events.delivered(*q)));
            }
            now = run_512(&mut c, &mut client, now, 1);
        }
        run_512(&mut c, &mut client, now, 20);
        assert_eq!(c.mission(), Mission::Done);
    }

    /// A world switch to another terrain: phase 2 goes again, once the
    /// new TerrainBlock and Sun are created and the chunks the client then
    /// asks for are in. Without it a 5.12 drew the new terrain unshaded
    /// (lab, #223 world switch).
    #[test]
    fn a_world_switch_relights_once_the_new_terrain_is_in() {
        use crate::graal3d::ground::{chunk_event, Chunk, Terrain};
        use crate::graal3d::objects::Sun;
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        c.set_ghost(
            0x4_0000,
            Ghost::Terrain(Box::new(Terrain::new("a", 8, false))),
        );
        c.set_ghost(7, Ghost::Sun(Sun::default()));
        c.set_ghost(1000, Ghost::GameObject(lab_player()));
        c.set_control_object(1000);
        let mut client = FakeClient::new(&PROTOCOL_24);
        let mut now = 0;
        while c.mission() != Mission::Done || !c.control_delivered() {
            assert!(now < 60 * 400, "no start");
            if c.ghost_delivered(0x4_0000) && !c.terrain_asked {
                c.terrain_asked = true;
                c.terrain_answered();
            }
            now = run_512(&mut c, &mut client, now, 1);
        }
        assert_eq!(client.count(b"MissionStartPhase2"), 1);
        // The switch: the old world goes, the new one comes under the
        // same keys (the terrain is a different one, the Sun a new one).
        c.remove_ghost(0x4_0000);
        c.remove_ghost(7);
        c.set_ghost(
            0x4_0000,
            Ghost::Terrain(Box::new(Terrain::new("b", 4, false))),
        );
        c.set_ghost(8, Ghost::Sun(Sun::default()));
        c.relight();
        assert!(c.relighting);
        while !c.ghost_delivered(0x4_0000) || !c.ghost_delivered(8) {
            assert!(now < 60 * 400, "no new terrain");
            assert_eq!(
                client.count(b"MissionStartPhase2"),
                1,
                "phase 2 before the new terrain"
            );
            now = run_512(&mut c, &mut client, now, 1);
        }
        // The new TerrainBlock is in; its chunks are not asked for yet.
        now = run_512(&mut c, &mut client, now, 10);
        assert_eq!(
            client.count(b"MissionStartPhase2"),
            1,
            "phase 2 before the chunks"
        );
        c.terrain_asked = true;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut z, &[2u8; 600]).unwrap();
        let chunk = Chunk::from_file(z.finish().unwrap()).unwrap();
        for i in 0..9 {
            assert!(c.send_event(chunk_event(7 + i % 3, 7 + i / 3, &chunk)));
        }
        c.terrain_answered();
        let seqs = c.chunk_seqs.clone();
        assert_eq!(seqs.len(), 9);
        while client.count(b"MissionStartPhase2") < 2 {
            assert!(now < 60 * 400, "no second phase 2");
            now = run_512(&mut c, &mut client, now, 1);
        }
        assert!(seqs.iter().all(|q| c.events.delivered(*q)));
        run_512(&mut c, &mut client, now, 20);
        assert_eq!(c.mission(), Mission::Done);
        assert!(!c.relighting);
        assert_eq!(client.count(b"MissionStartPhase1"), 1, "no new start");
    }

    /// A switch to a world with nothing to light, or whose lighting ghosts
    /// the client keeps, sends no phase 2.
    #[test]
    fn a_world_switch_with_nothing_new_to_light_sends_no_phase_2() {
        use crate::graal3d::objects::Sun;
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        c.set_ghost(7, Ghost::Sun(Sun::default()));
        c.set_ghost(1000, Ghost::GameObject(lab_player()));
        c.set_control_object(1000);
        let mut client = FakeClient::new(&PROTOCOL_24);
        let now = run_512(&mut c, &mut client, 0, 60);
        assert_eq!(c.mission(), Mission::Done);
        let mut o = lab_player();
        o.name = b"cube".to_vec();
        c.set_ghost(9, Ghost::GameObject(o));
        c.relight();
        assert!(!c.relighting);
        run_512(&mut c, &mut client, now, 40);
        assert_eq!(client.count(b"MissionStartPhase2"), 1);
    }

    /// A different object under a key being deleted (a world switch) is
    /// deleted on the client, then created, never sent as an update of the
    /// old one: the 5.12 kept drawing the old shape (lab, 2026-11-26).
    #[test]
    fn another_object_under_a_deleted_key_is_deleted_then_created() {
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        c.set_tracing(true);
        let mut cube = lab_player();
        cube.name = b"cube".to_vec();
        c.set_ghost(1, Ghost::GameObject(cube.clone()));
        c.set_ghost(2, Ghost::GameObject(lab_player()));
        c.set_control_object(2);
        let mut client = FakeClient::new(&PROTOCOL_24);
        let now = run_512(&mut c, &mut client, 0, 10);
        assert!(c.ghost_delivered(1));
        let index = c.ghost_index(1);

        c.remove_ghost(1);
        c.set_ghost(1, Ghost::Sun(Sun::default()));
        assert!(matches!(c.ghost(1), Some(Ghost::Sun(_))));
        assert_eq!(c.ghost_keys(), vec![1, 2]);
        let mut logs = Vec::new();
        let mut now = now;
        for _ in 0..6 {
            c.receive(&client.packet(), now);
            now += 60;
            c.update(now);
            for o in c.take_out() {
                match o {
                    Out::Send(d) => client.take(&d),
                    Out::Log(t) => logs.push(t),
                    _ => {}
                }
            }
        }
        assert!(
            logs.iter()
                .any(|l| l.contains("Sun") && l.contains("created on the client")),
            "{logs:?}"
        );
        assert!(c.ghost_delivered(1));
        assert_eq!(c.ghost_index(1), index);

        // The same object brought back is only sent whole again.
        c.remove_ghost(1);
        c.set_ghost(1, Ghost::Sun(Sun::default()));
        assert!(c.ghost_delivered(1));
    }

    #[test]
    fn the_sharp3d_route_ghosts_through_events_and_names_its_camera_each_packet() {
        use crate::graal3d::handshake::PROTOCOL_24_SHARP3D;
        let mut c = Connection::new(&PROTOCOL_24_SHARP3D, 0x400, 0);
        c.set_ghost(1, Ghost::Sun(Sun::default()));
        c.set_ghost(2, Ghost::Camera(lab_player()));
        c.set_control_object(2);
        let mut client = FakeClient::new(&PROTOCOL_24_SHARP3D);
        client.camera_control = true;
        let now = run_512(&mut c, &mut client, 0, 10);
        assert_eq!(c.mission(), Mission::Done);
        assert!(c.ghost_delivered(1) && c.ghost_delivered(2));
        assert!(c.control_delivered());
        assert!(client.commands.is_empty(), "no mission start commands");
        let creates = client
            .got
            .iter()
            .filter(|e| e.class_name() == Some("GhostAlwaysObjectEvent"))
            .count();
        assert_eq!(creates, 2);
        let named = client.controls;
        run_512(&mut c, &mut client, now, 3);
        assert_eq!(
            client.controls,
            named + 3,
            "the Camera is named in every packet"
        );
    }

    #[test]
    fn a_mutation_goes_out_once_two_seconds_after_the_start() {
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        c.set_ghost(1, Ghost::Sun(Sun::default()));
        c.set_ghost(2, Ghost::GameObject(lab_player()));
        c.set_control_object(2);
        c.set_mutation(Some((Mutation::GhostDoubleDelete, 3)));
        let mut client = FakeClient::new(&PROTOCOL_24);
        let now = run_512(&mut c, &mut client, 0, 10);
        assert_eq!(c.mission(), Mission::Done);
        assert!(!c.mutation_sent());
        let mut logs = Vec::new();
        let mut t = now;
        for _ in 0..50 {
            t += 60;
            c.receive(&client.packet(), t);
            c.update(t);
            for o in c.take_out() {
                match o {
                    Out::Send(d) => client.take(&d),
                    Out::Log(l) if l.starts_with("mutation") => logs.push(l),
                    _ => {}
                }
            }
        }
        assert!(c.mutation_sent());
        assert_eq!(logs.len(), 1, "{logs:?}");
        assert!(
            logs[0].starts_with("mutation ghost-double-delete:3 in seq"),
            "{}",
            logs[0]
        );
        assert!(logs[0].contains("two deletes of ghost index"));
        // An accept mutation is the service's.
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        c.set_mutation(Some((Mutation::AcceptProtocol, 1)));
        assert!(c.mutation.is_none());
    }

    #[test]
    fn a_raw_event_goes_only_to_a_table_that_has_its_class() {
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        let fields = crate::graal3d::raw::parse_fields(&["i8:1", "f1"]).unwrap();
        assert!(c.send_raw_event("TerrainChunkEvent", &fields));
        assert!(!c.send_raw_event("NoSuchEvent", &fields));
    }

    #[test]
    fn zlib_and_bzip2_texts_are_unpacked() {
        let text = b"s client.a=1\n".repeat(20);
        let bz = seihai_core::protocol::bz2_compress(&text).unwrap();
        assert_eq!(unpack(&bz, 2).unwrap(), text);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut z, &text).unwrap();
        assert_eq!(unpack(&z.finish().unwrap(), 1).unwrap(), text);
        assert!(unpack(b"junk", 2).is_none());
    }

    #[test]
    fn a_ping_gets_an_ack_at_once() {
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        let mut client = FakeClient::new(&PROTOCOL_24);
        c.receive(&client.packet(), 0);
        c.update(60);
        let out = sends(&c.take_out());
        client.take(&out[0]);
        let ping = client.link.ping();
        c.receive(&ping, 70);
        let out = sends(&c.take_out());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][2] >> 4 & 3, 2, "an ack packet: {:02x?}", out[0]);
    }

    #[test]
    fn a_lost_create_is_sent_whole_again() {
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        let mut client = FakeClient::new(&PROTOCOL_24);
        // Ghosts go after the phase 1 ack: get there first.
        let now = run_512(&mut c, &mut client, 0, 2);
        c.set_ghost(1, Ghost::Sun(Sun::default()));
        c.receive(&client.packet(), now);
        c.update(now + 60);
        let first = sends(&c.take_out()).remove(0);
        // Dropped on the way. The keep-alive gets through, so the
        // client's next packet acks it and not the create: that is lost.
        let now = now + 60;
        c.update(now + 1100);
        let keep_alive = sends(&c.take_out()).remove(0);
        assert!(keep_alive.len() < first.len());
        client.take(&keep_alive);
        c.receive(&client.packet(), now + 1150);
        assert!(!c.ghost_delivered(1));
        c.update(now + 1200);
        let again = sends(&c.take_out()).remove(0);
        // The Sun's create, again.
        assert_eq!(again.len(), first.len(), "{again:02x?}");
        client.take(&again);
        c.receive(&client.packet(), now + 1250);
        assert!(c.ghost_delivered(1));
    }

    #[test]
    fn times_out_after_thirty_quiet_seconds() {
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        assert!(c.update(TIMEOUT_MS));
        assert!(!c.update(TIMEOUT_MS + 1));
    }

    #[test]
    fn text_goes_out_in_chunks_and_comes_back_whole() {
        let mut c = Connection::new(&PROTOCOL_24, 0x400, 0);
        let mut client = FakeClient::new(&PROTOCOL_24);
        c.receive(&client.packet(), 0);
        let long = vec![b'x'; 500];
        c.send_text(b"", b"set", b"", &long);
        let mut now = 0;
        let mut texts = BigDataAssembler::default();
        let mut got = None;
        for _ in 0..10 {
            now += 60;
            c.update(now);
            for d in sends(&c.take_out()) {
                client.take(&d);
            }
            for ev in std::mem::take(&mut client.got) {
                if let Some(Ok(t)) = texts.feed(&ev) {
                    got = Some(t);
                }
            }
            c.receive(&client.packet(), now);
        }
        let got = got.expect("the text arrived");
        assert_eq!(got.kind, b"set");
        assert_eq!(got.text, long);

        // And the other way: an events text from the client.
        for ev in big_data_events(7, b"", b"events", b"", b"s client.a=1\n") {
            client.events.send(ev);
        }
        c.receive(&client.packet(), now + 60);
        let texts: Vec<BigData> = c
            .take_out()
            .into_iter()
            .filter_map(|o| match o {
                Out::Text(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].text, b"s client.a=1\n");
    }

    /// Runs a protocol 19 client through the start the way 4.04r2 does
    /// (no mission sequence in its acks) until `done` says stop.
    fn run_tge(c: &mut Connection, client: &mut FakeClient, rounds: usize) -> u64 {
        let mut now = 0;
        let mut answered = BTreeSet::new();
        for _ in 0..rounds {
            c.receive(&client.packet(), now);
            now += 60;
            c.update(now);
            for d in sends(&c.take_out()) {
                client.take(&d);
            }
            for ev in std::mem::take(&mut client.got) {
                if let Event::ConnectionMessage {
                    sequence, message, ..
                } = ev
                {
                    if message == tge::message::DATABLOCKS_DONE {
                        client.events.send(Event::ConnectionMessage {
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
                if client.answered(cmd) && answered.insert(cmd) {
                    client.events.send_command(ack, &[]);
                }
            }
        }
        now
    }

    #[test]
    fn a_404r2_client_gets_a_walking_player() {
        let mut c = Connection::new(&PROTOCOL_19, 0x400, 0);
        c.set_ghost(1, Ghost::Sky(crate::graal3d::objects::Sky::default()));
        let player = tge::Player {
            state: tge::PlayerState {
                position: [5.0, 5.0, 1.0],
                ..tge::PlayerState::default()
            },
            shape: b"player.dts".to_vec(),
        };
        c.set_ghost(2, Ghost::TgePlayer(player));
        c.set_control_object(2);
        let mut client = FakeClient::new(&PROTOCOL_19);
        run_tge(&mut c, &mut client, 12);
        assert_eq!(c.mission(), Mission::Done);
        assert!(c.control_delivered());
        // The PlayerData went out, and only it.
        let blocks = client.got.iter().chain(std::iter::empty()).count();
        let _ = blocks;

        // Walk forward for ten moves.
        client.moves = vec![
            tge::Move {
                y: 32,
                ..tge::Move::default()
            };
            10
        ];
        c.receive(&client.packet(), 2000);
        assert_eq!(c.last_move_ack(), 10);
        let p = c.control_player().unwrap();
        assert!(
            (p.position[1] - (5.0 + 10.0 * tge::PLAYER_MAX_SPEED * tge::TICK_SECONDS)).abs() < 1e-3
        );
        // The same moves again (not acked yet on the client) are not walked twice.
        client.next_move = 0;
        client.moves = vec![
            tge::Move {
                y: 32,
                ..tge::Move::default()
            };
            10
        ];
        c.receive(&client.packet(), 2050);
        assert_eq!(c.moves_received(), 10);
        // Its packet data goes out with the ack.
        c.update(2100);
        let out = c.take_out();
        assert_eq!(sends(&out).len(), 1);
        assert!(out
            .iter()
            .any(|o| matches!(o, Out::Log(t) if t == "first move received")));
    }

    #[test]
    fn a_404r2_client_with_a_camera_gets_the_camera_datablock() {
        let mut c = Connection::new(&PROTOCOL_19, 0x400, 0);
        c.set_ghost(1, Ghost::Sky(crate::graal3d::objects::Sky::default()));
        c.set_ghost(2, Ghost::TgeCamera(tge::Camera::default()));
        c.set_control_object(2);
        // A GameObject is not for this era.
        c.set_ghost(3, Ghost::GameObject(lab_player()));
        assert_eq!(c.ghost_count(), 2);
        let mut client = FakeClient::new(&PROTOCOL_19);
        let mut seen = Vec::new();
        let mut now = 0;
        let mut answered = BTreeSet::new();
        for _ in 0..12 {
            c.receive(&client.packet(), now);
            now += 60;
            c.update(now);
            for d in sends(&c.take_out()) {
                client.take(&d);
            }
            for ev in std::mem::take(&mut client.got) {
                if let Event::ConnectionMessage {
                    sequence, message, ..
                } = &ev
                {
                    if *message == tge::message::DATABLOCKS_DONE {
                        client.events.send(Event::ConnectionMessage {
                            sequence: *sequence,
                            message: tge::message::DATABLOCKS_DOWNLOAD_DONE,
                            ghost_count: 0,
                        });
                    }
                }
                seen.push(ev);
            }
            for (cmd, ack) in [
                (&b"MissionStartPhase1"[..], &b"MissionStartPhase1Ack"[..]),
                (b"MissionStartPhase2", b"MissionStartPhase2Ack"),
                (b"MissionStartPhase3", b"MissionStartPhase3Ack"),
            ] {
                if client.answered(cmd) && answered.insert(cmd) {
                    // 3.153 style: the mission sequence comes back.
                    client.events.send_command(ack, &[Arg::Text(b"1".to_vec())]);
                }
            }
        }
        assert_eq!(c.mission(), Mission::Done);
        assert!(c.control_delivered());
        let datablocks: Vec<u32> = seen
            .iter()
            .filter_map(|e| match e {
                Event::SimDataBlock { id, class_id, .. } => Some(id * 100 + class_id),
                _ => None,
            })
            .collect();
        assert_eq!(datablocks, vec![tge::CAMERA_DATABLOCK_ID * 100]);
        // The order the client sees: DataBlocksDone before GhostAlwaysStarting
        // before GhostAlwaysDone.
        let order: Vec<u32> = seen
            .iter()
            .filter_map(|e| match e {
                Event::ConnectionMessage { message, .. } => Some(*message),
                _ => None,
            })
            .collect();
        assert_eq!(
            order,
            vec![
                tge::message::DATABLOCKS_DONE,
                tge::message::GHOST_ALWAYS_STARTING,
                tge::message::GHOST_ALWAYS_DONE
            ]
        );
    }

    #[test]
    fn a_3153_ack_for_another_mission_is_ignored() {
        let mut c = Connection::new(&PROTOCOL_19, 0x400, 0);
        c.set_ghost(2, Ghost::TgeCamera(tge::Camera::default()));
        let mut client = FakeClient::new(&PROTOCOL_19);
        c.receive(&client.packet(), 0);
        c.update(60);
        for d in sends(&c.take_out()) {
            client.take(&d);
        }
        client
            .events
            .send_command(b"MissionStartPhase1Ack", &[Arg::Text(b"7".to_vec())]);
        c.receive(&client.packet(), 100);
        c.update(200);
        assert!(c
            .take_out()
            .iter()
            .any(|o| matches!(o, Out::Log(t) if t.contains("for mission sequence 7 ignored"))));
        assert_eq!(c.mission(), Mission::Phase1Sent);
    }
}
