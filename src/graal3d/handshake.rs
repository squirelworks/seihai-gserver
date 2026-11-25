//! The out-of-band handshake, server side, and the Torque profile of each
//! client line.
//!
//! The client sends a challenge request, the server answers with a digest
//! of the client's address, the client echoes it in a connect request,
//! and the server accepts with Graal's class table (or rejects). Every
//! packet of the link repeats the connect sequence the client picked.
//! Layouts: Graal_analysis `torque/udp-session.md` §3; the client half is
//! Ato's `torque::net::handshake`.

use super::bits::{BitReader, BitWriter};
use super::events::ClassTable;

/// Out-of-band packet types (the first byte; its low bit is 0).
pub mod kind {
    pub const CHALLENGE_REQUEST: u8 = 0x1a;
    pub const CHALLENGE_REJECT: u8 = 0x1c;
    pub const CHALLENGE_RESPONSE: u8 = 0x1e;
    pub const CONNECT_REQUEST: u8 = 0x20;
    pub const CONNECT_REJECT: u8 = 0x22;
    pub const CONNECT_ACCEPT: u8 = 0x24;
    pub const DISCONNECT: u8 = 0x26;
}

/// The network class every Graal3D client connects as.
pub const CONNECTION_CLASS: &[u8] = b"GameConnection";

/// The game string every Graal3D client sends from 3.153 on.
pub const GAME: &[u8] = b"Graal3D";

/// The most connect arguments a request may carry.
pub const MAX_ARGS: u32 = 16;

/// Whether a UDP packet is a data packet (first bit 1) rather than out-of-band.
pub fn is_data_packet(packet: &[u8]) -> bool {
    packet.first().is_some_and(|b| b & 1 == 1)
}

/// What one client line expects from the server.
#[derive(Debug)]
pub struct Profile {
    /// The accept must carry exactly this number.
    pub protocol: u32,
    /// Which builds speak it, for logs.
    pub clients: &'static str,
    /// The stock TGE game layer (3.153, 4.04r2): datablocks, moves and
    /// three mission phases. Otherwise the GameObject era (4.20r6, 5.x).
    pub tge: bool,
    /// Ghosted classes (class type 0), in the server's numbering.
    pub objects: &'static [&'static str],
    /// Datablock classes (type 1, protocol 19 only).
    pub datablocks: &'static [&'static str],
    /// Event classes (type 2 with datablocks, else 1).
    pub events: &'static [&'static str],
    /// The Sharp3D route (`graal3d_route = sharp3d`): the world goes out
    /// as GhostAlwaysObjectEvents, there are no mission start commands,
    /// and a Camera is the control object.
    pub ghost_always: bool,
}

impl Profile {
    /// The class type of events in the accept's table.
    pub fn event_type(&self) -> u32 {
        if self.tge {
            2
        } else {
            1
        }
    }

    /// Mission start phases the server drives.
    pub fn mission_phases(&self) -> u32 {
        if self.tge {
            3
        } else {
            2
        }
    }

    pub fn object_id(&self, name: &str) -> Option<u32> {
        self.objects
            .iter()
            .position(|n| *n == name)
            .map(|i| i as u32)
    }

    pub fn datablock_id(&self, name: &str) -> Option<u32> {
        self.datablocks
            .iter()
            .position(|n| *n == name)
            .map(|i| i as u32)
    }

    /// The accept's class table: objects, then datablocks, then events,
    /// each numbered from 0 in its own type.
    pub fn class_entries(&self) -> Vec<ClassEntry> {
        let mut out = Vec::new();
        let mut list = |class_type: u32, names: &[&str]| {
            for (i, n) in names.iter().enumerate() {
                out.push(ClassEntry {
                    class_type,
                    server_id: i as u32,
                    name: n.as_bytes().to_vec(),
                });
            }
        };
        list(0, self.objects);
        if self.tge {
            list(1, self.datablocks);
        }
        list(self.event_type(), self.events);
        out
    }

    /// The same table, looked up by name and id.
    pub fn class_table(&self) -> ClassTable {
        ClassTable::new(&self.class_entries(), self.event_type())
    }
}

