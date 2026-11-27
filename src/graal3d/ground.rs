//! The ground of a 3D world on protocols 21 and 24 (4.20r6, 4.20r8, 5.x):
//! the streamed terrain (`TerrainBlock` and its chunk events), `WaterBlock`
//! and the ground-cover replicators.
//!
//! Wire layouts: Graal_analysis `torque/terrain-512.md` (TerrainBlock,
//! TerrainChunkEvent, TerrainRequestUpdatesEvent, WaterBlock) and
//! `torque/replicators-512.md`. The terrain layouts were run live on a real
//! 5.12 there; the water and replicator layouts were drawn by a real 5.12.
//!
//! The terrain is 256 x 256 squares centred on the origin, streamed as 256
//! chunks of 16 x 16 squares. Each chunk is one zlib stream, kept on disk as
//! `heights<x>-<y>.dat` (x and y 0 to 15, no padding), the same file the
//! client keeps in its own cache. When the client has the TerrainBlock it
//! sends the stamp of every chunk it holds; the server sends each chunk whose
//! stamp differs. A stamp is the CRC-32 table run from 0xFFFFFFFF with no
//! final inversion over the inflated chunk, as the client re-encodes it, so
//! chunks written in the client's own form (`seihai-servertools gen terrain
//! --stream`) are not sent again on a return visit.

use super::bits::BitWriter;
use super::events::Event;
use super::objects::GameObject;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

/// Chunks a side.
pub const CHUNKS: usize = 16;
/// The 10-bit length field of a chunk event.
pub const MAX_CHUNK_BYTES: usize = 1023;
/// A huge square size killed a live 5.12 right after "Terrain lit".
pub const MAX_SQUARE_SIZE: u32 = 64;
/// The most chunks a side of a large terrain.
pub const MAX_LARGE_CHUNKS: u32 = 256;

/// One chunk as the server sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// The zlib stream.
    pub data: Vec<u8>,
    pub stamp: u32,
}

/// A `terrain` line, with its chunks loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terrain {
    /// The folder, from the world folder.
    pub folder: String,
    /// Metres a square (1 to 64).
    pub square_size: u32,
    pub repeat: bool,
    /// Index y * 16 + x.
    pub chunks: Vec<Option<Chunk>>,
    /// A terrain larger than the client's grid (`terrain ... repeat WxH`),
    /// streamed as a sliding window ([`Large`]); `chunks` is then unused.
    pub large: Option<Large>,
}

/// A terrain of `width` x `height` chunks, more than the 16 x 16 a client
/// holds. The client's grid is a window onto it that follows the player.
///
/// World chunk (X, Y), X from 0 to `width` - 1 and Y from 0 to `height` -
/// 1, covers x from (X - width / 2) * span to (X - width / 2 + 1) * span,
/// and the same for y, where span is 16 squares: the terrain is centred on
/// the origin, as the 16 x 16 one is. With `repeat` the client draws its
/// grid tiled, slot (gx, gy) at every 16 chunks, so world chunk X lies in
/// slot gx = (X - width / 2 + 8) mod 16 (X mod 16 when width / 2 - 8 is a
/// multiple of 16, as for 48). Each slot holds the world chunk of the 16
/// around the player's chunk (8 behind, 7 ahead) that falls in it. When the
/// player crosses into another chunk, the slots that changed are sent
/// again: 16 for a straight crossing, 31 for a diagonal one. A slot whose
/// chunk would be past the terrain's edge holds the nearest chunk of the
/// terrain that falls in it, so the edge repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Large {
    pub width: u32,
    pub height: u32,
    /// Index y * width + x; shared, since every TerrainBlock ghost holds a
    /// copy of the terrain.
    pub chunks: Arc<Vec<Option<Chunk>>>,
}

/// Each slot's world chunk, index gy * 16 + gx.
pub type Window = Vec<(u32, u32)>;

/// The client slot of world chunk `c` along an axis of `n` chunks.
pub fn slot_of(c: i64, n: u32) -> usize {
    (c - i64::from(n / 2) + 8).rem_euclid(CHUNKS as i64) as usize
}

