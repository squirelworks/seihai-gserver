//! Events: the commands, strings and text transfers each data packet
//! carries after its game section.
//!
//! ```text
//! unguaranteed: { flag 1, int12 class id, int14 bit length, payload } ... flag 0
//! guaranteed:   { flag 1, flag "next in order" (else int7 sequence),
//!                 int12 class id, int14 bit length, payload } ... flag 0
//! ```
//!
//! Class ids are the server's (from the accept's class table) in both
//! directions. Every payload carries its bit length, so a class the
//! client does not know is skipped. Guaranteed events are numbered 0, 1,
//! 2, ... each way and delivered strictly in order; one from a lost packet
//! goes out again. The ordering rules follow the open (MIT) Torque net
//! layer; the payloads and the length prefix are Graal's (§5 of the
//! clean-room note).

use super::bits::{BitReader, BitWriter, OutOfBits};
use super::handshake::ClassEntry;
use super::link::Notify;
use std::collections::{BTreeMap, HashMap, VecDeque};

/// How many guaranteed events may be sent past the oldest unacknowledged one.
pub const EVENT_WINDOW: u32 = 126;

/// Width of an event's bit-length prefix.
const LENGTH_BITS: u32 = 14;

/// The server's class ids by name, from the connect accept.
#[derive(Debug, Clone, Default)]
pub struct ClassTable {
    objects: HashMap<u32, String>,
    /// Datablock classes (protocol 19 only).
    datablocks: HashMap<u32, String>,
    events: HashMap<u32, String>,
    event_ids: HashMap<String, u32>,
}

impl ClassTable {
    /// `event_type` is the class type of events: 1 for protocols 21 and
    /// 24, 2 for protocol 19 (which also has datablocks).
    pub fn new(entries: &[ClassEntry], event_type: u32) -> Self {
        let mut t = ClassTable::default();
        for e in entries {
            let name = String::from_utf8_lossy(&e.name).into_owned();
            if e.class_type == 0 {
                t.objects.insert(e.server_id, name);
            } else if e.class_type == event_type {
                t.event_ids.insert(name.clone(), e.server_id);
                t.events.insert(e.server_id, name);
            } else if event_type == 2 && e.class_type == 1 {
                t.datablocks.insert(e.server_id, name);
            }
        }
        t
    }

    pub fn event_name(&self, id: u32) -> Option<&str> {
        self.events.get(&id).map(String::as_str)
    }

    pub fn event_id(&self, name: &str) -> Option<u32> {
        self.event_ids.get(name).copied()
    }

    pub fn object_name(&self, id: u32) -> Option<&str> {
        self.objects.get(&id).map(String::as_str)
    }

    /// A datablock class by its id (protocol 19).
    pub fn datablock_name(&self, id: u32) -> Option<&str> {
        self.datablocks.get(&id).map(String::as_str)
    }
}

/// One argument of a remote command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Empty,
    Text(Vec<u8>),
    /// An entry of the sender's string table ([`Event::NetString`]).
    Tag(u32),
    Int(i32),
}

/// An event, as far as the client reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Defines entry `id` of the sender's 32-entry string table.
    NetString {
        id: u32,
        text: Vec<u8>,
    },
    /// `commandToClient` / `commandToServer`: argument 0 is the name.
    RemoteCommand(Vec<Arg>),
    /// The start of a Graal text transfer (flags, weapons, scripts).
    BigDataStart {
        transfer: u32,
        weapon: Vec<u8>,
        kind: Vec<u8>,
        option: Vec<u8>,
        total: u32,
        compression: u32,
    },
    BigDataChunk {
        transfer: u32,
        bytes: Vec<u8>,
    },
    ConnectionMessage {
        sequence: u32,
        message: u32,
        ghost_count: u32,
    },
    /// One datablock of the mission's datablock phase (protocol 19). The
    /// datablock's own fields follow and are skipped by the event length.
    SimDataBlock {
        /// The datablock's object id (sent as id - 3 in 12 bits).
        id: u32,
        /// Its datablock class, in the server's numbering.
        class_id: u32,
        index: u32,
        total: u32,
    },
    /// `TerrainRequestUpdatesEvent`: the stamp of each of the 256 terrain
    /// chunks the client holds (index y * 16 + x, 0 for none).
    TerrainRequest(Vec<u32>),
    /// An event the server built itself (a protocol 19 datablock), sent
    /// as `class` with this payload. Never read.
    Raw {
        class: std::borrow::Cow<'static, str>,
        payload: BitWriter,
    },
    /// A class this side does not read, skipped by its length.
    Other {
        class_id: u32,
        name: Option<String>,
    },
}

