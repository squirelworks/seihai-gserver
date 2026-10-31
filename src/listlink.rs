//! The listserver link: register, keep the player count, check accounts.
//!
//! Off unless `listserver = host:port` is set, so a lab server never
//! registers anywhere unless told to.
//!
//! The link is encrypted by default (Seihai `documentation/secure-login.md`,
//! "Gserver to listserver link"): C1 with the inner token `SVLINK01`, the
//! listserver's signed S1, then the classic link inside ChaCha20-Poly1305
//! records. That matters because account checks (`VERIACC2`, 17) carry the
//! player's password, whatever client the player uses. The listserver's key
//! is pinned: `listserver_key`, or trust on first use in
//! `config/listserver.pin`. `listserver_secure = false` speaks the plain
//! link, for a listserver that does not know the handshake.
//!
//! Inside, the classic link: `SVO_REGISTERV3` (30) as one line, then zlib
//! frames. Sent: `NEWSERVER` (22), `SETPLYR` (7), `PLYRADD` (14),
//! `PLYRREM` (15), `VERIACC2` (17), `SETIP` (5) as the keep-alive.
//! Read: `SVI_VERIACC2` (11).
//!
//! It runs on two plain threads (one writes, one reads), not on tokio: the
//! channel's halves are blocking, and the link is one connection.

use crate::config::Config;
use seihai_core::protocol::seclogin::{
    self, ChannelRecv, ChannelSend, ClientHandshake, PinCheck, Pins,
};
use seihai_core::protocol::{zlib_compress, zlib_decompress};
use std::collections::{BTreeSet, HashMap};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod svo {
    pub const SETIP: u8 = 5;
    pub const SETPLYR: u8 = 7;
    pub const PLYRADD: u8 = 14;
    pub const PLYRREM: u8 = 15;
    pub const VERIACC2: u8 = 17;
    pub const NEWSERVER: u8 = 22;
    pub const REGISTERV3: u8 = 30;
}
const SVI_VERIACC2: u8 = 11;

/// How long a login waits for the listserver's answer.
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(10);
const KEEPALIVE: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_secs(15);

/// The listserver's answer to an account check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// The listserver's error text.
    Refused(String),
    /// No link, or no answer in time.
    Unreachable,
}

enum Cmd {
    Added(u16),
    Removed(u16),
    Verify {
        account: String,
        password: Vec<u8>,
        player: u16,
        reply: Sender<Verdict>,
    },
}

/// The handle the rest of the server uses.
pub struct ListLink {
    tx: Mutex<Sender<Cmd>>,
    state: Arc<Mutex<State>>,
    /// Ticket numbers for account checks (the "player" field of
    /// `VERIACC2`, echoed back): the player has no id yet at that point.
    ticket: std::sync::atomic::AtomicU16,
}

#[derive(Default)]
struct State {
    connected: bool,
    players: BTreeSet<u16>,
    /// Account checks in flight, by (lower-case account, player id).
    pending: HashMap<(String, u16), Sender<Verdict>>,
}

/// A GCHAR.
fn gchar(n: u8) -> u8 {
    n.saturating_add(32)
}

fn gstr(out: &mut Vec<u8>, s: &[u8]) {
    let s = &s[..s.len().min(223)];
    out.push(s.len() as u8 + 32);
    out.extend_from_slice(s);
}

fn gshort(out: &mut Vec<u8>, v: u16) {
    out.push(((v >> 7) & 0x7F) as u8 + 32);
    out.push((v & 0x7F) as u8 + 32);
}

fn message(id: u8, payload: &[u8]) -> Vec<u8> {
    let mut m = vec![gchar(id)];
    m.extend(payload.iter().copied().filter(|&b| b != b'\n'));
    m.push(b'\n');
    m
}

fn zframe(body: &[u8]) -> io::Result<Vec<u8>> {
    let z = zlib_compress(body)?;
    let len = u16::try_from(z.len()).map_err(|_| io::Error::other("listserver frame too long"))?;
    Ok([&len.to_be_bytes()[..], &z].concat())
}

