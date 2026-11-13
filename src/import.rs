//! `seihai-gserver import-v2`: make a world folder from a GServer-v2 server
//! folder (roadmap #56). The formats are in `docs/GSERVER-V2-WORLD.md`,
//! written from the fork's sample data files only; nothing here comes from
//! the fork's code.
//!
//! It carries the settings (`config/serveroptions.txt`), the version
//! allow-list, the accounts (with the staff list and bans) and the files
//! under `world/` ([`world_files`]): levels to `levels/`, the rest a
//! client may get to `files/`, by `config/foldersconfig.txt`. The source
//! folder is only read. Nothing in
//! the target world is overwritten: a file that is already there is kept
//! and reported. Every setting and account key that is not carried over is
//! reported too, so the admin can see what to set by hand.

use crate::accounts::{Account, Store};
use crate::bans::{Ban, Bans};
use crate::login::valid_account_name;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

/// What an import did and what it left.
#[derive(Debug, Default)]
pub struct Report {
    /// One line per thing written.
    pub done: Vec<String>,
    /// One line per thing not carried over, and why.
    pub left: Vec<String>,
}

/// `serveroptions.txt` keys carried over, with ours.
const SETTINGS: [(&str, &str); 5] = [
    ("name", "name"),
    ("description", "description"),
    ("serverport", "port"),
    ("maxplayers", "max_players"),
    ("playerlisticons", "status_list"),
];

/// `serveroptions.txt` keys read for something other than a setting.
const READ: [&str; 4] = ["startlevel", "startx", "starty", "staff"];

/// Account keys carried over or read on purpose (`NAME` is the file name).
const ACCOUNT_KEYS: [&str; 29] = [
    "NAME",
    "NICK",
    "LEVEL",
    "X",
    "Y",
    "MAXHP",
    "HP",
    "RUPEES",
    "ARROWS",
    "BOMBS",
    "GLOVEP",
    "SWORDP",
    "SWORD",
    "SHIELDP",
    "SHIELD",
    "HEAD",
    "BODY",
    "COLORS",
    "SPRITE",
    "STATUS",
    "AP",
    "KILLS",
    "DEATHS",
    "EMAIL",
    "COMMENTS",
    "IPRANGE",
    "FOLDERRIGHT",
    "LOCALRIGHTS",
    "BANNED",
];

/// `key = value` lines; `#` lines are comments, and so is a `#` after
/// whitespace in a value. Keys are lower case.
pub fn parse_options(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = match v.find(" #").or_else(|| v.find("\t#")) {
            Some(i) => &v[..i],
            None => v,
        };
        out.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
    out
}

/// An account file: the `GRACC001` header, then `KEY value` lines. `None`
/// without the header. Repeated keys keep every value, in order.
pub fn parse_account(text: &str) -> Option<Vec<(String, String)>> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "GRACC001" {
        return None;
    }
    Some(
        lines
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let l = l.trim_end_matches('\r');
                match l.split_once(' ') {
                    Some((k, v)) => (k.to_string(), v.trim().to_string()),
                    None => (l.trim().to_string(), String::new()),
                }
            })
            .collect(),
    )
}

/// `allowedversions.txt` with `//` comments as ours (`#`).
pub fn convert_allowed(text: &str) -> String {
    let mut out = String::from("# Imported from a GServer-v2 allowedversions.txt.\n");
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let converted = match line.find("//") {
            Some(i) => {
                let (entry, comment) = (line[..i].trim_end(), line[i + 2..].trim());
                match (entry.is_empty(), comment.is_empty()) {
                    (true, true) => "#".to_string(),
                    (true, false) => format!("# {comment}"),
                    (false, true) => entry.to_string(),
                    (false, false) => format!("{entry}  # {comment}"),
                }
            }
            None => line.to_string(),
        };
        out.push_str(&converted);
        out.push('\n');
    }
    out
}

/// A folder right with GServer-v2's `world/levels/` as our `levels/`.
pub fn convert_folder_right(line: &str) -> String {
    let line = line.trim();
    let Some((mode, path)) = line.split_once(char::is_whitespace) else {
        return line.to_string();
    };
    let path = path.trim();
    let path = match path.strip_prefix("world/levels/") {
        Some(rest) => format!("levels/{rest}"),
        None if path == "world/levels" => "levels".to_string(),
        None => path.to_string(),
    };
    format!("{mode} {path}")
}