impl Event {
    /// The class name this event is sent as.
    pub fn class_name(&self) -> Option<&str> {
        Some(match self {
            Event::NetString { .. } => "NetStringEvent",
            Event::RemoteCommand(_) => "RemoteCommandEvent",
            Event::BigDataStart { .. } => "BigDataStartEvent",
            Event::BigDataChunk { .. } => "BigDataChunkEvent",
            Event::ConnectionMessage { .. } => "ConnectionMessageEvent",
            Event::SimDataBlock { .. } => "SimDataBlockEvent",
            Event::TerrainRequest(_) => "TerrainRequestUpdatesEvent",
            Event::Raw { class, .. } => class.as_ref(),
            Event::Other { name, .. } => return name.as_deref(),
        })
    }

    fn read(class_id: u32, name: Option<&str>, r: &mut BitReader<'_>) -> Result<Event, OutOfBits> {
        Ok(match name {
            Some("NetStringEvent") => Event::NetString {
                id: r.int(5)?,
                text: r.pstring()?,
            },
            Some("RemoteCommandEvent") => {
                let argc = r.int(5)?;
                let mut args = Vec::with_capacity(argc as usize);
                for _ in 0..argc {
                    args.push(match r.int(2)? {
                        0 => Arg::Empty,
                        1 => Arg::Text(r.pstring()?),
                        2 => Arg::Tag(r.int(5)?),
                        _ => {
                            let negative = r.flag()?;
                            let v = if r.flag()? {
                                r.int(7)?
                            } else if r.flag()? {
                                r.int(15)?
                            } else {
                                r.int(31)?
                            } as i32;
                            Arg::Int(if negative { -v } else { v })
                        }
                    });
                }
                Event::RemoteCommand(args)
            }
            Some("BigDataStartEvent") => Event::BigDataStart {
                transfer: r.int(16)?,
                weapon: r.pstring()?,
                kind: r.pstring()?,
                option: r.pstring()?,
                total: r.int(24)?,
                compression: r.int(3)?,
            },
            Some("BigDataChunkEvent") => {
                let transfer = r.int(16)?;
                let len = r.int(8)? as usize;
                Event::BigDataChunk {
                    transfer,
                    bytes: r.raw(len)?,
                }
            }
            Some("ConnectionMessageEvent") => Event::ConnectionMessage {
                sequence: r.u32()?,
                message: r.int(3)?,
                ghost_count: r.int(11)?,
            },
            Some("SimDataBlockEvent") => Event::SimDataBlock {
                id: r.int(12)? + 3,
                class_id: r.int(12)?,
                index: r.int(12)?,
                total: r.int(13)?,
            },
            Some("TerrainRequestUpdatesEvent") => {
                let mut stamps = Vec::with_capacity(256);
                for _ in 0..256 {
                    stamps.push(r.u32()?);
                }
                Event::TerrainRequest(stamps)
            }
            _ => Event::Other {
                class_id,
                name: name.map(str::to_owned),
            },
        })
    }

    /// The payload's length in bits.
    pub fn payload_bits(&self) -> usize {
        let mut w = BitWriter::new();
        self.write(&mut w);
        w.bit_len()
    }

