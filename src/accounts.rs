//! Flat-file accounts: `accounts/<account>.txt`, one `KEY value` per line.
//!
//! ```text
//! SEIHAIACC 1
//! PASSWORD pbkdf2-sha256:100000:<salt hex>:<hash hex>
//! NICK Squirrel
//! LEVEL onlinestartlocal.nw
//! X 30
//! Y 30.5
//! SPRITE 2
//! MAXHP 3
//! HP 3
//! RUPEES 0
//! ARROWS 10
//! BOMBS 5
//! GLOVE 1
//! SWORD 1 sword1.png
//! SHIELD 1 shield1.png
//! HEAD head0.png
//! BODY body.png
//! COLORS 2,0,10,4,18
//! STATUS 20
//! ALIGN 0
//! CHEST onlinestartlocal.nw 30 36
//! FLAG quest.started
//! FLAG gr.x=30
//! STAFF 1
//! IPRANGE 127.0.0.1,192.168.1.*
//! EMAIL someone@example.org
//! LOADONLY 1
//! ADMINLEVEL 2
//! ADMINWORLDS main
//! FOLDER rw levels/*
//! FOLDER r config/*.txt
//! RCRIGHTS 513
//! COMMENT warned once for spam
//! ```
//!
//! `FOLDER` lines are the RC file browser's rights (`rcfiles.rs`): `r` or
//! `rw`, then a folder and a file mask.
//!
//! `EMAIL`, `LOADONLY`, `ADMINLEVEL` and `ADMINWORLDS` are the RC account
//! dialog's fields (`rcaccounts.rs`). They are kept and shown to an RC;
//! nothing acts on them yet, and `ADMINLEVEL` does not grant RC use.
//!
//! `STAFF` lets the account log in with an RC (`rc.rs`); `IPRANGE` lists
//! where from (exact addresses, `*` wildcards, or `a.b.c.d/n`). Without an
//! `IPRANGE` a staff account's RC may only come from loopback.
//!
//! The format is this project's own. Unknown keys are kept and written
//! back, so later stages can add fields without losing old ones. The
//! password is stored as a PBKDF2-SHA256 hash; `seihai-gserver account`
//! sets it. Status bit 2 (hidden) is cleared on load: a player is never
//! logged in hidden. `ALIGN` is alignment points (player prop 32), 0 to
//! 100: kills lower it and time online raises it (`rules.rs`); the list
//! link sends the stored value on `PLYRADD`.

use sha2::Sha256;
use std::io;
use std::path::{Path, PathBuf};

pub const PBKDF2_ROUNDS: u32 = 100_000;
const STATUS_HIDDEN: u8 = 0x02;

#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub name: String,
    pub password: String,
    pub nick: String,
    pub level: String,
    pub x: f32,
    pub y: f32,
    pub sprite: u8,
    pub max_hp: u8,
    /// Hearts, in halves on the wire.
    pub hp: f32,
    pub rupees: u32,
    pub arrows: u8,
    pub bombs: u8,
    pub glove: u8,
    pub sword: (u8, String),
    pub shield: (u8, String),
    pub head: String,
    pub body: String,
    pub colors: [u8; 5],
    pub status: u8,
    /// Alignment points (player prop 32), 0 to 100. Default 0.
    pub alignment: u8,
    /// Players this account killed (`KILLS`), counted from the victims'
    /// claims (`PLI_CLAIMPKER`).
    pub kills: u32,
    /// Times this account died (`DEATHS`), counted when the client sets
    /// its dead status bit, as the 1999 servers count them.
    pub deaths: u32,
    /// Chests this account has opened: level (lower case), x, y.
    pub chests: Vec<(String, u8, u8)>,
    /// Player flags as the client set them: `name` or `name=value`.
    pub flags: Vec<String>,
    /// `WEAPON` lines: the weapons the account holds, default weapons by
    /// name (`bow`, `bomb`) and the world's by their `REALNAME`.
    pub weapons: Vec<String>,
    /// `STAFF`: the account may log in with an RC.
    pub staff: bool,
    /// `IPRANGE`: where its RC may come from (see `rc::ip_allowed`).
    pub ip_ranges: Vec<String>,
    /// `FOLDER` lines: the RC file browser's rights (`rcfiles.rs`).
    pub folder_rights: Vec<String>,
    /// `RCRIGHTS`: the RC rights bits, as an RC sets them (not acted on
    /// yet).
    pub rc_rights: u32,
    /// `COMMENT` lines: staff notes from an RC.
    pub comments: Vec<String>,
    /// `EMAIL`, from the RC's account dialog.
    pub email: String,
    /// `LOADONLY`, from the RC's account dialog. Not acted on yet.
    pub load_only: bool,
    /// `ADMINLEVEL`, from the RC's account dialog. Not acted on yet.
    pub admin_level: u8,
    /// `ADMINWORLDS`, from the RC's account dialog.
    pub admin_worlds: String,
    /// Lines with keys this version does not use, kept as they were.
    pub other: Vec<String>,
}

