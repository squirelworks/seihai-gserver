//! The listserver link on loopback against a small fake listserver built
//! on `seihai_core::protocol::seclogin` (the same code seihai-listserver
//! uses): the link is encrypted, the gserver registers and lists its
//! players, account checks go through `VERIACC2`, the password never
//! shows on the wire, and a wrong listserver key stops the link before
//! anything is sent.

mod common;

use common::*;
use seihai_core::protocol::seclogin::{self, ClientHello, ServerIdentity};
use seihai_core::protocol::{zlib_compress, zlib_decompress, ClientVersion};
use seihai_gserver::server::{serve, Server};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Records every byte read from the socket.
struct Tap {
    s: TcpStream,
    seen: Arc<Mutex<Vec<u8>>>,
}

impl Read for Tap {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.s.read(buf)?;
        self.seen.lock().unwrap().extend_from_slice(&buf[..n]);
        Ok(n)
    }
}

impl Write for Tap {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.s.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.s.flush()
    }
}

/// What the fake listserver saw.
#[derive(Debug)]
enum Seen {
    /// The decrypted first line, `REGISTERV3`.
    Register(Vec<u8>),
    /// A decrypted message: id and payload.
    Msg(u8, Vec<u8>),
    /// The link closed; every raw byte read.
    Closed(Vec<u8>),
}

fn gstr(out: &mut Vec<u8>, s: &[u8]) {
    out.push(s.len() as u8 + 32);
    out.extend_from_slice(s);
}

/// A fake listserver: one gserver, encrypted. Answers `VERIACC2` with
/// `SUCCESS` for the password `labpw`.
fn fake_listserver(identity: ServerIdentity) -> (String, Receiver<Seen>, Arc<Mutex<Vec<u8>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    let tap = Arc::new(Mutex::new(Vec::new()));
    let seen = tap.clone();
    std::thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let mut r = BufReader::new(Tap {
            s: s.try_clone().unwrap(),
            seen: seen.clone(),
        });
        let mut w = s;
        let closed = |seen: &Arc<Mutex<Vec<u8>>>| Seen::Closed(seen.lock().unwrap().clone());
        let mut line = Vec::new();
        r.read_until(b'\n', &mut line).unwrap();
        let line = line.strip_suffix(b"\n").unwrap().to_vec();
        let hello = ClientHello::parse(&line, 0).unwrap().unwrap();
        assert_eq!(hello.inner, *seclogin::LINK_TOKEN.as_bytes());
        let (mut s1, opener) = hello.respond(&identity).unwrap();
        s1.push(b'\n');
        w.write_all(&s1).unwrap();
        let (mut send, mut recv) = opener.into_channel().split();
        let mut buf = Vec::new();
        let mut first = true;
        loop {
            match recv.read_record(&mut r) {
                Ok(Some(b)) => buf.extend(b),
                _ => {
                    let _ = tx.send(closed(&seen));
                    return;
                }
            }
            if first {
                let Some(n) = buf.iter().position(|&b| b == b'\n') else {
                    continue;
                };
                let reg: Vec<u8> = buf.drain(..=n).collect();
                let _ = tx.send(Seen::Register(reg));
                first = false;
            }
            while buf.len() >= 2 {
                let n = u16::from_be_bytes([buf[0], buf[1]]) as usize;
                if buf.len() < 2 + n {
                    break;
                }
                let body: Vec<u8> = buf.drain(..2 + n).skip(2).collect();
                let raw = zlib_decompress(&body).unwrap();
                for m in raw.split(|&b| b == b'\n').filter(|m| !m.is_empty()) {
                    let id = m[0] - 32;
                    let payload = m[1..].to_vec();
                    if id == 17 {
                        // GSTRING account, GSTRING password, GSHORT, GCHAR.
                        let an = (payload[0] - 32) as usize;
                        let account = &payload[1..1 + an];
                        let pn = (payload[1 + an] - 32) as usize;
                        let password = &payload[2 + an..2 + an + pn];
                        let rest = &payload[2 + an + pn..];
                        let mut out = vec![11 + 32];
                        gstr(&mut out, account);
                        out.extend_from_slice(&rest[..3]);
                        out.extend_from_slice(if password == b"labpw" {
                            b"SUCCESS"
                        } else {
                            b"Your password is incorrect."
                        });
                        out.push(b'\n');
                        let z = zlib_compress(&out).unwrap();
                        let frame = [&(z.len() as u16).to_be_bytes()[..], &z].concat();
                        w.write_all(&send.seal(&frame)).unwrap();
                    }
                    let _ = tx.send(Seen::Msg(id, payload));
                }
            }
        }
    });
    (addr, rx, tap)
}

