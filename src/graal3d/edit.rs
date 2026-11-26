//! In-game 3D world editing (`graal3d_edit`): a staff client's script
//! moves, adds or removes a world object with a `g3dedit` trigger, every
//! client in that world sees the change, and the world file is saved.
//!
//! The 5.12 client has Torque's `WorldEditor` and `TerrainEditor`, and a
//! `weapon3d` can create them, but they work only on the client's own
//! objects and save nothing; against a remote server their sync does
//! nothing (Graal_analysis `torque/3d-batch-w-rows-2026-11-26.md`,
//! T3D-126). So the client's editor script reports what it changed with
//! `triggerserver("gui", <weapon>, "g3dedit", ...)` and the server applies
//! and keeps it:
//!
//! ```text
//! g3dedit,move,<name>,<x>,<y>,<z>[,<axis x>,<axis y>,<axis z>,<angle>]
//! g3dedit,add,<name>,<shape>,<x>,<y>,<z>[,<axis x>,<axis y>,<axis z>,<angle>]
//! g3dedit,remove,<name>
//! g3dedit,list
//! ```
//!
//! The reply is the flag `clientr.g3dedit` (`ok <op> <name>` or
//! `error <why>`); `list` sets `clientr.g3dedit_objects` to every object of
//! the world as `name:shape:x:y:z:ax:ay:az:angle`, space separated.
//!
//! The world file is edited line by line ([`edit_text`]): only the
//! object's own line changes (its comment and any scale stay), a new one
//! goes after the last object line, and everything else is kept as
//! written. The old file is kept as `<file>.bak-<unix seconds>` and the new
//! one written through a temporary file and a rename.

use std::path::Path;

/// One edit from a client.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    Move {
        name: String,
        position: [f32; 3],
        /// Axis and angle (radians); `None` keeps the object's turn.
        turn: Option<[f32; 4]>,
    },
    Add {
        name: String,
        shape: String,
        position: [f32; 3],
        turn: Option<[f32; 4]>,
    },
    Remove {
        name: String,
    },
    List,
}

impl Edit {
    /// The object it names (`None` for `list`).
    pub fn name(&self) -> Option<&str> {
        match self {
            Edit::Move { name, .. } | Edit::Add { name, .. } | Edit::Remove { name } => Some(name),
            Edit::List => None,
        }
    }

    pub fn op(&self) -> &'static str {
        match self {
            Edit::Move { .. } => "move",
            Edit::Add { .. } => "add",
            Edit::Remove { .. } => "remove",
            Edit::List => "list",
        }
    }
}

/// A name or shape: one word a world file line can hold, and that the
/// list reply can carry.
fn word(s: &str, what: &str) -> Result<String, String> {
    if s.is_empty()
        || s.len() > 128
        || s.chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '#' | ',' | ':' | '"'))
    {
        return Err(format!("bad {what} `{s}`"));
    }
    Ok(s.to_string())
}

fn floats<const N: usize>(w: &[&str]) -> Result<[f32; N], String> {
    let mut out = [0.0; N];
    for (o, s) in out.iter_mut().zip(w) {
        *o = s
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite() && v.abs() < 1.0e6)
            .ok_or_else(|| format!("`{s}` is not a number"))?;
    }
    Ok(out)
}

/// Read the fields after `g3dedit` (the trigger's own fields, already
/// split on commas and unquoted).
pub fn parse(fields: &[&str]) -> Result<Edit, String> {
    let (op, rest) = fields.split_first().ok_or("no edit")?;
    let place = |w: &[&str]| -> Result<([f32; 3], Option<[f32; 4]>), String> {
        match w.len() {
            3 => Ok((floats::<3>(w)?, None)),
            7 => Ok((floats::<3>(&w[..3])?, Some(floats::<4>(&w[3..])?))),
            _ => Err("needs x,y,z and maybe an axis and angle".into()),
        }
    };
    match (*op, rest.len()) {
        ("list", 0) => Ok(Edit::List),
        ("remove", 1) => Ok(Edit::Remove {
            name: word(rest[0], "name")?,
        }),
        ("move", n) if n >= 1 => {
            let (position, turn) = place(&rest[1..])?;
            Ok(Edit::Move {
                name: word(rest[0], "name")?,
                position,
                turn,
            })
        }
        ("add", n) if n >= 2 => {
            let (position, turn) = place(&rest[2..])?;
            let shape = word(rest[1], "shape")?;
            let lower = shape.to_ascii_lowercase();
            if !(lower.ends_with(".3ds") || lower.ends_with(".dts") || lower.ends_with(".dif")) {
                return Err(format!("{shape} is not a .3ds, .dts or .dif"));
            }
            Ok(Edit::Add {
                name: word(rest[0], "name")?,
                shape,
                position,
                turn,
            })
        }
        _ => Err(format!("`{}` is not an edit", fields.join(","))),
    }
}