/// The world chunk along an axis of `n` chunks that slot `slot` holds when
/// the player is in world chunk `at` (which may be off the terrain).
pub fn chunk_for_slot(slot: usize, at: i64, n: u32) -> u32 {
    let first = at - 8;
    let k = CHUNKS as i64;
    let mut c = first + (slot as i64 - slot_of(first, n) as i64).rem_euclid(k);
    let n = i64::from(n);
    if c < 0 {
        c += k * ((-c + k - 1) / k);
    }
    if c >= n {
        c -= k * ((c - n + k) / k);
    }
    c as u32
}

/// The world chunk a coordinate is in, along an axis of `n` chunks of
/// `span` units (it may be off the terrain).
pub fn chunk_at(v: f32, span: f32, n: u32) -> i64 {
    if !v.is_finite() {
        return i64::from(n / 2);
    }
    (v / span).floor() as i64 + i64::from(n / 2)
}

impl Large {
    pub fn chunk(&self, x: u32, y: u32) -> Option<&Chunk> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.chunks
            .get((y * self.width + x) as usize)
            .and_then(Option::as_ref)
    }

    /// The window for a player at `position`.
    pub fn window(&self, position: [f32; 3], square_size: u32) -> Window {
        let span = (CHUNKS as u32 * square_size) as f32;
        let ax = chunk_at(position[0], span, self.width);
        let ay = chunk_at(position[1], span, self.height);
        (0..CHUNKS * CHUNKS)
            .map(|i| {
                (
                    chunk_for_slot(i % CHUNKS, ax, self.width),
                    chunk_for_slot(i / CHUNKS, ay, self.height),
                )
            })
            .collect()
    }

    /// Chunk events for the slots of `window` that are not `old` (all of
    /// them with no `old`) and whose chunk is not `stamps`' (none: every
    /// one). Slots whose world chunk has no file are left as they are.
    pub fn events(
        &self,
        window: &Window,
        old: Option<&Window>,
        stamps: Option<&[u32]>,
    ) -> Vec<Event> {
        let mut out = Vec::new();
        for (i, (x, y)) in window.iter().enumerate() {
            if old.is_some_and(|o| o.get(i) == Some(&(*x, *y))) {
                continue;
            }
            let Some(c) = self.chunk(*x, *y) else {
                continue;
            };
            if stamps.is_some_and(|s| s.get(i) == Some(&c.stamp)) {
                continue;
            }
            out.push(chunk_event((i % CHUNKS) as u32, (i / CHUNKS) as u32, c));
        }
        out
    }
}

impl Terrain {
    pub fn new(folder: &str, square_size: u32, repeat: bool) -> Terrain {
        Terrain {
            folder: folder.to_string(),
            square_size,
            repeat,
            chunks: vec![None; CHUNKS * CHUNKS],
            large: None,
        }
    }

    /// A terrain of `width` x `height` chunks ([`Large`]).
    pub fn new_large(folder: &str, square_size: u32, width: u32, height: u32) -> Terrain {
        let mut t = Terrain::new(folder, square_size, true);
        t.large = Some(Large {
            width,
            height,
            chunks: Arc::new(vec![None; (width * height) as usize]),
        });
        t
    }

    /// Reads `heights<x>-<y>.dat` from `dir`. Returns one note per file that
    /// is too long or does not inflate; those chunks are left out, as are
    /// missing files.
    pub fn load(&mut self, dir: &Path) -> Vec<String> {
        let mut notes = Vec::new();
        let mut found = 0;
        let (w, h) = self
            .large
            .as_ref()
            .map_or((CHUNKS, CHUNKS), |l| (l.width as usize, l.height as usize));
        let mut large = vec![None; if self.large.is_some() { w * h } else { 0 }];
        for y in 0..h {
            for x in 0..w {
                let name = format!("heights{x}-{y}.dat");
                let Ok(data) = std::fs::read(dir.join(&name)) else {
                    continue;
                };
                match Chunk::from_file(data) {
                    Ok(c) => {
                        if self.large.is_some() {
                            large[y * w + x] = Some(c);
                        } else {
                            self.chunks[y * CHUNKS + x] = Some(c);
                        }
                        found += 1;
                    }
                    Err(e) => notes.push(format!("{}/{name}: {e}; left out", self.folder)),
                }
            }
        }
        if let Some(l) = &mut self.large {
            l.chunks = Arc::new(large);
            if found > 0 && found < w * h {
                notes.push(format!(
                    "{}: {found} of the large terrain's {w} x {h} chunks found; the rest are left as the client has them",
                    self.folder
                ));
            }
        }
        if found == 0 {
            notes.push(format!(
                "{}: no heights<x>-<y>.dat chunks; the terrain is flat",
                self.folder
            ));
        }
        notes
    }