impl ListLink {
    /// Start the link in the background (it reconnects on its own).
    /// `None` when `listserver` is empty.
    pub fn start(cfg: &Config) -> Option<Arc<Self>> {
        if cfg.listserver.is_empty() {
            return None;
        }
        let (tx, rx) = mpsc::channel();
        let link = Arc::new(Self {
            tx: Mutex::new(tx),
            state: Arc::default(),
            ticket: Default::default(),
        });
        let state = link.state.clone();
        let cfg = cfg.clone();
        std::thread::Builder::new()
            .name("listlink".into())
            .spawn(move || run(cfg, rx, state))
            .ok()?;
        Some(link)
    }

    fn send(&self, c: Cmd) {
        let _ = self.tx.lock().unwrap().send(c);
    }

    pub fn connected(&self) -> bool {
        self.state.lock().unwrap().connected
    }

    pub fn player_added(&self, id: u16) {
        self.send(Cmd::Added(id));
    }

    pub fn player_removed(&self, id: u16) {
        self.send(Cmd::Removed(id));
    }

    /// Ask the listserver whether `account` and `password` are right. The
    /// answer comes on the receiver within [`VERIFY_TIMEOUT`].
    pub fn verify(&self, account: &str, password: &[u8]) -> Receiver<Verdict> {
        let (reply, rx) = mpsc::channel();
        let player = self
            .ticket
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % ((1 << 14) - 1)
            + 1;
        if !self.connected() {
            let _ = reply.send(Verdict::Unreachable);
        } else {
            self.send(Cmd::Verify {
                account: account.to_string(),
                password: password.to_vec(),
                player,
                reply,
            });
        }
        rx
    }
}

/// Where the sending side writes: plain or sealed.
enum Out {
    Plain(TcpStream),
    Sealed(TcpStream, ChannelSend),
}

impl Out {
    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            Out::Plain(s) => s.write_all(data),
            Out::Sealed(s, ch) => s.write_all(&ch.seal(data)),
        }?;
        match self {
            Out::Plain(s) | Out::Sealed(s, _) => s.flush(),
        }
    }
}

/// Connect, and for the encrypted link run the handshake and check the pin.
fn connect(cfg: &Config) -> io::Result<(Out, BufReader<TcpStream>, Option<ChannelRecv>)> {
    let addr = cfg.listserver.to_socket_addrs()?.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "listserver address resolves to nothing",
        )
    })?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    let _ = stream.set_nodelay(true);
    let mut w = stream.try_clone()?;
    let mut r = BufReader::new(stream);
    if !cfg.listserver_secure {
        return Ok((Out::Plain(w), r, None));
    }
    let bad = |e: seclogin::Error| io::Error::new(io::ErrorKind::InvalidData, e.to_string());
    let hs = ClientHandshake::new(b"", seclogin::LINK_TOKEN, b"").map_err(bad)?;
    w.write_all(&[hs.hello(), b"\n"].concat())?;
    w.flush()?;
    r.get_ref()
        .set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut s1 = Vec::new();
    (&mut r).take(1024).read_until(b'\n', &mut s1)?;
    r.get_ref().set_read_timeout(None)?;
    if !s1.starts_with(seclogin::TOKEN.as_bytes()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the listserver does not speak the encrypted link (set listserver_secure = false only if you trust the network)",
        ));
    }
    let v = hs.finish(&s1).map_err(bad)?;
    let fp = seclogin::fingerprint(&v.server_key);
    match seclogin::decode_key(&cfg.listserver_key) {
        Some(k) if k == v.server_key => {}
        Some(k) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "listserver key {fp} is not listserver_key {}",
                    seclogin::fingerprint(&k)
                ),
            ))
        }
        None => {
            let path = cfg.listserver_pin_path();
            match Pins::load(&path)?.check(&cfg.listserver, &v.server_key) {
                PinCheck::Matches => {}
                PinCheck::Unknown => {
                    Pins::append(&path, &cfg.listserver, &v.server_key)?;
                    eprintln!("listserver link: first visit to {}; trusting its key {fp} (saved to {})", cfg.listserver, path.display());
                }
                PinCheck::Mismatch { pinned } => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "listserver key CHANGED: was {}, now {fp}; not registering. Remove its line from {} if that was on purpose",
                            seclogin::fingerprint(&pinned),
                            path.display()
                        ),
                    ))
                }
            }
        }
    }
    let (tx, rx) = v.sealer.into_channel().split();
    Ok((Out::Sealed(w, tx), r, Some(rx)))
}

