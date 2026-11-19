//! The ghosted objects of a 3D world in the GameObject era (4.20r6, 4.20r8,
//! 5.x): `GameObject`, the Graal class every 3D thing is, `Sun`, `Sky` and
//! `Camera`. Layouts: Graal_analysis `torque/udp-session.md` §8 (lab-checked
//! with a real 5.12 client); the protocol 19 kinds are in [`super::tge`].
//!
//! A GameObject update is thirteen sections, each a flag and, when set,
//! its payload. All thirteen flags are always on the wire:
//!
//! ```text
//!  1 name        pstring                   8 shape data     int14 length + data
//!  2 velocity    3 f32                     9 chat           pstring
//!  3 shape       pstring (.dts .dif ...)  10 script classes pstring
//!  4 skin        pstring                  11 attr[1..8]     8 x (flag -> pstring)
//!  5 transform   3 f32 position, 3 f32    12 nickname       pstring + R, G, B, A bytes
//!                axis, f32 angle          13 flags          int16
//!  6 movetype    pstring
//!  7 movement data, only while the object has a movement object
//! ```
//!
//! The server never sends a movetype, so it always writes section 7's flag
//! as 0; it reads a client's movement data by the movetype it sent.

use super::bits::{BitReader, BitWriter, OutOfBits};
use super::tge;

type R<T> = Result<T, OutOfBits>;

/// GameObject dirty bits (the server's masks; the wire has only the flags).
pub mod bits {
    pub const NAME: u32 = 0x1;
    pub const VELOCITY: u32 = 0x2;
    pub const SHAPE: u32 = 0x8;
    pub const SKIN: u32 = 0x10;
    pub const TRANSFORM: u32 = 0x20;
    pub const MOVE_DATA: u32 = 0x40;
    pub const MOVE_TYPE: u32 = 0x80;
    pub const SHAPE_DATA: u32 = 0xf00;
    pub const CHAT: u32 = 0x1000;
    /// Never sent: the client's own writer never sets it.
    pub const CLASSES: u32 = 0x2000;
    /// `attr[i]` is `ATTR1 << (i - 1)`, i = 1..8.
    pub const ATTR1: u32 = 0x4000;
    pub const ATTRS: u32 = 0x3f_c000;
    pub const NICKNAME: u32 = 0x40_0000;
    pub const FLAGS: u32 = 0x80_0000;
}

/// Section 13.
pub mod flags {
    pub const EDITOR_OBJECT: u16 = 0x04;
    pub const SOLID: u16 = 0x08;
    pub const SHADOW: u16 = 0x10;
    pub const SHOW_TERRAIN: u16 = 0x20;
    pub const COLLISION: u16 = 0x40;
}

