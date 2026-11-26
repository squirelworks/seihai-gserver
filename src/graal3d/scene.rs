//! `config/graal3dworld.txt`: what the server ghosts to every 3D client.
//!
//! One entry per line, words separated by spaces, `#` starts a comment:
//!
//! ```text
//! object <name> <shape> <x> <y> <z> [<axis x> <axis y> <axis z> <angle> [<sx> <sy> <sz>]]
//! objectfor <accounts> <name> <shape> <x> <y> <z> ...
//!                           an object only those accounts (comma
//!                           separated) are sent; a client script can also
//!                           hide or show any object for its own player
//!                           (`g3dhide`, `g3dshow`)
//! sun <dir x> <dir y> <dir z> <r> <g> <b> <a> <ambient r> <g> <b> <a>
//! sky <textures> [<visible distance> [<fog r> <g> <b>]]
//!                           six names, comma separated, no spaces ("-" for none)
//! skyoptions <b1> <b2> <b3> <solid r> <g> <b>
//!                           the Sky's three unnamed bools (0/1) and solid colour
//! playershape <shape>       the shape of every player's own object
//! movetype interpolate|rigid|list
//!                           lab: give every player's object a movetype, so a
//!                           server move (g3dpos) glides there (interpolate,
//!                           rigid) or jumps (list) on 5.12; default none
//! spawn <x> <y> <z>         where a player's object starts
//! camera <x> <y> <z> [<axis x> <axis y> <axis z> <angle>]
//!                           where each client's Camera sits from the spawn
//!                           (protocols 21 and 24; default 0 -5 2), and how it
//!                           turns; without a rotation it faces the spawn point
//! tgecontrol player|camera  a protocol 19 client's control object (player)
//! tgeplayershape <file>     the protocol 19 Player's shape, a TGE .dts; without
//!                           one those clients get the Camera
//! weapon3d <name> [<file>]  a script the client runs in 3D mode, sent over the
//!                           text channel (addweapon, weaponscript): the
//!                           file's text, or without one the server's weapon
//!                           of that name (its clientside part)
//! terrain <folder> [<square size>] [repeat]
//!                           streamed terrain (protocols 21 and 24): the folder
//!                           of heights<x>-<y>.dat chunks; square size 1 to 64
//!                           metres (default 8)
//! water <x> <y> <z> <sx> <sy> <sz> [<surface texture> [<liquid type>]]
//!                           a WaterBlock from its corner; the surface is at
//!                           z + sz (protocols 21 and 24)
//! replicator <class> <x> <y> <z> <radius> <min> <max> <shapes> [<seed> [<x0> <y0> <x1> <y1>]]
//!                           ground cover (protocols 21 and 24): class
//!                           DynamicShapeReplicator, DynamicGrass or
//!                           DynamicGrassReplicator; radius in 8-unit cells
//!                           (1 to 64), min and max objects a cell (0 to 20;
//!                           0 to 100 for the grass classes), shapes comma
//!                           separated (for the grass classes textures), the
//!                           box the cover may grow in (default: anywhere);
//!                           `scale=<n>` or `scale=<min>,<max>` anywhere after
//!                           the class sizes the copies (default 1, grass 3)
//! rawobject <class> <fields...>
//!                           lab: a ghost of any class whose update is the
//!                           fields (see [`super::raw`]), to every client
//!                           whose class table has the class
//! rawevent [late] <class> <fields...>
//!                           lab: one guaranteed event of the class with the
//!                           fields as its payload, when the world is
//!                           ghosted, or with `late` after the mission start
//! ```