/// The account `name` from the GServer-v2 `fields`, starting at `start`
/// when it has no level. The keys it did not use go in `unused`, and its
/// ban (if any) is returned.
pub fn convert_account(
    name: &str,
    fields: &[(String, String)],
    start: (&str, f32, f32),
    unused: &mut BTreeSet<String>,
) -> (Account, Option<Ban>) {
    let mut a = Account::new(name, start.0, start.1, start.2);
    let get = |k: &str| {
        fields
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    let num = |k: &str| get(k).and_then(|v| v.parse::<f32>().ok());
    let byte = |k: &str| num(k).map(|v| v.clamp(0.0, 255.0) as u8);
    if let Some(v) = get("NICK").filter(|v| !v.is_empty()) {
        a.nick = v.to_string();
    }
    if let Some(v) = get("LEVEL").filter(|v| !v.is_empty()) {
        a.level = v.to_string();
    }
    if let Some(v) = num("X") {
        a.x = v;
    }
    if let Some(v) = num("Y") {
        a.y = v;
    }
    if let Some(v) = byte("MAXHP") {
        a.max_hp = v.min(20);
    }
    if let Some(v) = num("HP") {
        a.hp = v.clamp(0.0, 20.0);
    }
    if let Some(v) = num("RUPEES") {
        a.rupees = v.max(0.0) as u32;
    }
    if let Some(v) = byte("ARROWS") {
        a.arrows = v.min(99);
    }
    if let Some(v) = byte("BOMBS") {
        a.bombs = v.min(99);
    }
    if let Some(v) = byte("GLOVEP") {
        a.glove = v;
    }
    let image = |k: &str| get(k).map(str::to_string);
    if let Some(p) = byte("SWORDP") {
        a.sword.0 = p;
    }
    if let Some(i) = image("SWORD") {
        a.sword.1 = i;
    }
    if let Some(p) = byte("SHIELDP") {
        a.shield.0 = p;
    }
    if let Some(i) = image("SHIELD") {
        a.shield.1 = i;
    }
    if let Some(v) = image("HEAD").filter(|v| !v.is_empty()) {
        a.head = v;
    }
    if let Some(v) = image("BODY").filter(|v| !v.is_empty()) {
        a.body = v;
    }
    if let Some(v) = get("COLORS") {
        let c: Vec<u8> = v.split(',').filter_map(|c| c.trim().parse().ok()).collect();
        if let Ok(c) = <[u8; 5]>::try_from(c) {
            a.colors = c;
        }
    }
    if let Some(v) = byte("SPRITE") {
        a.sprite = v;
    }
    if let Some(v) = byte("STATUS") {
        a.status = v;
    }
    if let Some(v) = byte("AP") {
        a.alignment = v.min(100);
    }
    if let Some(v) = num("KILLS") {
        a.kills = v.max(0.0) as u32;
    }
    if let Some(v) = num("DEATHS") {
        a.deaths = v.max(0.0) as u32;
    }
    if let Some(v) = get("EMAIL") {
        a.email = v.to_string();
    }
    if let Some(v) = get("COMMENTS").filter(|v| !v.is_empty()) {
        a.comments = vec![v.to_string()];
    }
    if let Some(v) = get("IPRANGE") {
        a.ip_ranges = v
            .split(',')
            .map(str::trim)
            .filter(|r| !r.is_empty() && *r != "0.0.0.0")
            .map(str::to_string)
            .collect();
    }
    a.folder_rights = fields
        .iter()
        .filter(|(k, v)| k == "FOLDERRIGHT" && !v.is_empty())
        .map(|(_, v)| convert_folder_right(v))
        .collect();
    if let Some(v) = get("LOCALRIGHTS").and_then(|v| v.parse().ok()) {
        a.rc_rights = v;
    }
    let ban = get("BANNED")
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&b| b != 0)
        .map(|_| Ban {
            account: name.to_string(),
            until: None,
            admin: "import-v2".to_string(),
            reason: get("BANREASON").unwrap_or_default().to_string(),
            with: String::new(),
        });
    for (k, v) in fields {
        let read = ACCOUNT_KEYS.contains(&k.as_str()) || k == "BANREASON";
        // An empty value of a key we do not read loses nothing.
        if !read && !v.is_empty() {
            unused.insert(k.clone());
        }
    }
    (a, ban)
}

