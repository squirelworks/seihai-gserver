//! Accounts from an RC (stage 4 slice 4, issue #52): list, get, set, add
//! and delete, with the layouts seihai-rc saw from a real RC against the
//! fork (its `docs/PROTOCOL.md`, "Accounts").
//!
//! | RC sends | Server answers |
//! |---|---|
//! | 72 `RC_ACCOUNTLISTGET`: `GSTRING` pattern (`%` matches any run), `GSTRING` conditions | 70: a `GSTRING` per account |
//! | 77 `RC_ACCOUNTGET`: the account, the rest of the line | 73: account, password (always empty), e-mail, `GCHAR` banned, load only, admin level, `GSTRING` admin worlds, one byte 0x20, `GSTRING` ban reason |
//! | 78 `RC_ACCOUNTSET`: as 73 without the 0x20 byte; an empty password leaves it | nothing |
//! | 70 `RC_ACCOUNTADD`: as 78 | nothing |
//! | 71 `RC_ACCOUNTDEL`: the account, the rest of the line | nothing |
//!
//! The account file keeps the RC's fields as `EMAIL`, `LOADONLY`,
//! `ADMINLEVEL` and `ADMINWORLDS` (`accounts.rs`). They are stored and
//! shown, nothing acts on them yet; in particular the admin level does not
//! make an account staff, which only `STAFF` in the file does, so an RC
//! cannot raise an account to RC use.
//!
//! The ban fields are the world's ban file (`config/bans.txt`,
//! `bans.rs`), the same one `seihai-gserver ban` writes and a login
//! checks. A get says banned when a ban holds now, with its reason. A set
//! or add that says banned writes a ban with no end (the RC sends no ban
//! time), under the RC's account as the admin; one that already holds
//! keeps its end. One that says not banned lifts the ban. The reason goes
//! in as one line: the RC's lines joined with spaces.
//!
//! The player's own ban, as seihai-rc saw it (same section of its
//! `docs/PROTOCOL.md`):
//!
//! | RC sends | Server answers |
//! |---|---|
//! | 87 `RC_PLAYERBANGET`: the account | 64: `GSTRING` account, `GCHAR` banned, the reason tokenized |
//! | 88 `RC_PLAYERBANSET`: `GSTRING` account, `GCHAR` banned, the reason tokenized | nothing |
//!
//! A ban set on a player who is online disconnects them, as the fork did
//! at `1d7843a`; their next login meets the ban.
//!
//! Slice 6 adds a player's staff rights and comments, as seihai-rc saw
//! them from a real RC (`GSERV024`) against the fork (its
//! `docs/PROTOCOL.md`, "Comments, account ban, rights and props"):
//!
//! | RC sends | Server answers |
//! |---|---|
//! | 83 `RC_PLAYERRIGHTSGET`: the account | 62: `GSTRING` account, `GINT5` rights, `GSTRING` IP range, `GSHORT n`, `n` bytes of folder rights tokenized |
//! | 84 `RC_PLAYERRIGHTSSET`: as 62 | nothing |
//! | 85 `RC_PLAYERCOMMENTSGET`: the account | 63: `GSTRING` account, then the comments tokenized |
//! | 86 `RC_PLAYERCOMMENTSSET`: `GSTRING` account, the comments tokenized | nothing |
//!
//! The folder rights go out tokenized, one quoted item per line, so a
//! real RC keeps `rw levels/*` as one line (the fork once sent them plain
//! and the RC split them at the space). They are the account's `FOLDER`
//! lines (the file browser's rights, `rcfiles.rs`); the IP range is its
//! `IPRANGE` (where its RC may log in from, `rc::ip_allowed`), commas
//! between ranges; the rights bits are kept as `RCRIGHTS` and not acted on
//! yet, so every staff RC may do every RC action. Comments are `COMMENT`
//! lines. An RC session already open keeps the folder rights it logged in
//! with.
//!
//! Conditions: `adminlevel` with `>`, `<` or `=` and a number is applied;
//! any other condition is ignored and logged. A set or add of an account
//! name that does not pass `login::valid_account_name`, an add of one that
//! exists, a set of one that does not, and a delete of one that is online
//! are refused in the log only, as the RC gets no reply either way.