/// 3.153 and 4.04r2. The server ghosts a Sky and one stock TGE Player or
/// Camera per client, with a PlayerData or CameraData datablock.
pub const PROTOCOL_19: Profile = Profile {
    protocol: 19,
    clients: "3.153, 4.04r2",
    tge: true,
    objects: &["Camera", "Player", "Sky"],
    datablocks: &["CameraData", "PlayerData"],
    events: &[
        "BigDataChunkEvent",
        "BigDataStartEvent",
        "ConnectionMessageEvent",
        "GhostAlwaysObjectEvent",
        "LightningStrikeEvent",
        "NetStringEvent",
        "PathManagerEvent",
        "RemoteCommandEvent",
        "RequestReloadEvent",
        "Sim2DAudioEvent",
        "Sim3DAudioEvent",
        "SimDataBlockEvent",
        "SimpleMessageEvent",
        "TerrainChunkEvent",
        "TerrainRequestUpdatesEvent",
    ],
    ghost_always: false,
};

/// 4.20r6.
pub const PROTOCOL_21: Profile = Profile {
    protocol: 21,
    clients: "4.20r6",
    tge: false,
    objects: &[
        "Camera",
        "DynamicGrass",
        "DynamicGrassReplicator",
        "DynamicShapeReplicator",
        "GameObject",
        "SimpleNetObject",
        "Sky",
        "Sun",
        "TerrainBlock",
        "WaterBlock",
        "fxSunLight",
        "twSurfaceReference",
    ],
    datablocks: &[],
    events: &[
        "BigDataChunkEvent",
        "BigDataStartEvent",
        "ConnectionMessageEvent",
        "GhostAlwaysObjectEvent",
        "NetStringEvent",
        "RemoteCommandEvent",
        "RequestReloadEvent",
        "SimpleMessageEvent",
        "TerrainChunkEvent",
        "TerrainRequestUpdatesEvent",
    ],
    ghost_always: false,
};

/// 4.20r8 and 5.x (5.00r7, 5.12): the line run end to end in the labs.
pub const PROTOCOL_24: Profile = Profile {
    protocol: 24,
    clients: "4.20r8, 5.x",
    tge: false,
    objects: &[
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
    ],
    datablocks: &[],
    events: &[
        "BigDataChunkEvent",
        "BigDataStartEvent",
        "ConnectionMessageEvent",
        "GhostAlwaysObjectEvent",
        "NetStringEvent",
        "RemoteCommandEvent",
        "RequestReloadEvent",
        "TerrainChunkEvent",
        "TerrainRequestUpdatesEvent",
    ],
    ghost_always: false,
};

/// The Sharp3D route for protocol 24: the class table of a server a real
/// 5.12 joined and drew (a capture of 2026-11-24), in its id order.
pub const PROTOCOL_24_SHARP3D: Profile = Profile {
    protocol: 24,
    clients: "5.x, Sharp3D route",
    tge: false,
    objects: &["Sun", "GameObject", "Camera", "Sky"],
    datablocks: &[],
    events: &[
        "BigDataStartEvent",
        "BigDataChunkEvent",
        "ConnectionMessageEvent",
        "GhostAlwaysObjectEvent",
    ],
    ghost_always: true,
};

/// The Sharp3D route's profile for a protocol (24 only).
pub fn sharp3d_profile(protocol: u32) -> Option<&'static Profile> {
    (protocol == 24).then_some(&PROTOCOL_24_SHARP3D)
}

pub fn profile(protocol: u32) -> Option<&'static Profile> {
    [&PROTOCOL_19, &PROTOCOL_21, &PROTOCOL_24]
        .into_iter()
        .find(|p| p.protocol == protocol)
}

/// One entry of Graal's class table in the connect accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassEntry {
    pub class_type: u32,
    pub server_id: u32,
    pub name: Vec<u8>,
}

pub type Digest = [u32; 4];

/// A client's connect request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub sequence: u32,
    pub digest: Digest,
    pub class_name: Vec<u8>,
    pub class_group: u32,
    pub class_crc: u32,
    pub game: Vec<u8>,
    pub current_protocol: u32,
    pub minimum_protocol: u32,
    /// The account password, in clear (4.04r2, 5.x). Not checked: the
    /// link is only opened for an account already logged in over TCP.
    pub password: Vec<u8>,
    /// 4.04r2 and 5.x: the account, then the nickname (may be empty).
    pub args: Vec<Vec<u8>>,
}

/// The connect sequence of a challenge request.
pub fn read_challenge_request(packet: &[u8]) -> Option<u32> {
    if packet.first() != Some(&kind::CHALLENGE_REQUEST) {
        return None;
    }
    let mut r = BitReader::new(packet);
    r.u8().ok()?;
    r.u32().ok()
}