/// The register line and the first frame: the entry and the players.
fn hello(cfg: &Config, players: &BTreeSet<u16>) -> io::Result<Vec<u8>> {
    let mut out = message(
        svo::REGISTERV3,
        concat!("seihai-gserver ", env!("CARGO_PKG_VERSION")).as_bytes(),
    );
    let mut p = Vec::new();
    let port = cfg.port.to_string();
    for f in [
        cfg.name.as_bytes(),
        cfg.description.as_bytes(),
        b"English",
        b"2.31",
        b"",
        b"AUTO",
        port.as_bytes(),
        b"",
    ] {
        gstr(&mut p, f);
    }
    let mut body = message(svo::NEWSERVER, &p);
    body.extend(message(svo::SETPLYR, b""));
    for &id in players {
        body.extend(added(id));
    }
    out.extend(zframe(&body)?);
    Ok(out)
}

fn added(id: u16) -> Vec<u8> {
    let mut p = Vec::new();
    gshort(&mut p, id);
    p.push(gchar(0));
    message(svo::PLYRADD, &p)
}

/// The reading side: decrypted (or plain) bytes in, `SVI_VERIACC2` answers out.
fn reader(mut r: BufReader<TcpStream>, mut rx: Option<ChannelRecv>, state: Arc<Mutex<State>>) {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let more = match rx.as_mut() {
            Some(ch) => ch.read_record(&mut r),
            None => {
                let mut b = [0u8; 4096];
                r.read(&mut b).map(|n| (n > 0).then(|| b[..n].to_vec()))
            }
        };
        match more {
            Ok(Some(b)) => buf.extend(b),
            Ok(None) | Err(_) => break,
        }
        while buf.len() >= 2 {
            let n = u16::from_be_bytes([buf[0], buf[1]]) as usize;
            if buf.len() < 2 + n {
                break;
            }
            let body: Vec<u8> = buf.drain(..2 + n).skip(2).collect();
            let Ok(raw) = zlib_decompress(&body) else {
                continue;
            };
            for line in raw.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
                answer(line, &state);
            }
        }
    }
    let mut st = state.lock().unwrap();
    st.connected = false;
    for (_, tx) in st.pending.drain() {
        let _ = tx.send(Verdict::Unreachable);
    }
}

/// `SVI_VERIACC2`: GSTRING account, the player GSHORT and type GCHAR as
/// sent, then `SUCCESS` or the error text.
fn answer(line: &[u8], state: &Mutex<State>) {
    if line.first().map(|b| b.wrapping_sub(32)) != Some(SVI_VERIACC2) {
        return;
    }
    let n = line.get(1).map_or(0, |b| b.wrapping_sub(32) as usize);
    let (Some(account), Some(player)) = (line.get(2..2 + n), line.get(2 + n..4 + n)) else {
        return;
    };
    let player = (u16::from(player[0].wrapping_sub(32) & 0x7F) << 7)
        | u16::from(player[1].wrapping_sub(32) & 0x7F);
    let text = line.get(5 + n..).unwrap_or_default();
    let key = (
        String::from_utf8_lossy(account).to_ascii_lowercase(),
        player,
    );
    if let Some(tx) = state.lock().unwrap().pending.remove(&key) {
        let v = if text == b"SUCCESS" {
            Verdict::Ok
        } else {
            Verdict::Refused(String::from_utf8_lossy(text).into_owned())
        };
        let _ = tx.send(v);
    }
}