impl Account {
    /// A new account at the world's start, with no password set.
    pub fn new(name: &str, level: &str, x: f32, y: f32) -> Self {
        Account {
            name: name.to_string(),
            password: String::new(),
            nick: name.to_string(),
            level: level.to_string(),
            x,
            y,
            sprite: 2,
            max_hp: 3,
            hp: 3.0,
            rupees: 0,
            arrows: 10,
            bombs: 5,
            glove: 1,
            sword: (1, "sword1.png".into()),
            shield: (1, "shield1.png".into()),
            head: "head0.png".into(),
            body: "body.png".into(),
            colors: [2, 0, 10, 4, 18],
            status: 20,
            alignment: 0,
            kills: 0,
            deaths: 0,
            chests: Vec::new(),
            flags: Vec::new(),
            weapons: Vec::new(),
            staff: false,
            ip_ranges: Vec::new(),
            folder_rights: Vec::new(),
            rc_rights: 0,
            comments: Vec::new(),
            email: String::new(),
            load_only: false,
            admin_level: 0,
            admin_worlds: String::new(),
            other: Vec::new(),
        }
    }

    /// Full hearts (the account's max) for a player saved with none. Used
    /// at login only when the world sets `revive_dead`; GServer-v2 beta4
    /// keeps them at 0 (issue #51, lab run 2026-11-12).
    pub fn revive(&mut self) {
        if self.hp <= 0.0 {
            self.hp = f32::from(self.max_hp.max(1));
        }
    }

