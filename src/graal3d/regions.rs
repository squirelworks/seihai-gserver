//! A 3D overworld built from regions: a grid of smaller pieces, each with
//! its own terrain chunks and objects, that the server stitches into one
//! seamless world.
//!
//! ```text
//! regions <folder> <cols>x<rows> [<chunks a region>] [<square size>]
//! ```
//!
//! The folder holds one subfolder a region, `<i>-<j>`, i from 0 to cols - 1
//! going east and j from 0 to rows - 1 going north (the same way as the
//! terrain's chunks). Each one may hold:
//!
//! - `heights<x>-<y>.dat`, x and y from 0 to the chunks a region less one:
//!   its piece of the terrain, in the same form as any terrain chunk.
//! - `region.txt`: `object`, `objectfor`, `water` and `replicator` lines in
//!   the region's own coordinates, from its south-west corner. A
//!   replicator without a box grows only inside its region.
//!
//! The world is one large terrain ([`super::ground::Large`]) of cols x
//! chunks by rows x chunks, centred on the origin, plus every region's
//! objects moved to where the region lies. So a client gets what the
//! large terrain and distance scoping ([`super::scope`]) already give one
//! big world: the terrain slides round the player and objects are created
//! and deleted by distance, with no switch or warp at a region's edge.
//! Each client is told the region it stands in as `clientr.g3dregion`
//! (`<i>-<j>`).

use super::ground::{Chunk, ANYWHERE, CHUNKS, MAX_LARGE_CHUNKS};
use super::objects::Ghost;
use super::scene::Scene;
use std::path::Path;

/// The chunks a region side may have.
pub const REGION_CHUNKS: [u32; 4] = [2, 4, 8, 16];
/// Without a size on the line: 8 chunks, 1024 units at 8 metres a square.
pub const DEFAULT_REGION_CHUNKS: u32 = 8;
/// The lines a `region.txt` may hold.
pub const REGION_LINES: [&str; 4] = ["object", "objectfor", "water", "replicator"];

/// The `regions` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regions {
    pub folder: String,
    pub cols: u32,
    pub rows: u32,
    /// Terrain chunks a region side.
    pub chunks: u32,
    pub square_size: u32,
}

impl Regions {
    /// Checks a line's numbers: the whole terrain must be a large terrain
    /// (each side even, 16 to 256 chunks).
    pub fn new(
        folder: &str,
        cols: u32,
        rows: u32,
        chunks: u32,
        square_size: u32,
    ) -> Result<Regions, String> {
        if !REGION_CHUNKS.contains(&chunks) {
            return Err(format!(
                "a region is 2, 4, 8 or 16 chunks a side, not {chunks}"
            ));
        }
        for (what, n) in [("columns", cols), ("rows", rows)] {
            let side = n.saturating_mul(chunks);
            if side < CHUNKS as u32 || side > MAX_LARGE_CHUNKS || side % 2 != 0 {
                return Err(format!(
                    "{n} {what} of {chunks} chunks is {side} chunks; the world must be 16 to {MAX_LARGE_CHUNKS} chunks a side"
                ));
            }
        }
        Ok(Regions {
            folder: folder.to_string(),
            cols,
            rows,
            chunks,
            square_size,
        })
    }

    /// Units a region side.
    pub fn span(&self) -> f32 {
        (self.chunks * CHUNKS as u32 * self.square_size) as f32
    }

    /// The terrain's size in chunks.
    pub fn terrain_chunks(&self) -> (u32, u32) {
        (self.cols * self.chunks, self.rows * self.chunks)
    }

    /// The south-west corner of region (i, j).
    pub fn corner(&self, i: u32, j: u32) -> [f32; 2] {
        let chunk = (CHUNKS as u32 * self.square_size) as f32;
        let (w, h) = self.terrain_chunks();
        [
            -((w / 2) as f32) * chunk + i as f32 * self.span(),
            -((h / 2) as f32) * chunk + j as f32 * self.span(),
        ]
    }