use crate::accounts::{Account, Store};
use crate::bans::{Ban, Bans};
use crate::login::valid_account_name;
use crate::wire::{gchar, gstring, latin1, msg, text};
use seihai_core::protocol::read_gshort;

/// `PLO_RC_ACCOUNTLISTGET` reply id.
pub const PLO_RC_ACCOUNTLIST: u8 = 70;
/// `PLO_RC_ACCOUNTGET` reply id.
pub const PLO_RC_ACCOUNTGET: u8 = 73;
/// `PLO_RC_PLAYERRIGHTSGET` reply id.
pub const PLO_RC_PLAYERRIGHTSGET: u8 = 62;
/// `PLO_RC_PLAYERCOMMENTSGET` reply id.
pub const PLO_RC_PLAYERCOMMENTSGET: u8 = 63;
pub const PLI_RC_PLAYERRIGHTSGET: u8 = 83;
pub const PLI_RC_PLAYERRIGHTSSET: u8 = 84;
pub const PLI_RC_PLAYERCOMMENTSGET: u8 = 85;
pub const PLI_RC_PLAYERCOMMENTSSET: u8 = 86;
/// `PLO_RC_PLAYERBANGET` reply id.
pub const PLO_RC_PLAYERBANGET: u8 = 64;
pub const PLI_RC_PLAYERBANGET: u8 = 87;
pub const PLI_RC_PLAYERBANSET: u8 = 88;

pub const PLI_RC_ACCOUNTADD: u8 = 70;
pub const PLI_RC_ACCOUNTDEL: u8 = 71;
pub const PLI_RC_ACCOUNTLISTGET: u8 = 72;
pub const PLI_RC_ACCOUNTGET: u8 = 77;
pub const PLI_RC_ACCOUNTSET: u8 = 78;

/// The fields of the RC's account dialog.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fields {
    pub account: String,
    /// Empty leaves the stored password.
    pub password: Vec<u8>,
    pub email: String,
    pub banned: bool,
    pub load_only: bool,
    pub admin_level: u8,
    pub admin_worlds: String,
    /// Tokenized, as sent.
    pub ban_reason: Vec<u8>,
}

/// An account request from an RC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    List {
        pattern: String,
        conditions: String,
    },
    Get(String),
    Set(Fields),
    Add(Fields),
    Delete(String),
    RightsGet(String),
    RightsSet(Rights),
    CommentsGet(String),
    CommentsSet {
        account: String,
        lines: Vec<String>,
    },
    /// 73, by player id: the connection finds the account first and
    /// sends [`Op::PropsGet`].
    PropsGetId(u16),
    /// A 72 for `account`, as player `id`.
    PropsGet {
        id: u16,
        account: String,
    },
    PropsSet(crate::rcprops::Set),
    BanGet(String),
    BanSet {
        account: String,
        banned: bool,
        /// Tokenized, as sent.
        reason: Vec<u8>,
    },
}

/// A player's staff rights as 62 and 84 carry them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rights {
    pub account: String,
    pub rights: u32,
    pub ip_range: String,
    /// One per line, `rw levels/*`.
    pub folders: Vec<String>,
}

fn lines(t: &[u8]) -> Vec<String> {
    crate::wire::detokenize(t).iter().map(|l| text(l)).collect()
}

fn tokenized(lines: &[String]) -> Vec<u8> {
    let l: Vec<Vec<u8>> = lines.iter().map(|l| latin1(l)).collect();
    crate::wire::tokenize(&l)
}

fn read_gint5(b: &[u8], i: &mut usize) -> Option<u32> {
    let v = b.get(*i..*i + 5)?;
    *i += 5;
    Some(v.iter().fold(0u64, |a, &c| {
        (a << 7) | u64::from(c.wrapping_sub(0x20) & 0x7F)
    }) as u32)
}

fn read_gstring(b: &[u8], i: &mut usize) -> Option<Vec<u8>> {
    let n = usize::from(b.get(*i)?.wrapping_sub(0x20));
    let s = b.get(*i + 1..*i + 1 + n)?.to_vec();
    *i += 1 + n;
    Some(s)
}

