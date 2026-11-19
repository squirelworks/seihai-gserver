//! `config/graal3dworld.txt`: what the server ghosts to every 3D client.
//!
//! One entry per line, words separated by spaces, `#` starts a comment:
//!
//! ```text
//! object <name> <shape> <x> <y> <z> [<axis x> <axis y> <axis z> <angle>]
//! sun <dir x> <dir y> <dir z> <r> <g> <b> <a> <ambient r> <g> <b> <a>
//! sky <textures> [<visible distance> [<fog r> <g> <b>]]
//!                           six names, comma separated, no spaces ("-" for none)
//! skyoptions <b1> <b2> <b3> <solid r> <g> <b>
//!                           the Sky's three unnamed bools (0/1) and solid colour
//! playershape <shape>       the shape of every player's own object
//! spawn <x> <y> <z>         where a player's object starts
//! camera <x> <y> <z>        give each client a Camera this far from the spawn
//! tgecontrol player|camera  a protocol 19 client's control object (player)
//! tgeplayershape <file>     the protocol 19 Player's shape, a TGE .dts; without
//!                           one those clients get the Camera
//! weapon3d <name> <file>    a script the client runs in 3D mode, sent over the
//!                           text channel (addweapon, weaponscript)
//! ```

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
    /// Relative to the world folder.
    pub file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    /// The Sun and Sky, if any, are among them.
    pub objects: Vec<Ghost>,
    pub player_shape: Vec<u8>,
    pub spawn: Transform,
    pub camera_offset: Option<[f32; 3]>,
    pub tge_control: TgeControl,
    pub tge_player_shape: Vec<u8>,
    pub weapons: Vec<Weapon3d>,
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
            let w: Vec<&str> = line.split_whitespace().collect();
            let Some(first) = w.first() else {
                continue;
            };
            let ok = match (*first, w.len()) {
                ("object", 6 | 10) => {
                    let mut o = GameObject {
                        name: w[1].as_bytes().to_vec(),
                        shape: w[2].as_bytes().to_vec(),
                        ..GameObject::default()
                    };
                    let pos = floats::<3>(&w[3..6]);
                    let rot = if w.len() == 10 {
                        floats::<4>(&w[6..10])
                    } else {
                        Some([0.0, 0.0, 1.0, 0.0])
                    };
                    match (pos, rot) {
                        (Some(p), Some(r)) => {
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
                ("spawn", 4) => match floats::<3>(&w[1..]) {
                    Some(p) => {
                        s.spawn.position = p;
                        true
                    }
                    None => false,
                },
                ("camera", 4) => match floats::<3>(&w[1..]) {
                    Some(p) => {
                        s.camera_offset = Some(p);
                        true
                    }
                    None => false,
                },
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
                ("weapon3d", 3) => {
                    let file = PathBuf::from(w[2]);
                    let safe = file
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_)));
                    if safe {
                        s.weapons.push(Weapon3d {
                            name: w[1].to_string(),
                            file,
                        });
                    }
                    safe
                }
                _ => false,
            };
            if !ok {
                errors.push(format!("line {}: {}", n + 1, raw.trim()));
            }
        }
        (s, errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(s.tge_control, TgeControl::Camera);
        assert_eq!(s.tge_player_shape, b"player.dts");
        assert_eq!(s.weapons[0].name, "-walk3d");
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
