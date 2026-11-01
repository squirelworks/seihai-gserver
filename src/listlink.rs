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
//! `PLYRREM` (15), `VERIACC2` (17), `SVO_PMPLAYER` (29), `SETIP` (5) as
//! the keep-alive, and `SVO_PING` (16) only as the answer to the
//! listserver's ping (99). Id 16 is a pong: it is never sent on its own,
//! and a listserver does not answer it (answering 16 with another 99
//! would loop).
//! Read: `SVI_VERIACC2` (11), `SVI_PING` (99), `SVI_PMPLAYER` (29), and
//! the file transfer. `SVI_FILESTART` (2) and `SVI_FILESTART3` (15) both
//! open an empty partial (kind, then a GSTRING name). `SVI_FILEDATA3`
//! (16) appends to it. `SVI_FILEEND3` (17) finishes it and tells the
//! player. The retired data and end ids (3, 4, 12, 13, 14) are ignored.
//! `SVI_RAWDATA` (100) says the next GINT3 bytes are one message, so a
//! file chunk may hold any byte, including a newline.
//!
//! It runs on two plain threads (one writes, one reads), not on tokio: the
//! channel's halves are blocking, and the link is one connection. A
//! private message or a finished file wakes the player's session through
//! [`ListLink::notifier`].

use crate::config::Config;
use seihai_core::protocol::seclogin::{
    self, ChannelRecv, ChannelSend, ClientHandshake, PinCheck, Pins,
};
use seihai_core::protocol::{zlib_compress, zlib_decompress};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::sync::Notify;

mod svo {
    pub const SETIP: u8 = 5;
    pub const SETPLYR: u8 = 7;
    pub const PLYRADD: u8 = 14;
    pub const PLYRREM: u8 = 15;
    pub const PING: u8 = 16;
    pub const VERIACC2: u8 = 17;
    pub const NEWSERVER: u8 = 22;
    /// A private message for a player on another server.
    pub const PMPLAYER: u8 = 29;
    pub const REGISTERV3: u8 = 30;
}
const SVI_VERIACC2: u8 = 11;
/// Opens a file partial. Same fields as [`SVI_FILESTART3`].
const SVI_FILESTART: u8 = 2;
/// Opens a file partial: GCHAR kind, GSTRING name.
const SVI_FILESTART3: u8 = 15;
/// Appends to the partial opened for that kind and name.
const SVI_FILEDATA3: u8 = 16;
/// Finishes the partial and names the player it belongs to.
const SVI_FILEEND3: u8 = 17;
/// A private message for a player on this server. No player id: seven
/// text fields, packed the way [`seihai_core::net::gtokenize`] packs lines.
const SVI_PMPLAYER: u8 = 29;
/// The listserver's ping. Answered with [`svo::PING`], nothing else.
const SVI_PING: u8 = 99;
/// The next GINT3 bytes of the decompressed stream are one message.
const SVI_RAWDATA: u8 = 100;

/// Kind byte on a file-start message.
const KIND_HEAD: u8 = 0;
const KIND_BODY: u8 = 1;
const KIND_SWORD: u8 = 2;
const KIND_SHIELD: u8 = 3;

/// A partial, or one raw message, may not grow past this.
const MAX_PARTIAL: usize = 8 * 1024 * 1024;
/// Decompressed bytes held while a line or a raw message is still short.
const MAX_PLAIN: usize = 16 * 1024 * 1024;

/// First id handed to a player who is messaging in from another server.
const FIRST_EXTERNAL: u16 = 8192;

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

/// One player, as `PLYRADD` tells the listserver.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedPlayer {
    pub id: u16,
    pub account: String,
    pub nick: String,
    pub level: String,
    pub x: f32,
    pub y: f32,
    /// Alignment points (player prop 32). 0 until the account sets it.
    pub alignment: u8,
}

/// Something the listserver sent for one local player. The session takes
/// these and writes the client's packets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// A private message. `from_id` is an id on this server for that
    /// remote account, so the client can show the nickname.
    Private {
        from_id: u16,
        nick: String,
        account: String,
        text: String,
    },
    /// A head, body, sword or shield image that finished downloading.
    Image { kind: u8, name: String },
}