/// Read an account request: `id` is the message id, `rest` its payload.
/// `None` for another id or a payload that does not read.
pub fn parse(id: u8, rest: &[u8]) -> Option<Op> {
    let name = || text(rest).trim().to_string();
    match id {
        PLI_RC_ACCOUNTLISTGET => {
            let mut i = 0;
            let pattern = text(&read_gstring(rest, &mut i)?);
            let conditions = read_gstring(rest, &mut i).map_or(String::new(), |c| text(&c));
            Some(Op::List {
                pattern,
                conditions,
            })
        }
        PLI_RC_ACCOUNTGET if !name().is_empty() => Some(Op::Get(name())),
        PLI_RC_PLAYERRIGHTSGET if !name().is_empty() => Some(Op::RightsGet(name())),
        PLI_RC_PLAYERCOMMENTSGET if !name().is_empty() => Some(Op::CommentsGet(name())),
        PLI_RC_PLAYERBANGET if !name().is_empty() => Some(Op::BanGet(name())),
        PLI_RC_PLAYERBANSET => {
            let mut i = 0;
            let account = text(&read_gstring(rest, &mut i)?);
            let banned = rest.get(i)?.wrapping_sub(0x20) != 0;
            Some(Op::BanSet {
                account,
                banned,
                reason: rest[i + 1..].to_vec(),
            })
        }
        crate::rcprops::PLI_RC_PLAYERPROPSGET2 => read_gshort(rest).map(Op::PropsGetId),
        crate::rcprops::PLI_RC_PLAYERPROPSGET3 => {
            crate::rcprops::parse_get3(rest).map(|account| Op::PropsGet { id: 0, account })
        }
        crate::rcprops::PLI_RC_PLAYERPROPSSET2 => crate::rcprops::parse_set(rest).map(Op::PropsSet),
        PLI_RC_PLAYERRIGHTSSET => {
            let mut i = 0;
            let account = text(&read_gstring(rest, &mut i)?);
            let rights = read_gint5(rest, &mut i)?;
            let ip_range = text(&read_gstring(rest, &mut i)?);
            let n = usize::from(read_gshort(rest.get(i..)?)?);
            let folders = lines(rest.get(i + 2..i + 2 + n)?);
            Some(Op::RightsSet(Rights {
                account,
                rights,
                ip_range,
                folders,
            }))
        }
        PLI_RC_PLAYERCOMMENTSSET => {
            let mut i = 0;
            let account = text(&read_gstring(rest, &mut i)?);
            Some(Op::CommentsSet {
                account,
                lines: lines(&rest[i..]),
            })
        }
        PLI_RC_ACCOUNTDEL if !name().is_empty() => Some(Op::Delete(name())),
        PLI_RC_ACCOUNTSET | PLI_RC_ACCOUNTADD => {
            let mut i = 0;
            let account = text(&read_gstring(rest, &mut i)?);
            let password = read_gstring(rest, &mut i)?;
            let email = text(&read_gstring(rest, &mut i)?);
            let flags = rest.get(i..i + 3)?;
            i += 3;
            let admin_worlds = text(&read_gstring(rest, &mut i)?);
            let ban_reason = read_gstring(rest, &mut i).unwrap_or_default();
            let flag = |b: u8| b.wrapping_sub(0x20);
            let f = Fields {
                account,
                password,
                email,
                banned: flag(flags[0]) != 0,
                load_only: flag(flags[1]) != 0,
                admin_level: flag(flags[2]),
                admin_worlds,
                ban_reason,
            };
            Some(if id == PLI_RC_ACCOUNTSET {
                Op::Set(f)
            } else {
                Op::Add(f)
            })
        }
        _ => None,
    }
}

/// Whether `name` matches `pattern`, without case: `%` is any run of
/// characters, anything else itself.
pub fn matches(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some((b'%', rest)) => (0..=n.len()).any(|k| go(rest, &n[k..])),
            Some((c, rest)) => n
                .split_first()
                .is_some_and(|(d, nr)| c.eq_ignore_ascii_case(d) && go(rest, nr)),
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}

/// A test on an account file.
type Test = Box<dyn Fn(&Account) -> bool>;