fn run(cfg: Config, rx: Receiver<Cmd>, state: Arc<Mutex<State>>) {
    loop {
        let link = connect(&cfg).and_then(|(mut out, r, ch)| {
            let players = state.lock().unwrap().players.clone();
            out.write(&hello(&cfg, &players)?)?;
            Ok((out, r, ch))
        });
        let (mut out, r, ch) = match link {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "listserver link to {}: {e}; retrying in {}s",
                    cfg.listserver,
                    RETRY.as_secs()
                );
                if wait_or_apply(&rx, &state, RETRY).is_err() {
                    return;
                }
                continue;
            }
        };
        eprintln!(
            "listserver link to {}: registered ({})",
            cfg.listserver,
            if ch.is_some() { "encrypted" } else { "plain" }
        );
        state.lock().unwrap().connected = true;
        let st = state.clone();
        std::thread::spawn(move || reader(r, ch, st));
        let mut last = Instant::now();
        loop {
            if !state.lock().unwrap().connected {
                break;
            }
            let msg = match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Cmd::Added(id)) => {
                    state.lock().unwrap().players.insert(id);
                    added(id)
                }
                Ok(Cmd::Removed(id)) => {
                    state.lock().unwrap().players.remove(&id);
                    let mut p = Vec::new();
                    gshort(&mut p, id);
                    message(svo::PLYRREM, &p)
                }
                Ok(Cmd::Verify {
                    account,
                    password,
                    player,
                    reply,
                }) => {
                    let mut p = Vec::new();
                    gstr(&mut p, account.as_bytes());
                    gstr(&mut p, &password);
                    gshort(&mut p, player);
                    p.push(gchar(0));
                    state
                        .lock()
                        .unwrap()
                        .pending
                        .insert((account.to_ascii_lowercase(), player), reply);
                    message(svo::VERIACC2, &p)
                }
                Err(RecvTimeoutError::Timeout) if last.elapsed() >= KEEPALIVE => {
                    message(svo::SETIP, b"AUTO")
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            if zframe(&msg).and_then(|f| out.write(&f)).is_err() {
                break;
            }
            last = Instant::now();
        }
        state.lock().unwrap().connected = false;
        eprintln!(
            "listserver link to {}: closed; retrying in {}s",
            cfg.listserver,
            RETRY.as_secs()
        );
        if wait_or_apply(&rx, &state, RETRY).is_err() {
            return;
        }
    }
}

/// While the link is down: keep the player set current, answer account
/// checks as unreachable, and wait `d`. `Err` when the server is gone.
fn wait_or_apply(rx: &Receiver<Cmd>, state: &Mutex<State>, d: Duration) -> Result<(), ()> {
    let end = Instant::now() + d;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        match rx.recv_timeout(left) {
            Ok(Cmd::Added(id)) => {
                state.lock().unwrap().players.insert(id);
            }
            Ok(Cmd::Removed(id)) => {
                state.lock().unwrap().players.remove(&id);
            }
            Ok(Cmd::Verify { reply, .. }) => {
                let _ = reply.send(Verdict::Unreachable);
            }
            Err(RecvTimeoutError::Timeout) => return Ok(()),
            Err(RecvTimeoutError::Disconnected) => return Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn veriacc2_answers_reach_the_waiting_login() {
        let state = Mutex::new(State::default());
        let (tx, rx) = mpsc::channel();
        state
            .lock()
            .unwrap()
            .pending
            .insert(("test1".into(), 300), tx);
        let mut line = vec![gchar(SVI_VERIACC2)];
        gstr(&mut line, b"Test1");
        gshort(&mut line, 300);
        line.push(gchar(0));
        line.extend_from_slice(b"SUCCESS");
        answer(&line, &state);
        assert_eq!(rx.recv().unwrap(), Verdict::Ok);
    }
}
