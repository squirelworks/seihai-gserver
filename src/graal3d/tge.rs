//! The protocol 19 game layer (3.153 and 4.04r2): the stock Torque Game
//! Engine layer with Graal's changes, as far as a server needs it to take a
//! client through the mission start to a control object that walks.
//!
//! Layouts: Graal_analysis `torque/protocol19-server.md` and
//! `torque/udp-session.md` §10 (static reads of the 4.04r2 engine, written
//! up as behaviour), and the open (MIT) Torque net layer. The 3.153 layouts
//! are taken to be 4.04r2's (same Torque line, same net classes).
//!
//! What a protocol 19 client gets from this server:
//!
//! - one datablock: a PlayerData (or, with `tgecontrol camera` in the
//!   world file, a CameraData) with stock TGE 1.x values;
//! - a Sky: 4.04r2 reads the scene's Sky on its first 3D frame without a
//!   null check, so a world without one crashes it;
//! - a Player (or Camera) ghost that uses the datablock, which becomes its
//!   control object. The client's moves walk the Player ([`apply_move`]),
//!   and its packet data goes out again after each change.

use super::bits::{BitReader, BitWriter, OutOfBits};
use std::f32::consts::TAU;

type R<T> = Result<T, OutOfBits>;

/// Datablock ids start at 3; the wire carries id - 3 in 12 bits.
pub const FIRST_DATABLOCK_ID: u32 = 3;
pub const DATABLOCK_ID_BITS: u32 = 12;
/// This server's two datablocks. Only the one the control object uses goes out.
pub const CAMERA_DATABLOCK_ID: u32 = FIRST_DATABLOCK_ID;
pub const PLAYER_DATABLOCK_ID: u32 = FIRST_DATABLOCK_ID + 1;

/// `ConnectionMessageEvent` messages, numbered as TGE 1.x does (4.04r2):
/// the five NetConnection messages, then the datablock pair at 5 and 6.
pub mod message {
    /// Server: the ghost-always objects are out.
    pub const GHOST_ALWAYS_DONE: u32 = 0;
    /// Client: the answer to 0.
    pub const READY_FOR_NORMAL_GHOSTS: u32 = 1;
    pub const END_GHOSTING: u32 = 2;
    /// Server: the count is the ghosts to come.
    pub const GHOST_ALWAYS_STARTING: u32 = 3;
    pub const SEND_NEXT_DOWNLOAD_REQUEST: u32 = 4;
    /// Server: every datablock is out.
    pub const DATABLOCKS_DONE: u32 = 5;
    /// Client: they are loaded.
    pub const DATABLOCKS_DOWNLOAD_DONE: u32 = 6;
}

/// The PlayerData values the server's walk uses.
pub const PLAYER_MAX_SPEED: f32 = 10.0;
pub const PLAYER_MAX_LOOK_ANGLE: f32 = 1.4;
pub const PLAYER_MIN_LOOK_ANGLE: f32 = -1.4;
pub const PLAYER_MAX_FREELOOK_ANGLE: f32 = 3.0;
/// One move.
pub const TICK_SECONDS: f32 = 0.032;

/// `SimDataBlockEvent`: int12 (id - 3), int12 datablock class, int12
/// index, int13 total, then the datablock's data behind an int14 length.
pub fn sim_datablock_event(
    id: u32,
    class_id: u32,
    index: u32,
    total: u32,
    data: &BitWriter,
) -> BitWriter {
    let mut w = BitWriter::new();
    w.int(id - FIRST_DATABLOCK_ID, DATABLOCK_ID_BITS);
    w.int(class_id, 12);
    w.int(index, 12);
    w.int(total, 13);
    w.int(data.bit_len() as u32, 14);
    w.append(data);
    w
}

/// SimDataBlock, GameBaseData and ShapeBaseData: how CameraData and
/// PlayerData both start. Every optional field is left at its default.
fn shape_base_data(w: &mut BitWriter, name: &[u8], shape: &[u8]) {
    w.pstring_alone(name);
    w.pstring_alone(b""); // GameBaseData: category
    w.pstring_alone(b""); // and class name
    w.flag(false); // shape CRC check off
    w.pstring_alone(shape); // the shape file; none for a Camera
    for _ in 0..9 {
        w.flag(false); // mass, drag, density, max energy, camera distances, fov
    }
    w.pstring_alone(b""); // cloak texture
    w.flag(false);
    w.flag(false); // debris datablock
    w.flag(false);
    w.flag(false);
    w.flag(false);
    w.flag(false); // explosion datablock
    w.flag(false); // underwater explosion datablock
    w.flag(false);
    w.flag(false); // mounted images: none
    w.flag(false);
    w.flag(false);
    w.u32(0);
    w.flag(false);
    w.f32(0.0); // count of the per-sequence lists that follow: none
    w.flag(false);
}