enum Cmd {
    Added(ListedPlayer),
    /// The player's nickname changed. `PLYRADD` goes out again.
    Nick {
        id: u16,
        nick: String,
    },
    Removed(u16),
    /// A private message from a local player to an account on `to_server`.
    Private {
        from: u16,
        to_server: String,
        to_account: String,
        text: String,
    },
    Verify {
        account: String,
        password: Vec<u8>,
        player: u16,
        /// Login account type (`Login::client_type`).
        account_type: u8,
        reply: Sender<Verdict>,
    },
    /// The listserver sent ping 99. Reply with one empty `SVO_PING`.
    Pong,
}

/// The handle the rest of the server uses.
pub struct ListLink {
    tx: Mutex<Sender<Cmd>>,
    state: Arc<Mutex<State>>,
    /// Ticket numbers for account checks (the "player" field of
    /// `VERIACC2`, echoed back): the player has no id yet at that point.
    ticket: std::sync::atomic::AtomicU16,
    wake: Arc<Notify>,
}

struct State {
    connected: bool,
    players: BTreeMap<u16, ListedPlayer>,
    /// Account checks in flight, by (lower-case account, player id).
    pending: HashMap<(String, u16), Sender<Verdict>>,
    /// Deliveries waiting for a local player, by that player's id.
    inbox: HashMap<u16, Vec<Delivery>>,
    /// File bytes so far, by (kind, name). Dropped if the link closes.
    partials: HashMap<(u8, String), Vec<u8>>,
    /// Remote (server, account) to the id we show them under.
    externals: HashMap<(String, String), u16>,
    next_external: u16,
    world: PathBuf,
}