/// One condition this server reads, if it is one.
fn condition(c: &str) -> Option<Test> {
    let c = c.trim().to_ascii_lowercase();
    let rest = c.strip_prefix("adminlevel")?.trim_start();
    let (op, n) = rest.split_at(rest.find(|ch: char| !"<>=".contains(ch))?);
    let n: u8 = n.trim().parse().ok()?;
    match op {
        ">" => Some(Box::new(move |a| a.admin_level > n)),
        "<" => Some(Box::new(move |a| a.admin_level < n)),
        "=" | "==" => Some(Box::new(move |a| a.admin_level == n)),
        _ => None,
    }
}

/// The reason as the RC sends it (tokenized lines), as one line.
fn reason_line(t: &[u8]) -> String {
    lines(t)
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A reason as the RC reads it: one tokenized line, or nothing.
fn reason_tokens(ban: Option<&Ban>) -> Vec<u8> {
    match ban {
        Some(b) if !b.reason.is_empty() => tokenized(std::slice::from_ref(&b.reason)),
        _ => Vec::new(),
    }
}

/// `PLO 73` for `a`; `ban` is the one that holds on it now.
pub fn account_reply(a: &Account, ban: Option<&Ban>) -> Vec<u8> {
    let mut p = Vec::new();
    gstring(&latin1(&a.name), &mut p);
    gstring(b"", &mut p);
    gstring(&latin1(&a.email), &mut p);
    p.extend([
        gchar(u8::from(ban.is_some())),
        gchar(u8::from(a.load_only)),
        gchar(a.admin_level.min(0xDF)),
    ]);
    gstring(&latin1(&a.admin_worlds), &mut p);
    p.push(0x20);
    gstring(&reason_tokens(ban), &mut p);
    msg(PLO_RC_ACCOUNTGET, &p)
}

/// What an account request did: the reply for the RC (empty for none) and
/// a line for the log.
#[derive(Debug, Default)]
pub struct Done {
    pub reply: Vec<u8>,
    pub log: Option<String>,
    /// An account (lower case) to disconnect: banned while online.
    pub kick: Option<String>,
}

/// Where an added account starts: the world's start level and spot.
pub type Start<'a> = (&'a str, f32, f32);

/// Where an RC's request runs: the account files, the ban file, who is
/// online (lower case), where an added account starts, and the RC's own
/// account (the admin on a ban it writes).
pub struct Ctx<'a> {
    pub store: &'a Store,
    pub bans: &'a Bans,
    pub online: &'a [String],
    pub start: Start<'a>,
    pub admin: &'a str,
    /// The world, for players online (`rcprops.rs`); `None` reads and
    /// writes the files alone.
    pub world: Option<&'a crate::world::World>,
}

/// The ban that holds on `name` now; a ban file that cannot be read
/// counts as none.
fn ban_on(bans: &Bans, name: &str) -> Option<Ban> {
    bans.find(name).ok().flatten()
}

/// Write the RC's ban field for `account`: banned, or not. A ban that
/// already holds keeps its end. What changed, for the log; nothing when
/// it was not banned and stays so.
fn set_ban(ctx: &Ctx, account: &str, banned: bool, reason: &[u8]) -> Option<String> {
    let result = if banned {
        let until = ban_on(ctx.bans, account).and_then(|b| b.until);
        ctx.bans
            .add(Ban {
                account: account.to_string(),
                until,
                admin: ctx.admin.to_string(),
                reason: reason_line(reason),
            })
            .map(|()| "banned")
    } else {
        ctx.bans
            .remove(account)
            .map(|was| if was { "ban lifted" } else { "" })
    };
    match result {
        Ok("") => None,
        Ok(what) => Some(what.to_string()),
        Err(e) => Some(format!("ban not written: {e}")),
    }
}

