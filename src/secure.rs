//! The Seihai secure login (`SEI00001`) on the gserver side.
//!
//! Spec: Seihai's `documentation/secure-login.md`. The messages and the
//! cryptography are `seihai_core::protocol::seclogin`, shared with the
//! client; this module only fits them to the login path in `server`:
//!
//! 1. [`token_at`] spots C1 (the token where a version token goes).
//! 2. [`respond`] reads C1 and makes S1 and the opener.
//! 3. [`open`] opens C2 and rebuilds the classic login frame body
//!    (`prefix`, the inner token, the sealed tail, then the rest of C2's
//!    frame), so `login::parse` and every later check run unchanged.

use seihai_core::protocol::seclogin::{self, ClientHello, Error, Opener, ServerIdentity};
use seihai_core::protocol::zlib_compress;
use std::io;

/// Where `SEI00001` starts in a login line: after type and key (2), or
/// after the type alone (1, a gen-2 inner build). `None`: a classic login.
pub fn token_at(line: &[u8]) -> Option<usize> {
    [2, 1]
        .into_iter()
        .find(|&at| line.get(at..at + 8).is_some_and(seclogin::is_token))
}

/// What the server needs between S1 and C2.
pub struct Reply {
    /// S1, no line feed.
    pub s1: Vec<u8>,
    pub opener: Opener,
    /// C1's bytes before the token (type and key).
    pub prefix: Vec<u8>,
    /// The inner build's token.
    pub inner: [u8; 8],
}

/// Read C1 and make S1.
pub fn respond(line: &[u8], at: usize, id: &ServerIdentity) -> Result<Reply, Error> {
    let hello = ClientHello::parse(line, at).ok_or(Error::Malformed("no token"))??;
    let (s1, opener) = hello.respond(id)?;
    Ok(Reply {
        s1,
        opener,
        prefix: hello.prefix.clone(),
        inner: hello.inner,
    })
}

/// Open C2 (`body`, its frame decompressed) and rebuild the classic login
/// frame body: the login line with its line feed, then the bundled
/// messages that followed C2.
pub fn open(body: &[u8], opener: Opener, prefix: &[u8], inner: &[u8; 8]) -> Result<Vec<u8>, Error> {
    let nl = body
        .iter()
        .position(|&b| b == b'\n')
        .map_or(body.len(), |i| i + 1);
    let (c2, rest) = body.split_at(nl);
    let tail = opener.open(c2)?;
    let mut out = prefix.to_vec();
    out.extend_from_slice(inner);
    out.extend_from_slice(&tail);
    out.push(b'\n');
    out.extend_from_slice(rest);
    Ok(out)
}

/// `line` and a line feed as one plain zlib frame (how S1 goes out).
pub fn plain_frame(line: &[u8]) -> io::Result<Vec<u8>> {
    let z = zlib_compress(&[line, b"\n"].concat())?;
    let len = u16::try_from(z.len()).map_err(|_| io::Error::other("S1 frame too long"))?;
    Ok([&len.to_be_bytes()[..], &z].concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use seihai_core::protocol::seclogin::ClientHandshake;
    use seihai_core::protocol::{write_login, ClientVersion};

    /// What `open` rebuilds is byte for byte the classic login frame.
    #[test]
    fn rebuilt_frame_is_the_classic_one() {
        for v in [
            ClientVersion::V139,
            ClientVersion::V205,
            ClientVersion::V231,
            ClientVersion::V512,
        ] {
            let mut classic = Vec::new();
            write_login(&mut classic, v, 0x2B, "tester", "pw").unwrap();
            let at = if v.token().len() == 8 && classic[1..9] == *v.token().as_bytes() {
                1
            } else {
                2
            };
            let (prefix, rest) = classic.split_at(at);
            let tail = &rest[8..];
            let hs = ClientHandshake::new(prefix, v.token(), b"").unwrap();
            assert_eq!(token_at(hs.hello()), Some(at), "{v:?}");
            let id = ServerIdentity::generate();
            let r = respond(hs.hello(), at, &id).unwrap();
            let c2 = hs.finish(&r.s1).unwrap().sealer.seal(tail).unwrap();
            let body = [&c2[..], b"\nEEnglish\n"].concat();
            let rebuilt = open(&body, r.opener, &r.prefix, &r.inner).unwrap();
            assert_eq!(rebuilt, [&classic[..], b"\nEEnglish\n"].concat(), "{v:?}");
        }
        assert_eq!(token_at(b" !GNW03081\"ab\"cd"), None);
    }
}