impl Default for State {
    fn default() -> Self {
        State {
            connected: false,
            players: BTreeMap::new(),
            pending: HashMap::new(),
            inbox: HashMap::new(),
            partials: HashMap::new(),
            externals: HashMap::new(),
            next_external: FIRST_EXTERNAL,
            world: PathBuf::new(),
        }
    }
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
        let wake = Arc::new(Notify::new());
        let state = Arc::new(Mutex::new(State {
            world: cfg.world.clone(),
            ..State::default()
        }));
        let link = Arc::new(Self {
            tx: Mutex::new(tx.clone()),
            state: state.clone(),
            ticket: Default::default(),
            wake: wake.clone(),
        });
        let cfg = cfg.clone();
        std::thread::Builder::new()
            .name("listlink".into())
            .spawn(move || run(cfg, tx, rx, state, wake))
            .ok()?;
        Some(link)
    }

    fn send(&self, c: Cmd) {
        let _ = self.tx.lock().unwrap().send(c);
    }

    pub fn connected(&self) -> bool {
        self.state.lock().unwrap().connected
    }

    /// Wakes when a private message or a finished file is waiting.
    pub fn notifier(&self) -> &Notify {
        &self.wake
    }

    /// Take everything the listserver has sent for this player.
    pub fn take(&self, id: u16) -> Vec<Delivery> {
        self.state
            .lock()
            .unwrap()
            .inbox
            .remove(&id)
            .unwrap_or_default()
    }

    pub fn player_added(&self, player: ListedPlayer) {
        self.send(Cmd::Added(player));
    }

    /// The player renamed. Another `PLYRADD` carries the new nickname.
    pub fn set_nickname(&self, id: u16, nick: &str) {
        self.send(Cmd::Nick {
            id,
            nick: nick.to_string(),
        });
    }

    pub fn player_removed(&self, id: u16) {
        self.send(Cmd::Removed(id));
    }

    /// Send a private message to `to_account` on `to_server`. Dropped when
    /// the link is down or `from` is not a player the link has listed.
    /// The text is the message, not including the kind labels.
    pub fn send_private(&self, from: u16, to_server: &str, to_account: &str, text: &str) {
        if !self.connected() {
            return;
        }
        self.send(Cmd::Private {
            from,
            to_server: to_server.to_string(),
            to_account: to_account.to_string(),
            text: text.to_string(),
        });
    }

    /// Ask the listserver whether `account` and `password` are right. The
    /// answer comes on the receiver within [`VERIFY_TIMEOUT`].
    ///
    /// `account_type` is the login line's type byte
    /// ([`crate::login::Login::client_type`]): 0 for 1.21 to 2.17, 5 for
    /// 2.22 and later. `rc.rs` names RC as 1 and 6, and NC as 3; those
    /// logins are refused until that module accepts them.
    pub fn verify(&self, account: &str, password: &[u8], account_type: u8) -> Receiver<Verdict> {
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
                account_type,
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
fn hello(cfg: &Config, players: &BTreeMap<u16, ListedPlayer>) -> io::Result<Vec<u8>> {
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
    for player in players.values() {
        body.extend(plyradd(player));
    }
    out.extend(zframe(&body)?);
    Ok(out)
}

/// `PLYRADD`: GSHORT id, GCHAR type 0 (a player; an RC login will send
/// its own type once `rc.rs` lands), then the props the list keeps.
/// Account name (34), nickname (0), level (20), x (15), y (16), and
/// alignment (32). Alignment is the value stored on the player.
/// Each prop is a GCHAR id and the same value a 2.22 client reads.
fn plyradd(p: &ListedPlayer) -> Vec<u8> {
    let mut body = Vec::new();
    gshort(&mut body, p.id);
    body.push(gchar(0));
    prop_str(&mut body, 34, p.account.as_bytes());
    prop_str(&mut body, 0, p.nick.as_bytes());
    prop_str(&mut body, 20, p.level.as_bytes());
    body.push(gchar(15));
    body.push(gchar(tile2(p.x)));
    body.push(gchar(16));
    body.push(gchar(tile2(p.y)));
    body.push(gchar(32));
    body.push(gchar(p.alignment));
    message(svo::PLYRADD, &body)
}

/// `VERIACC2`: GSTRING account, GSTRING password, GSHORT player, GCHAR
/// account type (the login line's type byte).
fn veriacc2(account: &str, password: &[u8], player: u16, account_type: u8) -> Vec<u8> {
    let mut p = Vec::new();
    gstr(&mut p, account.as_bytes());
    gstr(&mut p, password);
    gshort(&mut p, player);
    p.push(gchar(account_type));
    message(svo::VERIACC2, &p)
}

fn prop_str(out: &mut Vec<u8>, id: u8, text: &[u8]) {
    out.push(gchar(id));
    gstr(out, text);
}

fn tile2(v: f32) -> u8 {
    if !v.is_finite() || v <= 0.0 {
        return 0;
    }
    u8::try_from((v * 2.0) as u32).unwrap_or(223)
}

/// Seven fields, one per line, packed so a comma or a quote in a field
/// stays inside that field.
fn pack_fields(fields: &[String]) -> Vec<u8> {
    seihai_core::net::gtokenize(&fields.join("\n")).into_bytes()
}

/// Decompressed bytes not yet taken as a whole message.
struct Inbound {
    plain: Vec<u8>,
    /// When set, the next message is this many bytes and may hold newlines.
    raw: Option<usize>,
}

/// The reading side: decrypted (or plain) bytes in, `SVI_VERIACC2` answers out.
/// A listserver ping (99) becomes [`Cmd::Pong`] on `tx`.
fn reader(
    mut r: BufReader<TcpStream>,
    mut rx: Option<ChannelRecv>,
    state: Arc<Mutex<State>>,
    tx: Sender<Cmd>,
    wake: Arc<Notify>,
) {
    let mut buf: Vec<u8> = Vec::new();
    let mut inbound = Inbound {
        plain: Vec::new(),
        raw: None,
    };
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
        let mut stop = false;
        while buf.len() >= 2 {
            let n = u16::from_be_bytes([buf[0], buf[1]]) as usize;
            if buf.len() < 2 + n {
                break;
            }
            let body: Vec<u8> = buf.drain(..2 + n).skip(2).collect();
            let Ok(raw) = zlib_decompress(&body) else {
                continue;
            };
            if !feed(&mut inbound, &raw, &state, &tx, &wake) {
                stop = true;
                break;
            }
        }
        if stop {
            break;
        }
    }
    let mut st = state.lock().unwrap();
    st.connected = false;
    st.partials.clear();
    for (_, reply) in st.pending.drain() {
        let _ = reply.send(Verdict::Unreachable);
    }
}