/// Carry out `op` on the account and ban files.
pub fn run(ctx: &Ctx, op: Op) -> Done {
    let (store, online, start) = (ctx.store, ctx.online, ctx.start);
    let log = |s: String| Done {
        log: Some(s),
        ..Done::default()
    };
    match op {
        Op::List {
            pattern,
            conditions,
        } => {
            let mut ignored = Vec::new();
            let mut tests = Vec::new();
            for c in conditions.split(',').filter(|c| !c.trim().is_empty()) {
                match condition(c) {
                    Some(t) => tests.push(t),
                    None => ignored.push(c.trim().to_string()),
                }
            }
            let mut p = Vec::new();
            for name in store.list().unwrap_or_default() {
                if !matches(&pattern, &name) {
                    continue;
                }
                if !tests.is_empty() {
                    let Ok(Some(a)) = store.load(&name) else {
                        continue;
                    };
                    if !tests.iter().all(|t| t(&a)) {
                        continue;
                    }
                }
                gstring(&latin1(&name), &mut p);
            }
            Done {
                reply: msg(PLO_RC_ACCOUNTLIST, &p),
                log: (!ignored.is_empty())
                    .then(|| format!("account list: conditions not read: {}", ignored.join(", "))),
                ..Done::default()
            }
        }
        Op::Get(name) => match valid_account_name(&name).then(|| store.load(&name)) {
            Some(Ok(Some(a))) => Done {
                reply: account_reply(&a, ban_on(ctx.bans, &a.name).as_ref()),
                ..Done::default()
            },
            _ => log(format!("account get {name}: no such account")),
        },
        Op::Set(f) | Op::Add(f) if !valid_account_name(&f.account) => {
            log(format!("account {}: not a valid name", f.account))
        }
        Op::Set(f) => match store.load(&f.account) {
            Ok(Some(mut a)) => {
                apply(&mut a, &f);
                let ban = set_ban(ctx, &a.name, f.banned, &f.ban_reason)
                    .map_or(String::new(), |b| format!(" ({b})"));
                let mut d = saved(store, &a, format!("account set {}{ban}", a.name));
                d.kick = kick_if_online(ctx, &a.name, f.banned);
                d
            }
            _ => log(format!("account set {}: no such account", f.account)),
        },
        Op::Add(f) => match store.load(&f.account) {
            Ok(None) => {
                let mut a = Account::new(&f.account, start.0, start.1, start.2);
                apply(&mut a, &f);
                let ban = set_ban(ctx, &a.name, f.banned, &f.ban_reason)
                    .map_or(String::new(), |b| format!(" ({b})"));
                saved(store, &a, format!("account add {}{ban}", a.name))
            }
            _ => log(format!("account add {}: it exists", f.account)),
        },
        Op::RightsGet(name) | Op::CommentsGet(name) | Op::BanGet(name)
            if !valid_account_name(&name) =>
        {
            log(format!("account {name}: not a valid name"))
        }
        Op::RightsGet(name) => match store.load(&name) {
            Ok(Some(a)) => {
                let mut p = Vec::new();
                gstring(&latin1(&a.name), &mut p);
                crate::wire::gint5(u64::from(a.rc_rights), &mut p);
                gstring(&latin1(&a.ip_ranges.join(",")), &mut p);
                let folders = tokenized(&a.folder_rights);
                let folders = &folders[..folders.len().min(usize::from(u16::MAX >> 2))];
                crate::wire::gshort(folders.len() as u16, &mut p);
                p.extend_from_slice(folders);
                Done {
                    reply: msg(PLO_RC_PLAYERRIGHTSGET, &p),
                    ..Done::default()
                }
            }
            _ => log(format!("rights get {name}: no such account")),
        },
        Op::CommentsGet(name) => match store.load(&name) {
            Ok(Some(a)) => {
                let mut p = Vec::new();
                gstring(&latin1(&a.name), &mut p);
                p.extend(tokenized(&a.comments));
                Done {
                    reply: msg(PLO_RC_PLAYERCOMMENTSGET, &p),
                    ..Done::default()
                }
            }
            _ => log(format!("comments get {name}: no such account")),
        },
        Op::RightsSet(r) => match valid_account_name(&r.account).then(|| store.load(&r.account)) {
            Some(Ok(Some(mut a))) => {
                a.rc_rights = r.rights;
                a.ip_ranges = r
                    .ip_range
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                a.folder_rights = r
                    .folders
                    .iter()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                let what = format!(
                    "rights set {}: {:#x}, from {:?}, folders {:?}",
                    a.name, a.rc_rights, a.ip_ranges, a.folder_rights
                );
                saved(store, &a, what)
            }
            _ => log(format!("rights set {}: no such account", r.account)),
        },
        Op::CommentsSet { account, lines } => {
            match valid_account_name(&account).then(|| store.load(&account)) {
                Some(Ok(Some(mut a))) => {
                    a.comments = lines;
                    saved(store, &a, format!("comments set {}", a.name))
                }
                _ => log(format!("comments set {account}: no such account")),
            }
        }
        Op::BanGet(name) => match store.load(&name) {
            Ok(Some(a)) => {
                let ban = ban_on(ctx.bans, &a.name);
                let mut p = Vec::new();
                gstring(&latin1(&a.name), &mut p);
                p.push(gchar(u8::from(ban.is_some())));
                p.extend(reason_tokens(ban.as_ref()));
                Done {
                    reply: msg(PLO_RC_PLAYERBANGET, &p),
                    ..Done::default()
                }
            }
            _ => log(format!("ban get {name}: no such account")),
        },
        Op::BanSet {
            account,
            banned,
            reason,
        } => match valid_account_name(&account).then(|| store.load(&account)) {
            Some(Ok(Some(a))) => Done {
                log: Some(format!(
                    "ban set {}: {}",
                    a.name,
                    set_ban(ctx, &a.name, banned, &reason)
                        .as_deref()
                        .unwrap_or("not banned")
                )),
                kick: kick_if_online(ctx, &a.name, banned),
                ..Done::default()
            },
            _ => log(format!("ban set {account}: no such account")),
        },
        Op::PropsGetId(id) => log(format!("props get player {id}: not online")),
        Op::PropsGet { id, account } => {
            match valid_account_name(&account).then(|| store.load(&account)) {
                Some(Ok(Some(mut a))) => {
                    if let Some(l) = ctx.world.and_then(|w| w.snapshot_blocking(&a.name)) {
                        l.apply_to(&mut a);
                    }
                    Done {
                        reply: crate::rcprops::reply(id, &a),
                        ..Done::default()
                    }
                }
                _ => log(format!("props get {account}: no such account")),
            }
        }
        Op::PropsSet(set) => {
            let is_online = online.contains(&set.account.to_ascii_lowercase());
            if is_online && ctx.world.is_none() {
                return log(format!(
                    "props set {}: refused, the player is online",
                    set.account
                ));
            }
            match valid_account_name(&set.account).then(|| store.load(&set.account)) {
                Some(Ok(Some(mut a))) => {
                    // Online: the player first, so a logout after this
                    // writes the new values; then the file, for the rest.
                    let live = is_online
                        && ctx
                            .world
                            .is_some_and(|w| w.rc_props_set_blocking(set.clone()));
                    crate::rcprops::apply(&mut a, &set);
                    let what = if live { " (online)" } else { "" };
                    saved(store, &a, format!("props set {}{what}", a.name))
                }
                _ => log(format!("props set {}: no such account", set.account)),
            }
        }
        Op::Delete(name) => {
            if !valid_account_name(&name) {
                return log(format!("account delete {name}: not a valid name"));
            }
            if online.contains(&name.to_ascii_lowercase()) {
                return log(format!("account delete {name}: refused, it is online"));
            }
            match store.delete(&name) {
                Ok(true) => log(format!("account delete {name}")),
                Ok(false) => log(format!("account delete {name}: no such account")),
                Err(e) => log(format!("account delete {name}: {e}")),
            }
        }
    }
}

