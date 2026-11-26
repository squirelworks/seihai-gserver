//! The start-up check of a 3D world's files: every shape and texture the
//! world file names is looked up in the file folders, and every `.3ds`,
//! `.dts` and PNG is checked for the faults that crashed a real client.
//!
//! The clients do not defend themselves (Graal_analysis
//! `torque/3d-fuzz-content-512-2026-11-25.md`): on 5.12 a cut `.3ds`, a
//! face count or index past the mesh, a cut `.dts` or a huge buffer-size
//! word, and a PNG claiming 30000 x 30000 each killed the client that
//! loaded it, and a bad login player shape killed 4.04r2 at login. Other
//! files load but draw nothing or white: a `.3ds` with no material, a `.dts`
//! of another version than 24, a 16-bit PNG, BMP, TGA or DDS, an image over
//! 1024 a side (`torque/live-512-own-server-textures-2026-11-25.md`).
//!
//! `graal3d_shapecheck` decides what a bad shape does: `skip` (the
//! default) leaves its object out of the world (and a bad player shape
//! unused), `warn` logs it and sends it anyway (labs that send broken
//! shapes on purpose), `off` checks nothing.

use super::objects::Ghost;
use super::scene::Scene;

/// `graal3d_shapecheck`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShapeCheck {
    #[default]
    Skip,
    Warn,
    Off,
}

impl ShapeCheck {
    pub fn parse(v: &str) -> Option<ShapeCheck> {
        match v.to_ascii_lowercase().as_str() {
            "skip" => Some(ShapeCheck::Skip),
            "warn" => Some(ShapeCheck::Warn),
            "off" => Some(ShapeCheck::Off),
            _ => None,
        }
    }
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// Loads, but the client draws it badly or not at all.
    Warn(String),
    /// Crashed a real client, or cannot be drawn at all.
    Bad(String),
}

fn le16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn le32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// A shape by its extension: `.3ds` and `.dts` are checked, others pass.
pub fn check_shape(name: &str, data: &[u8]) -> Verdict {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".3ds") {
        check_3ds(data)
    } else if lower.ends_with(".dts") {
        check_dts(data)
    } else {
        Verdict::Ok
    }
}

/// 3DS chunks that hold other chunks (main, editor, object, mesh,
/// material); an object chunk starts with its name.
const MAIN: u16 = 0x4d4d;
const EDITOR: u16 = 0x3d3d;
const OBJECT: u16 = 0x4000;
const MESH: u16 = 0x4100;
const VERTICES: u16 = 0x4110;
const FACES: u16 = 0x4120;
const MATERIAL: u16 = 0xafff;

#[derive(Default)]
struct Walk {
    materials: usize,
    faces: usize,
    /// The vertex count of the mesh being read.
    vertices: Option<usize>,
}

/// A `.3ds`: every chunk inside its parent, every list inside its chunk,
/// every face on a vertex of its mesh, and at least one material.
pub fn check_3ds(data: &[u8]) -> Verdict {
    if le16(data, 0) != Some(MAIN) {
        return Verdict::Bad("not a 3DS file".into());
    }
    let Some(len) = le32(data, 2) else {
        return Verdict::Bad("cut short".into());
    };
    if len as usize > data.len() || len < 6 {
        return Verdict::Bad(format!(
            "cut short: the file says {len} bytes, it has {}",
            data.len()
        ));
    }
    let mut w = Walk::default();
    if let Err(e) = walk_3ds(&data[6..len as usize], MAIN, &mut w) {
        return Verdict::Bad(e);
    }
    if w.materials == 0 {
        return Verdict::Bad("no materials: 5.12 and 4.20r6 draw none of it".into());
    }
    if w.faces == 0 {
        return Verdict::Warn("no faces".into());
    }
    Verdict::Ok
}