    /// The payload, with strings that never share a prefix.
    fn write(&self, w: &mut BitWriter) {
        match self {
            Event::NetString { id, text } => {
                w.int(*id, 5);
                w.pstring_alone(text);
            }
            Event::RemoteCommand(args) => {
                w.int(args.len().min(31) as u32, 5);
                for a in args.iter().take(31) {
                    match a {
                        Arg::Empty => w.int(0, 2),
                        Arg::Text(t) => {
                            w.int(1, 2);
                            w.pstring_alone(t);
                        }
                        Arg::Tag(id) => {
                            w.int(2, 2);
                            w.int(*id, 5);
                        }
                        Arg::Int(v) => {
                            w.int(3, 2);
                            w.flag(*v < 0);
                            let m = v.unsigned_abs().min((1 << 31) - 1);
                            if w.flag(m < 1 << 7) {
                                w.int(m, 7);
                            } else if w.flag(m < 1 << 15) {
                                w.int(m, 15);
                            } else {
                                w.int(m, 31);
                            }
                        }
                    }
                }
            }
            Event::BigDataStart {
                transfer,
                weapon,
                kind,
                option,
                total,
                compression,
            } => {
                w.int(*transfer, 16);
                w.pstring_alone(weapon);
                w.pstring_alone(kind);
                w.pstring_alone(option);
                w.int(*total, 24);
                w.int(*compression, 3);
            }
            Event::BigDataChunk { transfer, bytes } => {
                w.int(*transfer, 16);
                w.int(bytes.len() as u32, 8);
                w.raw(bytes);
            }
            Event::ConnectionMessage {
                sequence,
                message,
                ghost_count,
            } => {
                w.u32(*sequence);
                w.int(*message, 3);
                w.int(*ghost_count, 11);
            }
            Event::SimDataBlock {
                id,
                class_id,
                index,
                total,
            } => {
                w.int(id.saturating_sub(3), 12);
                w.int(*class_id, 12);
                w.int(*index, 12);
                w.int(*total, 13);
            }
            Event::TerrainRequest(stamps) => {
                for i in 0..256 {
                    w.u32(stamps.get(i).copied().unwrap_or(0));
                }
            }
            Event::Raw { payload, .. } => w.append(payload),
            Event::Other { .. } => {}
        }
    }
}

/// A remote command with its name and arguments resolved to text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub name: Vec<u8>,
    pub args: Vec<Arg>,
}

/// Why the event lists of a packet could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventError {
    Short(OutOfBits),
    /// A payload read past its own bit length.
    Overrun {
        class_id: u32,
    },
}

impl From<OutOfBits> for EventError {
    fn from(e: OutOfBits) -> Self {
        EventError::Short(e)
    }
}

/// The event state of one link: ordering, resends and string tables.
#[derive(Debug, Default)]
pub struct Events {
    next_receive: u32,
    waiting: BTreeMap<u32, Event>,
    /// Their string table.
    tags: HashMap<u32, Vec<u8>>,
    /// Guaranteed events not yet in a packet, in sequence order.
    to_send: VecDeque<(u32, Event)>,
    next_send: u32,
    /// What each of our data packets carried.
    in_flight: HashMap<u32, Vec<(u32, Event)>>,
    /// Delivered sequences past the oldest unacknowledged one.
    delivered: BTreeMap<u32, ()>,
    /// Every sequence below this is delivered.
    acked_below: u32,
}