/// The account to disconnect when a ban was just set on it while online.
fn kick_if_online(ctx: &Ctx, name: &str, banned: bool) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    (banned && ctx.online.contains(&lower)).then_some(lower)
}

fn apply(a: &mut Account, f: &Fields) {
    if !f.password.is_empty() {
        a.set_password(&f.password);
    }
    a.email = f.email.clone();
    a.load_only = f.load_only;
    a.admin_level = f.admin_level;
    a.admin_worlds = f.admin_worlds.clone();
}

fn saved(store: &Store, a: &Account, what: String) -> Done {
    Done {
        reply: Vec::new(),
        log: Some(match store.save(a) {
            Ok(()) => what,
            Err(e) => format!("{what}: {e}"),
        }),
        ..Done::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::split_messages;

    const START: Start = ("start.nw", 30.0, 30.0);

    /// `run` with the ban file in a `bans` folder beside the accounts.
    fn go(store: &Store, online: &[String], op: Op) -> Done {
        let bans = Bans::new(&store.dir().with_extension("bans"));
        let ctx = Ctx {
            store,
            bans: &bans,
            online,
            start: START,
            admin: "boss",
            world: None,
        };
        run(&ctx, op)
    }

    #[test]
    fn patterns() {
        assert!(matches("%", "anyone"));
        assert!(matches("b%", "Bob"));
        assert!(matches("%o%", "bob"));
        assert!(!matches("b%", "alice"));
        assert!(matches("bob", "BOB"));
        assert!(!matches("bob", "bobby"));
        assert!(matches("", ""));
    }

    #[test]
    fn list_get_set_add_delete() {
        let dir = std::env::temp_dir().join(format!("seihai-rcacc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        for n in ["alice", "bob", "boss"] {
            store.save(&Account::new(n, "start.nw", 1.0, 1.0)).unwrap();
        }
        let list = |pattern: &str, conditions: &str| {
            let d = go(
                &store,
                &[],
                Op::List {
                    pattern: pattern.into(),
                    conditions: conditions.into(),
                },
            );
            let m = &split_messages(&d.reply)[0];
            assert_eq!(m.id, PLO_RC_ACCOUNTLIST);
            let mut i = 0;
            let mut names = Vec::new();
            while let Some(s) = read_gstring(&m.payload, &mut i) {
                names.push(text(&s));
            }
            (names, d.log)
        };
        assert_eq!(list("b%", "").0, ["bob", "boss"]);
        assert_eq!(list("%", "adminlevel>0").0, Vec::<String>::new());

        // A set through the wire layout, read back.
        let mut p = Vec::new();
        gstring(b"bob", &mut p);
        gstring(b"newpw", &mut p);
        gstring(b"bob@example.org", &mut p);
        p.extend([gchar(0), gchar(1), gchar(2)]);
        gstring(b"main", &mut p);
        gstring(b"", &mut p);
        let op = parse(PLI_RC_ACCOUNTSET, &p).unwrap();
        assert!(go(&store, &[], op).reply.is_empty());
        let bob = store.load("bob").unwrap().unwrap();
        assert!(bob.check_password(b"newpw"));
        assert_eq!((bob.admin_level, bob.load_only), (2, true));
        assert!(!bob.staff, "an admin level is not RC use");
        let (names, log) = list("%", "adminlevel>1,online=1");
        assert_eq!(names, ["bob"]);
        assert!(log.unwrap().contains("online=1"));

        let got = go(&store, &[], parse(PLI_RC_ACCOUNTGET, b"Bob").unwrap()).reply;
        let m = &split_messages(&got)[0];
        assert_eq!(m.id, PLO_RC_ACCOUNTGET);
        let mut want = Vec::new();
        gstring(b"Bob", &mut want);
        gstring(b"", &mut want);
        gstring(b"bob@example.org", &mut want);
        want.extend([gchar(0), gchar(1), gchar(2)]);
        gstring(b"main", &mut want);
        want.extend([0x20, 0x20]);
        assert_eq!(m.payload, want);

        // An add of a new name, then of one that exists.
        let mut add = p.clone();
        add[1..4].copy_from_slice(b"cat");
        let d = go(&store, &[], parse(PLI_RC_ACCOUNTADD, &add).unwrap());
        assert_eq!(d.log.as_deref(), Some("account add cat"));
        let cat = store.load("cat").unwrap().unwrap();
        assert!(cat.check_password(b"newpw"));
        assert_eq!(cat.level, "start.nw");
        let d = go(&store, &[], parse(PLI_RC_ACCOUNTADD, &add).unwrap());
        assert!(d.log.unwrap().contains("exists"));

        // Delete: not while online.
        let d = go(&store, &["cat".into()], Op::Delete("Cat".into()));
        assert!(d.log.unwrap().contains("online"));
        go(&store, &[], Op::Delete("cat".into()));
        assert!(store.load("cat").unwrap().is_none());
        assert!(go(&store, &[], Op::Get("../x".into())).reply.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
