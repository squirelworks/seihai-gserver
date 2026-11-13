//! The login line: the first message of the first frame.
//!
//! ```text
//! GCHAR type            0 for 1.21 to 2.17, 4 for 2.19 to 2.21 and 3.0 to
//!                       3.01 (gen 4), 5 for 2.22 and later
//! GCHAR key             not in gen 2 (1.21 to 1.39): the token follows the type
//! char[8] token         the build's version token, e.g. GNW03081
//! GSTRING account
//! GSTRING password
//! rest of the line      5.00 on: platform and machine fields, comma list
//! ```
//!
//! Gen 2 and gen 3 both use type 0, and the key byte of a gen-3 line can be
//! any printable byte, so the two are told apart by which offset holds a
//! token: the known tokens are `seihai-core`'s version profiles plus any
//! token the allow-list names.

use seihai_core::protocol::{ClientVersion, EncryptionGen};

/// A parsed login line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub client_type: u8,
    pub gen: EncryptionGen,
    /// The key byte (`0` in gen 2, which has none).
    pub key: u8,
    pub token: String,
    /// The build the token names, when `seihai-core` has a profile for it.
    pub version: Option<ClientVersion>,
    pub account: String,
    pub password: Vec<u8>,
    /// Whatever followed the password on the line (5.00 on: `win,...`).
    pub tail: Vec<u8>,
}

/// Why a login line could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginError {
    Short,
    /// A login type this server does not know.
    Type(u8),
    /// A login type this server knows but does not serve yet (RC, NC).
    /// `gen` and `key` are that type's framing, so the refusal can be
    /// sent in frames the client reads.
    NotServed {
        client_type: u8,
        gen: EncryptionGen,
        key: u8,
    },
    BadAccount,
}

/// The login types this server knows but does not serve yet, by name, and
/// the framing each one reads after its login frame. None now: NC (3) is
/// served since stage 4 slice 6 (`nc.rs`).
pub fn unserved_type(client_type: u8) -> Option<(&'static str, EncryptionGen)> {
    let _ = client_type;
    None
}

/// NPC Control (`nc.rs`): type 3, token `NCL11012`, no key byte, plain
/// zlib frames.
pub fn is_nc(client_type: u8) -> bool {
    client_type == crate::nc::NC_LOGIN_TYPE
}

/// RC login types (stage 4, `rc.rs`), from `seihai-rc`'s RC sessions:
///
/// - 1, the RCs from 2000 to 2002: no key byte, plain zlib frames;
/// - 6, the newer RCs: a key byte after the type, then gen-5 frames.
pub fn is_rc(client_type: u8) -> bool {
    matches!(client_type, 1 | 6)
}

/// Login type 4: the 2.19 to 2.21 and 3.0 to 3.01 clients (`G3D16053`,
/// `G3D27063`). After the login frame their frames are gen 4: no
/// compression byte, every body bz2, the first 4 dwords XORed with the
/// gen-5 LCG (graal_analysis `protocol/codec-and-framing.md`,
/// `ENCRYPT_GEN_4`). `seihai-core` has no gen 4, so the login reads as gen
/// 5 and the codec switches its framing.
pub fn is_gen4(client_type: u8) -> bool {
    client_type == 4
}

/// What the log calls the build that logged in. An RC's token (`GSERV025`
/// from RC2 2.05 and seihai-rc) and an NC's (`NCL11012`, `NCL15031`) are
/// not game builds, so they are "RC" and "NC" rather than "unknown build"
/// (lab, 2026-11-12).
pub fn build_label(client_type: u8, version: Option<ClientVersion>) -> &'static str {
    match version {
        _ if is_rc(client_type) => "RC",
        _ if is_nc(client_type) => "NC",
        Some(v) => v.label(),
        None => "unknown build",
    }
}

impl LoginError {
    /// The text the client is sent when the login is refused.
    pub fn client_text(&self) -> String {
        match self {
            LoginError::NotServed { client_type, .. } => {
                let name = unserved_type(*client_type).map_or("these", |(n, _)| n);
                format!("This server does not take {name} logins yet.")
            }
            _ => "This server cannot read that login.".to_string(),
        }
    }
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoginError::Short => write!(f, "login line too short"),
            LoginError::Type(t) => write!(f, "login type {t} is not taken by this server"),
            LoginError::NotServed { client_type, .. } => {
                let name = unserved_type(*client_type).map_or("?", |(n, _)| n);
                write!(f, "login type {client_type} ({name}) is not served yet")
            }
            LoginError::BadAccount => write!(
                f,
                "account name is empty or has characters this server does not allow"
            ),
        }
    }
}