/// Write the world `world` from the GServer-v2 server folder `src`.
pub fn import_v2(src: &Path, world: &Path) -> io::Result<Report> {
    let mut r = Report::default();
    let options_path = src.join("config").join("serveroptions.txt");
    let options = std::fs::read_to_string(&options_path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "{}: {e} (not a GServer-v2 server folder?)",
                options_path.display()
            ),
        )
    })?;
    let options = parse_options(&options);
    let ours = world.join("config").join("server.txt");
    if ours.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists; import into a new folder", ours.display()),
        ));
    }
    std::fs::create_dir_all(world.join("config"))?;

    // Where new players start: serveroptions, else the default account.
    let default = std::fs::read_to_string(src.join("accounts").join("defaultaccount.txt"))
        .ok()
        .and_then(|t| parse_account(&t))
        .unwrap_or_default();
    let from_default = |k: &str| {
        default
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
    };
    let start_level = options
        .get("startlevel")
        .cloned()
        .or_else(|| from_default("LEVEL"))
        .unwrap_or_else(|| "onlinestartlocal.nw".into());
    let coord = |opt: &str, acc: &str, dflt: f32| {
        options
            .get(opt)
            .cloned()
            .or_else(|| from_default(acc))
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(dflt)
    };
    let (start_x, start_y) = (coord("startx", "X", 30.0), coord("starty", "Y", 30.5));

    let mut cfg = String::from(
        "# Imported from a GServer-v2 server folder by `seihai-gserver import-v2`.\n\
         # See docs/GSERVER-V2-WORLD.md; settings not listed here are the defaults.\n",
    );
    for (theirs, mine) in SETTINGS {
        if let Some(v) = options.get(theirs).filter(|v| !v.is_empty()) {
            cfg.push_str(&format!("{mine} = {v}\n"));
        }
    }
    cfg.push_str(&format!(
        "start_level = {start_level}\nstart_x = {start_x}\nstart_y = {start_y}\n"
    ));
    std::fs::write(&ours, cfg)?;
    r.done.push(format!("{}", ours.display()));
    let unread: Vec<&str> = options
        .iter()
        .filter(|(k, v)| {
            !v.is_empty() && !SETTINGS.iter().any(|(t, _)| t == k) && !READ.contains(&k.as_str())
        })
        .map(|(k, _)| k.as_str())
        .collect();
    if !unread.is_empty() {
        r.left.push(format!(
            "serveroptions.txt, not carried over: {}",
            unread.join(", ")
        ));
    }

    // The version allow-list.
    let allowed = world.join("config").join("allowedversions.txt");
    match std::fs::read_to_string(src.join("config").join("allowedversions.txt")) {
        Ok(t) if !allowed.exists() => {
            std::fs::write(&allowed, convert_allowed(&t))?;
            r.done.push(format!("{}", allowed.display()));
        }
        Ok(_) => r.left.push(format!("{} exists; kept", allowed.display())),
        Err(e) => r.left.push(format!("allowedversions.txt: {e}")),
    }

    // Accounts, with the staff list and bans.
    let staff: BTreeSet<String> = options
        .get("staff")
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|n| !n.is_empty() && !n.starts_with('('))
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let store = Store::new(world.join("accounts"));
    let bans = Bans::new(world);
    let mut unused = BTreeSet::new();
    let mut made = 0;
    let mut names: Vec<_> = match std::fs::read_dir(src.join("accounts")) {
        Ok(d) => d.filter_map(Result::ok).map(|e| e.path()).collect(),
        Err(e) => {
            r.left.push(format!("accounts: {e}"));
            Vec::new()
        }
    };
    names.sort();
    let start = (start_level.as_str(), start_x, start_y);
    for path in names {
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".txt"))
        else {
            continue;
        };
        if name.eq_ignore_ascii_case("defaultaccount") {
            continue;
        }
        if !valid_account_name(name) {
            r.left
                .push(format!("account {name}: not a valid name here"));
            continue;
        }
        let Some(fields) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| parse_account(&t))
        else {
            r.left.push(format!("account {name}: not a GRACC001 file"));
            continue;
        };
        if store.load(name)?.is_some() {
            r.left.push(format!("account {name}: exists; kept"));
            continue;
        }
        let (mut a, ban) = convert_account(name, &fields, start, &mut unused);
        a.staff = staff.contains(&name.to_ascii_lowercase());
        store.save(&a)?;
        made += 1;
        if let Some(b) = ban {
            bans.add(b)?;
            r.done.push(format!("ban on {name} (no end)"));
        }
    }
    r.done.push(format!(
        "{made} account(s) in {}, with no password: set one with `seihai-gserver account NAME --password-env VAR`, or use listserver_accounts",
        store.dir().display()
    ));
    for s in &staff {
        if !store.load(s).is_ok_and(|a| a.is_some()) {
            r.left.push(format!("staff {s}: no such account"));
        }
    }
    if !unused.is_empty() {
        r.left.push(format!(
            "account keys not carried over: {}",
            unused.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    let rules = match std::fs::read_to_string(src.join("config").join("foldersconfig.txt")) {
        Ok(t) => parse_folders(&t),
        Err(_) => {
            r.left.push(
                "config/foldersconfig.txt: not read; levels are *.nw, *.graal and *.gmap anywhere in world/, the rest files"
                    .into(),
            );
            default_folders()
        }
    };
    world_files(&src.join("world"), world, &rules, &mut r)?;
    Ok(r)
}

/// What a `foldersconfig.txt` line lets a client have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Level,
    /// `file`, `head`, `body`, `sword` and `shield`: all go to `files/`.
    File,
}

/// `kind pattern` lines; `#` lines are comments. Unknown kinds are
/// skipped.
pub fn parse_folders(text: &str) -> Vec<(Kind, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (kind, pattern) = l.split_once(char::is_whitespace)?;
            let kind = match kind.to_ascii_lowercase().as_str() {
                "level" => Kind::Level,
                "file" | "head" | "body" | "sword" | "shield" => Kind::File,
                _ => return None,
            };
            Some((kind, pattern.trim().replace('\\', "/")))
        })
        .collect()
}