impl Events {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads both event lists of a received data packet. Returns the
    /// events now deliverable, in order: the unguaranteed ones, then the
    /// guaranteed ones that are next in line. String table entries are
    /// recorded as they are delivered.
    pub fn read(
        &mut self,
        r: &mut BitReader<'_>,
        classes: &ClassTable,
    ) -> Result<Vec<Event>, EventError> {
        let mut out = Vec::new();
        while r.flag()? {
            out.push(Self::read_one(r, classes)?);
        }
        let mut prev: Option<u32> = None;
        while r.flag()? {
            let seq7 = match (r.flag()?, prev) {
                (true, Some(p)) => (p + 1) & 0x7f,
                // "Next in order" on the first event means sequence -1 + 1.
                (true, None) => 0x7f,
                (false, _) => r.int(7)?,
            };
            prev = Some(seq7);
            let ev = Self::read_one(r, classes)?;
            let mut seq = seq7 | (self.next_receive & !0x7f);
            if seq < self.next_receive {
                seq += 128;
            }
            self.waiting.entry(seq).or_insert(ev);
        }
        while let Some(ev) = self.waiting.remove(&self.next_receive) {
            self.next_receive += 1;
            out.push(ev);
        }
        for ev in &out {
            if let Event::NetString { id, text } = ev {
                self.tags.insert(*id, text.clone());
            }
        }
        Ok(out)
    }

    fn read_one(r: &mut BitReader<'_>, classes: &ClassTable) -> Result<Event, EventError> {
        let class_id = r.int(12)?;
        let len = r.int(LENGTH_BITS)? as usize;
        let end = r.position() + len;
        let ev = Event::read(class_id, classes.event_name(class_id), r)?;
        if r.position() > end {
            return Err(EventError::Overrun { class_id });
        }
        r.seek(end)?;
        Ok(ev)
    }

    /// The command an event carries, with tagged strings looked up.
    pub fn command(&self, ev: &Event) -> Option<Command> {
        let Event::RemoteCommand(args) = ev else {
            return None;
        };
        let name = match args.first()? {
            Arg::Text(t) => t.clone(),
            Arg::Tag(id) => self.tags.get(id)?.clone(),
            _ => return None,
        };
        Some(Command {
            name,
            args: args[1..].to_vec(),
        })
    }

    /// Queues a guaranteed event to send. Returns its sequence.
    pub fn send(&mut self, ev: Event) -> u32 {
        let seq = self.next_send;
        self.to_send.push_back((seq, ev));
        self.next_send += 1;
        seq
    }

    /// Whether the client has guaranteed event `seq`.
    pub fn delivered(&self, seq: u32) -> bool {
        seq < self.acked_below || self.delivered.contains_key(&seq)
    }

    /// Queues `commandToServer(name, args...)` with the name as plain text.
    pub fn send_command(&mut self, name: &[u8], args: &[Arg]) {
        let mut all = vec![Arg::Text(name.to_vec())];
        all.extend_from_slice(args);
        self.send(Event::RemoteCommand(all));
    }

    /// Whether anything is waiting to go out.
    pub fn has_pending(&self) -> bool {
        !self.to_send.is_empty()
    }

    /// Writes both event lists into data packet `packet` (no
    /// unguaranteed events; queued guaranteed ones while the event
    /// window and `max_bits` allow). An event whose class the server did
    /// not list stays queued.
    pub fn write(&mut self, w: &mut BitWriter, packet: u32, classes: &ClassTable, max_bits: usize) {
        self.write_with(w, packet, classes, max_bits, None);
    }

    /// [`Events::write`] with one unguaranteed entry (its class, int14
    /// length and payload, already written): a lab mutation's.
    pub fn write_with(
        &mut self,
        w: &mut BitWriter,
        packet: u32,
        classes: &ClassTable,
        max_bits: usize,
        unguaranteed: Option<&BitWriter>,
    ) {
        if let Some(u) = unguaranteed {
            w.flag(true);
            w.append(u);
        }
        w.flag(false); // no (more) unguaranteed events
        let mut prev: Option<u32> = None;
        let mut sent = Vec::new();
        while let Some((seq, ev)) = self.to_send.front() {
            if *seq > self.acked_below + EVENT_WINDOW - 1 {
                break;
            }
            let Some(id) = ev.class_name().and_then(|n| classes.event_id(n)) else {
                break;
            };
            let mut payload = BitWriter::new();
            ev.write(&mut payload);
            let seq_bits = if prev.map(|p| p + 1) == Some(*seq) {
                0
            } else {
                7
            };
            let bits = 1 + 1 + seq_bits + 12 + LENGTH_BITS as usize + payload.bit_len() + 1;
            if w.bit_len() + bits > max_bits && !sent.is_empty() {
                break;
            }
            w.flag(true);
            if !w.flag(seq_bits == 0) {
                w.int(seq & 0x7f, 7);
            }
            w.int(id, 12);
            w.int(payload.bit_len() as u32, LENGTH_BITS);
            w.append(&payload);
            prev = Some(*seq);
            sent.push(self.to_send.pop_front().expect("front exists"));
        }
        w.flag(false);
        if !sent.is_empty() {
            self.in_flight.insert(packet, sent);
        }
    }