    /// The region a point is in, if any.
    pub fn at(&self, x: f32, y: f32) -> Option<(u32, u32)> {
        let [x0, y0] = self.corner(0, 0);
        let (i, j) = (
            ((x - x0) / self.span()).floor(),
            ((y - y0) / self.span()).floor(),
        );
        (i.is_finite()
            && j.is_finite()
            && (0.0..self.cols as f32).contains(&i)
            && (0.0..self.rows as f32).contains(&j))
        .then_some((i as u32, j as u32))
    }

    pub fn name(i: u32, j: u32) -> String {
        format!("{i}-{j}")
    }
}

/// What a world's regions hold, for the start-up log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Loaded {
    /// Regions with a folder.
    pub found: usize,
    /// Terrain chunks read.
    pub chunks: usize,
    /// World objects (`object`, `objectfor`) a region, index j * cols + i.
    pub objects: Vec<usize>,
}

/// Reads every region of the scene's `regions` line from under
/// `world_dir`: its chunks go in the scene's large terrain and its lines
/// are added to the scene, moved to where the region lies. One note per
/// thing left out.
pub fn load(scene: &mut Scene, world_dir: &Path) -> (Loaded, Vec<String>) {
    let mut notes = Vec::new();
    let mut loaded = Loaded::default();
    let Some(r) = scene.regions.clone() else {
        return (loaded, notes);
    };
    let base = world_dir.join(&r.folder);
    let (w, h) = r.terrain_chunks();
    let mut chunks = vec![None; (w * h) as usize];
    loaded.objects = vec![0; (r.cols * r.rows) as usize];
    for j in 0..r.rows {
        for i in 0..r.cols {
            let name = Regions::name(i, j);
            let dir = base.join(&name);
            if !dir.is_dir() {
                continue;
            }
            loaded.found += 1;
            for y in 0..r.chunks {
                for x in 0..r.chunks {
                    let file = format!("heights{x}-{y}.dat");
                    let Ok(data) = std::fs::read(dir.join(&file)) else {
                        continue;
                    };
                    match Chunk::from_file(data) {
                        Ok(c) => {
                            let (cx, cy) = (i * r.chunks + x, j * r.chunks + y);
                            chunks[(cy * w + cx) as usize] = Some(c);
                            loaded.chunks += 1;
                        }
                        Err(e) => notes.push(format!("{}/{name}/{file}: {e}; left out", r.folder)),
                    }
                }
            }
            let Ok(text) = std::fs::read_to_string(dir.join("region.txt")) else {
                continue;
            };
            let n = add_region(scene, &r, i, j, &text, &mut notes);
            loaded.objects[(j * r.cols + i) as usize] = n;
        }
    }
    if let Some(l) = scene.terrain.as_mut().and_then(|t| t.large.as_mut()) {
        l.chunks = std::sync::Arc::new(chunks);
    }
    if loaded.found == 0 {
        notes.push(format!(
            "{}: no region folders (<i>-<j>); the world is flat and empty",
            r.folder
        ));
    } else if loaded.chunks == 0 {
        notes.push(format!(
            "{}: no region has heights<x>-<y>.dat chunks; the terrain is flat",
            r.folder
        ));
    }
    (loaded, notes)
}

