//! The login line: the first message of the first frame.
//!
//! ```text
//! GCHAR type            0 for 1.21 to 2.17, 5 for 2.22 and later
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
    /// A login type this server does not take yet (RC, NC, NPC-server).
    Type(u8),
    BadAccount,
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoginError::Short => write!(f, "login line too short"),
            LoginError::Type(t) => write!(f, "login type {t} is not served yet"),
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

/// The first known build for `token` and `gen`. Several builds share a
/// token (5.30r3, 5.31r4 and 6.00r2 all send `G3D26090`); the oldest is
/// taken, which only matters for which messages it is sent.
pub fn version_for(token: &str, gen: EncryptionGen) -> Option<ClientVersion> {
    versions_for(token, gen).next()
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
        5 => {
            let t = token_at(1).ok_or(LoginError::Short)?;
            (EncryptionGen::Gen5, rest[0].wrapping_sub(0x20), t, 9)
        }
        other => return Err(LoginError::Type(other)),
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
        assert_eq!(
            parse(b"!!GSERV025\"ab\"cd\n", &[]),
            Err(LoginError::Type(1))
        );
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