fn walk_3ds(mut d: &[u8], parent: u16, w: &mut Walk) -> Result<(), String> {
    if parent == OBJECT {
        // The object's name, NUL terminated.
        let end = d
            .iter()
            .position(|b| *b == 0)
            .ok_or("an object name runs off its chunk")?;
        d = &d[end + 1..];
    }
    while !d.is_empty() {
        let (Some(id), Some(len)) = (le16(d, 0), le32(d, 2)) else {
            return Err("a chunk header is cut".into());
        };
        let len = len as usize;
        if len < 6 || len > d.len() {
            return Err(format!(
                "chunk {id:04x} is longer than its parent ({len} of {} bytes)",
                d.len()
            ));
        }
        let body = &d[6..len];
        match id {
            EDITOR | OBJECT => walk_3ds(body, id, w)?,
            MESH => {
                let before = w.vertices.take();
                walk_3ds(body, id, w)?;
                w.vertices = before;
            }
            MATERIAL => w.materials += 1,
            VERTICES => {
                let n = le16(body, 0).ok_or("a vertex list is cut")? as usize;
                if 2 + n * 12 > body.len() {
                    return Err(format!(
                        "a vertex list claims {n} vertices it does not hold"
                    ));
                }
                w.vertices = Some(n);
            }
            FACES => {
                let n = le16(body, 0).ok_or("a face list is cut")? as usize;
                if 2 + n * 8 > body.len() {
                    return Err(format!("a face list claims {n} faces it does not hold"));
                }
                let verts = w.vertices.unwrap_or(0);
                for f in 0..n {
                    for k in 0..3 {
                        let v = le16(body, 2 + f * 8 + k * 2).unwrap_or(0) as usize;
                        if v >= verts {
                            return Err(format!("face {f} uses vertex {v}, the mesh has {verts}"));
                        }
                    }
                }
                w.faces += n;
            }
            _ => {}
        }
        d = &d[len..];
    }
    Ok(())
}

/// A `.dts`: version 24 (the only one the real 5.12 drew), the memory
/// buffer its header claims inside the file, its 16- and 8-bit section
/// starts inside that buffer, and its node, object, decal and sub-shape
/// counts no larger than the buffer.
pub fn check_dts(data: &[u8]) -> Verdict {
    let Some(word) = le32(data, 0) else {
        return Verdict::Bad("cut short".into());
    };
    let version = word & 0xffff;
    if version != 24 {
        return Verdict::Bad(format!(
            "DTS version {version}: the 5.12 client draws only version 24"
        ));
    }
    let (Some(size), Some(start16), Some(start8)) = (le32(data, 4), le32(data, 8), le32(data, 12))
    else {
        return Verdict::Bad("the header is cut".into());
    };
    let words = size as u64;
    if words == 0 || 16 + words * 4 > data.len() as u64 {
        return Verdict::Bad(format!(
            "the header claims a {} byte buffer; the file has {}",
            words * 4,
            data.len().saturating_sub(16)
        ));
    }
    if start16 as u64 > words || start8 as u64 > words || start16 > start8 {
        return Verdict::Bad("a section start lies outside the buffer".into());
    }
    for (i, what) in ["node", "object", "decal", "sub-shape"].iter().enumerate() {
        let n = le32(data, 16 + i * 4).unwrap_or(0) as i32;
        if n < 0 || n as u64 > words {
            return Verdict::Bad(format!("a {what} count of {n} does not fit the buffer"));
        }
    }
    Verdict::Ok
}

/// A texture. A PNG must claim a size from 1 to 8192 a side (30000 x 30000
/// crashed 5.12); 16-bit PNGs, images over 1024 a side, BMP, TGA, DDS and
/// empty files draw white or not at all. `terrain`: a terrain texture,
/// which the client wants 256 x 256 24-bit.
pub fn check_texture(name: &str, data: &[u8], terrain: bool) -> Verdict {
    if data.is_empty() {
        return Verdict::Warn("empty".into());
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (Some(w), Some(h), Some(&depth), Some(&color)) =
            (be32(data, 16), be32(data, 20), data.get(24), data.get(25))
        else {
            return Verdict::Bad("a PNG with no header".into());
        };
        if !(1..=8192).contains(&w) || !(1..=8192).contains(&h) {
            return Verdict::Bad(format!("a PNG claiming {w}x{h} crashed 5.12"));
        }
        if depth == 16 {
            return Verdict::Warn("a 16-bit PNG draws white on 5.12".into());
        }
        if w > 1024 || h > 1024 {
            return Verdict::Warn(format!("{w}x{h}: over 1024 a side draws white on 5.12"));
        }
        if terrain && (w != 256 || h != 256 || color != 2) {
            return Verdict::Warn(format!(
                "terrain textures should be 256x256 24-bit; the client draws its default for this one ({w}x{h})"
            ));
        }
        return Verdict::Ok;
    }
    let lower = name.to_ascii_lowercase();
    if data.starts_with(b"BM")
        || data.starts_with(b"DDS ")
        || lower.ends_with(".tga")
        || lower.ends_with(".bmp")
        || lower.ends_with(".dds")
    {
        return Verdict::Warn(
            "BMP, TGA and DDS draw white on 5.12; use 8-bit PNG, JPEG or GIF".into(),
        );
    }
    Verdict::Ok
}