/// One region's `region.txt` added to `scene`; the number of world objects
/// it added.
fn add_region(
    scene: &mut Scene,
    r: &Regions,
    i: u32,
    j: u32,
    text: &str,
    notes: &mut Vec<String>,
) -> usize {
    let name = Regions::name(i, j);
    let mut kept = String::new();
    for (n, line) in text.lines().enumerate() {
        let first = line
            .split('#')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .next();
        match first {
            Some(word) if !REGION_LINES.contains(&word) => notes.push(format!(
                "{}/{name}/region.txt line {}: only object, objectfor, water and replicator lines go in a region",
                r.folder,
                n + 1
            )),
            _ => kept.push_str(line),
        }
        // Keep the line numbers of the errors below.
        kept.push('\n');
    }
    let (sub, errors) = Scene::parse(&kept);
    for e in errors {
        notes.push(format!(
            "{}/{name}/region.txt {e} (not understood)",
            r.folder
        ));
    }
    let [x0, y0] = r.corner(i, j);
    let span = r.span();
    let first = scene.objects.len();
    let mut added = 0;
    for mut o in sub.objects {
        match &mut o {
            Ghost::GameObject(g) => {
                g.transform.position[0] += x0;
                g.transform.position[1] += y0;
                scene.region_of.insert(g.name.clone(), name.clone());
                added += 1;
            }
            Ghost::Water(wt) => {
                wt.position[0] += x0;
                wt.position[1] += y0;
            }
            Ghost::Replicator(rp) => {
                rp.position[0] += x0;
                rp.position[1] += y0;
                let clamp = |v: f32| v.round().clamp(-32767.0, 32767.0) as i16;
                rp.area = if rp.area == ANYWHERE {
                    [clamp(x0), clamp(y0), clamp(x0 + span), clamp(y0 + span)]
                } else {
                    [
                        clamp(f32::from(rp.area[0]) + x0),
                        clamp(f32::from(rp.area[1]) + y0),
                        clamp(f32::from(rp.area[2]) + x0),
                        clamp(f32::from(rp.area[3]) + y0),
                    ]
                };
            }
            _ => {}
        }
        scene.objects.push(o);
    }
    for (k, accounts) in sub.only {
        scene.only.insert(first + k, accounts);
    }
    added
}

/// The most of `points` within `radius` of any one spot, and that spot:
/// spots are tried on a grid a quarter of the radius apart, so the count
/// may be a little under the true most.
pub fn densest(points: &[[f32; 2]], radius: f32) -> (usize, [f32; 2]) {
    if points.is_empty() || radius.is_nan() || radius <= 0.0 {
        return (0, [0.0; 2]);
    }
    let cell = radius;
    let key = |v: f32| (v / cell).floor() as i64;
    let mut grid: std::collections::HashMap<(i64, i64), Vec<[f32; 2]>> = Default::default();
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for p in points {
        grid.entry((key(p[0]), key(p[1]))).or_default().push(*p);
        for a in 0..2 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let step = radius / 4.0;
    let r2 = radius * radius;
    let mut best = (0, [0.0; 2]);
    let mut y = lo[1];
    while y <= hi[1] + step {
        let mut x = lo[0];
        while x <= hi[0] + step {
            let (kx, ky) = (key(x), key(y));
            let mut n = 0;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if let Some(ps) = grid.get(&(kx + dx, ky + dy)) {
                        n += ps
                            .iter()
                            .filter(|p| (p[0] - x).powi(2) + (p[1] - y).powi(2) <= r2)
                            .count();
                    }
                }
            }
            if n > best.0 {
                best = (n, [x, y]);
            }
            x += step;
        }
        y += step;
    }
    best
}

/// The world objects' x and y.
pub fn object_points(scene: &Scene) -> Vec<[f32; 2]> {
    scene
        .objects
        .iter()
        .filter_map(|o| match o {
            Ghost::GameObject(g) => Some([g.transform.position[0], g.transform.position[1]]),
            _ => None,
        })
        .collect()
}

