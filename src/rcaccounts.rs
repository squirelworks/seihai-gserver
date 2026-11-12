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
//! cannot raise an account to RC use. The ban fields are not read or
//! written here yet: the ban file (`bans.rs`) comes with the anti-cheat
//! PR, and the next slice joins them. A get says not banned.
//!
//! Conditions: `adminlevel` with `>`, `<` or `=` and a number is applied;
//! any other condition is ignored and logged. A set or add of an account
//! name that does not pass `login::valid_account_name`, an add of one that
//! exists, a set of one that does not, and a delete of one that is online
//! are refused in the log only, as the RC gets no reply either way.

use crate::accounts::{Account, Store};
use crate::login::valid_account_name;
use crate::wire::{gchar, gstring, latin1, msg, text};

/// `PLO_RC_ACCOUNTLISTGET` reply id.
pub const PLO_RC_ACCOUNTLIST: u8 = 70;
/// `PLO_RC_ACCOUNTGET` reply id.
pub const PLO_RC_ACCOUNTGET: u8 = 73;

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
    List { pattern: String, conditions: String },
    Get(String),
    Set(Fields),
    Add(Fields),
    Delete(String),
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

/// `PLO 73` for `a`.
pub fn account_reply(a: &Account) -> Vec<u8> {
    let mut p = Vec::new();
    gstring(&latin1(&a.name), &mut p);
    gstring(b"", &mut p);
    gstring(&latin1(&a.email), &mut p);
    p.extend([
        gchar(0),
        gchar(u8::from(a.load_only)),
        gchar(a.admin_level.min(0xDF)),
    ]);
    gstring(&latin1(&a.admin_worlds), &mut p);
    p.push(0x20);
    gstring(b"", &mut p);
    msg(PLO_RC_ACCOUNTGET, &p)
}

/// What an account request did: the reply for the RC (empty for none) and
/// a line for the log.
#[derive(Debug, Default)]
pub struct Done {
    pub reply: Vec<u8>,
    pub log: Option<String>,
}

/// Where an added account starts: the world's start level and spot.
pub type Start<'a> = (&'a str, f32, f32);

/// Carry out `op` on the account files. `online` are the accounts
/// online now, lower case.
pub fn run(store: &Store, online: &[String], start: Start, op: Op) -> Done {
    let log = |s: String| Done {
        reply: Vec::new(),
        log: Some(s),
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
            }
        }
        Op::Get(name) => match valid_account_name(&name).then(|| store.load(&name)) {
            Some(Ok(Some(a))) => Done {
                reply: account_reply(&a),
                log: None,
            },
            _ => log(format!("account get {name}: no such account")),
        },
        Op::Set(f) | Op::Add(f) if !valid_account_name(&f.account) => {
            log(format!("account {}: not a valid name", f.account))
        }
        Op::Set(f) => match store.load(&f.account) {
            Ok(Some(mut a)) => {
                apply(&mut a, &f);
                saved(store, &a, format!("account set {}", a.name))
            }
            _ => log(format!("account set {}: no such account", f.account)),
        },
        Op::Add(f) => match store.load(&f.account) {
            Ok(None) => {
                let mut a = Account::new(&f.account, start.0, start.1, start.2);
                apply(&mut a, &f);
                saved(store, &a, format!("account add {}", a.name))
            }
            _ => log(format!("account add {}: it exists", f.account)),
        },
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::split_messages;

    const START: Start = ("start.nw", 30.0, 30.0);

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
            let d = run(
                &store,
                &[],
                START,
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
        assert!(run(&store, &[], START, op).reply.is_empty());
        let bob = store.load("bob").unwrap().unwrap();
        assert!(bob.check_password(b"newpw"));
        assert_eq!((bob.admin_level, bob.load_only), (2, true));
        assert!(!bob.staff, "an admin level is not RC use");
        let (names, log) = list("%", "adminlevel>1,online=1");
        assert_eq!(names, ["bob"]);
        assert!(log.unwrap().contains("online=1"));

        let got = run(
            &store,
            &[],
            START,
            parse(PLI_RC_ACCOUNTGET, b"Bob").unwrap(),
        )
        .reply;
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
        let d = run(&store, &[], START, parse(PLI_RC_ACCOUNTADD, &add).unwrap());
        assert_eq!(d.log.as_deref(), Some("account add cat"));
        let cat = store.load("cat").unwrap().unwrap();
        assert!(cat.check_password(b"newpw"));
        assert_eq!(cat.level, "start.nw");
        let d = run(&store, &[], START, parse(PLI_RC_ACCOUNTADD, &add).unwrap());
        assert!(d.log.unwrap().contains("exists"));

        // Delete: not while online.
        let d = run(&store, &["cat".into()], START, Op::Delete("Cat".into()));
        assert!(d.log.unwrap().contains("online"));
        run(&store, &[], START, Op::Delete("cat".into()));
        assert!(store.load("cat").unwrap().is_none());
        assert!(run(&store, &[], START, Op::Get("../x".into()))
            .reply
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