/// A CameraData: ShapeBaseData with nothing of its own, no shape.
pub fn camera_data(name: &[u8]) -> BitWriter {
    let mut w = BitWriter::new();
    shape_base_data(&mut w, name, b"");
    w
}

/// A PlayerData with `shape` and the stock 1.x values. The shape must be
/// one the client can load: 4.04r2 drops the link on a Player without a
/// shape instance ("Invalid packet GameBase::unpackUpdate()", lab
/// 2026-11-19).
pub fn player_data(name: &[u8], shape: &[u8]) -> BitWriter {
    let mut w = BitWriter::new();
    shape_base_data(&mut w, name, shape);
    w.flag(true); // render first person
    w.flag(false); // Graal's md3 model name: none
    for v in [
        PLAYER_MIN_LOOK_ANGLE,
        PLAYER_MAX_LOOK_ANGLE,
        PLAYER_MAX_FREELOOK_ANGLE,
        1.5,   // max time scale
        1.0,   // max step height
        360.0, // run force
        PLAYER_MAX_SPEED,
        PLAYER_MAX_SPEED,
        PLAYER_MAX_SPEED,
        PLAYER_MAX_SPEED,
        PLAYER_MAX_SPEED,
        PLAYER_MAX_SPEED,
        80.0, // run surface angle
    ] {
        w.f32(v);
    }
    w.u32(30); // recover delay (ticks)
    for v in [1.0, 75.0, 500.0, 1000.0, 78.0] {
        // recover run force scale, jump force, min and max jump speed,
        // jump surface angle
        w.f32(v);
    }
    w.int(30, 7); // jump delay
    for v in [
        75.0, // swim force
        80.0, 38.0, 1.0, // horizontal max speed, resist speed, factor
        80.0, 38.0, 1.0, // up max speed, resist speed, factor
        1.0, 45.0, 300.0, 0.25, // splash velocity, angle, freq mod, vel epsilon
        0.4,  // bubble emit time
        2.0, 3.0, 2.0,  // medium, hard, exit splash sound velocity
        0.1,  // footstep splash height
        25.0, // min impact speed
    ] {
        w.f32(v);
    }
    for _ in 0..19 {
        w.flag(false); // sounds
    }
    for v in [1.0, 1.0, 2.3] {
        w.f32(v); // bounding box
    }
    w.flag(false); // foot puff emitter
    w.u32(15); // foot puff parts
    w.f32(0.25); // foot puff radius
    w.flag(false); // decal
    w.f32(0.0); // decal offset
    w.flag(false); // dust emitter
    w.flag(false); // splash
    for _ in 0..3 {
        w.flag(false); // splash emitters
    }
    for v in [10.0, 10.0, 10.0, 10.0, 20.0, 20.0, 20.0, 1.0, 10.0] {
        // ground impact min speed, shake frequency and amplitude, duration,
        // falloff
        w.f32(v);
    }
    w
}

/// A Sky material list a 4.04r2 client can draw: it reads entry 6 (the
/// environment map) on its first frame with no null check, so six
/// sky-box names or none crash it (lab 2026-11-19). Shorter lists are
/// padded to seven entries with the first name, or `fallback` when empty.
pub fn sky_material_list(list: &[u8], fallback: &[u8]) -> Vec<u8> {
    let mut names: Vec<&[u8]> = if list.is_empty() {
        Vec::new()
    } else {
        list.split(|b| *b == b',').collect()
    };
    let pad = match names.first() {
        Some(n) if !n.is_empty() => *n,
        _ => fallback,
    };
    while names.len() < 7 {
        names.push(pad);
    }
    names.join(&b","[..])
}

/// Torque's `writeFloat`: a 0..1 value scaled to `bits` and rounded.
fn unit_float(w: &mut BitWriter, v: f32, bits: u32) {
    let max = ((1u32 << bits) - 1) as f32;
    w.int((v.clamp(0.0, 1.0) * max).round() as u32, bits);
}

/// Torque's `writeSignedFloat`: -1..1.
fn signed_unit_float(w: &mut BitWriter, v: f32, bits: u32) {
    unit_float(w, (v.clamp(-1.0, 1.0) + 1.0) * 0.5, bits);
}