/// A number as the world file writes it: up to four decimals, no trailing
/// zeros.
fn num(v: f32) -> String {
    let mut s = format!("{v:.4}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

/// The object words of a line (after `object`, or after `objectfor
/// <accounts>`), and where they start in its word list.
fn object_words(line: &str) -> Option<(usize, Vec<&str>)> {
    let body = line.split('#').next().unwrap_or("");
    let w: Vec<&str> = body.split_whitespace().collect();
    match w.first().copied() {
        Some("object") => Some((1, w)),
        Some("objectfor") if w.len() >= 3 => Some((2, w)),
        _ => None,
    }
}

/// Apply `edit` to a world file's text. Lines other than the object's own
/// stay as written.
pub fn edit_text(text: &str, edit: &Edit) -> Result<String, String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let find = |lines: &[String], name: &str| {
        lines
            .iter()
            .position(|l| object_words(l).is_some_and(|(at, w)| w.get(at) == Some(&name)))
    };
    match edit {
        Edit::List => return Ok(text.to_string()),
        Edit::Move {
            name,
            position,
            turn,
        } => {
            let i = find(&lines, name).ok_or_else(|| format!("no object line for {name}"))?;
            let line = &lines[i];
            let (at, w) = object_words(line).expect("found above");
            let indent = &line[..line.len() - line.trim_start().len()];
            let comment = line.find('#').map(|c| {
                let before = line[..c].trim_end().len();
                &line[before..]
            });
            let mut out: Vec<String> = w[..at + 2].iter().map(|s| s.to_string()).collect();
            out.extend(position.iter().map(|v| num(*v)));
            let old_turn: Vec<String> = w
                .get(at + 5..at + 9)
                .map(|t| t.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            let scale: Vec<String> = w
                .get(at + 9..)
                .map(|t| t.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            match turn {
                Some(t) => out.extend(t.iter().map(|v| num(*v))),
                None => out.extend(old_turn.iter().cloned()),
            }
            if !scale.is_empty() {
                if out.len() == at + 5 {
                    // A scale needs a turn before it.
                    out.extend(["0", "0", "1", "0"].map(String::from));
                }
                out.extend(scale);
            }
            lines[i] = format!("{indent}{}{}", out.join(" "), comment.unwrap_or(""));
        }
        Edit::Add {
            name,
            shape,
            position,
            turn,
        } => {
            if find(&lines, name).is_some() {
                return Err(format!("there is already an object {name}"));
            }
            let mut out = vec!["object".to_string(), name.clone(), shape.clone()];
            out.extend(position.iter().map(|v| num(*v)));
            if let Some(t) = turn {
                out.extend(t.iter().map(|v| num(*v)));
            }
            let after = lines
                .iter()
                .rposition(|l| object_words(l).is_some())
                .map_or(lines.len(), |i| i + 1);
            lines.insert(after, out.join(" "));
        }
        Edit::Remove { name } => {
            let i = find(&lines, name).ok_or_else(|| format!("no object line for {name}"))?;
            lines.remove(i);
        }
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') || text.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

/// Write `text` to `path`: the old file kept as `<file>.bak-<unix
/// seconds>`, the new one through a temporary file and a rename. Returns
/// the backup's path.
pub fn save(path: &Path, text: &str) -> std::io::Result<Option<std::path::PathBuf>> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let backup = if path.exists() {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut b = dir.join(format!("{name}.bak-{secs}"));
        let mut n = 2;
        while b.exists() {
            b = dir.join(format!("{name}.bak-{secs}-{n}"));
            n += 1;
        }
        std::fs::copy(path, &b)?;
        Some(b)
    } else {
        None
    };
    let tmp = dir.join(format!(".{name}.g3dedit.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(backup)
}

/// One object for the `list` reply.
pub fn list_entry(name: &[u8], shape: &[u8], t: &super::objects::Transform) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}",
        String::from_utf8_lossy(name),
        String::from_utf8_lossy(shape),
        num(t.position[0]),
        num(t.position[1]),
        num(t.position[2]),
        num(t.axis[0]),
        num(t.axis[1]),
        num(t.axis[2]),
        num(t.angle)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORLD: &str = "\
# The inn and its yard.
sun 0.57 0.57 -0.57 0.8 0.8 0.8 1 0.3 0.3 0.35 1
object inn inn.3ds 4 4 0   # the inn
objectfor one,two door door.3ds 2 2 0 0 0 1 1.5 2 2 1
object tree tree.3ds 10 -3 0
spawn 0 0 1
";

    #[test]
    fn parses_edits() {
        assert_eq!(parse(&["list"]), Ok(Edit::List));
        assert_eq!(
            parse(&["move", "inn", "1", "2", "3"]),
            Ok(Edit::Move {
                name: "inn".into(),
                position: [1.0, 2.0, 3.0],
                turn: None
            })
        );
        assert_eq!(
            parse(&["add", "rock2", "rock.3ds", "1", "2", "3", "0", "0", "1", "0.5"]),
            Ok(Edit::Add {
                name: "rock2".into(),
                shape: "rock.3ds".into(),
                position: [1.0, 2.0, 3.0],
                turn: Some([0.0, 0.0, 1.0, 0.5])
            })
        );
        for bad in [
            &["move", "inn", "1", "2"][..],
            &["move", "inn", "1", "2", "x"],
            &["move", "inn", "1", "2", "inf"],
            &["add", "a b", "r.3ds", "1", "2", "3"],
            &["add", "a", "r.png", "1", "2", "3"],
            &["add", "a:b", "r.3ds", "1", "2", "3"],
            &["remove"],
            &["delete", "x"],
            &[],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn move_keeps_the_rest_of_the_line_and_file() {
        let e = parse(&["move", "inn", "5", "4.25", "0"]).unwrap();
        let t = edit_text(WORLD, &e).unwrap();
        assert!(
            t.contains("\nobject inn inn.3ds 5 4.25 0   # the inn\n"),
            "{t}"
        );
        assert_eq!(t.lines().count(), WORLD.lines().count());
        let changed = t.lines().zip(WORLD.lines()).filter(|(a, b)| a != b).count();
        assert_eq!(changed, 1);

        // An objectfor keeps its accounts, its turn when none is given, its scale.
        let e = parse(&["move", "door", "3", "2", "0"]).unwrap();
        let t = edit_text(WORLD, &e).unwrap();
        assert!(t.contains("objectfor one,two door door.3ds 3 2 0 0 0 1 1.5 2 2 1\n"));
        let e = parse(&["move", "door", "3", "2", "0", "0", "0", "1", "3"]).unwrap();
        let t = edit_text(WORLD, &e).unwrap();
        assert!(t.contains("objectfor one,two door door.3ds 3 2 0 0 0 1 3 2 2 1\n"));
    }

    #[test]
    fn add_and_remove() {
        let e = parse(&["add", "rock", "rock.3ds", "1", "1", "0"]).unwrap();
        let t = edit_text(WORLD, &e).unwrap();
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[5], "object rock rock.3ds 1 1 0");
        assert_eq!(lines[6], "spawn 0 0 1");
        assert!(edit_text(&t, &e).is_err());
        let t = edit_text(&t, &parse(&["remove", "tree"]).unwrap()).unwrap();
        assert!(!t.contains("tree.3ds"));
        assert!(edit_text(&t, &parse(&["remove", "tree"]).unwrap()).is_err());
        assert_eq!(
            edit_text("", &parse(&["add", "a", "a.3ds", "0", "0", "0"]).unwrap()).unwrap(),
            "object a a.3ds 0 0 0\n"
        );
    }

    #[test]
    fn the_example_editor_weapon_compiles() {
        let src = include_str!("../../docs/examples/g3deditor.gs2");
        let code = crate::graal3d::service::compile_weapon3d(src).expect("compiles");
        assert!(!code.is_empty());
        for action in ["\"list\"", "\"move\"", "\"add\"", "\"remove\""] {
            assert!(src.contains(action), "{action}");
        }
    }

    #[test]
    fn save_keeps_a_backup() {
        let d = std::env::temp_dir().join(format!("seihai-gserver-g3dedit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = d.join("config/graal3dworld.txt");
        assert_eq!(save(&p, "spawn 0 0 1\n").unwrap(), None);
        let b = save(&p, "spawn 1 1 1\n").unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "spawn 0 0 1\n");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "spawn 1 1 1\n");
        let b2 = save(&p, "spawn 2 2 2\n").unwrap().unwrap();
        assert_ne!(b, b2);
        let _ = std::fs::remove_dir_all(&d);
    }
}