impl ConnectRequest {
    pub fn read(packet: &[u8]) -> Option<ConnectRequest> {
        if packet.first() != Some(&kind::CONNECT_REQUEST) {
            return None;
        }
        let mut r = BitReader::new(packet);
        r.u8().ok()?;
        let mut read = || -> Result<ConnectRequest, super::bits::OutOfBits> {
            let sequence = r.u32()?;
            let mut digest = [0; 4];
            for d in &mut digest {
                *d = r.u32()?;
            }
            let class_name = r.string()?;
            let class_group = r.u32()?;
            let class_crc = r.u32()?;
            let game = r.string()?;
            let current_protocol = r.u32()?;
            let minimum_protocol = r.u32()?;
            let password = r.string()?;
            let argc = r.u32()?;
            let mut args = Vec::new();
            if argc <= MAX_ARGS {
                for _ in 0..argc {
                    args.push(r.string()?);
                }
            }
            Ok(ConnectRequest {
                sequence,
                digest,
                class_name,
                class_group,
                class_crc,
                game,
                current_protocol,
                minimum_protocol,
                password,
                args,
            })
        };
        let req = read().ok()?;
        Some(req)
    }

    /// The account the client asks for (argument 0).
    pub fn account(&self) -> &[u8] {
        self.args.first().map_or(&[][..], Vec::as_slice)
    }

    /// Why the request is refused, or `None` if it may go on. The texts
    /// are the client's own string ids, so the player sees a known message.
    pub fn check(&self) -> Option<&'static str> {
        if self.class_name != CONNECTION_CLASS {
            return Some("CHR_INVALID");
        }
        if self.game != GAME {
            return Some("CHR_GAME");
        }
        if profile(self.current_protocol).is_none() || self.minimum_protocol > self.current_protocol
        {
            return Some("CHR_PROTOCOL");
        }
        if self.account().is_empty() {
            return Some("CR_INVALID_ARGS");
        }
        None
    }
}

pub fn challenge_response(sequence: u32, digest: &Digest) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.u8(kind::CHALLENGE_RESPONSE);
    w.u32(sequence);
    for d in digest {
        w.u32(*d);
    }
    w.into_bytes()
}

/// The accept, with the class table Graal inserts before the protocol.
pub fn connect_accept(sequence: u32, profile: &Profile) -> Vec<u8> {
    connect_accept_tweaked(sequence, profile, &super::mutate::AcceptTweak::default())
}

/// The accept with a lab mutation's changes (`graal3d_mutate`).
pub fn connect_accept_tweaked(
    sequence: u32,
    profile: &Profile,
    tweak: &super::mutate::AcceptTweak,
) -> Vec<u8> {
    let mut entries = profile.class_entries();
    let ev = profile.event_type();
    for _ in 0..tweak.drop_events {
        if let Some(i) = entries.iter().rposition(|e| e.class_type == ev) {
            entries.remove(i);
        }
    }
    for name in &tweak.extra_events {
        let id = entries.iter().filter(|e| e.class_type == ev).count() as u32;
        entries.push(ClassEntry {
            class_type: ev,
            server_id: id,
            name: name.as_bytes().to_vec(),
        });
    }
    if let Some((i, name)) = tweak.rename_object {
        if let Some(e) = entries.iter_mut().filter(|e| e.class_type == 0).nth(i) {
            e.name = name.as_bytes().to_vec();
        }
    }
    let mut w = BitWriter::new();
    w.u8(kind::CONNECT_ACCEPT);
    w.u32(sequence);
    for e in entries {
        w.flag(true);
        w.int(e.class_type, 12);
        w.int(e.server_id, 12);
        w.string(&e.name);
    }
    w.flag(false);
    w.u32(tweak.protocol.unwrap_or(profile.protocol));
    w.into_bytes()
}

pub fn connect_reject(sequence: u32, reason: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.u8(kind::CONNECT_REJECT);
    w.u32(sequence);
    w.string(reason);
    w.into_bytes()
}

pub fn disconnect(sequence: u32, reason: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.u8(kind::DISCONNECT);
    w.u32(sequence);
    w.string(reason);
    w.into_bytes()
}