fn wrap_angle(a: f32) -> f32 {
    let a = a % TAU;
    if a < 0.0 {
        a + TAU
    } else {
        a
    }
}

/// Camera modes (3 bits). The orbit modes carry more; the server sends
/// only the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraMode {
    Stationary = 0,
    FreeRotate = 1,
    Fly = 2,
    OrbitObject = 3,
    OrbitPoint = 4,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub position: [f32; 3],
    /// rot.x, radians.
    pub pitch: f32,
    /// rot.z, radians.
    pub yaw: f32,
    pub mode: CameraMode,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            position: [0.0; 3],
            pitch: 0.0,
            yaw: 0.0,
            mode: CameraMode::Fly,
        }
    }
}

/// GameBase's part of an update: no name, the position, the datablock on
/// a create; then ShapeBase's main flag clear.
fn game_base(w: &mut BitWriter, position: &[f32; 3], datablock: Option<u32>) {
    w.flag(false); // object name
    w.flag(true); // position
    for v in position {
        w.f32(*v);
    }
    if w.flag(datablock.is_some()) {
        w.int(
            datablock.unwrap_or(FIRST_DATABLOCK_ID) - FIRST_DATABLOCK_ID,
            DATABLOCK_ID_BITS,
        );
    }
    w.flag(false); // ShapeBase: no damage, images, threads or chat
}

impl Camera {
    /// A Camera ghost update. A create carries the datablock.
    pub fn write_update(&self, w: &mut BitWriter, create: bool) {
        game_base(w, &self.position, create.then_some(CAMERA_DATABLOCK_ID));
        w.flag(false); // the client controls it: no
        w.flag(true); // move section
        for v in self.position {
            w.f32(v);
        }
        w.f32(self.pitch);
        w.f32(self.yaw);
    }

    /// The Camera's packet data as a control object: ShapeBase's energy and
    /// recharge rate, position, rot.x, rot.z, the mode.
    pub fn write_packet_data(&self, w: &mut BitWriter) {
        w.f32(0.0);
        w.f32(0.0);
        for v in self.position {
            w.f32(v);
        }
        w.f32(self.pitch);
        w.f32(self.yaw);
        let mode = match self.mode {
            CameraMode::OrbitObject | CameraMode::OrbitPoint => CameraMode::Fly,
            m => m,
        };
        w.int(mode as u32, 3);
    }

    /// `None` for a mode the server cannot read to the end.
    pub fn read_packet_data(r: &mut BitReader<'_>) -> R<Option<Camera>> {
        r.f32()?;
        r.f32()?;
        let position = [r.f32()?, r.f32()?, r.f32()?];
        let pitch = r.f32()?;
        let yaw = r.f32()?;
        let mode = match r.int(3)? {
            0 => CameraMode::Stationary,
            1 => CameraMode::FreeRotate,
            2 => CameraMode::Fly,
            3 => CameraMode::OrbitObject,
            4 => CameraMode::OrbitPoint,
            _ => return Ok(None),
        };
        if matches!(mode, CameraMode::OrbitObject | CameraMode::OrbitPoint) {
            for _ in 0..3 {
                r.f32()?; // min, max and current orbit distance
            }
            if mode == CameraMode::OrbitPoint {
                return Ok(None); // a compressed point follows: not read
            }
            r.flag()?;
            r.int(12)?;
        }
        Ok(Some(Camera {
            position,
            pitch,
            yaw,
            mode,
        }))
    }
}

/// A stock TGE Player's state as this server keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlayerState {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    /// Yaw, radians.
    pub rot_z: f32,
    /// Pitch, radians.
    pub head_x: f32,
    /// Free-look yaw, radians.
    pub head_z: f32,
    /// int3, 0: the move state.
    pub action_state: u32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Player {
    pub state: PlayerState,
    /// Its PlayerData's shape file.
    pub shape: Vec<u8>,
}