    pub fn parse(name: &str, text: &str) -> Self {
        let mut a = Account::new(name, "", 0.0, 0.0);
        a.head.clear();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            let v = v.trim();
            let num = |d| v.parse().unwrap_or(d);
            match k {
                "SEIHAIACC" | "" => {}
                "PASSWORD" => a.password = v.to_string(),
                "NICK" => a.nick = v.to_string(),
                "LEVEL" => a.level = v.to_string(),
                "X" => a.x = num(0.0),
                "Y" => a.y = num(0.0),
                "SPRITE" => a.sprite = v.parse().unwrap_or(2),
                "MAXHP" => a.max_hp = v.parse::<f32>().map(|h| h as u8).unwrap_or(3),
                "HP" => a.hp = num(3.0),
                "RUPEES" => a.rupees = v.parse().unwrap_or(0),
                "ARROWS" => a.arrows = v.parse().unwrap_or(0),
                "BOMBS" => a.bombs = v.parse().unwrap_or(0),
                "GLOVE" => a.glove = v.parse().unwrap_or(1),
                "SWORD" => a.sword = power_and_image(v),
                "SHIELD" => a.shield = power_and_image(v),
                "HEAD" => a.head = v.to_string(),
                "BODY" => a.body = v.to_string(),
                "COLORS" => {
                    for (slot, c) in a.colors.iter_mut().zip(v.split(',')) {
                        *slot = c.trim().parse().unwrap_or(0);
                    }
                }
                "STATUS" => a.status = v.parse().unwrap_or(0),
                "ALIGN" => a.alignment = v.parse::<u8>().unwrap_or(0).min(100),
                "KILLS" => a.kills = v.parse().unwrap_or(0),
                "DEATHS" => a.deaths = v.parse().unwrap_or(0),
                "CHEST" => {
                    let mut w = v.split_whitespace();
                    let level = w.next().map(str::to_ascii_lowercase);
                    let x = w.next().and_then(|n| n.parse().ok());
                    let y = w.next().and_then(|n| n.parse().ok());
                    match (level, x, y) {
                        (Some(l), Some(x), Some(y)) => a.chests.push((l, x, y)),
                        _ => a.other.push(line.to_string()),
                    }
                }
                "FLAG" if !v.is_empty() => a.flags.push(v.to_string()),
                "WEAPON" if !v.is_empty() => {
                    if !a.weapons.iter().any(|w| w.eq_ignore_ascii_case(v)) {
                        a.weapons.push(v.to_string());
                    }
                }
                "STAFF" => a.staff = !matches!(v, "0" | "false" | "no"),
                "FOLDER" if !v.is_empty() => a.folder_rights.push(v.to_string()),
                "RCRIGHTS" => a.rc_rights = v.parse().unwrap_or(0),
                "COMMENT" => a.comments.push(v.to_string()),
                "EMAIL" => a.email = v.to_string(),
                "LOADONLY" => a.load_only = !matches!(v, "0" | "false" | "no"),
                "ADMINLEVEL" => a.admin_level = v.parse().unwrap_or(0),
                "ADMINWORLDS" => a.admin_worlds = v.to_string(),
                "IPRANGE" => a.ip_ranges.extend(
                    v.split(',')
                        .map(str::trim)
                        .filter(|r| !r.is_empty())
                        .map(str::to_string),
                ),
                _ => a.other.push(line.to_string()),
            }
        }
        a.status &= !STATUS_HIDDEN;
        a
    }

    pub fn to_text(&self) -> String {
        let mut s = String::from("SEIHAIACC 1\n");
        let mut kv = |k: &str, v: String| {
            s.push_str(k);
            s.push(' ');
            s.push_str(&v);
            s.push('\n');
        };
        kv("PASSWORD", self.password.clone());
        kv("NICK", self.nick.clone());
        kv("LEVEL", self.level.clone());
        kv("X", self.x.to_string());
        kv("Y", self.y.to_string());
        kv("SPRITE", self.sprite.to_string());
        kv("MAXHP", self.max_hp.to_string());
        kv("HP", self.hp.to_string());
        kv("RUPEES", self.rupees.to_string());
        kv("ARROWS", self.arrows.to_string());
        kv("BOMBS", self.bombs.to_string());
        kv("GLOVE", self.glove.to_string());
        kv("SWORD", format!("{} {}", self.sword.0, self.sword.1));
        kv("SHIELD", format!("{} {}", self.shield.0, self.shield.1));
        kv("HEAD", self.head.clone());
        kv("BODY", self.body.clone());
        kv("COLORS", self.colors.map(|c| c.to_string()).join(","));
        kv("STATUS", self.status.to_string());
        kv("ALIGN", self.alignment.to_string());
        if self.kills > 0 {
            kv("KILLS", self.kills.to_string());
        }
        if self.deaths > 0 {
            kv("DEATHS", self.deaths.to_string());
        }
        for (level, x, y) in &self.chests {
            kv("CHEST", format!("{level} {x} {y}"));
        }
        for f in &self.flags {
            kv("FLAG", f.replace(['\n', '\r'], " "));
        }
        for w in &self.weapons {
            kv("WEAPON", w.replace(['\n', '\r'], " "));
        }
        if self.staff {
            kv("STAFF", "1".into());
        }
        if !self.ip_ranges.is_empty() {
            kv("IPRANGE", self.ip_ranges.join(","));
        }
        let one_line = |s: &str| s.replace(['\n', '\r'], " ");
        for r in &self.folder_rights {
            kv("FOLDER", one_line(r));
        }
        if self.rc_rights != 0 {
            kv("RCRIGHTS", self.rc_rights.to_string());
        }
        for c in &self.comments {
            kv("COMMENT", one_line(c));
        }
        if !self.email.is_empty() {
            kv("EMAIL", one_line(&self.email));
        }
        if self.load_only {
            kv("LOADONLY", "1".into());
        }
        if self.admin_level > 0 {
            kv("ADMINLEVEL", self.admin_level.to_string());
        }
        if !self.admin_worlds.is_empty() {
            kv("ADMINWORLDS", one_line(&self.admin_worlds));
        }
        for line in &self.other {
            s.push_str(line);
            s.push('\n');
        }
        s
    }

    pub fn set_password(&mut self, password: &[u8]) {
        self.password = hash_password(password);
    }

    pub fn check_password(&self, password: &[u8]) -> bool {
        verify_password(&self.password, password)
    }
}

fn power_and_image(v: &str) -> (u8, String) {
    let (p, img) = v.split_once(' ').unwrap_or((v, ""));
    (p.parse().unwrap_or(0), img.trim().to_string())
}

/// `pbkdf2-sha256:<rounds>:<salt hex>:<hash hex>` with a fresh 16-byte salt.
pub fn hash_password(password: &[u8]) -> String {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt).expect("the system random source works");
    let hash = pbkdf2::pbkdf2_hmac_array::<Sha256, 32>(password, &salt, PBKDF2_ROUNDS);
    format!(
        "pbkdf2-sha256:{PBKDF2_ROUNDS}:{}:{}",
        hex(&salt),
        hex(&hash)
    )
}