/// The client builds (from `seihai-core`) that use `token` with `gen`.
pub fn versions_for(token: &str, gen: EncryptionGen) -> impl Iterator<Item = ClientVersion> + '_ {
    ClientVersion::all_versions()
        .iter()
        .copied()
        .filter(move |v| v.token() == token && v.encryption() == gen)
}

/// Builds `seihai-core` has no profile for yet, served as the nearest build
/// with the same login line and wire. Each token was read from its binary
/// and seen on the wire against the real client (graal-client-repro lab,
/// 2026-11-13):
///
/// - 1.25 (`GSERV009`) and 1.27 (`GSERV011`): gen 2, the 1.21 login line,
///   and a one-byte player id in `PLO_OTHERPLPROPS`, as 1.21;
/// - 1.40 rev 1 (`GNW23090`): gen 3 with the key byte and no language
///   packet, as 1.41 rev 1.
pub const STAND_INS: &[(&str, EncryptionGen, ClientVersion)] = &[
    ("GSERV009", EncryptionGen::Gen2, ClientVersion::V121),
    ("GSERV011", EncryptionGen::Gen2, ClientVersion::V121),
    ("GNW23090", EncryptionGen::Gen3, ClientVersion::V141),
];

/// The first known build for `token` and `gen`. Several builds share a
/// token (5.30r3, 5.31r4 and 6.00r2 all send `G3D26090`); the oldest is
/// taken, which only matters for which messages it is sent. A token in
/// [`STAND_INS`] gets its stand-in.
pub fn version_for(token: &str, gen: EncryptionGen) -> Option<ClientVersion> {
    versions_for(token, gen).next().or_else(|| {
        STAND_INS
            .iter()
            .find(|(t, g, _)| *t == token && *g == gen)
            .map(|&(_, _, v)| v)
    })
}

/// Read the login line. `extra_tokens` are tokens from the allow-list that
/// `seihai-core` has no profile for; they help tell a gen-2 line from a
/// gen-3 one.
pub fn parse(line: &[u8], extra_tokens: &[String]) -> Result<Login, LoginError> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let (&t, rest) = line.split_first().ok_or(LoginError::Short)?;
    let client_type = t.wrapping_sub(0x20);
    let token_at = |at: usize| -> Option<String> {
        let b = rest.get(at..at + 8)?;
        b.iter()
            .all(|c| c.is_ascii_alphanumeric())
            .then(|| String::from_utf8_lossy(b).into_owned())
    };
    let known =
        |tok: &str, gen| version_for(tok, gen).is_some() || extra_tokens.iter().any(|e| e == tok);
    let (gen, key, token, at) = match client_type {
        0 => {
            let gen2 = token_at(0).filter(|t| known(t, EncryptionGen::Gen2));
            let gen3 = token_at(1).filter(|t| known(t, EncryptionGen::Gen3));
            match (gen2, gen3) {
                (Some(t), _) => (EncryptionGen::Gen2, 0, t, 8),
                (None, Some(t)) => (EncryptionGen::Gen3, rest[0].wrapping_sub(0x20), t, 9),
                // Neither is known: read it as gen 3 (the key is there), so
                // the allow-list check names the token it really was.
                (None, None) => {
                    let t = token_at(1)
                        .or_else(|| token_at(0))
                        .ok_or(LoginError::Short)?;
                    (EncryptionGen::Gen3, rest[0].wrapping_sub(0x20), t, 9)
                }
            }
        }
        // Type 4 (gen 4) has the gen-5 line and cipher; only its frames
        // differ (`Login::gen4`, `Codec::gen4`).
        4..=6 => {
            let t = token_at(1).ok_or(LoginError::Short)?;
            (EncryptionGen::Gen5, rest[0].wrapping_sub(0x20), t, 9)
        }
        1 | 3 => (
            EncryptionGen::Gen2,
            0,
            token_at(0).ok_or(LoginError::Short)?,
            8,
        ),
        other => {
            return Err(match unserved_type(other) {
                Some((_, EncryptionGen::Gen5)) => LoginError::NotServed {
                    client_type: other,
                    gen: EncryptionGen::Gen5,
                    key: rest.first().ok_or(LoginError::Short)?.wrapping_sub(0x20) & 0x7F,
                },
                Some((_, gen)) => LoginError::NotServed {
                    client_type: other,
                    gen,
                    key: 0,
                },
                None => LoginError::Type(other),
            })
        }
    };
    let mut i = at;
    let mut gstring = || -> Result<Vec<u8>, LoginError> {
        let n = rest.get(i).ok_or(LoginError::Short)?.wrapping_sub(0x20) as usize;
        let s = rest
            .get(i + 1..i + 1 + n)
            .ok_or(LoginError::Short)?
            .to_vec();
        i += 1 + n;
        Ok(s)
    };
    let account = gstring()?;
    let password = gstring()?;
    let account = String::from_utf8(account).map_err(|_| LoginError::BadAccount)?;
    if !valid_account_name(&account) {
        return Err(LoginError::BadAccount);
    }
    Ok(Login {
        client_type,
        gen,
        key: key & 0x7F,
        version: version_for(&token, gen),
        token,
        account,
        password,
        tail: rest[i..].to_vec(),
    })
}