/// `false` when the stream is too large to keep reading.
fn feed(
    inbound: &mut Inbound,
    chunk: &[u8],
    state: &Mutex<State>,
    tx: &Sender<Cmd>,
    wake: &Notify,
) -> bool {
    inbound.plain.extend_from_slice(chunk);
    if inbound.plain.len() > MAX_PLAIN {
        return false;
    }
    loop {
        if let Some(n) = inbound.raw {
            if n > MAX_PARTIAL {
                return false;
            }
            if inbound.plain.len() < n {
                return true;
            }
            let msg: Vec<u8> = inbound.plain.drain(..n).collect();
            inbound.raw = None;
            if on_line(&msg, state, tx) {
                wake.notify_one();
            }
            continue;
        }
        let Some(i) = inbound.plain.iter().position(|&b| b == b'\n') else {
            return true;
        };
        let mut line: Vec<u8> = inbound.plain.drain(..=i).collect();
        line.pop();
        if line.is_empty() {
            continue;
        }
        if let Some(n) = raw_size(&line) {
            inbound.raw = Some(n);
            continue;
        }
        if on_line(&line, state, tx) {
            wake.notify_one();
        }
    }
}

/// `SVI_RAWDATA`: a GINT3 count of the following message.
fn raw_size(line: &[u8]) -> Option<usize> {
    if line.first().map(|b| b.wrapping_sub(32)) != Some(SVI_RAWDATA) || line.len() < 4 {
        return None;
    }
    let mut n = 0usize;
    for b in &line[1..4] {
        n = (n << 7) | usize::from(b.wrapping_sub(32) & 0x7f);
    }
    Some(n)
}

/// One decrypted listserver message. `true` when a player's session
/// should look at its inbox.
fn on_line(line: &[u8], state: &Mutex<State>, tx: &Sender<Cmd>) -> bool {
    let payload = line.get(1..).unwrap_or_default();
    match line.first().map(|b| b.wrapping_sub(32)) {
        Some(SVI_VERIACC2) => {
            take_verdict(line, state);
            false
        }
        // Empty pong. The body of the ping is not a nonce we were told
        // to echo, so it is not copied back.
        Some(SVI_PING) => {
            let _ = tx.send(Cmd::Pong);
            false
        }
        Some(SVI_PMPLAYER) => take_pm(payload, state),
        Some(SVI_FILESTART) | Some(SVI_FILESTART3) => {
            open_partial(payload, state);
            false
        }
        Some(SVI_FILEDATA3) => {
            append_partial(payload, state);
            false
        }
        Some(SVI_FILEEND3) => finish_partial(payload, state),
        _ => false,
    }
}

/// `SVI_VERIACC2`: GSTRING account, the player GSHORT and type GCHAR as
/// sent, then `SUCCESS` or the error text.
fn take_verdict(line: &[u8], state: &Mutex<State>) {
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

/// Split a packed field list. A quoted field keeps commas and newlines;
/// `""` inside quotes is one quote. A bare field runs to the next comma.
fn unpack_fields(s: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut i = 0;
    let mut quoted = false;
    while i < s.len() {
        let b = s[i];
        if quoted {
            if b == b'"' && s.get(i + 1) == Some(&b'"') {
                cur.push(b'"');
                i += 2;
                continue;
            }
            if b == b'"' {
                quoted = false;
                i += 1;
                continue;
            }
            cur.push(b);
            i += 1;
            continue;
        }
        if b == b'"' && cur.is_empty() {
            quoted = true;
            i += 1;
            continue;
        }
        if b == b',' {
            out.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        cur.push(b);
        i += 1;
    }
    if !cur.is_empty() || s.last() == Some(&b',') {
        out.push(cur);
    }
    out
}

fn field_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `SVI_PMPLAYER`: destination server, sender account, sender nickname,
/// weapon, kind, destination account, then the message (the rest, so a
/// newline in it stays). The destination account is a player here.
fn take_pm(payload: &[u8], state: &Mutex<State>) -> bool {
    let fields = unpack_fields(payload);
    if fields.len() < 7 {
        return false;
    }
    let server = field_text(&fields[0]);
    let account = field_text(&fields[1]);
    let nick = field_text(&fields[2]);
    let dest = field_text(&fields[5]);
    let text = fields[6..]
        .iter()
        .map(|f| field_text(f))
        .collect::<Vec<_>>()
        .join("\n");
    let mut st = state.lock().unwrap();
    let Some(id) = st
        .players
        .values()
        .find(|p| p.account.eq_ignore_ascii_case(&dest))
        .map(|p| p.id)
    else {
        return false;
    };
    let from_id = external_id(&mut st, &server, &account);
    st.inbox.entry(id).or_default().push(Delivery::Private {
        from_id,
        nick,
        account,
        text,
    });
    true
}

fn external_id(st: &mut State, server: &str, account: &str) -> u16 {
    let key = (server.to_ascii_lowercase(), account.to_ascii_lowercase());
    if let Some(&id) = st.externals.get(&key) {
        return id;
    }
    let id = st.next_external;
    st.next_external = st.next_external.wrapping_add(1);
    if !(FIRST_EXTERNAL..1 << 14).contains(&st.next_external) {
        st.next_external = FIRST_EXTERNAL;
    }
    st.externals.insert(key, id);
    id
}

fn folder(kind: u8) -> Option<&'static str> {
    match kind {
        KIND_HEAD => Some("heads"),
        KIND_BODY => Some("bodies"),
        KIND_SWORD => Some("swords"),
        KIND_SHIELD => Some("shields"),
        _ => None,
    }
}

/// A file name is one path segment: letters, digits, `.`, `_` and `-`.
fn safe_name(name: &[u8]) -> Option<String> {
    if name.is_empty() || name.len() > 64 || name.starts_with(b".") {
        return None;
    }
    if !name
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'_' | b'-'))
    {
        return None;
    }
    if name.windows(2).any(|w| w == b"..") {
        return None;
    }
    std::str::from_utf8(name).ok().map(str::to_string)
}