fn start_with_link(world: &std::path::Path, extra: &str) -> (String, Arc<Server>) {
    let (mut server, warnings) = seihai_gserver::load_world(world).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(server.cfg.apply(extra).is_empty());
    seihai_gserver::load_secure_identity(&mut server).unwrap();
    seihai_gserver::start_listlink(&mut server);
    let server = Arc::new(server);
    let (tx, rx) = mpsc::channel();
    let s = server.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(l.local_addr().unwrap()).unwrap();
            let _ = serve(s, l).await;
        });
    });
    (rx.recv().unwrap().to_string(), server)
}

fn wait_connected(server: &Server) {
    let end = Instant::now() + Duration::from_secs(10);
    while !server.listlink.as_ref().unwrap().connected() {
        assert!(Instant::now() < end, "listserver link never came up");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Next decrypted message with this id, skipping others.
fn next_msg(rx: &Receiver<Seen>, want: u8) -> Vec<u8> {
    loop {
        match rx
            .recv_timeout(Duration::from_secs(10))
            .expect("no message")
        {
            Seen::Msg(id, p) if id == want => return p,
            Seen::Closed(_) => panic!("link closed waiting for {want}"),
            _ => {}
        }
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn encrypted_link_registers_lists_players_and_checks_accounts() {
    let w = world("listlink", "*\n");
    let id = ServerIdentity::generate();
    let key = id.public_key();
    let (ls, seen, tap) = fake_listserver(id);
    let (addr, server) = start_with_link(
        &w,
        &format!(
            "listserver = {ls}\nlistserver_accounts = true\ncreate_accounts = false\nname = Lab Link\ndescription = loopback test\n"
        ),
    );
    wait_connected(&server);
    match seen.recv_timeout(Duration::from_secs(5)).unwrap() {
        Seen::Register(r) => {
            assert_eq!(r[0] - 32, 30, "REGISTERV3 first");
            assert!(contains(&r, b"seihai-gserver"));
        }
        other => panic!("{other:?}"),
    }
    let entry = next_msg(&seen, 22);
    assert!(contains(&entry, b"Lab Link") && contains(&entry, b"loopback test"));
    // The key was trusted on first use and written to the world's pin file.
    let pins = std::fs::read_to_string(w.join("config/listserver.pin")).unwrap();
    assert!(pins.contains(&seclogin::encode_key(&key)));

    // A login checked by the listserver: no local account, no local
    // password, so only `VERIACC2` can let it in.
    let v = ClientVersion::V231;
    run_one(
        &addr,
        &server,
        v,
        "viaList",
        Duration::from_millis(200),
        None,
    )
    .unwrap();
    let check = next_msg(&seen, 17);
    assert!(contains(&check, b"viaList") && contains(&check, PASSWORD.as_bytes()));
    next_msg(&seen, 14); // PLYRADD
                         // The account file was made for the player's state, with no password.
    let file = std::fs::read_to_string(w.join("accounts/viaList.txt"))
        .or_else(|_| std::fs::read_to_string(w.join("accounts/vialist.txt")))
        .unwrap();
    assert!(!file.contains("pbkdf2"), "{file}");

    // A wrong password is refused with the usual text.
    let why = refused(&addr, v, "viaList2", "wrong").expect("refused");
    assert!(why.contains("wrong"), "{why}");
    next_msg(&seen, 17);

    // Every raw byte the gserver sent: no password, no account name.
    let raw = tap.lock().unwrap().clone();
    assert!(raw.len() > 100, "{} bytes", raw.len());
    assert!(!contains(&raw, PASSWORD.as_bytes()));
    assert!(!contains(&raw, b"wrong"));
    assert!(!contains(&raw, b"viaList"));
    assert!(!contains(&raw, b"Lab Link"));
}

#[test]
fn wrong_listserver_key_sends_nothing_after_the_hello() {
    let w = world("listlink-pin", "*\n");
    let (ls, seen, tap) = fake_listserver(ServerIdentity::generate());
    let other = seclogin::encode_key(&ServerIdentity::generate().public_key());
    let (_addr, _server) = start_with_link(
        &w,
        &format!("listserver = {ls}\nlistserver_key = {other}\n"),
    );
    match seen
        .recv_timeout(Duration::from_secs(10))
        .expect("link closed")
    {
        Seen::Closed(raw) => {
            let lines = raw.iter().filter(|&&b| b == b'\n').count();
            assert_eq!(lines, 1, "only C1 crossed: {raw:?}");
            assert!(raw.starts_with(b"SEI00001SVLINK01"));
        }
        other => panic!("the gserver kept talking: {other:?}"),
    }
    assert!(!w.join("config/listserver.pin").exists());
    let _ = tap;
}