/// A file as the client would get it: the name, its bytes.
pub type Lookup<'a> = dyn Fn(&str) -> Option<(String, Vec<u8>)> + 'a;

/// The image extensions a bare texture name is tried with.
pub const IMAGE_EXTENSIONS: [&str; 7] = [".png", ".jpg", ".jpeg", ".gif", ".bmp", ".tga", ".dds"];

fn find_texture(find: &Lookup<'_>, name: &str) -> Option<(String, Vec<u8>)> {
    find(name).or_else(|| {
        IMAGE_EXTENSIONS
            .iter()
            .find_map(|e| find(&format!("{name}{e}")))
    })
}

/// Checks every file the scene names. Bad shapes are taken out under
/// [`ShapeCheck::Skip`]. Returns one line per finding.
pub fn check_scene(
    scene: &mut Scene,
    find: &Lookup<'_>,
    heights_info: Option<Vec<u8>>,
    mode: ShapeCheck,
) -> Vec<String> {
    check_scene_files(scene, find, heights_info, mode).notes
}

/// What [`check_scene_files`] found.
#[derive(Debug, Default)]
pub struct Checked {
    /// One line per finding.
    pub notes: Vec<String>,
    /// Lower-case names of the files that would crash a client, under
    /// [`ShapeCheck::Skip`]: a 3D client asking for one is answered as if
    /// the server had no such file, which it survives (an oversized terrain
    /// PNG fetched by name crashed 5.12).
    pub refused: std::collections::BTreeSet<String>,
    /// `heights.info`, then each texture it names once, as found: what
    /// `graal3d_pushterrain` sends. A refused texture is never pushed.
    pub terrain_files: Vec<String>,
}