fn gstring_at(p: &[u8]) -> Option<(String, usize)> {
    let n = p.first()?.wrapping_sub(32) as usize;
    let bytes = p.get(1..1 + n)?;
    Some((safe_name(bytes)?, 1 + n))
}

fn read_gshort(b: &[u8]) -> Option<u16> {
    let pair = b.get(..2)?;
    Some(
        (u16::from(pair[0].wrapping_sub(32) & 0x7f) << 7)
            | u16::from(pair[1].wrapping_sub(32) & 0x7f),
    )
}

fn read_gint5(b: &[u8]) -> Option<u64> {
    let bytes = b.get(..5)?;
    let mut n = 0u64;
    for &byte in bytes {
        n = (n << 7) | u64::from(byte.wrapping_sub(32) & 0x7f);
    }
    Some(n)
}

/// `SVI_FILESTART` / `SVI_FILESTART3`: GCHAR kind, GSTRING name.
fn open_partial(payload: &[u8], state: &Mutex<State>) {
    let Some(&kind_b) = payload.first() else {
        return;
    };
    let kind = kind_b.wrapping_sub(32);
    if folder(kind).is_none() {
        return;
    }
    let Some((name, _)) = gstring_at(payload.get(1..).unwrap_or(&[])) else {
        return;
    };
    state
        .lock()
        .unwrap()
        .partials
        .insert((kind, name), Vec::new());
}

fn append_partial(payload: &[u8], state: &Mutex<State>) {
    let Some(&kind_b) = payload.first() else {
        return;
    };
    let kind = kind_b.wrapping_sub(32);
    let Some((name, used)) = gstring_at(payload.get(1..).unwrap_or(&[])) else {
        return;
    };
    let data = payload.get(1 + used..).unwrap_or(&[]);
    let mut st = state.lock().unwrap();
    let key = (kind, name);
    let too_big = st
        .partials
        .get(&key)
        .is_some_and(|buf| buf.len().saturating_add(data.len()) > MAX_PARTIAL);
    if !st.partials.contains_key(&key) {
        return;
    }
    if too_big {
        st.partials.remove(&key);
        return;
    }
    st.partials.get_mut(&key).unwrap().extend_from_slice(data);
}

/// `SVI_FILEEND3`: GSHORT player, GCHAR kind, GCHAR compress (1 = zlib),
/// GINT5 mod time, GINT5 length, then the name as the remaining bytes.
/// The partial's length must match. A zlib body is expanded first.
fn finish_partial(payload: &[u8], state: &Mutex<State>) -> bool {
    let Some(player) = read_gshort(payload) else {
        return false;
    };
    let Some(&kind_b) = payload.get(2) else {
        return false;
    };
    let Some(&compress_b) = payload.get(3) else {
        return false;
    };
    let kind = kind_b.wrapping_sub(32);
    let compress = compress_b.wrapping_sub(32);
    let Some(mtime) = read_gint5(payload.get(4..).unwrap_or(&[])) else {
        return false;
    };
    let Some(len) = read_gint5(payload.get(9..).unwrap_or(&[])) else {
        return false;
    };
    let Some(name) = payload.get(14..).and_then(safe_name) else {
        return false;
    };
    let bytes = {
        let mut st = state.lock().unwrap();
        st.partials.remove(&(kind, name.clone()))
    };
    let Some(bytes) = bytes else {
        return false;
    };
    let bytes = match compress {
        0 if bytes.len() as u64 == len => bytes,
        1 => match zlib_decompress(&bytes) {
            Ok(plain) if plain.len() as u64 == len => plain,
            _ => return false,
        },
        _ => return false,
    };
    let world = state.lock().unwrap().world.clone();
    if world.as_os_str().is_empty() {
        return false;
    }
    let Some(path) = placed(&world, kind, &name) else {
        return false;
    };
    if write_finished(&path, &bytes, mtime).is_err() {
        eprintln!("listserver file: could not write {}", path.display());
        return false;
    }
    let mut st = state.lock().unwrap();
    if st.players.contains_key(&player) {
        st.inbox
            .entry(player)
            .or_default()
            .push(Delivery::Image { kind, name });
        true
    } else {
        false
    }
}