impl Player {
    /// A Player ghost update. Stock TGE leaves the move section out only
    /// for the client that controls the object, and never on its create.
    pub fn write_update(&self, w: &mut BitWriter, create: bool, controlled: bool) {
        let p = &self.state;
        game_base(w, &p.position, create.then_some(PLAYER_DATABLOCK_ID));
        w.flag(false); // impact sound
        w.flag(false); // the int3 field
        w.flag(false); // action
        w.flag(false); // the u32 field
        if w.flag(controlled && !create) {
            return; // the client predicts it from its own moves and the packet data
        }
        w.flag(true); // move section
        w.flag(false); // falling
        w.flag(false);
        w.int(p.action_state & 7, 3);
        w.flag(false); // recover ticks
        w.int(3, 2); // compressed point type 3: three raw floats
        for v in p.position {
            w.f32(v);
        }
        w.flag(false); // velocity: the packet data carries it
        unit_float(w, wrap_angle(p.rot_z) / TAU, 7);
        signed_unit_float(w, p.head_x / PLAYER_MAX_LOOK_ANGLE, 6);
        signed_unit_float(w, p.head_z / PLAYER_MAX_FREELOOK_ANGLE, 6);
        Move::default().write(w);
        w.flag(false);
    }
}

impl PlayerState {
    /// The Player's packet data: ShapeBase's energy and recharge rate, the
    /// action state, recover and jump ticks, "not mounted" with position,
    /// velocity and ticks since a jump surface, head.x, head.z, rot.z,
    /// and no controlled object.
    pub fn write_packet_data(&self, w: &mut BitWriter) {
        w.f32(0.0);
        w.f32(0.0);
        w.int(self.action_state & 7, 3);
        w.flag(false); // recover ticks
        w.flag(false); // jump delay
        w.flag(true); // not mounted
        for v in self.position {
            w.f32(v);
        }
        for v in self.velocity {
            w.f32(v);
        }
        w.int(15, 4); // long since it touched a jump surface
        w.f32(self.head_x);
        w.f32(self.head_z);
        w.f32(self.rot_z);
        w.flag(false); // controls nothing
    }

    pub fn read_packet_data(r: &mut BitReader<'_>) -> R<Option<PlayerState>> {
        let mut p = PlayerState::default();
        r.f32()?;
        r.f32()?;
        p.action_state = r.int(3)?;
        if r.flag()? {
            r.int(7)?;
        }
        if r.flag()? {
            r.int(7)?;
        }
        if r.flag()? {
            p.position = [r.f32()?, r.f32()?, r.f32()?];
            p.velocity = [r.f32()?, r.f32()?, r.f32()?];
            r.int(4)?;
        }
        p.head_x = r.f32()?;
        p.head_z = r.f32()?;
        p.rot_z = r.f32()?;
        if r.flag()? {
            return Ok(None); // it controls another object: not read here
        }
        Ok(Some(p))
    }
}

/// One Torque move as the 4.04r2 client packs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    /// int16 each, only when not zero: signed fractions of a turn.
    pub yaw: Option<u32>,
    pub pitch: Option<u32>,
    pub roll: Option<u32>,
    /// int6 each; 16 is still, 0 and 32 are full speed.
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub free_look: bool,
    pub triggers: [bool; 10],
}

impl Default for Move {
    fn default() -> Self {
        Move {
            yaw: None,
            pitch: None,
            roll: None,
            x: 16,
            y: 16,
            z: 16,
            free_look: false,
            triggers: [false; 10],
        }
    }
}

impl Move {
    pub fn read(r: &mut BitReader<'_>) -> R<Move> {
        let mut m = Move::default();
        let angle = |r: &mut BitReader<'_>| -> R<Option<u32>> {
            Ok(if r.flag()? { Some(r.int(16)?) } else { None })
        };
        m.yaw = angle(r)?;
        m.pitch = angle(r)?;
        m.roll = angle(r)?;
        m.x = r.int(6)?;
        m.y = r.int(6)?;
        m.z = r.int(6)?;
        m.free_look = r.flag()?;
        for t in &mut m.triggers {
            *t = r.flag()?;
        }
        Ok(m)
    }

    pub fn write(&self, w: &mut BitWriter) {
        for a in [self.yaw, self.pitch, self.roll] {
            if w.flag(a.is_some()) {
                w.int(a.unwrap_or(0) & 0xffff, 16);
            }
        }
        w.int(self.x, 6);
        w.int(self.y, 6);
        w.int(self.z, 6);
        w.flag(self.free_look);
        for t in self.triggers {
            w.flag(t);
        }
    }
}