/// The machine ids on a 5.00 or later login line, from its tail: the
/// fields after the platform, as in
///
/// ```text
/// win,"<id>","<id>","<id>","6.2 9200 "
/// mac,"","<id>",""
/// linux,<id>,<id>,<id>,<kernel>
/// ```
///
/// The ids are hashes of the machine (the network card, the system
/// folder; `seihai-core`'s `protocol/chain.rs` says which per build).
/// Only the three after the platform count: the field after them is the
/// OS version or the kernel, which many machines share. Empty fields
/// (Seihai's own client sends `""`, and a MAC field is empty under Wine
/// or qemu) and fields that do not look like an id are left out. Lower
/// case. Builds before 5.00 send no tail and have none.
pub fn machine_ids(tail: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(tail);
    let mut fields = Vec::new();
    let (mut field, mut quoted) = (String::new(), false);
    for c in text.trim_end_matches(['\r', '\n']).chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            c => field.push(c),
        }
    }
    fields.push(field);
    let mut ids: Vec<String> = fields
        .iter()
        .skip(1)
        .take(3)
        .map(|f| f.trim())
        .filter(|f| valid_machine_id(f))
        .map(str::to_ascii_lowercase)
        .collect();
    ids.dedup();
    ids
}

/// A machine id this server keeps: 4 to 64 letters, digits, `-`, `_`,
/// `.` or `:` (an MD5 in hex is 32).
pub fn valid_machine_id(id: &str) -> bool {
    (4..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

/// Account names this server takes: 1 to 32 of letters, digits, `_`, `-`,
/// `.` and `@`, not starting with `.`. They name files, so nothing that
/// can leave the accounts folder gets through.
pub fn valid_account_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.@".contains(&b))
}

#[cfg(test)]
mod tests {

    #[test]
    fn rc_logins_are_labelled_rc() {
        assert_eq!(build_label(6, None), "RC");
        assert_eq!(build_label(1, None), "RC");
        assert_eq!(build_label(3, None), "NC");
        assert_eq!(build_label(0, None), "unknown build");
    }

    #[test]
    fn type_4_is_read_as_the_gen5_line() {
        // type 4 ('$'), key, the 3.01 token, account, password
        let l = parse(b"$7G3D27063'labacct'labpass\n", &["G3D27063".into()]).unwrap();
        assert_eq!(
            (l.client_type, l.gen, l.key),
            (4, EncryptionGen::Gen5, 0x17)
        );
        assert_eq!(
            (l.token.as_str(), l.account.as_str()),
            ("G3D27063", "labacct")
        );
        assert!(is_gen4(l.client_type) && !is_gen4(5));
    }

    #[test]
    fn stand_ins_for_125_127_140() {
        // gen 2, no key byte: id 0, token, account, password
        let l = parse(
            b" GSERV009%guest%guest
",
            &[],
        )
        .unwrap();
        assert_eq!(
            (l.gen, l.version),
            (EncryptionGen::Gen2, Some(ClientVersion::V121))
        );
        let l = parse(
            b" GSERV011%guest%guest
",
            &[],
        )
        .unwrap();
        assert_eq!(l.version, Some(ClientVersion::V121));
        // gen 3: type 0, key byte, token (the real 1.40r1 line, lab)
        let l = parse(
            b" rGNW23090(labguest%guest
",
            &[],
        )
        .unwrap();
        assert_eq!(
            (l.gen, l.key, l.token.as_str(), l.version),
            (
                EncryptionGen::Gen3,
                b'r' - 0x20,
                "GNW23090",
                Some(ClientVersion::V141)
            )
        );
        // a stand-in never hides a real profile
        for &(t, g, _) in STAND_INS {
            assert!(versions_for(t, g).next().is_none(), "{t} now has a profile");
        }
        assert_eq!(
            build_label(5, Some(ClientVersion::V222)),
            ClientVersion::V222.label()
        );
    }
    use super::*;
    use seihai_core::protocol::write_login;

    fn line(v: ClientVersion, key: u8) -> Vec<u8> {
        let mut b = Vec::new();
        write_login(&mut b, v, key, "tester", "pw").unwrap();
        b.push(b'\n');
        b
    }

    /// Every build Seihai can play logs in with the generation and token
    /// its profile names, whatever the key byte.
    #[test]
    fn every_profile_parses() {
        for &v in ClientVersion::all_versions() {
            for key in [0u8, 0x27, 0x41, 0x7F] {
                let l =
                    parse(&line(v, key), &[]).unwrap_or_else(|e| panic!("{v:?} key {key}: {e}"));
                assert_eq!(l.gen, v.encryption(), "{v:?}");
                assert_eq!(l.token, v.token(), "{v:?}");
                assert_eq!(l.version.map(|x| x.token()), Some(v.token()));
                assert_eq!(l.account, "tester");
                assert_eq!(l.password, b"pw");
                if v.encryption() != EncryptionGen::Gen2 {
                    assert_eq!(l.key, key & 0x7F);
                }
                assert_eq!(
                    l.tail.is_empty(),
                    v.profile().protocol.lobby.is_none(),
                    "{v:?}"
                );
            }
        }
    }

    #[test]
    fn machine_ids_from_the_tail() {
        let md5 = "0123456789ABCDEF0123456789abcdef";
        let win = format!("win,\"{md5}\",\"\",\"feedfacefeedface\",\"6.2 9200 \"");
        assert_eq!(
            machine_ids(win.as_bytes()),
            vec![md5.to_ascii_lowercase(), "feedfacefeedface".into()]
        );
        // Seihai's own client: nothing to ban on.
        assert!(machine_ids(b"win,\"\",\"\",\"\",\"6.2 9200 \"").is_empty());
        // The Mac build, and Linux with the kernel string that is not an id.
        assert_eq!(machine_ids(b"mac,\"\",abcd1234,\"\""), vec!["abcd1234"]);
        assert_eq!(
            machine_ids(b"linux,aaaa1111,bbbb2222,,Linux 5.10.0 x86_64"),
            vec!["aaaa1111", "bbbb2222"]
        );
        // Pre-5.00: no tail. Junk is not an id.
        assert!(machine_ids(b"").is_empty());
        assert!(machine_ids(b"win,\"a b\tc\",\"../x\",xy").is_empty());
    }

    #[test]
    fn unknown_tokens_and_types() {
        // A gen-3 line with a token nobody knows still names that token.
        let l = parse(b" !ZZZ12345\"ab\"cd\n", &[]).unwrap();
        assert_eq!(
            (l.gen, l.token.as_str(), l.version),
            (EncryptionGen::Gen3, "ZZZ12345", None)
        );
        // The same token from the allow-list, without a key: gen 2.
        let l = parse(b" ZZZ12345\"ab\"cd\n", &["ZZZ12345".into()]).unwrap();
        assert_eq!(l.gen, EncryptionGen::Gen2);
        // RCs: type 1 has no key and reads gen 2; type 6 keeps its key
        // and reads gen 5. Neither token is a game build.
        let l = parse(b"!GSERV023%admin\"pw\n", &[]).unwrap();
        assert_eq!(
            (l.client_type, l.gen, l.key, l.token.as_str(), l.version),
            (1, EncryptionGen::Gen2, 0, "GSERV023", None)
        );
        assert_eq!((l.account.as_str(), &l.password[..]), ("admin", &b"pw"[..]));
        let l = parse(b"&JGSERV025%admin\"pw\n", &[]).unwrap();
        assert_eq!(
            (l.client_type, l.gen, l.key, l.token.as_str()),
            (6, EncryptionGen::Gen5, 0x2A, "GSERV025")
        );
        assert!(is_rc(1) && is_rc(6) && !is_rc(0) && !is_rc(3) && !is_rc(5));
        let l = parse(b"#NCL11012\"ab\"cd\n", &[]).unwrap();
        assert_eq!(
            (l.client_type, l.gen, l.key, l.token.as_str()),
            (3, EncryptionGen::Gen2, 0, "NCL11012")
        );
        assert!(is_nc(3) && !is_nc(1));
        assert_eq!(parse(b"&", &[]), Err(LoginError::Short));
        assert_eq!(parse(b"\"x", &[]), Err(LoginError::Type(2)));
        assert_eq!(parse(b" !GNW03081", &[]), Err(LoginError::Short));
        assert_eq!(
            parse(b" !GNW03081#../\"cd\n", &[]),
            Err(LoginError::BadAccount)
        );
    }

    #[test]
    fn account_names() {
        assert!(valid_account_name("test205"));
        assert!(valid_account_name("a.b@c-d_e"));
        assert!(!valid_account_name(""));
        assert!(!valid_account_name(".hidden"));
        assert!(!valid_account_name("a/b"));
        assert!(!valid_account_name("a\\b"));
        assert!(!valid_account_name("a b"));
    }
}