    /// The TerrainBlock update: always whole.
    pub fn write_block(&self, w: &mut BitWriter) {
        w.flag(true); // init
        w.pstring_alone(b"Terrain");
        w.u32(0); // reset to 1 by the client when the block is added
        w.pstring_alone(b""); // read and dropped
        w.pstring_alone(b"");
        w.f32(1.0); // three values read and dropped
        w.f32(0.01);
        w.u32(8);
        w.u32(self.square_size);
        w.u8(u8::from(self.repeat));
    }

    /// The answer to a terrain request from a player at `position`, and for
    /// a large terrain the window it fills.
    pub fn answer_at(&self, stamps: &[u32], position: [f32; 3]) -> (Vec<Event>, Option<Window>) {
        match &self.large {
            Some(l) => {
                let w = l.window(position, self.square_size);
                (l.events(&w, None, Some(stamps)), Some(w))
            }
            None => (self.answer(stamps), None),
        }
    }

    /// Chunk events for every chunk whose stamp differs from the client's
    /// (`stamps`, index y * 16 + x; 0 for none).
    pub fn answer(&self, stamps: &[u32]) -> Vec<Event> {
        let mut out = Vec::new();
        for (i, c) in self.chunks.iter().enumerate() {
            let Some(c) = c else { continue };
            if stamps.get(i) == Some(&c.stamp) {
                continue;
            }
            out.push(chunk_event((i % CHUNKS) as u32, (i / CHUNKS) as u32, c));
        }
        out
    }
}

impl Chunk {
    /// A chunk file: a zlib stream of at most 1023 bytes.
    pub fn from_file(data: Vec<u8>) -> Result<Chunk, String> {
        if data.is_empty() {
            return Err("empty".into());
        }
        if data.len() > MAX_CHUNK_BYTES {
            return Err(format!(
                "{} bytes, over the {MAX_CHUNK_BYTES} a chunk event carries",
                data.len()
            ));
        }
        let mut inflated = Vec::new();
        flate2::read::ZlibDecoder::new(&data[..])
            .take(64 * 1024)
            .read_to_end(&mut inflated)
            .map_err(|e| format!("does not inflate ({e})"))?;
        Ok(Chunk {
            stamp: stamp(&inflated),
            data,
        })
    }
}

/// The client's chunk stamp: CRC-32 with no final inversion.
pub fn stamp(inflated: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(inflated);
    crc.sum() ^ 0xffff_ffff
}

/// `TerrainChunkEvent`: chunk x and y (4 bits each), the stamp (ignored by
/// the client), the length (10 bits) and the zlib bytes.
pub fn chunk_event(x: u32, y: u32, c: &Chunk) -> Event {
    let mut w = BitWriter::new();
    w.int(x & 15, 4);
    w.int(y & 15, 4);
    w.u32(c.stamp);
    let n = c.data.len().min(MAX_CHUNK_BYTES);
    w.int(n as u32, 10);
    w.raw(&c.data[..n]);
    Event::Raw {
        class: "TerrainChunkEvent".into(),
        payload: w,
    }
}