/// Walks `p` by one move: yaw and pitch turn the player and its head, x
/// and y set the velocity along the ground at [`PLAYER_MAX_SPEED`], and
/// the position advances one tick. No gravity, collision or jumping: the
/// server has no terrain to stand on.
pub fn apply_move(p: &mut PlayerState, m: &Move) {
    let angle = |v: Option<u32>| v.map_or(0.0, |v| f32::from(v as u16 as i16) * (TAU / 65536.0));
    p.rot_z = wrap_angle(p.rot_z + angle(m.yaw));
    p.head_x = (p.head_x + angle(m.pitch)).clamp(PLAYER_MIN_LOOK_ANGLE, PLAYER_MAX_LOOK_ANGLE);
    let x = ((m.x as f32 - 16.0) / 16.0).clamp(-1.0, 1.0);
    let y = ((m.y as f32 - 16.0) / 16.0).clamp(-1.0, 1.0);
    // Torque's z rotation: forward (0, 1) turns to (sin, cos), right
    // (1, 0) to (cos, -sin).
    let (s, c) = p.rot_z.sin_cos();
    p.velocity = [
        (x * c + y * s) * PLAYER_MAX_SPEED,
        (-x * s + y * c) * PLAYER_MAX_SPEED,
        0.0,
    ];
    p.position[0] += p.velocity[0] * TICK_SECONDS;
    p.position[1] += p.velocity[1] * TICK_SECONDS;
}

/// The client's game section (between the rate fields and the events).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClientGame {
    pub first_person: bool,
    pub position: [f32; 3],
    pub control_ghost: Option<u32>,
    /// The client sent its control object's state instead of moves.
    pub sends_state: bool,
    /// The index of the first move, then the moves.
    pub first_move: u32,
    pub moves: Vec<Move>,
    pub fov: Option<u32>,
}

impl ClientGame {
    /// `None` when the client sends a state the server cannot read to the
    /// end (it has no length to skip by). A state is read as Camera
    /// packet data, the only kind a client of this server would send.
    pub fn read(r: &mut BitReader<'_>) -> R<Option<ClientGame>> {
        let mut g = ClientGame {
            first_person: r.flag()?,
            position: [r.f32()?, r.f32()?, r.f32()?],
            ..ClientGame::default()
        };
        if r.flag()? {
            g.control_ghost = Some(r.int(12)?);
        }
        g.sends_state = r.flag()?;
        if g.sends_state {
            if Camera::read_packet_data(r)?.is_none() {
                return Ok(None);
            }
        } else {
            g.first_move = r.u32()?;
            let count = r.int(5)?;
            for _ in 0..count {
                g.moves.push(Move::read(r)?);
            }
        }
        if r.flag()? {
            g.fov = Some(r.int(8)?);
        }
        Ok(Some(g))
    }

    /// For tests: the client's side.
    pub fn write(&self, w: &mut BitWriter) {
        w.flag(self.first_person);
        for v in self.position {
            w.f32(v);
        }
        if w.flag(self.control_ghost.is_some()) {
            w.int(self.control_ghost.unwrap_or(0), 12);
        }
        w.flag(false);
        w.u32(self.first_move);
        w.int(self.moves.len() as u32, 5);
        for m in &self.moves {
            m.write(w);
        }
        if w.flag(self.fov.is_some()) {
            w.int(self.fov.unwrap_or(0), 8);
        }
    }
}

/// What the server's game section names as the control object.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ServerControl {
    pub server_time: u32,
    pub ghost: u32,
    /// The packet data: the Player's when set, else the Camera's.
    pub player: Option<PlayerState>,
    pub camera: Camera,
}