/// [`check_scene`], with the refused files and the terrain's files.
pub fn check_scene_files(
    scene: &mut Scene,
    find: &Lookup<'_>,
    heights_info: Option<Vec<u8>>,
    mode: ShapeCheck,
) -> Checked {
    let mut notes = Vec::new();
    let refused = std::cell::RefCell::new(std::collections::BTreeSet::new());
    let refuse = |name: &str| {
        if mode == ShapeCheck::Skip {
            refused.borrow_mut().insert(name.to_ascii_lowercase());
        }
    };
    let shape_ok = |label: &str, shape: &[u8], notes: &mut Vec<String>| -> bool {
        let name = String::from_utf8_lossy(shape).into_owned();
        if name.is_empty() {
            return true;
        }
        let Some((_, data)) = find(&name) else {
            notes.push(format!("{label}: {name} is not in the file folders"));
            return true;
        };
        if mode == ShapeCheck::Off {
            return true;
        }
        match check_shape(&name, &data) {
            Verdict::Ok => true,
            Verdict::Warn(e) => {
                notes.push(format!("{label}: {name}: {e}"));
                true
            }
            Verdict::Bad(e) => {
                refuse(&name);
                let keep = mode == ShapeCheck::Warn;
                let what = if keep { "sent anyway" } else { "left out" };
                notes.push(format!("{label}: {name}: {e} ({what})"));
                keep
            }
        }
    };

    let mut kept = Vec::with_capacity(scene.objects.len());
    let only = std::mem::take(&mut scene.only);
    for (i, o) in std::mem::take(&mut scene.objects).into_iter().enumerate() {
        let ok = match &o {
            Ghost::GameObject(g) => shape_ok(
                &format!("object {}", String::from_utf8_lossy(&g.name)),
                &g.shape,
                &mut notes,
            ),
            // Grass takes textures; they are checked below.
            Ghost::Replicator(r) if r.grass() => {
                for t in String::from_utf8_lossy(&r.shapes).split(',') {
                    let lower = t.to_ascii_lowercase();
                    if lower.ends_with(".3ds") || lower.ends_with(".dts") {
                        notes.push(format!(
                            "replicator {}: {t} is a shape; this class draws a texture named in its list and draws nothing for a shape",
                            r.class
                        ));
                    }
                }
                true
            }
            Ghost::Replicator(r) => {
                let shapes = r.shapes.clone();
                shapes
                    .split(|b| *b == b',')
                    .all(|s| shape_ok("replicator", s, &mut notes))
            }
            _ => true,
        };
        if ok {
            // An `objectfor` object keeps its accounts at its new index.
            if let Some(a) = only.get(&i) {
                scene.only.insert(kept.len(), a.clone());
            }
            kept.push(o);
        }
    }
    scene.objects = kept;
    if !shape_ok("playershape", &scene.player_shape.clone(), &mut notes) {
        scene.player_shape.clear();
    }
    if !shape_ok(
        "tgeplayershape",
        &scene.tge_player_shape.clone(),
        &mut notes,
    ) {
        // Protocol 19 then controls a Camera.
        scene.tge_player_shape.clear();
    }

    // Whether the texture is there and safe to send.
    let texture = |label: &str, name: &str, terrain: bool, notes: &mut Vec<String>| -> bool {
        if name.is_empty() || name == "-" {
            return false;
        }
        match find_texture(find, name) {
            None => {
                notes.push(format!(
                    "{label}: texture {name} is not in the file folders (the client draws it white)"
                ));
                false
            }
            Some((found, data)) => {
                if mode == ShapeCheck::Off {
                    return true;
                }
                match check_texture(&found, &data, terrain) {
                    Verdict::Ok => true,
                    Verdict::Warn(e) => {
                        notes.push(format!("{label}: {found}: {e}"));
                        true
                    }
                    // A texture cannot be left out of the world; under skip
                    // the client is told the server has no such file.
                    Verdict::Bad(e) => {
                        refuse(&found);
                        let what = if mode == ShapeCheck::Skip {
                            " (refused when asked for)"
                        } else {
                            ""
                        };
                        notes.push(format!("{label}: {found}: ERROR {e}{what}"));
                        mode != ShapeCheck::Skip
                    }
                }
            }
        }
    };
    let mut terrain_files = Vec::new();
    for o in &scene.objects {
        match o {
            Ghost::Sky(s) => {
                for t in String::from_utf8_lossy(&s.material_list).split(',') {
                    texture("sky", t, false, &mut notes);
                }
            }
            Ghost::Replicator(r) if r.grass() => {
                for t in String::from_utf8_lossy(&r.shapes).split(',') {
                    let lower = t.to_ascii_lowercase();
                    if !(lower.ends_with(".3ds") || lower.ends_with(".dts")) {
                        texture("replicator", t, false, &mut notes);
                    }
                }
            }
            Ghost::Water(wb) => {
                texture(
                    "water",
                    &String::from_utf8_lossy(&wb.surface_texture),
                    false,
                    &mut notes,
                );
            }
            _ => {}
        }
    }
    if scene.terrain.is_some() {
        match heights_info {
            None => notes.push(
                "terrain: heights.info is not in the file folders (the client draws the terrain white)"
                    .into(),
            ),
            Some(info) => {
                terrain_files.push("heights.info".to_string());
                let text = String::from_utf8_lossy(&info).into_owned();
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    // A name listed again is the same file: check and push it once.
                    if terrain_files.iter().any(|f| f == line) {
                        continue;
                    }
                    if line.contains([' ', ',']) {
                        notes.push(format!(
                            "terrain: heights.info line {line:?} has a space or comma; the client reads one file name per line"
                        ));
                    }
                    if !line.contains('.') {
                        notes.push(format!(
                            "terrain: heights.info names {line} without an extension; a client that saves it so cannot load it later"
                        ));
                    }
                    if texture("terrain", line, true, &mut notes) {
                        terrain_files.push(line.to_string());
                    }
                }
            }
        }
    }
    Checked {
        notes,
        refused: refused.into_inner(),
        terrain_files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3DS with one mesh (`verts` vertices, the given faces) and
    /// optionally a material.
    fn three_ds(verts: u16, faces: &[[u16; 3]], material: bool) -> Vec<u8> {
        fn chunk(id: u16, body: &[u8]) -> Vec<u8> {
            let mut c = id.to_le_bytes().to_vec();
            c.extend_from_slice(&((body.len() + 6) as u32).to_le_bytes());
            c.extend_from_slice(body);
            c
        }
        let mut v = verts.to_le_bytes().to_vec();
        v.extend(std::iter::repeat_n(0u8, verts as usize * 12));
        let mut f = (faces.len() as u16).to_le_bytes().to_vec();
        for face in faces {
            for i in face {
                f.extend_from_slice(&i.to_le_bytes());
            }
            f.extend_from_slice(&0u16.to_le_bytes());
        }
        let mut mesh = chunk(VERTICES, &v);
        mesh.extend(chunk(FACES, &f));
        let mut obj = b"cube\0".to_vec();
        obj.extend(chunk(MESH, &mesh));
        let mut editor = Vec::new();
        if material {
            editor.extend(chunk(MATERIAL, &chunk(0xa000, b"red\0")));
        }
        editor.extend(chunk(OBJECT, &obj));
        chunk(MAIN, &chunk(EDITOR, &editor))
    }

    fn dts(version: u32, words: u32, counts: [i32; 4]) -> Vec<u8> {
        let mut d = Vec::new();
        for v in [version, words, 0, 0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        let mut buf = vec![0u8; words as usize * 4];
        for (i, c) in counts.iter().enumerate() {
            if i * 4 + 4 <= buf.len() {
                buf[i * 4..i * 4 + 4].copy_from_slice(&c.to_le_bytes());
            }
        }
        d.extend(buf);
        d
    }

    fn png(w: u32, h: u32, depth: u8, color: u8) -> Vec<u8> {
        let mut d = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[depth, color, 0, 0, 0]);
        d
    }

    #[test]
    fn a_good_3ds_passes_and_the_crashing_faults_do_not() {
        let good = three_ds(3, &[[0, 1, 2]], true);
        assert_eq!(check_3ds(&good), Verdict::Ok);
        assert!(matches!(
            check_3ds(&three_ds(3, &[[0, 1, 2]], false)),
            Verdict::Bad(_)
        ));
        assert!(matches!(
            check_3ds(&good[..good.len() / 2]),
            Verdict::Bad(_)
        ));
        assert!(matches!(
            check_3ds(&three_ds(3, &[[0, 1, 65534]], true)),
            Verdict::Bad(_)
        ));
        // A face count past the list.
        let mut big = good.clone();
        let at = big
            .windows(2)
            .rposition(|w| w == FACES.to_le_bytes())
            .unwrap()
            + 6;
        big[at..at + 2].copy_from_slice(&65535u16.to_le_bytes());
        assert!(matches!(check_3ds(&big), Verdict::Bad(_)));
    }

    #[test]
    fn a_dts_must_be_version_24_and_fit_its_file() {
        assert_eq!(check_dts(&dts(24, 8, [1, 1, 0, 1])), Verdict::Ok);
        assert!(matches!(
            check_dts(&dts(23, 8, [1, 1, 0, 1])),
            Verdict::Bad(_)
        ));
        let mut cut = dts(24, 8, [1, 1, 0, 1]);
        cut.truncate(30);
        assert!(matches!(check_dts(&cut), Verdict::Bad(_)));
        assert!(matches!(
            check_dts(&dts(24, 8, [-1, 1, 0, 1])),
            Verdict::Bad(_)
        ));
        assert!(matches!(
            check_dts(&dts(24, 8, [0x7fff_ffff, 1, 0, 1])),
            Verdict::Bad(_)
        ));
        let mut huge = dts(24, 8, [1, 1, 0, 1]);
        huge[4..8].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
        assert!(matches!(check_dts(&huge), Verdict::Bad(_)));
    }

    #[test]
    fn textures_are_checked_by_their_header() {
        assert_eq!(
            check_texture("a.png", &png(256, 256, 8, 2), true),
            Verdict::Ok
        );
        assert!(matches!(
            check_texture("a.png", &png(30000, 30000, 8, 2), false),
            Verdict::Bad(_)
        ));
        assert!(matches!(
            check_texture("a.png", &png(65536, 1, 8, 2), false),
            Verdict::Bad(_)
        ));
        assert!(matches!(
            check_texture("a.png", &png(64, 64, 16, 2), false),
            Verdict::Warn(_)
        ));
        assert!(matches!(
            check_texture("a.png", &png(2048, 64, 8, 2), false),
            Verdict::Warn(_)
        ));
        assert!(matches!(
            check_texture("a.png", &png(128, 128, 8, 2), true),
            Verdict::Warn(_)
        ));
        assert!(matches!(
            check_texture("a.bmp", b"BM....", false),
            Verdict::Warn(_)
        ));
        assert_eq!(check_texture("a.jpg", b"\xff\xd8\xff", false), Verdict::Ok);
    }

    #[test]
    fn bad_shapes_are_left_out_unless_warn() {
        let text = "object Good good.3ds 0 0 0\nobject Bad bad.3ds 0 0 0\nplayershape bad.3ds\ntgeplayershape bad.dts\nsky sky_fr 500\n";
        let files = |n: &str| -> Option<(String, Vec<u8>)> {
            match n {
                "good.3ds" => Some((n.into(), three_ds(3, &[[0, 1, 2]], true))),
                "bad.3ds" => Some((n.into(), three_ds(3, &[[0, 1, 2]], false))),
                "bad.dts" => Some((n.into(), dts(23, 8, [0; 4]))),
                "sky_fr.png" => Some((n.into(), png(30000, 30000, 8, 2))),
                _ => None,
            }
        };
        let (mut scene, _) = Scene::parse(text);
        let notes = check_scene(&mut scene, &files, None, ShapeCheck::Skip);
        let objects = scene
            .objects
            .iter()
            .filter(|o| matches!(o, Ghost::GameObject(_)))
            .count();
        assert_eq!(objects, 1, "{notes:?}");
        assert!(scene.player_shape.is_empty());
        assert!(scene.tge_player_shape.is_empty());
        assert!(notes
            .iter()
            .any(|n| n.contains("sky_fr.png") && n.contains("ERROR")));

        let (mut scene, _) = Scene::parse(text);
        check_scene(&mut scene, &files, None, ShapeCheck::Warn);
        assert_eq!(scene.objects.len(), 3);
        assert!(!scene.player_shape.is_empty());
    }

    #[test]
    fn grass_takes_a_texture_not_a_shape() {
        let (mut scene, errors) = Scene::parse(
            "replicator DynamicGrass 0 0 0 4 1 3 grass.3ds,blades.png
",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let none = |_: &str| -> Option<(String, Vec<u8>)> { None };
        let notes = check_scene(&mut scene, &none, None, ShapeCheck::Skip);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("grass.3ds is a shape"), "{notes:?}");
        assert!(notes[1].contains("texture blades.png"), "{notes:?}");
        assert_eq!(scene.objects.len(), 1);
    }

    #[test]
    fn a_terrain_needs_its_heights_info_and_textures() {
        let (mut scene, _) = Scene::parse("terrain world/t\n");
        let none = |_: &str| -> Option<(String, Vec<u8>)> { None };
        let notes = check_scene(&mut scene, &none, None, ShapeCheck::Skip);
        assert!(notes.iter().any(|n| n.contains("heights.info")));
        let notes = check_scene(
            &mut scene,
            &none,
            Some(b"g3d_grass.png\ngrass\n".to_vec()),
            ShapeCheck::Skip,
        );
        assert_eq!(notes.len(), 3, "{notes:?}");
    }
}