/// Without a folders config: levels by extension anywhere, the rest files.
fn default_folders() -> Vec<(Kind, String)> {
    vec![
        (Kind::Level, "**.nw".into()),
        (Kind::Level, "**.graal".into()),
        (Kind::Level, "**.gmap".into()),
        (Kind::File, "**".into()),
    ]
}

/// Whether `path` (relative, `/` between parts) matches `pattern`, without
/// case: `*` is any run within one folder name, `**` any run across
/// folders (ours, for [`default_folders`]).
pub fn glob(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match p {
            [] => n.is_empty(),
            [b'*', b'*', rest @ ..] => (0..=n.len()).any(|k| go(rest, &n[k..])),
            [b'*', rest @ ..] => (0..=n.len())
                .take_while(|&k| k == 0 || n[k - 1] != b'/')
                .any(|k| go(rest, &n[k..])),
            [c, rest @ ..] => n
                .split_first()
                .is_some_and(|(d, nr)| c.eq_ignore_ascii_case(d) && go(rest, nr)),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
}

/// Copy the files under `src` (a GServer-v2 `world/`) into `world`: the
/// first rule a file matches says where. A level goes into `levels/` under
/// its own name (ours has no level subfolders; a `.gmap` is a level here
/// too, as `levels/` is where this server reads gmaps). Anything else a
/// rule lets through goes into `files/` under the same path. A file no
/// rule matches is not copied; nor is one already in the target, or a
/// second level with a name taken.
pub fn world_files(
    src: &Path,
    world: &Path,
    rules: &[(Kind, String)],
    r: &mut Report,
) -> io::Result<()> {
    let mut all = Vec::new();
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(src) {
                if let Some(rel) = rel.to_str() {
                    all.push((rel.replace('\\', "/"), path.clone()));
                }
            }
        }
    }
    all.sort();
    let (mut levels, mut files) = (0, 0);
    let mut unmatched = Vec::new();
    for (rel, path) in all {
        let name = rel.rsplit('/').next().unwrap_or(&rel);
        if name.starts_with('.') {
            continue;
        }
        let Some((kind, _)) = rules.iter().find(|(_, p)| glob(p, &rel)) else {
            unmatched.push(rel);
            continue;
        };
        let target = match kind {
            Kind::Level if crate::levels::valid_level_name(name) => world.join("levels").join(name),
            Kind::Level => {
                r.left.push(format!("world/{rel}: not a level name here"));
                continue;
            }
            Kind::File => world.join("files").join(&rel),
        };
        if target.exists() {
            r.left
                .push(format!("world/{rel}: {} exists; kept", target.display()));
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&path, &target)?;
        match kind {
            Kind::Level => levels += 1,
            Kind::File => files += 1,
        }
    }
    r.done.push(format!(
        "{levels} level(s) in {}, {files} file(s) in {}",
        world.join("levels").display(),
        world.join("files").display()
    ));
    if !unmatched.is_empty() {
        let n = unmatched.len();
        unmatched.truncate(10);
        r.left.push(format!(
            "{n} file(s) under world/ no foldersconfig rule lets a client have, not copied: {}{}",
            unmatched.join(", "),
            if n > 10 { ", ..." } else { "" }
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_and_comments() {
        let o = parse_options(
            "# a comment\nname = My Server\n#startx = 30\ntriggerhack_rc = false\t\t# gr.rcchat\nempty =\nurl = http://example.org/#top\n",
        );
        assert_eq!(o["name"], "My Server");
        assert!(!o.contains_key("startx"));
        assert_eq!(o["triggerhack_rc"], "false");
        assert_eq!(o["empty"], "");
        assert_eq!(o["url"], "http://example.org/#top");
    }

    #[test]
    fn allowed_versions_comments() {
        let t = "// List of version strings.\n//GNW13110\t// 1.41r1\nGNW22122:GNW28015\nGNW03014 // 2.05\n\n";
        assert_eq!(
            convert_allowed(t),
            "# Imported from a GServer-v2 allowedversions.txt.\n# List of version strings.\n# GNW13110\t// 1.41r1\nGNW22122:GNW28015\nGNW03014  # 2.05\n\n"
        );
    }

    #[test]
    fn folders_config_and_globs() {
        let rules = parse_folders(
            "# Folder Configuration\nhead    heads/*\nlevel   *.nw\nlevel   levels/*.nw\nfile    images/*.png\nfile    global/*\nodd     x/*\n",
        );
        assert_eq!(rules.len(), 5);
        assert_eq!(rules[1], (Kind::Level, "*.nw".to_string()));
        assert!(glob("*.nw", "start.NW"));
        assert!(!glob("*.nw", "levels/start.nw"), "one folder deep only");
        assert!(glob("levels/*.nw", "levels/start.nw"));
        assert!(glob("global/*", "global/a.png"));
        assert!(!glob("global/*", "global/heads/a.png"));
        assert!(glob("**.nw", "a/b/c.nw"));
        assert!(glob("**", "a/b/c.png"));
    }

    #[test]
    fn folder_rights() {
        assert_eq!(convert_folder_right("rw world/levels/*"), "rw levels/*");
        assert_eq!(convert_folder_right("r weapons/*"), "r weapons/*");
        assert_eq!(
            convert_folder_right("rw world/images/*"),
            "rw world/images/*"
        );
    }

    #[test]
    fn an_account_converts() {
        let text = "GRACC001\nNAME Kay\nNICK Kay the Brave\nLEVEL house.nw\nX 12.00\nY 40.50\nMAXHP 5.00\nHP 4.50\nRUPEES 120\nARROWS 10\nBOMBS 5\nGLOVEP 2\nSHIELDP 2\nSWORDP 3\nBOW \nHEAD head7.png\nBODY body2.png\nSWORD sword3.png\nSHIELD shield2.png\nCOLORS 1,2,3,4,5\nSPRITE 2\nSTATUS 20\nAP 70\nKILLS 4\nDEATHS 2\nRATING 1500.00\nBANNED 1\nBANREASON spam\nCOMMENTS warned\nEMAIL\nIPRANGE *.*.*.*\nFOLDERRIGHT rw world/levels/*\nFOLDERRIGHT r config/*\nLOCALRIGHTS 513\nWEAPON bomb\n";
        let fields = parse_account(text).unwrap();
        let mut unused = BTreeSet::new();
        let (a, ban) = convert_account("kay", &fields, ("start.nw", 1.0, 2.0), &mut unused);
        assert_eq!(a.name, "kay");
        assert_eq!(a.nick, "Kay the Brave");
        assert_eq!((a.level.as_str(), a.x, a.y), ("house.nw", 12.0, 40.5));
        assert_eq!((a.max_hp, a.hp, a.rupees), (5, 4.5, 120));
        assert_eq!((a.arrows, a.bombs, a.glove), (10, 5, 2));
        assert_eq!(a.sword, (3, "sword3.png".to_string()));
        assert_eq!(a.shield, (2, "shield2.png".to_string()));
        assert_eq!(
            (a.head.as_str(), a.body.as_str()),
            ("head7.png", "body2.png")
        );
        assert_eq!(a.colors, [1, 2, 3, 4, 5]);
        assert_eq!(
            (a.sprite, a.status, a.alignment, a.kills, a.deaths),
            (2, 20, 70, 4, 2)
        );
        assert_eq!(a.comments, ["warned"]);
        assert_eq!(a.ip_ranges, ["*.*.*.*"]);
        assert_eq!(a.folder_rights, ["rw levels/*", "r config/*"]);
        assert_eq!(a.rc_rights, 513);
        let ban = ban.unwrap();
        assert_eq!((ban.until, ban.reason.as_str()), (None, "spam"));
        assert_eq!(unused.into_iter().collect::<Vec<_>>(), ["RATING", "WEAPON"]);
        assert!(parse_account("SEIHAIACC 1\n").is_none());
        let (b, _) = convert_account(
            "x",
            &[("IPRANGE".into(), "0.0.0.0".into())],
            ("s.nw", 1.0, 2.0),
            &mut BTreeSet::new(),
        );
        assert!(b.ip_ranges.is_empty(), "the template's 0.0.0.0 is none");
    }
}