/// The connect sequence of a disconnect packet.
pub fn read_disconnect(packet: &[u8]) -> Option<u32> {
    if packet.first() != Some(&kind::DISCONNECT) {
        return None;
    }
    let mut r = BitReader::new(packet);
    r.u8().ok()?;
    r.u32().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The real 5.12 client's connect request (udp-session.md §3).
    const REQUEST_512: &str = "2000040000000102030405060708090a0b0c0d0e0f1d6213fd6a4af4185809000000e0ffffffff41ac650e47670c0000000c00008008d1e976dc8548169b000000c082de7f0300";

    #[test]
    fn reads_the_captured_challenge_request() {
        assert_eq!(read_challenge_request(&[0x1a, 0, 4, 0, 0]), Some(0x400));
        assert_eq!(read_challenge_request(&[0x1a, 0, 4]), None);
    }

    #[test]
    fn answers_the_challenge_as_captured() {
        let digest = [0x0302_0100, 0x0706_0504, 0x0b0a_0908, 0x0f0e_0d0c];
        let mut want = vec![0x1e, 0x00, 0x04, 0x00, 0x00];
        want.extend(0u8..16);
        assert_eq!(challenge_response(0x400, &digest), want);
    }

    #[test]
    fn reads_the_real_512_connect_request() {
        let req = ConnectRequest::read(&hex(REQUEST_512)).unwrap();
        assert_eq!(req.sequence, 0x400);
        assert_eq!(
            req.digest,
            [0x0302_0100, 0x0706_0504, 0x0b0a_0908, 0x0f0e_0d0c]
        );
        assert_eq!(req.class_name, b"GameConnection");
        assert_eq!(req.class_crc, 0xffff_ffff);
        assert_eq!(req.game, b"Graal3D");
        assert_eq!((req.current_protocol, req.minimum_protocol), (24, 24));
        assert_eq!(req.args, vec![b"guest".to_vec(), Vec::new()]);
        assert_eq!(req.check(), None);
    }

    #[test]
    fn refuses_with_the_clients_own_reasons() {
        let mut req = ConnectRequest::read(&hex(REQUEST_512)).unwrap();
        req.game = b"Torque Game Engine Demo".to_vec();
        assert_eq!(req.check(), Some("CHR_GAME"));
        let mut req = ConnectRequest::read(&hex(REQUEST_512)).unwrap();
        req.current_protocol = 9;
        assert_eq!(req.check(), Some("CHR_PROTOCOL"));
        let mut req = ConnectRequest::read(&hex(REQUEST_512)).unwrap();
        req.args.clear();
        assert_eq!(req.check(), Some("CR_INVALID_ARGS"));
    }

    #[test]
    fn the_full_protocol_24_accept_matches_the_note() {
        // 276 bytes, 19 classes, sequence 0x400 (udp-session.md §3).
        let want = hex(
            "24000400000100001aac96e82d0340006470ae5022c6b196ad05000168e15ca1448c632d5bebfa7a304e500b0003d0c2b94289184fe7acbfebebc138
             412d00104085d844bfe26ef1180240011c984e2a01c0000e4c3d058003c8b07eb3648a188a270100012a3838e12d62289e048004a8b039997a92e77e
             860100002322b8774ec8d5664f93b8f404032000464470ef9c90534883b8f4040340005aaca6448f81955cdf5af61f979e60000c408bd8591b748e22
             2b6dc5dde231c4a527180004d0c1ed610ac6d4c7a52718000550a2eb91c2afa6082585b8f40403c0004a747d64f3dfa0eb8766884b4f30000e60c4fa
             cd9269b5d9d3242e3dc10040801aeb374ba6ae8f6cfe1bc8b10e09dfe2d213c000000000",
        );
        let got = connect_accept(0x400, &PROTOCOL_24);
        assert_eq!(got.len(), 276);
        assert_eq!(got, want);
    }

    #[test]
    fn an_empty_table_accept_is_the_captured_one() {
        static EMPTY: Profile = Profile {
            protocol: 24,
            clients: "",
            tge: false,
            objects: &[],
            datablocks: &[],
            events: &[],
            ghost_always: false,
        };
        assert_eq!(connect_accept(0x400, &EMPTY), hex("24000400003000000000"));
    }

    #[test]
    fn protocol_19_numbers_three_class_types() {
        let entries = PROTOCOL_19.class_entries();
        assert_eq!(entries[0].class_type, 0);
        assert!(entries
            .iter()
            .any(|e| e.class_type == 1 && e.name == b"PlayerData"));
        assert!(entries
            .iter()
            .any(|e| e.class_type == 2 && e.name == b"SimDataBlockEvent"));
        let t = PROTOCOL_19.class_table();
        assert_eq!(t.event_id("NetStringEvent"), Some(5));
        assert_eq!(t.datablock_name(1), Some("PlayerData"));
    }

    #[test]
    fn disconnects_round_trip() {
        let d = disconnect(0x400, b"bye");
        assert_eq!(read_disconnect(&d), Some(0x400));
        assert!(!is_data_packet(&d));
    }
}