use super::ground::{Replicator, Terrain, Water, ANYWHERE, MAX_SQUARE_SIZE, REPLICATOR_CLASSES};
use super::objects::{GameObject, Ghost, Sky, Sun, Transform};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TgeControl {
    #[default]
    Player,
    Camera,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Weapon3d {
    pub name: String,
    /// Relative to the world folder; `None`: the server's weapon.
    pub file: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    /// The Sun and Sky, if any, are among them.
    pub objects: Vec<Ghost>,
    pub player_shape: Vec<u8>,
    /// `movetype`: the players' objects' movetype (empty: none).
    pub player_movetype: Vec<u8>,
    pub spawn: Transform,
    pub camera_offset: Option<[f32; 3]>,
    /// An axis and an angle in radians; `None` faces the spawn point.
    pub camera_rotation: Option<[f32; 4]>,
    pub tge_control: TgeControl,
    pub tge_player_shape: Vec<u8>,
    pub weapons: Vec<Weapon3d>,
    /// The `terrain` line; its chunks are read by [`Scene::load_terrain`].
    pub terrain: Option<Terrain>,
    /// Lab: the `rawevent` lines.
    pub raw_events: Vec<super::raw::RawEvent>,
    /// `objectfor`: the index in `objects` of each object only some
    /// players see, and their lower-case accounts.
    pub only: std::collections::BTreeMap<usize, Vec<String>>,
    /// The files `graal3d_pushterrain` sends for this world's terrain
    /// ([`super::assets::Checked::terrain_files`]).
    pub push_files: Vec<String>,
    /// The file it was read from, which `g3dedit` saves to
    /// ([`super::edit`]); `None` for a world made in code.
    pub file: Option<PathBuf>,
}

fn floats<const N: usize>(words: &[&str]) -> Option<[f32; N]> {
    if words.len() < N {
        return None;
    }
    let mut out = [0.0; N];
    for (o, w) in out.iter_mut().zip(words) {
        *o = w.parse().ok().filter(|f: &f32| f.is_finite())?;
    }
    Some(out)
}

impl Scene {
    /// The scene and one error per line not understood.
    pub fn parse(text: &str) -> (Scene, Vec<String>) {
        let mut s = Scene::default();
        let mut errors = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("");
            let mut w: Vec<&str> = line.split_whitespace().collect();
            // `objectfor <accounts> <object words>`: an object line seen
            // only by those accounts.
            let mut only = None;
            if w.first() == Some(&"objectfor") && w.len() >= 3 {
                only = Some(
                    w[1].split(',')
                        .filter(|a| !a.is_empty())
                        .map(str::to_ascii_lowercase)
                        .collect::<Vec<_>>(),
                );
                w.drain(..2);
                w.insert(0, "object");
            }
            // `replicator ... scale=<n>` or `scale=<min>,<max>`: the copies'
            // size, anywhere after the class.
            let mut rep_scale: Option<Option<[f32; 2]>> = None;
            if w.first() == Some(&"replicator") {
                w.retain(|t| match t.strip_prefix("scale=") {
                    Some(v) => {
                        let n: Vec<f32> = v.split(',').filter_map(|x| x.parse().ok()).collect();
                        rep_scale = Some(match n[..] {
                            [a] if a > 0.0 => Some([a, a]),
                            [a, b] if a > 0.0 && b >= a => Some([a, b]),
                            _ => None,
                        });
                        false
                    }
                    None => true,
                });
            }
            let objects_before = s.objects.len();
            let Some(first) = w.first() else {
                continue;
            };
            let ok = match (*first, w.len()) {
                ("object", 6 | 10 | 13) => {
                    let mut o = GameObject {
                        name: w[1].as_bytes().to_vec(),
                        shape: w[2].as_bytes().to_vec(),
                        ..GameObject::default()
                    };
                    let pos = floats::<3>(&w[3..6]);
                    let rot = if w.len() >= 10 {
                        floats::<4>(&w[6..10])
                    } else {
                        Some([0.0, 0.0, 1.0, 0.0])
                    };
                    // GameObject section 2 is the scale: the Linux 4.20 drew
                    // (2, 2, 2) twice the size (lab, 2026-11-25).
                    let scale = if w.len() == 13 {
                        floats::<3>(&w[10..13])
                    } else {
                        Some([1.0; 3])
                    };
                    match (pos, rot, scale) {
                        (Some(p), Some(r), Some(sc)) => {
                            o.scale = sc;
                            o.transform = Transform {
                                position: p,
                                axis: [r[0], r[1], r[2]],
                                angle: r[3],
                            };
                            s.objects.push(Ghost::GameObject(o));
                            true
                        }
                        _ => false,
                    }
                }
                ("sun", 12) => match floats::<11>(&w[1..]) {
                    Some(v) => {
                        s.objects.push(Ghost::Sun(Sun {
                            direction: [v[0], v[1], v[2]],
                            color: [v[3], v[4], v[5], v[6]],
                            ambient: [v[7], v[8], v[9], v[10]],
                            ..Sun::default()
                        }));
                        true
                    }
                    None => false,
                },
                ("sky", 2 | 3 | 6) => {
                    let mut sky = Sky::default();
                    if w[1] != "-" {
                        sky.material_list = w[1].as_bytes().to_vec();
                    }
                    let mut ok = true;
                    if w.len() >= 3 {
                        match floats::<1>(&w[2..3]) {
                            Some([d]) => sky.visible_distance = d,
                            None => ok = false,
                        }
                    }
                    if w.len() == 6 {
                        match floats::<3>(&w[3..6]) {
                            Some(f) => sky.fog_color = f,
                            None => ok = false,
                        }
                    }
                    if ok {
                        s.objects.push(Ghost::Sky(sky));
                    }
                    ok
                }
                ("skyoptions", 7) => {
                    let v = floats::<6>(&w[1..]);
                    let sky = s.objects.iter_mut().rev().find_map(|o| match o {
                        Ghost::Sky(sky) => Some(sky),
                        _ => None,
                    });
                    match (v, sky) {
                        (Some(v), Some(sky)) => {
                            for (b, f) in sky.bools.iter_mut().zip(&v[..3]) {
                                *b = u8::from(*f != 0.0);
                            }
                            sky.solid_color = [v[3], v[4], v[5]];
                            true
                        }
                        _ => false,
                    }
                }
                ("playershape", 2) => {
                    s.player_shape = w[1].as_bytes().to_vec();
                    true
                }
                ("movetype", 2) => match w[1] {
                    "interpolate" | "rigid" | "list" => {
                        s.player_movetype = w[1].as_bytes().to_vec();
                        true
                    }
                    _ => false,
                },
                ("spawn", 4) => match floats::<3>(&w[1..]) {
                    Some(p) => {
                        s.spawn.position = p;
                        true
                    }
                    None => false,
                },
                ("camera", 4 | 8) => {
                    let rot = if w.len() == 8 {
                        floats::<4>(&w[4..8]).map(Some)
                    } else {
                        Some(None)
                    };
                    match (floats::<3>(&w[1..4]), rot) {
                        (Some(p), Some(r)) => {
                            s.camera_offset = Some(p);
                            s.camera_rotation = r;
                            true
                        }
                        _ => false,
                    }
                }
                ("tgecontrol", 2) => match w[1] {
                    "player" => {
                        s.tge_control = TgeControl::Player;
                        true
                    }
                    "camera" => {
                        s.tge_control = TgeControl::Camera;
                        true
                    }
                    _ => false,
                },
                ("tgeplayershape", 2) => {
                    s.tge_player_shape = w[1].as_bytes().to_vec();
                    true
                }
                ("terrain", 2..=4) => {
                    let mut size = 8;
                    let mut repeat = false;
                    let mut ok = true;
                    for word in &w[2..] {
                        if *word == "repeat" {
                            repeat = true;
                        } else {
                            match word.parse::<u32>() {
                                Ok(n) if (1..=MAX_SQUARE_SIZE).contains(&n) => size = n,
                                _ => ok = false,
                            }
                        }
                    }
                    let safe = std::path::Path::new(w[1])
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)));
                    if ok && safe {
                        // One TerrainBlock a world: 5.12 lays every chunk on
                        // the last block only, so a later line replaces it.
                        if s.terrain.is_some() {
                            errors.push(format!(
                                "line {}: a second terrain line replaces the first (one TerrainBlock a world)",
                                n + 1
                            ));
                        }
                        s.terrain = Some(Terrain::new(w[1], size, repeat));
                    }
                    ok && safe
                }
                ("water", 7..=9) => {
                    let liquid = match w.get(8) {
                        Some(t) => t.parse::<u32>().ok(),
                        None => Some(0),
                    };
                    match (floats::<6>(&w[1..7]), liquid) {
                        (Some(v), Some(liquid_type)) => {
                            s.objects.push(Ghost::Water(Water {
                                position: [v[0], v[1], v[2]],
                                scale: [v[3], v[4], v[5]],
                                surface_texture: w
                                    .get(7)
                                    .map_or_else(Vec::new, |t| t.as_bytes().to_vec()),
                                liquid_type,
                            }));
                            true
                        }
                        _ => false,
                    }
                }
                ("replicator", 9 | 10 | 14) => {
                    let class = REPLICATOR_CLASSES
                        .iter()
                        .find(|c| c.eq_ignore_ascii_case(w[1]))
                        .copied();
                    let small = |t: &str, lo: u8, hi: u8| {
                        t.parse::<u8>().ok().filter(|v| (lo..=hi).contains(v))
                    };
                    let seed = match w.get(9) {
                        Some(t) => t.parse::<u32>().ok(),
                        None => Some(0),
                    };
                    let area = if w.len() == 14 {
                        let v: Vec<i16> = w[10..14].iter().filter_map(|t| t.parse().ok()).collect();
                        <[i16; 4]>::try_from(v).ok()
                    } else {
                        Some(ANYWHERE)
                    };
                    let count = s
                        .objects
                        .iter()
                        .filter(|o| matches!(o, Ghost::Replicator(_)))
                        .count();
                    // The grass classes take up to 100 a cell.
                    let most = if class == Some("DynamicShapeReplicator") {
                        20
                    } else {
                        100
                    };
                    // Grass quads at scale 1 are about 0.6 units: too small to
                    // see from a player's height; 3 drew on a real 5.12.
                    let scale = match rep_scale {
                        None if most == 100 => Some([3.0, 3.0]),
                        None => Some([1.0, 1.0]),
                        Some(v) => v,
                    };
                    match (
                        class,
                        floats::<3>(&w[2..5]),
                        small(w[5], 1, 64),
                        small(w[6], 0, most),
                        small(w[7], 0, most),
                        seed,
                        area,
                        scale,
                    ) {
                        (
                            Some(class),
                            Some(position),
                            Some(radius),
                            Some(min),
                            Some(max),
                            Some(seed),
                            Some(area),
                            Some(scale),
                        ) => {
                            s.objects.push(Ghost::Replicator(Replicator {
                                class,
                                name: format!("G3DReplicator{}", count + 1).into_bytes(),
                                position,
                                radius,
                                min: min.min(max),
                                max: max.max(min),
                                shapes: w[8].as_bytes().to_vec(),
                                seed,
                                area,
                                scale,
                            }));
                            true
                        }
                        _ => false,
                    }
                }
                ("weapon3d", 2 | 3) => {
                    let file = w.get(2).map(PathBuf::from);
                    let safe = file.as_ref().is_none_or(|f| {
                        f.components()
                            .all(|c| matches!(c, std::path::Component::Normal(_)))
                    });
                    if safe {
                        s.weapons.push(Weapon3d {
                            name: w[1].to_string(),
                            file,
                        });
                    }
                    safe
                }
                ("rawobject", 2..) => match super::raw::parse_fields(&w[2..]) {
                    Ok(fields) => {
                        s.objects.push(Ghost::Raw(super::raw::RawObject {
                            class: w[1].to_string(),
                            fields,
                        }));
                        true
                    }
                    Err(e) => {
                        errors.push(format!("line {}: {e}", n + 1));
                        continue;
                    }
                },
                ("rawevent", 2..) => {
                    let late = w[1] == "late";
                    let at = usize::from(late) + 1;
                    match w
                        .get(at)
                        .map(|c| (c, super::raw::parse_fields(&w[at + 1..])))
                    {
                        Some((class, Ok(fields))) => {
                            s.raw_events.push(super::raw::RawEvent {
                                class: class.to_string(),
                                fields,
                                late,
                            });
                            true
                        }
                        Some((_, Err(e))) => {
                            errors.push(format!("line {}: {e}", n + 1));
                            continue;
                        }
                        None => false,
                    }
                }
                _ => false,
            };
            if let Some(accounts) = only.filter(|_| ok && s.objects.len() > objects_before) {
                s.only.insert(objects_before, accounts);
            }
            if !ok {
                errors.push(format!("line {}: {}", n + 1, raw.trim()));
            }
        }
        (s, errors)
    }

    /// Reads the `terrain` line's chunks from under `world_dir`; one note
    /// per chunk left out.
    pub fn load_terrain(&mut self, world_dir: &std::path::Path) -> Vec<String> {
        match &mut self.terrain {
            Some(t) => {
                let dir = world_dir.join(&t.folder);
                t.load(&dir)
            }
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replicator_takes_a_scale() {
        let scale = |line: &str| match Scene::parse(line).0.objects.first() {
            Some(Ghost::Replicator(r)) => Some(r.scale),
            _ => None,
        };
        assert_eq!(
            scale("replicator DynamicShapeReplicator 0 0 0 4 1 3 a.3ds"),
            Some([1.0, 1.0])
        );
        assert_eq!(
            scale("replicator DynamicGrass 0 0 0 4 1 3 g.png"),
            Some([3.0, 3.0])
        );
        assert_eq!(
            scale("replicator DynamicGrass scale=2 0 0 0 4 1 3 g.png 7"),
            Some([2.0, 2.0])
        );
        assert_eq!(
            scale("replicator DynamicShapeReplicator 0 0 0 4 1 3 a.3ds scale=1,2.5"),
            Some([1.0, 2.5])
        );
        let (_, errors) = Scene::parse("replicator DynamicGrass 0 0 0 4 1 3 g.png scale=0");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn grass_takes_more_a_cell_than_shapes() {
        let (s, errors) = Scene::parse("replicator DynamicGrass 0 0 0 4 50 100 blades.png\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(s.objects.len(), 1);
        let (_, errors) = Scene::parse("replicator DynamicShapeReplicator 0 0 0 4 1 21 a.3ds\n");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn reads_every_kind_of_line() {
        let (s, errors) = Scene::parse(
            "# a lab world\n\
             object house house.dif 1 2 3\n\
             object tower tower.dts 4 5 6 0 0 1 1.5\n\
             sun 0 0 -1 1 1 1 1 0.2 0.2 0.2 1\n\
             sky sky_fr,sky_rt,sky_bk,sky_lf,sky_up,sky_dn 800 0.1 0.2 0.3\n\
             skyoptions 0 1 0 0.5 0.5 0.5\n\
             playershape labcube.3ds\n\
             movetype rigid\n\
             spawn 10 20 1  # on the hill\n\
             camera 0 -5 2\n\
             tgecontrol camera\n\
             tgeplayershape player.dts\n\
             weapon3d -walk3d weapons/walk3d.txt\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(s.objects.len(), 4);
        let Ghost::GameObject(t) = &s.objects[1] else {
            panic!()
        };
        assert_eq!(t.transform.angle, 1.5);
        let Ghost::Sky(sky) = &s.objects[3] else {
            panic!()
        };
        assert_eq!(sky.visible_distance, 800.0);
        assert_eq!(sky.bools, [0, 1, 0]);
        assert_eq!(s.spawn.position, [10.0, 20.0, 1.0]);
        assert_eq!(s.camera_offset, Some([0.0, -5.0, 2.0]));
        assert_eq!(s.camera_rotation, None);
        assert_eq!(s.tge_control, TgeControl::Camera);
        assert_eq!(s.tge_player_shape, b"player.dts");
        assert_eq!(s.player_movetype, b"rigid");
        assert_eq!(s.weapons[0].name, "-walk3d");
        assert_eq!(Scene::parse("movetype walk\n").1.len(), 1);
    }

    #[test]
    fn an_objectfor_line_names_its_accounts() {
        let (s, errors) =
            Scene::parse("object a a.dts 0 0 0\nobjectfor One,two b b.dts 1 1 1\nobjectfor x\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(s.objects.len(), 2);
        assert_eq!(
            s.only.get(&1),
            Some(&vec!["one".to_string(), "two".to_string()])
        );
        assert!(!s.only.contains_key(&0));
    }

    #[test]
    fn a_camera_line_may_turn_the_camera() {
        let (s, errors) = Scene::parse("camera 0 -8 3 1 0 0 -0.3\n");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(s.camera_offset, Some([0.0, -8.0, 3.0]));
        assert_eq!(s.camera_rotation, Some([1.0, 0.0, 0.0, -0.3]));
        let (_, errors) = Scene::parse("camera 0 -8 3 1 0 0\n");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn names_the_lines_it_did_not_take() {
        let (_, errors) = Scene::parse(
            "spawn 1 2\nskyoptions 0 0 0 1 1 1\nweapon3d w ../secret.txt\nobject a b x 0 0\n",
        );
        assert_eq!(errors.len(), 4);
        assert!(errors[0].starts_with("line 1"));
    }
}