fn placed(world: &Path, kind: u8, name: &str) -> Option<PathBuf> {
    let dir = world.join(folder(kind)?);
    let path = dir.join(name);
    if path.file_name().and_then(|s| s.to_str()) != Some(name) {
        return None;
    }
    Some(path)
}

fn write_finished(path: &Path, bytes: &[u8], mtime: u64) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("writing");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    if let Some(when) = UNIX_EPOCH.checked_add(Duration::from_secs(mtime)) {
        let _ = std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(when));
    }
    Ok(())
}

fn run(
    cfg: Config,
    tx: Sender<Cmd>,
    rx: Receiver<Cmd>,
    state: Arc<Mutex<State>>,
    wake: Arc<Notify>,
) {
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
        let to_writer = tx.clone();
        let to_wake = wake.clone();
        std::thread::spawn(move || reader(r, ch, st, to_writer, to_wake));
        let mut last = Instant::now();
        loop {
            if !state.lock().unwrap().connected {
                break;
            }
            let msg = match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(Cmd::Added(player)) => {
                    let msg = plyradd(&player);
                    state.lock().unwrap().players.insert(player.id, player);
                    msg
                }
                Ok(Cmd::Nick { id, nick }) => {
                    let mut st = state.lock().unwrap();
                    let Some(player) = st.players.get_mut(&id) else {
                        continue;
                    };
                    player.nick = nick;
                    plyradd(player)
                }
                Ok(Cmd::Removed(id)) => {
                    state.lock().unwrap().players.remove(&id);
                    let mut p = Vec::new();
                    gshort(&mut p, id);
                    message(svo::PLYRREM, &p)
                }
                Ok(Cmd::Private {
                    from,
                    to_server,
                    to_account,
                    text,
                }) => {
                    let st = state.lock().unwrap();
                    let Some(player) = st.players.get(&from) else {
                        continue;
                    };
                    let fields = [
                        to_server,
                        player.account.clone(),
                        player.nick.clone(),
                        "GraalEngine".to_string(),
                        "pmplayer".to_string(),
                        to_account,
                        text,
                    ];
                    let mut body = Vec::new();
                    gshort(&mut body, from);
                    body.extend(pack_fields(&fields));
                    message(svo::PMPLAYER, &body)
                }
                Ok(Cmd::Pong) => message(svo::PING, b""),
                Ok(Cmd::Verify {
                    account,
                    password,
                    player,
                    account_type,
                    reply,
                }) => {
                    state
                        .lock()
                        .unwrap()
                        .pending
                        .insert((account.to_ascii_lowercase(), player), reply);
                    veriacc2(&account, &password, player, account_type)
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
            Ok(Cmd::Added(player)) => {
                state.lock().unwrap().players.insert(player.id, player);
            }
            Ok(Cmd::Nick { id, nick }) => {
                if let Some(player) = state.lock().unwrap().players.get_mut(&id) {
                    player.nick = nick;
                }
            }
            Ok(Cmd::Removed(id)) => {
                state.lock().unwrap().players.remove(&id);
            }
            Ok(Cmd::Verify { reply, .. }) => {
                let _ = reply.send(Verdict::Unreachable);
            }
            // The link is down, so a ping or a PM has nobody to reach.
            Ok(Cmd::Pong | Cmd::Private { .. }) => {}
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
        let (cmd_tx, cmd_rx) = mpsc::channel();
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
        on_line(&line, &state, &cmd_tx);
        assert_eq!(rx.recv().unwrap(), Verdict::Ok);
        assert!(cmd_rx.try_recv().is_err(), "a verdict is not a ping");
    }

    #[test]
    fn listserver_ping_requests_an_empty_pong() {
        let state = Mutex::new(State::default());
        let (tx, rx) = mpsc::channel();
        on_line(&[gchar(SVI_PING), b'x', b'y'], &state, &tx);
        on_line(&[gchar(SVI_PING)], &state, &tx);
        on_line(&[gchar(svo::SETIP)], &state, &tx);
        assert!(matches!(rx.try_recv(), Ok(Cmd::Pong)));
        assert!(matches!(rx.try_recv(), Ok(Cmd::Pong)));
        assert!(rx.try_recv().is_err());
        let pong = message(svo::PING, b"");
        assert_eq!(pong, vec![gchar(svo::PING), b'\n']);
    }

    fn sample() -> ListedPlayer {
        ListedPlayer {
            id: 7,
            account: "Via".into(),
            nick: "Squirrel".into(),
            level: "onlinestartlocal.nw".into(),
            x: 30.0,
            y: 30.5,
            alignment: 0,
        }
    }

    #[test]
    fn plyradd_carries_the_nickname_and_props_after_the_id() {
        let line = plyradd(&sample());
        assert_eq!(line[0], gchar(svo::PLYRADD));
        let payload = &line[1..line.len() - 1];
        let id = read_gshort(payload).unwrap();
        assert_eq!(id, 7);
        assert_eq!(payload[2].wrapping_sub(32), 0, "type stays a player");
        let props = seihai_core::protocol::parse_player_props(&payload[3..]);
        let text = |id: u8| {
            props
                .iter()
                .find(|p| p.id == id)
                .map(|p| String::from_utf8_lossy(&p.raw[1..]).into_owned())
        };
        assert_eq!(text(0).as_deref(), Some("Squirrel"));
        assert_eq!(text(34).as_deref(), Some("Via"));
        assert_eq!(text(20).as_deref(), Some("onlinestartlocal.nw"));
        assert_eq!(
            props.iter().find(|p| p.id == 15).and_then(|p| p.gchar()),
            Some(60)
        );
        assert_eq!(
            props.iter().find(|p| p.id == 16).and_then(|p| p.gchar()),
            Some(61)
        );
        assert_eq!(
            props.iter().find(|p| p.id == 32).and_then(|p| p.gchar()),
            Some(0)
        );
        let used: usize = props.iter().map(|p| 1 + p.raw.len()).sum();
        assert_eq!(used, payload.len() - 3);
    }

    #[test]
    fn plyradd_sends_a_stored_alignment() {
        let mut player = sample();
        player.alignment = 40;
        let line = plyradd(&player);
        let payload = &line[1..line.len() - 1];
        let props = seihai_core::protocol::parse_player_props(&payload[3..]);
        assert_eq!(
            props.iter().find(|p| p.id == 32).and_then(|p| p.gchar()),
            Some(40)
        );
    }

    #[test]
    fn veriacc2_type_byte_is_the_argument() {
        let line = veriacc2("tester", b"pw", 3, 5);
        assert_eq!(line[0].wrapping_sub(32), svo::VERIACC2);
        let payload = &line[1..line.len() - 1];
        let an = (payload[0] - 32) as usize;
        let pn = (payload[1 + an] - 32) as usize;
        let at = 2 + an + pn;
        let player = (u16::from(payload[at].wrapping_sub(32) & 0x7f) << 7)
            | u16::from(payload[at + 1].wrapping_sub(32) & 0x7f);
        assert_eq!(player, 3);
        assert_eq!(payload[at + 2].wrapping_sub(32), 5);
        let zero = veriacc2("tester", b"pw", 3, 0);
        let z = &zero[1..zero.len() - 1];
        assert_eq!(z[at + 2].wrapping_sub(32), 0);
    }

    #[test]
    fn private_message_round_trips_to_the_local_account() {
        let state = Mutex::new(State::default());
        let player = sample();
        state.lock().unwrap().players.insert(player.id, player);
        let packed = pack_fields(&[
            "Other".into(),
            "far".into(),
            "Far Nick".into(),
            "GraalEngine".into(),
            "pmplayer".into(),
            "via".into(),
            "hello, \"there\"".into(),
        ]);
        let mut line = vec![gchar(SVI_PMPLAYER)];
        line.extend(packed);
        assert!(on_line(&line, &state, &mpsc::channel().0));
        let got = state.lock().unwrap().inbox.get(&7).cloned().unwrap();
        assert!(matches!(
            &got[0],
            Delivery::Private { from_id, nick, text, .. }
                if *from_id == FIRST_EXTERNAL && nick == "Far Nick" && text == "hello, \"there\""
        ));
    }

    fn gint5(v: u64, out: &mut Vec<u8>) {
        for i in (0..5).rev() {
            out.push(((v >> (7 * i)) & 0x7f) as u8 + 32);
        }
    }

    fn file_end(player: u16, kind: u8, compress: u8, mtime: u64, len: u64, name: &str) -> Vec<u8> {
        let mut p = vec![gchar(SVI_FILEEND3)];
        gshort(&mut p, player);
        p.push(gchar(kind));
        p.push(gchar(compress));
        gint5(mtime, &mut p);
        gint5(len, &mut p);
        p.extend(name.as_bytes());
        p
    }

    #[test]
    fn file_start_then_data_and_end_writes_the_image() {
        let dir = std::env::temp_dir().join(format!("seihai-gserver-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = Mutex::new(State {
            world: dir.clone(),
            ..State::default()
        });
        state.lock().unwrap().players.insert(7, sample());
        let (cmd_tx, _) = mpsc::channel();
        let mut start = vec![gchar(SVI_FILESTART), gchar(KIND_HEAD)];
        gstr(&mut start, b"labhead.png");
        on_line(&start, &state, &cmd_tx);
        let mut data = vec![gchar(SVI_FILEDATA3), gchar(KIND_HEAD)];
        gstr(&mut data, b"labhead.png");
        data.extend(b"HEAD");
        on_line(&data, &state, &cmd_tx);
        let end = file_end(7, KIND_HEAD, 0, 1_700_000_000, 4, "labhead.png");
        assert!(on_line(&end, &state, &cmd_tx));
        assert_eq!(
            std::fs::read(dir.join("heads/labhead.png")).unwrap(),
            b"HEAD"
        );
        let inbox = state.lock().unwrap().inbox.get(&7).cloned().unwrap();
        assert_eq!(
            inbox,
            vec![Delivery::Image {
                kind: KIND_HEAD,
                name: "labhead.png".into()
            }]
        );

        // A name that leaves the folder is not a file.
        let mut bad = vec![gchar(SVI_FILESTART3), gchar(KIND_BODY)];
        gstr(&mut bad, b"../x");
        on_line(&bad, &state, &cmd_tx);
        assert!(state.lock().unwrap().partials.is_empty());
        assert!(!dir.join("x").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn raw_file_data_keeps_a_newline_and_zlib() {
        let dir = std::env::temp_dir().join(format!("seihai-gserver-raw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let state = Mutex::new(State {
            world: dir.clone(),
            ..State::default()
        });
        state.lock().unwrap().players.insert(7, sample());
        let plain = b"aa\nbb";
        let z = zlib_compress(plain).unwrap();
        let mut data = vec![gchar(SVI_FILEDATA3), gchar(KIND_BODY)];
        gstr(&mut data, b"body.png");
        data.extend(&z);
        let mut raw_head = vec![gchar(SVI_RAWDATA)];
        gint3_test(data.len() as u32, &mut raw_head);
        let mut bytes = raw_head;
        bytes.push(b'\n');
        bytes.extend(&data);
        let mut start = vec![gchar(SVI_FILESTART3), gchar(KIND_BODY)];
        gstr(&mut start, b"body.png");
        start.push(b'\n');
        let mut all = start;
        all.extend(&bytes);
        let end = file_end(7, KIND_BODY, 1, 10, plain.len() as u64, "body.png");
        all.extend(&end);
        all.push(b'\n');
        let wake = Notify::new();
        let mut inbound = Inbound {
            plain: Vec::new(),
            raw: None,
        };
        assert!(feed(&mut inbound, &all, &state, &mpsc::channel().0, &wake));
        assert_eq!(std::fs::read(dir.join("bodies/body.png")).unwrap(), plain);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn gint3_test(v: u32, out: &mut Vec<u8>) {
        for i in (0..3).rev() {
            out.push(((v >> (7 * i)) & 0x7f) as u8 + 32);
        }
    }
}