/// Torque's compact rotation: a quaternion's x, y and z, and whether w is
/// negative. From an axis and an angle in radians.
fn write_quat(w: &mut BitWriter, axis: [f32; 3], angle: f32) {
    let len = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    let (s, c) = (angle / 2.0).sin_cos();
    let q = if len > 0.0 && angle != 0.0 {
        [axis[0] / len * s, axis[1] / len * s, axis[2] / len * s, c]
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    w.f32(q[0]);
    w.f32(q[1]);
    w.f32(q[2]);
    w.flag(q[3] < 0.0);
}

/// A `water` line: an unrotated `WaterBlock` from its corner.
#[derive(Debug, Clone, PartialEq)]
pub struct Water {
    /// The corner; the surface is at z + the z size.
    pub position: [f32; 3],
    pub scale: [f32; 3],
    pub surface_texture: Vec<u8>,
    /// Stock TGE's: 0 water, 1 ocean, 2 river, 3 stagnant, 4 lava, ...
    pub liquid_type: u32,
}

impl Water {
    /// The whole block; what the line does not set takes stock TGE 1.x's
    /// defaults.
    pub fn write(&self, w: &mut BitWriter) {
        w.flag(false); // no name
        for v in self.position {
            w.f32(v);
        }
        write_quat(w, [0.0, 0.0, 1.0], 0.0);
        for v in self.scale {
            w.f32(v);
        }
        w.pstring_alone(&self.surface_texture);
        for _ in 0..4 {
            w.pstring_alone(b""); // specular mask, env maps over/under, shore
        }
        w.pstring_alone(b""); // two submerge textures
        w.pstring_alone(b"");
        w.u32(self.liquid_type);
        // density, viscosity, wave magnitude, surface opacity, env intensity
        for v in [1.0, 5.0, 1.0, 0.75, 0.4] {
            w.f32(v);
        }
        w.u8(1); // remove wet edges
        w.u8(1); // use depth mask
                 // tessellation (surface, shore), parallax, flow angle and rate,
                 // distortion grid scale, magnitude and time, shore depth, min and
                 // max alpha, depth gradient
        for v in [
            50.0, 60.0, 0.5, 0.0, 0.0, 0.1, 0.05, 0.5, 20.0, 0.03, 1.0, 1.0,
        ] {
            w.f32(v);
        }
        for c in [255u8, 255, 255, 255] {
            w.u8(c); // specular colour
        }
        w.f32(6.0); // specular power
        w.u8(0); // ignore terrain
    }
}

/// The least radius, in cells, a 5.12 grows grass at (see
/// [`Replicator::sent_radius`]).
pub const GRASS_MIN_RADIUS: u8 = 8;

/// The three replicator classes.
pub const REPLICATOR_CLASSES: [&str; 3] = [
    "DynamicShapeReplicator",
    "DynamicGrass",
    "DynamicGrassReplicator",
];

/// A `replicator` line: ground cover on a grid of 8-unit cells around the
/// client's control object.
#[derive(Debug, Clone, PartialEq)]
pub struct Replicator {
    /// One of [`REPLICATOR_CLASSES`].
    pub class: &'static str,
    pub name: Vec<u8>,
    pub position: [f32; 3],
    /// Cells, 1 to 64.
    pub radius: u8,
    /// Objects a cell, 0 to 20 (0 to 100 for the grass classes).
    pub min: u8,
    pub max: u8,
    /// Comma separated, no spaces: shapes, or for the grass classes
    /// textures.
    pub shapes: Vec<u8>,
    pub seed: u32,
    /// World x/y region a cell's corner must lie in: x0, y0, x1, y1.
    pub area: [i16; 4],
    /// Each copy's size, picked between these (the same on every axis).
    /// For the grass classes this is the quad size: 1 is about 0.6 units.
    pub scale: [f32; 2],
}

/// A whole-world box.
pub const ANYWHERE: [i16; 4] = [-32767, -32767, 32767, 32767];

/// A signed intN: a sign flag, then an N-1 bit magnitude.
fn signed(w: &mut BitWriter, v: i32, n: u32) {
    let max = (1i32 << (n - 1)) - 1;
    let v = v.clamp(-max, max);
    w.flag(v < 0);
    w.int(v.unsigned_abs(), n - 1);
}

impl Replicator {
    /// `DynamicGrass` and `DynamicGrassReplicator` build their own ground
    /// pieces from a texture named in `shapefiles`, not shapes, and take up
    /// to 100 objects a cell (Graal_analysis `torque/replicators-512.md`).
    pub fn grass(&self) -> bool {
        self.class != "DynamicShapeReplicator"
    }

    /// The radius sent: the grass classes get at least 8 cells. On a real
    /// 5.12, radius 4 with 5 or 10 a cell drew nothing where 8 drew, but
    /// radius 4 with 40 to 60 a cell drew (conformance C036), so the cause
    /// is not radius alone; a larger radius only widens the patch.
    pub fn sent_radius(&self) -> u8 {
        if self.grass() {
            self.radius.max(GRASS_MIN_RADIUS)
        } else {
            self.radius
        }
    }

    /// A whole GameObject update (name and position), then the replicator's
    /// part. Fixed: any turn about z, terrain only, slopes to 90
    /// degrees, no alignment, shown.
    pub fn write(&self, w: &mut BitWriter) {
        use super::objects::bits;
        let mut o = GameObject {
            name: self.name.clone(),
            ..GameObject::default()
        };
        o.transform.position = self.position;
        o.write(w, bits::NAME | bits::TRANSFORM);
        w.flag(true);
        for v in self.position {
            w.f32(v);
        }
        write_quat(w, [0.0, 0.0, 1.0], 0.0);
        w.u32(self.seed);
        w.pstring_alone(&self.shapes);
        w.u8(self.sent_radius());
        w.u8(self.min);
        w.u8(self.max);
        let [x0, y0, x1, y1] = self.area;
        for v in [x0, y0, -32767, x1, y1, 32767] {
            signed(w, v.into(), 16);
        }
        let [lo, hi] = self.scale;
        for v in [lo, lo, lo, hi, hi, hi] {
            w.f32(v); // scale min, max
        }
        for v in [0.0, 0.0, 0.0, 0.0, 0.0, 360.0] {
            w.f32(v); // rotation min, max
        }
        w.f32(0.0); // offset z
                    // on terrain, interiors, statics, water, the water surface
        for on in [true, false, false, false, false] {
            w.flag(on);
        }
        signed(w, 90, 8); // allowed slope
        w.flag(false); // align to terrain
        for v in [1.0, 1.0, 1.0] {
            w.f32(v); // terrain alignment
        }
        w.flag(false); // surface exclusion mode
        signed(w, 0, 8); // surface type
        w.flag(false); // hide replications
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graal3d::bits::BitReader;
    use std::io::Write;

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// A large terrain whose chunk (x, y) inflates to [x, y] as two
    /// little-endian u16s.
    fn large(w: u32, h: u32) -> Large {
        let mut chunks = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let raw = [x as u8, (x >> 8) as u8, y as u8, (y >> 8) as u8];
                chunks.push(Some(Chunk::from_file(zlib(&raw)).unwrap()));
            }
        }
        Large {
            width: w,
            height: h,
            chunks: Arc::new(chunks),
        }
    }

    #[test]
    fn world_chunks_map_to_slots_mod_16() {
        // 48 chunks a side: slot = X mod 16.
        for x in [0i64, 7, 15, 16, 31, 47] {
            assert_eq!(slot_of(x, 48), (x % 16) as usize);
        }
        // 16 a side is the plain grid: slot = X.
        for x in 0..16 {
            assert_eq!(slot_of(x, 16), x as usize);
        }
        // 32 a side: centred, so chunk 16 (the origin) is slot 8.
        assert_eq!(slot_of(16, 32), 8);
        assert_eq!(slot_of(0, 32), 8);
        assert_eq!(slot_of(8, 32), 0);
    }

    #[test]
    fn the_window_is_the_16_chunks_around_the_player() {
        let l = large(48, 48);
        // At the origin (world chunk 24, 24) the window runs 16 to 31.
        let w = l.window([0.0, 0.0, 0.0], 8);
        let xs: std::collections::BTreeSet<u32> = w.iter().map(|c| c.0).collect();
        assert_eq!(xs, (16..32).collect());
        for (i, (x, y)) in w.iter().enumerate() {
            assert_eq!(slot_of(i64::from(*x), 48), i % 16);
            assert_eq!(slot_of(i64::from(*y), 48), i / 16);
        }
        // A 16 x 16 large terrain is the plain grid wherever the player is.
        let l16 = large(16, 16);
        let plain: Window = (0..256).map(|i| (i % 16, i / 16)).collect();
        assert_eq!(l16.window([0.0; 3], 8), plain);
        assert_eq!(l16.window([900.0, -900.0, 0.0], 8), plain);
    }

    #[test]
    fn a_chunk_crossing_sends_16_straight_or_31_diagonal() {
        let l = large(48, 48);
        let span = 128.0; // 16 squares of 8
        let w0 = l.window([10.0, 10.0, 0.0], 8);
        // East one chunk: one column changes, to world chunk 32.
        let w1 = l.window([10.0 + span, 10.0, 0.0], 8);
        let ev = l.events(&w1, Some(&w0), None);
        assert_eq!(ev.len(), 16);
        let changed: Vec<usize> = (0..256).filter(|i| w0[*i] != w1[*i]).collect();
        assert!(changed.iter().all(|i| i % 16 == 0), "{changed:?}");
        assert!(changed.iter().all(|i| w1[*i].0 == 32));
        // Diagonal: a column and a row, 31 slots.
        let w2 = l.window([10.0 + span, 10.0 + span, 0.0], 8);
        assert_eq!(l.events(&w2, Some(&w0), None).len(), 31);
        // Moving within a chunk changes nothing.
        let w3 = l.window([100.0, 100.0, 0.0], 8);
        assert!(l.events(&w3, Some(&w0), None).is_empty());
        // The event names the slot and carries the world chunk.
        let e = &ev[0];
        let crate::graal3d::events::Event::Raw { payload, .. } = e else {
            panic!()
        };
        let bytes = payload.clone().into_bytes();
        let mut r = BitReader::new(&bytes);
        assert_eq!((r.int(4).unwrap(), r.int(4).unwrap()), (0, 0));
        assert_eq!(r.u32().unwrap(), l.chunk(32, w1[0].1).unwrap().stamp);
    }

    #[test]
    fn past_the_edge_a_slot_keeps_the_nearest_chunk_in_it() {
        let l = large(48, 48);
        // Far east of the terrain: every chunk is still on it.
        for p in [
            [1.0e4, 0.0, 0.0],
            [-1.0e4, -1.0e4, 0.0],
            [f32::NAN, 0.0, 0.0],
        ] {
            let w = l.window(p, 8);
            assert!(w.iter().all(|(x, y)| *x < 48 && *y < 48), "{p:?}");
            for (i, (x, y)) in w.iter().enumerate() {
                assert_eq!(slot_of(i64::from(*x), 48), i % 16);
                assert_eq!(slot_of(i64::from(*y), 48), i / 16);
            }
        }
        assert_eq!(chunk_for_slot(0, 100, 48), 32);
        assert_eq!(chunk_for_slot(15, -50, 48), 15);
    }

    #[test]
    fn a_large_answer_fills_the_window_and_honours_stamps() {
        let mut t = Terrain::new_large("t", 8, 48, 48);
        t.large = Some(large(48, 48));
        let (ev, w) = t.answer_at(&[0; 256], [0.0; 3]);
        assert_eq!(ev.len(), 256);
        let w = w.unwrap();
        // A client holding slot 0's chunk gets the other 255.
        let mut stamps = vec![0; 256];
        let l = t.large.as_ref().unwrap();
        stamps[0] = l.chunk(w[0].0, w[0].1).unwrap().stamp;
        assert_eq!(t.answer_at(&stamps, [0.0; 3]).0.len(), 255);
        // Returning far east: the window there, not the centre's.
        let (_, far) = t.answer_at(&[0; 256], [2000.0, 0.0, 0.0]);
        assert_ne!(far.unwrap(), w);
    }

    #[test]
    fn a_large_terrain_loads_every_chunk_file() {
        let dir = std::env::temp_dir().join(format!("g3d-large-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (x, y) in [(0, 0), (31, 0), (31, 17)] {
            std::fs::write(
                dir.join(format!("heights{x}-{y}.dat")),
                zlib(&[x as u8, y as u8]),
            )
            .unwrap();
        }
        let mut t = Terrain::new_large("t", 8, 32, 18);
        let notes = t.load(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        let l = t.large.as_ref().unwrap();
        assert!(l.chunk(31, 17).is_some());
        assert!(l.chunk(1, 1).is_none());
        assert!(l.chunk(32, 0).is_none());
        assert!(t.chunks.iter().all(Option::is_none));
    }

    #[test]
    fn the_stamp_is_crc32_without_the_final_inversion() {
        // CRC-32 of "123456789" is 0xCBF43926.
        assert_eq!(stamp(b"123456789"), 0xCBF4_3926 ^ 0xffff_ffff);
    }

    #[test]
    fn chunks_load_and_only_changed_ones_are_answered() {
        let dir = std::env::temp_dir().join(format!("g3d-terrain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let flat = vec![2u8, 0, 0];
        std::fs::write(dir.join("heights0-0.dat"), zlib(&flat)).unwrap();
        std::fs::write(dir.join("heights15-1.dat"), zlib(&[2u8, 1, 0])).unwrap();
        std::fs::write(dir.join("heights3-3.dat"), b"not zlib").unwrap();
        std::fs::write(dir.join("heights4-4.dat"), vec![0u8; 2000]).unwrap();
        let mut t = Terrain::new("t", 8, false);
        let notes = t.load(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(t.chunks.iter().flatten().count(), 2);
        let c0 = t.chunks[0].clone().unwrap();
        assert_eq!(c0.stamp, stamp(&flat));

        // A fresh client gets both; one holding chunk (0, 0) gets one.
        assert_eq!(t.answer(&[0; 256]).len(), 2);
        let mut have = [0u32; 256];
        have[0] = c0.stamp;
        let ev = t.answer(&have);
        assert_eq!(ev.len(), 1);
        let Event::Raw { class, payload } = &ev[0] else {
            panic!()
        };
        assert_eq!(*class, "TerrainChunkEvent");
        let mut r = BitReader::new(payload.bytes());
        assert_eq!(r.int(4).unwrap(), 15);
        assert_eq!(r.int(4).unwrap(), 1);
        r.u32().unwrap();
        let n = r.int(10).unwrap() as usize;
        assert_eq!(r.raw(n).unwrap(), zlib(&[2u8, 1, 0]));
    }

    #[test]
    fn the_block_has_the_noted_layout() {
        let t = Terrain::new("t", 8, true);
        let mut w = BitWriter::new();
        t.write_block(&mut w);
        let mut r = BitReader::new(w.bytes());
        assert!(r.flag().unwrap());
        r.pstring().unwrap();
        assert_eq!(r.u32().unwrap(), 0);
        r.pstring().unwrap();
        r.pstring().unwrap();
        assert_eq!(r.f32().unwrap(), 1.0);
        assert_eq!(r.f32().unwrap(), 0.01);
        assert_eq!(r.u32().unwrap(), 8);
        assert_eq!(r.u32().unwrap(), 8);
        assert_eq!(r.u8().unwrap(), 1);
    }

    #[test]
    fn signed_ints_are_a_sign_and_a_magnitude() {
        let mut w = BitWriter::new();
        signed(&mut w, -5, 8);
        signed(&mut w, 40000, 16);
        let mut r = BitReader::new(w.bytes());
        assert!(r.flag().unwrap());
        assert_eq!(r.int(7).unwrap(), 5);
        assert!(!r.flag().unwrap());
        assert_eq!(r.int(15).unwrap(), 32767);
    }

    #[test]
    fn water_and_replicators_write_their_whole_update() {
        let water = Water {
            position: [-1024.0, -1024.0, -19.5],
            scale: [2048.0, 2048.0, 20.0],
            surface_texture: b"water_surface".to_vec(),
            liquid_type: 0,
        };
        let mut w = BitWriter::new();
        water.write(&mut w);
        let mut r = BitReader::new(w.bytes());
        assert!(!r.flag().unwrap());
        assert_eq!(r.f32().unwrap(), -1024.0);
        // 1 + 6x32 + 1 + 3x32 bits, then seven strings and the rest.
        let fixed = 1 + 7 * 32 + 1 + 32 + 5 * 32 + 16 + 12 * 32 + 32 + 32 + 8;
        assert!(w.bit_len() > fixed);

        let rep = Replicator {
            class: "DynamicGrass",
            name: b"Grass1".to_vec(),
            position: [10.0, -20.0, 1.5],
            radius: 4,
            min: 1,
            max: 3,
            shapes: b"grass.dts".to_vec(),
            seed: 1,
            area: ANYWHERE,
            scale: [3.0, 3.0],
        };
        let mut w = BitWriter::new();
        rep.write(&mut w);
        let mut r = BitReader::new(w.bytes());
        let mut o = GameObject::default();
        let mut mt = Vec::new();
        o.read(&mut r, &mut mt).unwrap();
        assert_eq!(o.name, b"Grass1");
        assert!(r.flag().unwrap());
        assert_eq!(r.f32().unwrap(), 10.0);
        for _ in 0..5 {
            r.f32().unwrap();
        }
        r.flag().unwrap();
        assert_eq!(r.u32().unwrap(), 1);
        assert_eq!(r.pstring().unwrap(), b"grass.dts");
        // Grass goes out with at least 8 cells; shapes keep their radius.
        assert_eq!(r.u8().unwrap(), 8);
        let shapes = Replicator {
            class: "DynamicShapeReplicator",
            ..rep
        };
        assert_eq!(shapes.sent_radius(), 4);
    }
}