/// A position, then a rotation as an axis and an angle in radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub position: [f32; 3],
    pub axis: [f32; 3],
    pub angle: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Transform {
            position: [0.0; 3],
            axis: [0.0, 0.0, 1.0],
            angle: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GameObject {
    pub name: Vec<u8>,
    pub velocity: [f32; 3],
    pub shape: Vec<u8>,
    pub skin: Vec<u8>,
    pub transform: Transform,
    pub chat: Vec<u8>,
    pub attr: [Vec<u8>; 8],
    pub nickname: Vec<u8>,
    pub nick_color: [u8; 4],
    pub flags: u16,
}

impl Default for GameObject {
    fn default() -> Self {
        GameObject {
            name: Vec::new(),
            velocity: [0.0; 3],
            shape: Vec::new(),
            skin: Vec::new(),
            transform: Transform::default(),
            chat: Vec::new(),
            attr: Default::default(),
            nickname: Vec::new(),
            nick_color: [255; 4],
            flags: 0,
        }
    }
}

fn floats(w: &mut BitWriter, v: &[f32]) {
    for f in v {
        w.f32(*f);
    }
}

fn read_floats<const N: usize>(r: &mut BitReader<'_>) -> R<[f32; N]> {
    let mut out = [0.0; N];
    for f in &mut out {
        *f = r.f32()?;
    }
    Ok(out)
}

/// interpolate and rigid movement data: position (3 x 32), a quaternion
/// (4 x 8), two vectors (6 x 9), one flag.
const RIGID_MOVE_BITS: usize = 3 * 32 + 4 * 8 + 6 * 9 + 1;
/// One `list` entry, after its flag.
const LIST_ENTRY_BITS: usize = 8 * 32;

/// The movetypes that make a movement object.
pub fn has_movement_object(move_type: &[u8]) -> bool {
    matches!(move_type, b"interpolate" | b"list" | b"rigid")
}

impl GameObject {
    /// The sections that differ from `before`.
    pub fn changes_from(&self, before: &GameObject) -> u32 {
        use bits::*;
        let mut m = 0;
        if self.name != before.name {
            m |= NAME;
        }
        if self.velocity != before.velocity {
            m |= VELOCITY;
        }
        if self.shape != before.shape {
            m |= SHAPE;
        }
        if self.skin != before.skin {
            m |= SKIN;
        }
        if self.transform != before.transform {
            m |= TRANSFORM;
        }
        if self.chat != before.chat {
            m |= CHAT;
        }
        for i in 0..8 {
            if self.attr[i] != before.attr[i] {
                m |= ATTR1 << i;
            }
        }
        if self.nickname != before.nickname || self.nick_color != before.nick_color {
            m |= NICKNAME;
        }
        if self.flags != before.flags {
            m |= FLAGS;
        }
        m
    }

    /// What a create carries: name, transform and flags always, the rest
    /// when set.
    pub fn full_mask(&self) -> u32 {
        use bits::*;
        let mut m = NAME | TRANSFORM | FLAGS;
        if self.velocity != [0.0; 3] {
            m |= VELOCITY;
        }
        if !self.shape.is_empty() {
            m |= SHAPE;
        }
        if !self.skin.is_empty() {
            m |= SKIN;
        }
        if !self.chat.is_empty() {
            m |= CHAT;
        }
        for (i, a) in self.attr.iter().enumerate() {
            if !a.is_empty() {
                m |= ATTR1 << i;
            }
        }
        if !self.nickname.is_empty() {
            m |= NICKNAME;
        }
        m
    }

    /// Writes the thirteen sections for `mask`.
    pub fn write(&self, w: &mut BitWriter, mask: u32) {
        use bits::*;
        if w.flag(mask & NAME != 0) {
            w.pstring_alone(&self.name);
        }
        if w.flag(mask & VELOCITY != 0) {
            floats(w, &self.velocity);
        }
        if w.flag(mask & SHAPE != 0) {
            w.pstring_alone(&self.shape);
        }
        if w.flag(mask & SKIN != 0) {
            w.pstring_alone(&self.skin);
        }
        if w.flag(mask & TRANSFORM != 0) {
            floats(w, &self.transform.position);
            floats(w, &self.transform.axis);
            w.f32(self.transform.angle);
        }
        w.flag(false); // 6 movetype: never sent
        w.flag(false); // 7 movement data: the client has no movement object from us
        w.flag(false); // 8 shape data (animation threads)
        if w.flag(mask & CHAT != 0) {
            w.pstring_alone(&self.chat);
        }
        w.flag(false); // 10 script classes
        if w.flag(mask & ATTRS != 0) {
            for (i, a) in self.attr.iter().enumerate() {
                if w.flag(mask & (ATTR1 << i) != 0) {
                    w.pstring_alone(a);
                }
            }
        }
        if w.flag(mask & NICKNAME != 0) {
            w.pstring_alone(&self.nickname);
            for c in self.nick_color {
                w.u8(c);
            }
        }
        if w.flag(mask & FLAGS != 0) {
            w.int(self.flags.into(), 16);
        }
    }

    /// Reads an update into `self`. `move_type` is the movetype the
    /// writer's object has (it decides whether section 7 is there) and is
    /// updated from section 6. Returns the sections it carried.
    pub fn read(&mut self, r: &mut BitReader<'_>, move_type: &mut Vec<u8>) -> R<u32> {
        use bits::*;
        let mut m = 0;
        if r.flag()? {
            self.name = r.pstring()?;
            m |= NAME;
        }
        if r.flag()? {
            self.velocity = read_floats(r)?;
            m |= VELOCITY;
        }
        if r.flag()? {
            self.shape = r.pstring()?;
            m |= SHAPE;
        }
        if r.flag()? {
            self.skin = r.pstring()?;
            m |= SKIN;
        }
        if r.flag()? {
            self.transform.position = read_floats(r)?;
            self.transform.axis = read_floats(r)?;
            self.transform.angle = r.f32()?;
            m |= TRANSFORM;
        }
        if r.flag()? {
            *move_type = r.pstring()?;
            m |= MOVE_TYPE;
        }
        if r.flag()? && has_movement_object(move_type) {
            if move_type == b"list" {
                while r.flag()? {
                    r.seek(r.position() + LIST_ENTRY_BITS)?;
                }
            } else {
                r.seek(r.position() + RIGID_MOVE_BITS)?;
            }
            m |= MOVE_DATA;
        }
        if r.flag()? {
            let len = r.int(14)? as usize;
            r.seek(r.position() + len)?;
            m |= SHAPE_DATA;
        }
        if r.flag()? {
            self.chat = r.pstring()?;
            m |= CHAT;
        }
        if r.flag()? {
            r.pstring()?;
            m |= CLASSES;
        }
        if r.flag()? {
            for (i, a) in self.attr.iter_mut().enumerate() {
                if r.flag()? {
                    *a = r.pstring()?;
                    m |= ATTR1 << i;
                }
            }
        }
        if r.flag()? {
            self.nickname = r.pstring()?;
            for c in &mut self.nick_color {
                *c = r.u8()?;
            }
            m |= NICKNAME;
        }
        if r.flag()? {
            self.flags = r.int(16)? as u16;
            m |= FLAGS;
        }
        Ok(m)
    }
}

/// `flag` init, then name, light direction, colour RGBA and ambient RGBA.
#[derive(Debug, Clone, PartialEq)]
pub struct Sun {
    pub name: Vec<u8>,
    pub direction: [f32; 3],
    pub color: [f32; 4],
    pub ambient: [f32; 4],
}

impl Default for Sun {
    fn default() -> Self {
        Sun {
            name: b"Sun".to_vec(),
            direction: [0.57735, 0.57735, -0.57735],
            color: [0.8, 0.8, 0.8, 1.0],
            ambient: [0.3, 0.3, 0.35, 1.0],
        }
    }
}

impl Sun {
    /// The server always sends the whole Sun.
    pub fn write(&self, w: &mut BitWriter) {
        w.flag(true);
        w.pstring_alone(&self.name);
        floats(w, &self.direction);
        floats(w, &self.color);
        floats(w, &self.ambient);
    }

    pub fn read(r: &mut BitReader<'_>) -> R<Option<Sun>> {
        if !r.flag()? {
            return Ok(None);
        }
        Ok(Some(Sun {
            name: r.pstring()?,
            direction: read_floats(r)?,
            color: read_floats(r)?,
            ambient: read_floats(r)?,
        }))
    }
}

/// A Sky, as the server sends it: always whole (init and material list).
/// The three bools and the solid colour have no known names; they are the
/// fields the client reads at those places.
#[derive(Debug, Clone, PartialEq)]
pub struct Sky {
    pub name: Vec<u8>,
    /// Six textures, comma separated: front, right, back, left, top, bottom.
    pub material_list: Vec<u8>,
    pub visible_distance: f32,
    pub fog_color: [f32; 3],
    pub bools: [u8; 3],
    pub solid_color: [f32; 3],
}

impl Default for Sky {
    fn default() -> Self {
        Sky {
            name: b"Sky".to_vec(),
            material_list: Vec::new(),
            visible_distance: 500.0,
            fog_color: [0.5, 0.6, 0.8],
            bools: [1, 0, 0],
            solid_color: [0.4, 0.6, 0.9],
        }
    }
}

impl Sky {
    pub fn write(&self, w: &mut BitWriter) {
        w.flag(true); // init
        w.pstring_alone(&self.name);
        w.flag(true); // material list
        w.f32(self.visible_distance);
        w.pstring_alone(&self.material_list);
        floats(w, &self.fog_color);
        w.u32(0); // no fog volumes
        w.u8(self.bools[0]);
        w.u8(self.bools[1]);
        floats(w, &self.solid_color);
        w.u8(self.bools[2]);
        w.flag(false);
        for _ in 0..3 {
            w.pstring_alone(b""); // no cloud layer
            w.f32(0.0); // height
            w.f32(0.0); // speed
        }
        floats(w, &[0.0; 3]); // wind
        w.u32(0);
        w.flag(false); // no storm fog
        w.flag(false); // mask 0x20
        w.flag(false); // mask 0x40
        w.flag(true); // the writer repeats the init state here
        w.flag(false); // mask 0x2
        w.flag(false); // mask 0x4
        w.flag(false); // mask 0x8
        w.flag(false); // mask 0x10
    }

    /// Reads a Sky update (the whole reader, every part guarded).
    pub fn read(r: &mut BitReader<'_>) -> R<Sky> {
        let mut sky = Sky::default();
        let init = r.flag()?;
        if init {
            sky.name = r.pstring()?;
        }
        if r.flag()? {
            sky.visible_distance = r.f32()?;
            sky.material_list = r.pstring()?;
        }
        if init {
            sky.fog_color = read_floats(r)?;
            let volumes = r.u32()?;
            sky.bools[0] = r.u8()?;
            sky.bools[1] = r.u8()?;
            sky.solid_color = read_floats(r)?;
            sky.bools[2] = r.u8()?;
            r.flag()?;
            for _ in 0..volumes.min(64) {
                read_floats::<6>(r)?;
                r.u8()?;
            }
            for _ in 0..3 {
                r.pstring()?;
                r.f32()?;
                r.f32()?;
            }
            read_floats::<3>(r)?;
            r.u32()?;
            if r.flag()? {
                read_floats::<5>(r)?;
            }
        }
        if r.flag()? {
            r.u8()?;
        }
        if r.flag()? {
            r.u8()?;
        }
        r.flag()?;
        for n in [2, 3, 4, 3] {
            if r.flag()? {
                for _ in 0..n {
                    r.u32()?;
                }
            }
        }
        Ok(sky)
    }
}

/// One object the server ghosts.
#[derive(Debug, Clone, PartialEq)]
pub enum Ghost {
    GameObject(GameObject),
    Sun(Sun),
    Sky(Sky),
    /// A GameObject-era Camera: a GameObject update, then the camera mode
    /// and followed-object flags, both left clear.
    Camera(GameObject),
    /// Protocol 19: a stock TGE Camera.
    TgeCamera(tge::Camera),
    /// Protocol 19: a stock TGE Player.
    TgePlayer(tge::Player),
}

/// Protocol 19 masks: position and rotation, and the datablock (creates).
pub const TGE_MOVE: u32 = 1;
pub const TGE_DATABLOCK: u32 = 2;

impl Ghost {
    pub fn class_name(&self) -> &'static str {
        match self {
            Ghost::GameObject(_) => "GameObject",
            Ghost::Sun(_) => "Sun",
            Ghost::Sky(_) => "Sky",
            Ghost::Camera(_) | Ghost::TgeCamera(_) => "Camera",
            Ghost::TgePlayer(_) => "Player",
        }
    }

    /// The name logs give it.
    pub fn label(&self) -> String {
        match self {
            Ghost::GameObject(o) | Ghost::Camera(o) => String::from_utf8_lossy(&o.name).into(),
            Ghost::Sun(s) => String::from_utf8_lossy(&s.name).into(),
            Ghost::Sky(s) => String::from_utf8_lossy(&s.name).into(),
            Ghost::TgeCamera(_) => "camera".into(),
            Ghost::TgePlayer(_) => "player".into(),
        }
    }

    pub fn is_tge(&self) -> bool {
        matches!(self, Ghost::TgeCamera(_) | Ghost::TgePlayer(_))
    }

    /// Whether a client of that era takes this kind; a Sky goes to both.
    pub fn sent_to(&self, tge_era: bool) -> bool {
        matches!(self, Ghost::Sky(_)) || self.is_tge() == tge_era
    }

    pub fn full_mask(&self) -> u32 {
        match self {
            Ghost::GameObject(o) | Ghost::Camera(o) => o.full_mask(),
            Ghost::Sun(_) | Ghost::Sky(_) => 1,
            Ghost::TgeCamera(_) | Ghost::TgePlayer(_) => TGE_MOVE | TGE_DATABLOCK,
        }
    }

    /// What an update must carry to turn `before` into this.
    pub fn changes_from(&self, before: &Ghost) -> u32 {
        match (self, before) {
            (Ghost::GameObject(a), Ghost::GameObject(b)) | (Ghost::Camera(a), Ghost::Camera(b)) => {
                a.changes_from(b)
            }
            (Ghost::Sun(a), Ghost::Sun(b)) => u32::from(a != b),
            (Ghost::Sky(a), Ghost::Sky(b)) => u32::from(a != b),
            (Ghost::TgeCamera(a), Ghost::TgeCamera(b)) => {
                if a == b {
                    0
                } else {
                    TGE_MOVE
                }
            }
            (Ghost::TgePlayer(a), Ghost::TgePlayer(b)) => {
                if a.state == b.state {
                    0
                } else {
                    TGE_MOVE
                }
            }
            _ => self.full_mask(),
        }
    }

    /// The update for `mask`. `controlled`: the receiving client controls
    /// this object (a TGE Player then leaves its move out after the create).
    pub fn write(&self, w: &mut BitWriter, mask: u32, controlled: bool) {
        match self {
            Ghost::GameObject(o) => o.write(w, mask),
            Ghost::Camera(o) => {
                o.write(w, mask);
                w.flag(false); // camera mode: the client's scripts choose it
                w.flag(false); // followed object: none
            }
            Ghost::Sun(s) => s.write(w),
            Ghost::Sky(s) => s.write(w),
            Ghost::TgeCamera(c) => c.write_update(w, mask & TGE_DATABLOCK != 0),
            Ghost::TgePlayer(p) => p.write_update(w, mask & TGE_DATABLOCK != 0, controlled),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
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

    #[test]
    fn the_game_object_create_matches_the_note() {
        // udp-session.md §8: 471 bits.
        let o = lab_player();
        let mut w = BitWriter::new();
        o.write(&mut w, o.full_mask());
        assert_eq!(w.bit_len(), 471);
        assert_eq!(
            w.into_bytes(),
            hex("6d887574de1b5979938ddce9c2c81df6bb2f1d431300000000000000000000f00700000000000000000000f00700000000e8c0c8ddffffff7f2400")
        );
    }

    #[test]
    fn an_empty_update_is_thirteen_zero_bits() {
        let mut w = BitWriter::new();
        lab_player().write(&mut w, 0);
        assert_eq!(w.bit_len(), 13);
        assert_eq!(w.into_bytes(), [0, 0]);
    }

    #[test]
    fn the_sun_matches_the_note() {
        let mut w = BitWriter::new();
        Sun::default().write(&mut w);
        assert_eq!(w.bit_len(), 379);
        assert_eq!(
            w.into_bytes(),
            hex("1d987ab2699ef8b1699ef8b1699ef86d6666fa696666fa696666fa010000fcd1ccccf4d1ccccf4999999f5010000fc01")
        );
    }

    #[test]
    fn the_sky_matches_the_note() {
        let sky = Sky {
            material_list: b"sky_fr,sky_rt,sky_bk,sky_lf,sky_up,sky_dn".to_vec(),
            ..Sky::default()
        };
        let mut w = BitWriter::new();
        sky.write(&mut w);
        assert_eq!(w.bit_len(), 956);
        let bytes = w.into_bytes();
        assert_eq!(
            bytes,
            hex("1d984e2a0100f4874c69938a6cb368b749456610ed36a9c8769368b7494536b668b74945e6eb68b7494516080000e0473333e3a79999e90700000020
                 00a09999d9473333e3c7ccccec0700000000000000000000000000000000000000000000000000000000000000000000000000000000000000008000")
        );
        assert_eq!(Sky::read(&mut BitReader::new(&bytes)).unwrap(), sky);
    }

    #[test]
    fn updates_read_back() {
        let mut o = lab_player();
        o.chat = b"hi".to_vec();
        o.attr[2] = b"three".to_vec();
        o.velocity = [1.0, 2.0, 3.0];
        let mut w = BitWriter::new();
        o.write(&mut w, o.full_mask());
        let bytes = w.into_bytes();
        let mut back = GameObject::default();
        let mut mt = Vec::new();
        let m = back.read(&mut BitReader::new(&bytes), &mut mt).unwrap();
        assert_eq!(back, o);
        assert_eq!(m, o.full_mask());
    }

    #[test]
    fn movement_data_is_skipped_by_the_movetype() {
        let mut w = BitWriter::new();
        for _ in 0..5 {
            w.flag(false);
        }
        w.flag(true);
        w.pstring_alone(b"rigid");
        w.flag(true);
        w.int(0, 32);
        for _ in 0..(RIGID_MOVE_BITS - 32) {
            w.flag(false);
        }
        w.flag(false);
        w.flag(true);
        w.pstring_alone(b"yo");
        for _ in 0..4 {
            w.flag(false);
        }
        let bytes = w.into_bytes();
        let mut o = GameObject::default();
        let mut mt = Vec::new();
        let m = o.read(&mut BitReader::new(&bytes), &mut mt).unwrap();
        assert_eq!(mt, b"rigid");
        assert_eq!(o.chat, b"yo");
        assert_eq!(m, bits::MOVE_TYPE | bits::MOVE_DATA | bits::CHAT);
    }
}