/// The start-up lines for a world with regions: what was found, the
/// fullest region, and the densest spot against what one client can hold
/// with `scope` (its radius plus hysteresis, since an object stays that
/// long). Lines that start with `WARNING` need the operator.
pub fn report(scene: &Scene, scope: &super::scope::Options, budget: usize) -> Vec<String> {
    let (Some(r), Some(l)) = (&scene.regions, &scene.regions_loaded) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let total: usize = l.objects.iter().sum();
    let fullest = l
        .objects
        .iter()
        .enumerate()
        .max_by_key(|(_, n)| **n)
        .map(|(k, n)| (Regions::name(k as u32 % r.cols, k as u32 / r.cols), *n));
    let (w, h) = r.terrain_chunks();
    out.push(format!(
        "regions {}: {} of {}x{} regions found ({} units a side), {} terrain chunks of {w} x {h}, {total} region objects{}",
        r.folder,
        l.found,
        r.cols,
        r.rows,
        r.span(),
        l.chunks,
        fullest
            .filter(|(_, n)| *n > 0)
            .map(|(name, n)| format!("; the fullest region is {name} with {n}"))
            .unwrap_or_default()
    ));
    let all = object_points(scene);
    if !scope.applies(all.len()) {
        if all.len() > budget {
            out.push(format!(
                "WARNING regions {}: {} world objects and distance scoping is off for this world, so a client is sent them all at join and the ones past its table are left out; set graal3d_scope = on or auto",
                r.folder,
                all.len()
            ));
        }
        return out;
    }
    let reach = scope.radius + scope.hysteresis;
    let (n, at) = densest(&all, reach);
    let place = r
        .at(at[0], at[1])
        .map(|(i, j)| format!(" in region {}", Regions::name(i, j)))
        .unwrap_or_default();
    let line = format!(
        "regions {}: at most about {n} objects within {reach:.0} units of one spot (near {:.0},{:.0}{place}); a client holds {budget}",
        r.folder, at[0], at[1]
    );
    if n > budget {
        out.push(format!(
            "WARNING {line}: a player there gets the nearest {budget} only; spread the objects out, or make the place its own world behind a warp"
        ));
    } else {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_file() -> Vec<u8> {
        use std::io::Write;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&[7u8; 64]).unwrap();
        z.finish().unwrap()
    }

    #[test]
    fn a_line_must_make_a_large_terrain() {
        assert!(Regions::new("r", 4, 4, 8, 8).is_ok());
        assert!(Regions::new("r", 2, 2, 8, 8).is_ok()); // 16 x 16 chunks
        assert!(Regions::new("r", 1, 4, 8, 8).is_err()); // 8 chunks wide
        assert!(Regions::new("r", 4, 4, 3, 8).is_err());
        assert!(Regions::new("r", 64, 4, 8, 8).is_err()); // 512 chunks
        assert!(Regions::new("r", 17, 4, 2, 8).is_err()); // 8 chunks tall
        assert!(Regions::new("r", 17, 8, 2, 8).is_ok()); // 34 x 16
        assert!(Regions::new("r", 9, 2, 2, 8).is_err()); // 18 x 4
    }

    #[test]
    fn corners_and_the_region_at_a_point() {
        let r = Regions::new("r", 4, 2, 8, 8).unwrap();
        // 32 x 16 chunks of 128 units: x from -2048, y from -1024.
        assert_eq!(r.span(), 1024.0);
        assert_eq!(r.corner(0, 0), [-2048.0, -1024.0]);
        assert_eq!(r.corner(3, 1), [1024.0, 0.0]);
        assert_eq!(r.at(-2048.0, -1024.0), Some((0, 0)));
        assert_eq!(r.at(10.0, 10.0), Some((2, 1)));
        assert_eq!(r.at(2047.9, 1023.9), Some((3, 1)));
        assert_eq!(r.at(2048.0, 0.0), None);
        assert_eq!(r.at(f32::NAN, 0.0), None);
    }

    #[test]
    fn regions_are_stitched_into_one_world() {
        let dir = std::env::temp_dir().join(format!("seihai-regions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = dir.join("w/0-0");
        let b = dir.join("w/1-1");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("heights0-0.dat"), chunk_file()).unwrap();
        std::fs::write(b.join("heights7-7.dat"), chunk_file()).unwrap();
        std::fs::write(b.join("heights8-0.dat"), chunk_file()).unwrap(); // past the region
        std::fs::write(
            a.join("region.txt"),
            "object hut hut.dts 10 20 1\nreplicator DynamicGrass 5 5 0 4 1 3 g.png\nsun 0 0 -1 1 1 1 1 0 0 0 1\n",
        )
        .unwrap();
        std::fs::write(
            b.join("region.txt"),
            "objectfor bob door door.dts 1 2 0\nobject tower tower.dts 1000 1000 5 0 0 1 1.5\n",
        )
        .unwrap();
        let (mut s, errors) = Scene::parse("regions w 2x2\nspawn 10 10 1\n");
        assert!(errors.is_empty(), "{errors:?}");
        let (loaded, notes) = load(&mut s, &dir);
        assert_eq!(loaded.found, 2);
        assert_eq!(loaded.chunks, 2);
        assert_eq!(loaded.objects, vec![1, 0, 0, 2]);
        assert_eq!(notes.len(), 1, "{notes:?}"); // the sun line
        assert!(notes[0].contains("0-0/region.txt line 3"));
        let t = s.terrain.as_ref().unwrap();
        let l = t.large.as_ref().unwrap();
        assert_eq!((l.width, l.height), (16, 16));
        assert!(l.chunk(0, 0).is_some());
        assert!(l.chunk(15, 15).is_some());
        assert_eq!(l.chunks.iter().flatten().count(), 2);
        let objs: Vec<_> = s
            .objects
            .iter()
            .filter_map(|o| match o {
                Ghost::GameObject(g) => Some((g.name.clone(), g.transform.position)),
                _ => None,
            })
            .collect();
        // Region 0-0's corner is -1024, -1024; 1-1's is 0, 0.
        assert_eq!(objs[0], (b"hut".to_vec(), [-1014.0, -1004.0, 1.0]));
        assert_eq!(objs[1], (b"door".to_vec(), [1.0, 2.0, 0.0]));
        assert_eq!(objs[2], (b"tower".to_vec(), [1000.0, 1000.0, 5.0]));
        let door = s
            .objects
            .iter()
            .position(|o| matches!(o, Ghost::GameObject(g) if g.name == b"door"))
            .unwrap();
        assert_eq!(s.only.get(&door), Some(&vec!["bob".to_string()]));
        assert_eq!(
            s.region_of.get(b"tower".as_slice()).map(String::as_str),
            Some("1-1")
        );
        let grass = s
            .objects
            .iter()
            .find_map(|o| match o {
                Ghost::Replicator(r) => Some(r.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(grass.position, [-1019.0, -1019.0, 0.0]);
        assert_eq!(grass.area, [-1024, -1024, 0, 0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_report_warns_of_a_crowded_spot() {
        let (mut s, _) = Scene::parse("regions w 2x2\n");
        let mut text = String::new();
        for k in 0..300 {
            text.push_str(&format!("object o{k} o.dts {} {} 0\n", k % 20, k / 20));
        }
        let r = s.regions.clone().unwrap();
        let mut notes = Vec::new();
        let n = add_region(&mut s, &r, 1, 0, &text, &mut notes);
        assert_eq!(n, 300);
        s.regions_loaded = Some(Loaded {
            found: 1,
            chunks: 0,
            objects: vec![0, 300, 0, 0],
        });
        let on = super::super::scope::Options {
            mode: super::super::scope::Mode::On,
            ..Default::default()
        };
        let lines = report(&s, &on, 3584);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].contains("fullest region is 1-0 with 300"),
            "{}",
            lines[0]
        );
        assert!(lines[1].contains("about 300 objects") && lines[1].contains("region 1-0"));
        assert!(!lines[1].starts_with("WARNING"));
        assert!(report(&s, &on, 100)[1].starts_with("WARNING"));
        let off = super::super::scope::Options::default();
        assert!(report(&s, &off, 100)[1].contains("scoping is off"));
        assert_eq!(report(&s, &off, 3584).len(), 1);
    }

    #[test]
    fn the_densest_spot_is_found() {
        let mut pts = Vec::new();
        for k in 0..100 {
            pts.push([5000.0 + (k % 10) as f32, 5000.0 + (k / 10) as f32]);
        }
        for k in 0..50 {
            pts.push([k as f32 * 400.0, 0.0]);
        }
        let (n, at) = densest(&pts, 100.0);
        assert_eq!(n, 100);
        assert!((at[0] - 5000.0).abs() < 100.0 && (at[1] - 5000.0).abs() < 100.0);
        assert_eq!(densest(&[], 100.0).0, 0);
    }
}
