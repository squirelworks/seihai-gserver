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

/// Chunks a side.
pub const CHUNKS: usize = 16;
/// The 10-bit length field of a chunk event.
pub const MAX_CHUNK_BYTES: usize = 1023;
/// A huge square size killed a live 5.12 right after "Terrain lit".
pub const MAX_SQUARE_SIZE: u32 = 64;

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
    /// The two texture names of the block's update, empty by default: in
    /// stock TGE 1.x's order the detail and the bump texture (lab knobs;
    /// the static read had them read and dropped).
    pub detail: String,
    pub bump: String,
    /// Index y * 16 + x.
    pub chunks: Vec<Option<Chunk>>,
}

impl Terrain {
    pub fn new(folder: &str, square_size: u32, repeat: bool) -> Terrain {
        Terrain {
            folder: folder.to_string(),
            square_size,
            repeat,
            detail: String::new(),
            bump: String::new(),
            chunks: vec![None; CHUNKS * CHUNKS],
        }
    }

    /// Reads `heights<x>-<y>.dat` from `dir`. Returns one note per file that
    /// is too long or does not inflate; those chunks are left out, as are
    /// missing files.
    pub fn load(&mut self, dir: &Path) -> Vec<String> {
        let mut notes = Vec::new();
        let mut found = 0;
        for y in 0..CHUNKS {
            for x in 0..CHUNKS {
                let name = format!("heights{x}-{y}.dat");
                let Ok(data) = std::fs::read(dir.join(&name)) else {
                    continue;
                };
                match Chunk::from_file(data) {
                    Ok(c) => {
                        self.chunks[y * CHUNKS + x] = Some(c);
                        found += 1;
                    }
                    Err(e) => notes.push(format!("{}/{name}: {e}; left out", self.folder)),
                }
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
                  // Stock TGE 1.x sends the detail and the bump texture here.
        w.pstring_alone(self.detail.as_bytes());
        w.pstring_alone(self.bump.as_bytes());
        w.f32(1.0); // three values read and dropped
        w.f32(0.01);
        w.u32(8);
        w.u32(self.square_size);
        w.u8(u8::from(self.repeat));
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

    /// The radius sent: a real 5.12 builds no grass quads at all below 8
    /// cells (radius 4 drew nothing, 8 drew; lab, 2026-11-26), so the grass
    /// classes get at least 8.
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