    /// Takes the link's report on one of our data packets: delivered
    /// events are done, lost ones go back in the queue in order.
    pub fn notify(&mut self, n: Notify) {
        let Some(events) = self.in_flight.remove(&n.sequence) else {
            return;
        };
        if n.delivered {
            for (seq, _) in events {
                self.delivered.insert(seq, ());
            }
            while self.delivered.remove(&self.acked_below).is_some() {
                self.acked_below += 1;
            }
        } else {
            for (seq, ev) in events.into_iter().rev() {
                let at = self
                    .to_send
                    .iter()
                    .position(|(s, _)| *s > seq)
                    .unwrap_or(self.to_send.len());
                self.to_send.insert(at, (seq, ev));
            }
        }
    }
}

/// The client's answer to a mission start command (protocol 24).
pub fn mission_start_answer(command: &[u8]) -> Option<&'static [u8]> {
    match command {
        b"MissionStartPhase1" => Some(b"MissionStartPhase1Ack"),
        b"MissionStartPhase2" => Some(b"MissionStartPhase2Ack"),
        _ => None,
    }
}

/// One finished Graal text transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigData {
    /// The weapon whose `onReceiveText` gets it; empty for the client itself.
    pub weapon: Vec<u8>,
    /// What to do with it: `set`, `unset`, `events`, `addweapon`, ...
    pub kind: Vec<u8>,
    pub option: Vec<u8>,
    pub text: Vec<u8>,
}

/// Puts text transfers back together from their start and chunk events.
#[derive(Debug, Default)]
pub struct BigDataAssembler {
    open: HashMap<u32, (BigData, usize, u32)>,
}

impl BigDataAssembler {
    /// Feeds one event. Returns a transfer once all of it has arrived.
    /// Compressed transfers (zlib, bzip2) come back as `Err` with their
    /// packed text and the compression (`graal_text::unpack` reads them).
    pub fn feed(&mut self, ev: &Event) -> Option<Result<BigData, (BigData, u32)>> {
        match ev {
            Event::BigDataStart {
                transfer,
                weapon,
                kind,
                option,
                total,
                compression,
            } => {
                let d = BigData {
                    weapon: weapon.clone(),
                    kind: kind.clone(),
                    option: option.clone(),
                    text: Vec::new(),
                };
                self.open
                    .insert(*transfer, (d, *total as usize, *compression));
                self.finish(*transfer)
            }
            Event::BigDataChunk { transfer, bytes } => {
                self.open.get_mut(transfer)?.0.text.extend_from_slice(bytes);
                self.finish(*transfer)
            }
            _ => None,
        }
    }

    fn finish(&mut self, transfer: u32) -> Option<Result<BigData, (BigData, u32)>> {
        let (d, total, _) = self.open.get(&transfer)?;
        if d.text.len() < *total {
            return None;
        }
        let (d, _, compression) = self.open.remove(&transfer)?;
        Some(if compression == 0 {
            Ok(d)
        } else {
            Err((d, compression))
        })
    }
}