/// The server's game section: the last move ack, no damage flash, the
/// control object when given (always "dirty"), no camera, no fov.
pub fn write_server_game(w: &mut BitWriter, last_move_ack: u32, control: Option<&ServerControl>) {
    w.u32(last_move_ack);
    w.flag(false); // no damage flash or white-out
    if w.flag(control.is_some()) {
        let c = control.expect("checked");
        w.flag(true); // dirty
        w.u32(c.server_time);
        w.int(c.ghost, 12);
        match &c.player {
            Some(p) => p.write_packet_data(w),
            None => c.camera.write_packet_data(w),
        }
    }
    w.flag(false); // camera: the control object is the view
    w.flag(false); // field of view: unchanged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_round_trip() {
        let m = Move {
            yaw: Some(0x1234),
            x: 0,
            y: 32,
            free_look: true,
            triggers: [
                true, false, true, false, false, false, false, false, false, true,
            ],
            ..Move::default()
        };
        let mut w = BitWriter::new();
        m.write(&mut w);
        let b = w.into_bytes();
        assert_eq!(Move::read(&mut BitReader::new(&b)).unwrap(), m);
    }

    #[test]
    fn a_still_move_is_23_bits() {
        let mut w = BitWriter::new();
        Move::default().write(&mut w);
        assert_eq!(w.bit_len(), 3 + 18 + 1 + 10);
    }

    #[test]
    fn the_client_game_section_round_trips() {
        let g = ClientGame {
            first_person: true,
            position: [1.0, 2.0, 3.0],
            control_ghost: Some(1),
            first_move: 40,
            moves: vec![
                Move::default(),
                Move {
                    y: 32,
                    ..Move::default()
                },
            ],
            fov: Some(90),
            ..ClientGame::default()
        };
        let mut w = BitWriter::new();
        g.write(&mut w);
        let b = w.into_bytes();
        assert_eq!(ClientGame::read(&mut BitReader::new(&b)).unwrap(), Some(g));
    }

    #[test]
    fn walking_forward_goes_along_y() {
        let mut p = PlayerState::default();
        apply_move(
            &mut p,
            &Move {
                y: 32,
                ..Move::default()
            },
        );
        assert!((p.position[1] - PLAYER_MAX_SPEED * TICK_SECONDS).abs() < 1e-6);
        assert!(p.position[0].abs() < 1e-6);
        // A quarter turn right, then forward goes along +x.
        let mut p = PlayerState::default();
        apply_move(
            &mut p,
            &Move {
                yaw: Some(0x4000),
                y: 32,
                ..Move::default()
            },
        );
        assert!((p.rot_z - TAU / 4.0).abs() < 1e-4);
        assert!((p.position[0] - PLAYER_MAX_SPEED * TICK_SECONDS).abs() < 1e-4);
    }

    #[test]
    fn looking_is_clamped() {
        let mut p = PlayerState::default();
        for _ in 0..10 {
            apply_move(
                &mut p,
                &Move {
                    pitch: Some(0x2000),
                    ..Move::default()
                },
            );
        }
        assert_eq!(p.head_x, PLAYER_MAX_LOOK_ANGLE);
    }

    #[test]
    fn packet_data_reads_back() {
        let p = PlayerState {
            position: [1.0, 2.0, 3.0],
            velocity: [4.0, 5.0, 0.0],
            rot_z: 1.0,
            head_x: 0.5,
            head_z: -0.25,
            action_state: 0,
        };
        let mut w = BitWriter::new();
        p.write_packet_data(&mut w);
        let b = w.into_bytes();
        assert_eq!(
            PlayerState::read_packet_data(&mut BitReader::new(&b)).unwrap(),
            Some(p)
        );
        let c = Camera {
            position: [1.0, 2.0, 3.0],
            pitch: 0.1,
            yaw: 0.2,
            mode: CameraMode::Fly,
        };
        let mut w = BitWriter::new();
        c.write_packet_data(&mut w);
        let b = w.into_bytes();
        assert_eq!(
            Camera::read_packet_data(&mut BitReader::new(&b)).unwrap(),
            Some(c)
        );
    }

    #[test]
    fn sky_lists_are_padded_to_seven() {
        assert_eq!(
            sky_material_list(b"", b"g3d_sky"),
            b"g3d_sky,g3d_sky,g3d_sky,g3d_sky,g3d_sky,g3d_sky,g3d_sky"
        );
        assert_eq!(sky_material_list(b"a,b,c,d,e,f", b"x"), b"a,b,c,d,e,f,a");
        assert_eq!(sky_material_list(b"a,b,c,d,e,f,g", b"x"), b"a,b,c,d,e,f,g");
    }

    #[test]
    fn datablock_events_carry_the_length() {
        let data = player_data(b"", b"player.dts");
        let ev = sim_datablock_event(PLAYER_DATABLOCK_ID, 1, 0, 1, &data);
        let b = ev.bytes().to_vec();
        let mut r = BitReader::new(&b);
        assert_eq!(r.int(12).unwrap(), 1);
        assert_eq!(r.int(12).unwrap(), 1);
        assert_eq!(r.int(12).unwrap(), 0);
        assert_eq!(r.int(13).unwrap(), 1);
        assert_eq!(r.int(14).unwrap() as usize, data.bit_len());
    }

    #[test]
    fn a_controlled_player_update_stops_after_the_controlled_flag() {
        let p = Player::default();
        let mut create = BitWriter::new();
        p.write_update(&mut create, true, true);
        let mut later = BitWriter::new();
        p.write_update(&mut later, false, true);
        assert!(later.bit_len() < create.bit_len());
        // GameBase (1 + 1 + 96 + 1) + ShapeBase (1) + four flags + the controlled flag.
        assert_eq!(later.bit_len(), 99 + 1 + 4 + 1);
    }
}