/// Check `password` against a stored hash. An empty or unreadable stored
/// value matches nothing.
pub fn verify_password(stored: &str, password: &[u8]) -> bool {
    let parts: Vec<&str> = stored.split(':').collect();
    let ["pbkdf2-sha256", rounds, salt, hash] = parts[..] else {
        return false;
    };
    let (Ok(rounds), Some(salt), Some(want)) = (rounds.parse::<u32>(), unhex(salt), unhex(hash))
    else {
        return false;
    };
    if rounds == 0 || rounds > 10_000_000 || want.len() != 32 {
        return false;
    }
    let got = pbkdf2::pbkdf2_hmac_array::<Sha256, 32>(password, &salt, rounds);
    // Compare every byte, so the time taken does not say how many matched.
    got.iter().zip(&want).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The accounts folder.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Store { dir: dir.into() }
    }

    /// File names are the account in lower case, so `Bob` and `bob` are one
    /// account on every file system.
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.txt", name.to_ascii_lowercase()))
    }

    /// The account, or `None` if it has no file. `name` must already have
    /// passed [`crate::login::valid_account_name`].
    pub fn load(&self, name: &str) -> io::Result<Option<Account>> {
        match std::fs::read_to_string(self.path(name)) {
            Ok(t) => Ok(Some(Account::parse(name, &t))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Write the account (through a temporary file, so a crash never
    /// leaves half a file).
    pub fn save(&self, a: &Account) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path(&a.name);
        let tmp = path.with_extension("txt.tmp");
        std::fs::write(&tmp, a.to_text())?;
        std::fs::rename(tmp, path)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every account with a file, by file name (lower case), sorted. No
    /// folder: none.
    pub fn list(&self) -> io::Result<Vec<String>> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let stem = name.strip_suffix(".txt")?;
                crate::login::valid_account_name(stem).then(|| stem.to_string())
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// Remove the account's file; whether there was one.
    pub fn delete(&self, name: &str) -> io::Result<bool> {
        match std::fs::remove_file(self.path(name)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords() {
        let h = hash_password(b"labpw");
        assert!(verify_password(&h, b"labpw"));
        assert!(!verify_password(&h, b"labpx"));
        assert!(!verify_password("", b""));
        assert!(!verify_password("plain:labpw", b"labpw"));
        assert_ne!(hash_password(b"labpw"), h, "fresh salt each time");
    }

    #[test]
    fn round_trip_keeps_unknown_keys_and_drops_hidden() {
        let mut a = Account::new("Bob", "start.nw", 30.0, 30.5);
        a.status = 20 | STATUS_HIDDEN;
        a.alignment = 40;
        a.other.push("FUTUREKEY 1 2 3".into());
        let b = Account::parse("Bob", &a.to_text());
        assert_eq!(b.status, 20);
        assert_eq!(b.alignment, 40);
        assert_eq!(b.other, vec!["FUTUREKEY 1 2 3".to_string()]);
        assert_eq!(Account::parse("Bob", "NICK Bob\n").alignment, 0);
        assert_eq!(Account::parse("Bob", "ALIGN 250\n").alignment, 100);
        assert_eq!(b.sword, (1, "sword1.png".to_string()));
        assert_eq!(b.colors, [2, 0, 10, 4, 18]);
        assert_eq!((b.x, b.y), (30.0, 30.5));
    }

    #[test]
    fn chests_and_flags_round_trip() {
        let mut a = Account::new("Bob", "start.nw", 30.0, 30.5);
        a.chests.push(("house.nw".into(), 10, 12));
        a.flags = vec!["quest.started".into(), "gr.x=30".into()];
        let b = Account::parse("Bob", &a.to_text());
        assert_eq!(b.chests, a.chests);
        assert_eq!(b.flags, a.flags);
        assert!(b.other.is_empty(), "{:?}", b.other);
        // A chest line that does not read is kept as it was.
        let c = Account::parse(
            "Bob",
            "CHEST House.nw 10
CHEST House.nw 1 2
",
        );
        assert_eq!(c.chests, [("house.nw".to_string(), 1, 2)]);
        assert_eq!(c.other, ["CHEST House.nw 10"]);
    }

    #[test]
    fn store_is_case_insensitive() {
        let dir = std::env::temp_dir().join(format!("seihai-gserver-acc-{}", std::process::id()));
        let s = Store::new(&dir);
        let mut a = Account::new("Bob", "start.nw", 1.0, 2.0);
        a.set_password(b"pw");
        s.save(&a).unwrap();
        let b = s.load("BOB").unwrap().unwrap();
        assert!(b.check_password(b"pw"));
        assert!(s.load("alice").unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