/// Splits `text` into the start and chunk events of one transfer
/// (uncompressed, chunks of up to 191 bytes).
pub fn big_data_events(
    transfer: u32,
    weapon: &[u8],
    kind: &[u8],
    option: &[u8],
    text: &[u8],
) -> Vec<Event> {
    let mut out = vec![Event::BigDataStart {
        transfer,
        weapon: weapon.to_vec(),
        kind: kind.to_vec(),
        option: option.to_vec(),
        total: text.len() as u32,
        compression: 0,
    }];
    out.extend(text.chunks(191).map(|c| Event::BigDataChunk {
        transfer,
        bytes: c.to_vec(),
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::super::link::Link;
    use super::*;

    /// The protocol-24 classes in the order the Seihai gserver lists them.
    fn classes() -> ClassTable {
        let objects = [
            "Camera",
            "DynamicGrass",
            "DynamicGrassReplicator",
            "DynamicShapeReplicator",
            "GameObject",
            "Sky",
            "Sun",
            "TerrainBlock",
            "WaterBlock",
            "fxSunLight",
        ];
        let events = [
            "BigDataChunkEvent",
            "BigDataStartEvent",
            "ConnectionMessageEvent",
            "GhostAlwaysObjectEvent",
            "NetStringEvent",
            "RemoteCommandEvent",
            "RequestReloadEvent",
            "TerrainChunkEvent",
            "TerrainRequestUpdatesEvent",
        ];
        let mut e: Vec<ClassEntry> = objects
            .iter()
            .enumerate()
            .map(|(i, n)| ClassEntry {
                class_type: 0,
                server_id: i as u32,
                name: n.as_bytes().to_vec(),
            })
            .collect();
        e.extend(events.iter().enumerate().map(|(i, n)| ClassEntry {
            class_type: 1,
            server_id: i as u32,
            name: n.as_bytes().to_vec(),
        }));
        ClassTable::new(&e, 1)
    }

    /// Writes one packet's events from `from` and reads them in `to`.
    fn carry(from: &mut Events, to: &mut Events, packet: u32) -> Vec<Event> {
        let t = classes();
        let mut w = BitWriter::new();
        from.write(&mut w, packet, &t, 450 * 8);
        let bytes = w.into_bytes();
        to.read(&mut BitReader::new(&bytes), &t).unwrap()
    }

    #[test]
    fn a_tagged_mission_start_is_answered() {
        // The server's way: define the name as tag 3, then send the
        // command with argument 0 as that tag.
        let (mut server, mut client) = (Events::new(), Events::new());
        server.send(Event::NetString {
            id: 3,
            text: b"MissionStartPhase1".to_vec(),
        });
        server.send(Event::RemoteCommand(vec![
            Arg::Tag(3),
            Arg::Int(-5),
            Arg::Int(70000),
            Arg::Empty,
        ]));
        let got = carry(&mut server, &mut client, 1);
        assert_eq!(got.len(), 2);
        let cmd = client.command(&got[1]).unwrap();
        assert_eq!(cmd.name, b"MissionStartPhase1");
        assert_eq!(cmd.args, [Arg::Int(-5), Arg::Int(70000), Arg::Empty]);
        assert_eq!(
            mission_start_answer(&cmd.name),
            Some(&b"MissionStartPhase1Ack"[..])
        );

        client.send_command(b"MissionStartPhase1Ack", &[]);
        let got = carry(&mut client, &mut server, 1);
        assert_eq!(
            server.command(&got[0]).unwrap().name,
            b"MissionStartPhase1Ack"
        );
        assert_eq!(mission_start_answer(b"setFlag"), None);
    }

    #[test]
    fn lost_events_go_out_again_in_order() {
        let (mut client, mut server) = (Link::new(0x400), Link::new(0x400));
        let (mut out, mut inn) = (Events::new(), Events::new());
        let t = classes();
        for i in 0..3 {
            out.send_command(format!("c{i}").as_bytes(), &[]);
        }
        // Packet 1 carries all three and is lost.
        let mut w = client.data(None, None).unwrap();
        out.write(&mut w, client.last_sent(), &t, 450 * 8);
        out.send_command(b"c3", &[]);
        // Packet 2 carries c3 only and arrives: it waits for 0..2.
        let mut w = client.data(None, None).unwrap();
        out.write(&mut w, client.last_sent(), &t, 450 * 8);
        let p2 = w.into_bytes();
        let mut got = server.receive(&p2).unwrap().data.unwrap();
        assert!(inn.read(&mut got.body, &t).unwrap().is_empty(), "c3 waits");
        // The server's reply reports 1 lost, 2 delivered.
        let reply = server.data(None, None).unwrap().into_bytes();
        for n in client.receive(&reply).unwrap().notifies {
            out.notify(n);
        }
        // Packet 3 resends 0..2 and they come out with c3 after them.
        let mut w = client.data(None, None).unwrap();
        out.write(&mut w, client.last_sent(), &t, 450 * 8);
        let p3 = w.into_bytes();
        let mut got = server.receive(&p3).unwrap().data.unwrap();
        let names: Vec<_> = inn
            .read(&mut got.body, &t)
            .unwrap()
            .iter()
            .map(|e| inn.command(e).unwrap().name)
            .collect();
        assert_eq!(
            names,
            [
                b"c0".to_vec(),
                b"c1".to_vec(),
                b"c2".to_vec(),
                b"c3".to_vec()
            ]
        );
        let _ = Notify {
            sequence: 0,
            delivered: true,
        };
    }

    #[test]
    fn unknown_classes_are_skipped_by_length() {
        let t = classes();
        let mut w = BitWriter::new();
        w.flag(true); // one unguaranteed event
        w.int(6, 12); // RequestReloadEvent: not read by this client
        w.int(13, LENGTH_BITS);
        w.int(0x1abc, 13);
        w.flag(false);
        w.flag(true); // one guaranteed event, sequence 0 given
        w.flag(false);
        w.int(0, 7);
        w.int(4, 12); // NetStringEvent
        let mut p = BitWriter::new();
        p.int(1, 5);
        p.pstring_alone(b"x");
        w.int(p.bit_len() as u32, LENGTH_BITS);
        w.append(&p);
        w.flag(false);
        let bytes = w.into_bytes();
        let got = Events::new().read(&mut BitReader::new(&bytes), &t).unwrap();
        assert_eq!(
            got[0],
            Event::Other {
                class_id: 6,
                name: Some("RequestReloadEvent".into())
            }
        );
        assert_eq!(
            got[1],
            Event::NetString {
                id: 1,
                text: b"x".to_vec()
            }
        );
    }

    #[test]
    fn big_data_text_comes_back_whole() {
        let text: Vec<u8> = (0..500).map(|i| b'a' + (i % 26) as u8).collect();
        let (mut server, mut client) = (Events::new(), Events::new());
        for ev in big_data_events(7, b"", b"events", b"", &text) {
            server.send(ev);
        }
        // More than one 450-byte packet's worth: it takes two.
        let mut got = carry(&mut server, &mut client, 1);
        assert!(server.has_pending());
        got.extend(carry(&mut server, &mut client, 2));
        assert_eq!(got.len(), 4, "start and three chunks");
        let mut asm = BigDataAssembler::default();
        let done: Vec<_> = got.iter().filter_map(|e| asm.feed(e)).collect();
        assert_eq!(
            done,
            [Ok(BigData {
                weapon: vec![],
                kind: b"events".to_vec(),
                option: vec![],
                text
            })]
        );
    }

    #[test]
    fn the_event_window_holds_back_past_126() {
        let mut out = Events::new();
        for _ in 0..130 {
            out.send(Event::ConnectionMessage {
                sequence: 0,
                message: 0,
                ghost_count: 0,
            });
        }
        let t = classes();
        let mut w = BitWriter::new();
        out.write(&mut w, 1, &t, usize::MAX);
        assert_eq!(out.to_send.len(), 4);
        out.notify(Notify {
            sequence: 1,
            delivered: true,
        });
        let mut w = BitWriter::new();
        out.write(&mut w, 2, &t, usize::MAX);
        assert!(!out.has_pending());
    }
}
