//! `SEND_TO_NC` (`0x4D`) and `SEND_TO_RC` (`0x4E`) on loopback.
//!
//! A stand-in NPC-server sends both, including an empty line. Every
//! logged-in NPC Control session receives the NC line as `PLO_RC_CHAT`
//! (74), and every logged-in Remote Control session receives the RC line
//! the same way. The other kind does not. A player receives neither.
//! Only `127.0.0.1` is used.

use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{split_messages, ClientVersion, SessionState};
use seihai_gserver::accounts::{Account, Store};
use seihai_gserver::npclink::serve_link;
use seihai_gserver::server::serve;
use seihai_npcserver::link::{self, Message, LINK_VERSION};
use seihai_rc::nc::{NcEvent, NcSession};
use seihai_rc::rc::{Event, LoginParams, RcKind, Session};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PASSWORD: &str = "labpw";
const TOKEN: &str = "lab-token-0123456789";
const START: &str = "onlinestartlocal.nw";
const LIMIT: Duration = Duration::from_secs(10);

fn world() -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("seihai-gserver-staff-lines-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("levels")).unwrap();
    std::fs::write(dir.join("levels").join(START), "GLEVNW01\n").unwrap();
    std::fs::write(
        dir.join("config/server.txt"),
        format!(
            "name = Staff Lab\ncreate_accounts = true\nheartbeat = 1\n\
             npcserver = external\nnpcserver_token = {TOKEN}\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("config/allowedversions.txt"), "*\n").unwrap();
    let store = Store::new(dir.join("accounts"));
    for name in ["boss", "aide"] {
        let mut a = Account::new(name, START, 30.0, 30.0);
        a.set_password(PASSWORD.as_bytes());
        a.staff = true;
        store.save(&a).unwrap();
    }
    dir
}

fn start(world: &Path) -> (String, String) {
    let (server, warnings) = seihai_gserver::load_world(world).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let server = Arc::new(server);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let game = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let link = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send((game.local_addr().unwrap(), link.local_addr().unwrap()))
                .unwrap();
            tokio::spawn(serve_link(server.npc.clone(), link));
            let _ = serve(server, game).await;
        });
    });
    let (game, link) = rx.recv().unwrap();
    (game.to_string(), link.to_string())
}

fn connect_npc(addr: &str) -> TcpStream {
    let mut s = TcpStream::connect(addr).unwrap();
    link::write_frame(
        &mut s,
        &[Message::Hello {
            link_version: LINK_VERSION,
            token: TOKEN.as_bytes().to_vec(),
            program: b"stand-in".to_vec(),
        }],
    )
    .unwrap();
    let heard = Arc::new(Mutex::new(Vec::new()));
    let (got, mut r) = (heard.clone(), s.try_clone().unwrap());
    std::thread::spawn(move || {
        while let Ok(msgs) = link::read_frame(&mut r) {
            got.lock().unwrap().extend(msgs);
        }
    });
    let end = Instant::now() + LIMIT;
    while Instant::now() < end {
        if heard
            .lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, Message::Welcome { .. }))
        {
            return s;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no WELCOME from the gserver: {:?}", heard.lock().unwrap());
}

fn rc(addr: &str, account: &str) -> Session {
    Session::login(
        addr,
        &LoginParams {
            kind: RcKind::Gen5,
            token: RcKind::Gen5.default_token().into(),
            account: account.into(),
            password: PASSWORD.into(),
            key: 0x2A,
            nickname: None,
            identity: None,
        },
        LIMIT,
    )
    .unwrap()
}

fn until_rc(s: &mut Session, what: &str, done: impl Fn(&Event) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + LIMIT;
    let mut seen = Vec::new();
    while let Some(ev) = s.next_event(deadline).unwrap() {
        let hit = done(&ev);
        seen.push(ev);
        if hit {
            return seen;
        }
    }
    panic!("RC saw no {what}; got {seen:?}");
}

fn until_nc(s: &mut NcSession, what: &str, done: impl Fn(&NcEvent) -> bool) -> Vec<NcEvent> {
    let deadline = Instant::now() + LIMIT;
    let mut seen = Vec::new();
    while let Some(ev) = s.next_event(deadline).unwrap() {
        let hit = done(&ev);
        seen.push(ev);
        if hit {
            return seen;
        }
    }
    panic!("NC saw no {what}; got {seen:?}");
}

fn drain_rc(s: &mut Session, quiet: Duration) -> Vec<Event> {
    let mut out = Vec::new();
    let deadline = Instant::now() + quiet;
    while let Some(ev) = s.next_event(deadline).unwrap() {
        out.push(ev);
    }
    out
}

fn drain_nc(s: &mut NcSession, quiet: Duration) -> Vec<NcEvent> {
    let mut out = Vec::new();
    let deadline = Instant::now() + quiet;
    while let Some(ev) = s.next_event(deadline).unwrap() {
        out.push(ev);
    }
    out
}

fn rc_lines(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::RcChat(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn nc_lines(events: &[NcEvent]) -> Vec<&[u8]> {
    events
        .iter()
        .filter_map(|e| match e {
            NcEvent::Message { id: 74, payload } => Some(payload.as_slice()),
            _ => None,
        })
        .collect()
}

/// Messages a player session has been sent, as (id, payload).
type Seen = Arc<Mutex<Vec<(u8, Vec<u8>)>>>;

struct Player {
    seen: Seen,
    /// Kept so the login stays up for the whole test.
    _sess: seihai_core::net::GserverSession,
}

impl Player {
    fn login(addr: &str) -> Player {
        let v = ClientVersion::V222;
        let mut sess = login_gserver(addr, v, random_key(), "ada", PASSWORD, true).unwrap();
        let rx = sess.conn.spawn_reader().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let got = seen.clone();
        let entered = Arc::new(Mutex::new(false));
        let flag = entered.clone();
        std::thread::spawn(move || {
            let mut st = SessionState::for_version(v);
            while let Ok(Ok(body)) = rx.recv() {
                got.lock()
                    .unwrap()
                    .extend(split_messages(&body).into_iter().map(|m| (m.id, m.payload)));
                st.apply_body(&body);
                if st.current_level_name().is_some() {
                    *flag.lock().unwrap() = true;
                }
            }
        });
        let end = Instant::now() + LIMIT;
        while Instant::now() < end {
            if *entered.lock().unwrap() {
                return Player { seen, _sess: sess };
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("player never entered; {:?}", seen.lock().unwrap());
    }
}

#[test]
fn a_staff_line_reaches_only_logged_in_sessions_of_its_kind() {
    let w = world();
    let (game, link) = start(&w);
    let mut npc = connect_npc(&link);
    link::write_frame(
        &mut npc,
        &[Message::NcNpc {
            npc_id: 1,
            name: b"bell".to_vec(),
            kind: Vec::new(),
            level: b"start.nw".to_vec(),
        }],
    )
    .unwrap();

    let player = Player::login(&game);
    let mut boss = rc(&game, "boss");
    until_rc(
        &mut boss,
        "its own arrival",
        |e| matches!(e, Event::RcChat(t) if t == "New RC: boss"),
    );
    let mut aide = rc(&game, "aide");
    until_rc(
        &mut aide,
        "its own arrival",
        |e| matches!(e, Event::RcChat(t) if t == "New RC: aide"),
    );
    until_rc(
        &mut boss,
        "the other RC",
        |e| matches!(e, Event::RcChat(t) if t == "New RC: aide"),
    );

    let mut nc_boss = NcSession::login(&game, "boss", PASSWORD, LIMIT).unwrap();
    until_nc(&mut nc_boss, "the NPC list", |e| {
        matches!(e, NcEvent::Npc(_))
    });
    let mut nc_aide = NcSession::login(&game, "aide", PASSWORD, LIMIT).unwrap();
    until_nc(&mut nc_aide, "the NPC list", |e| {
        matches!(e, NcEvent::Npc(_))
    });

    link::write_frame(
        &mut npc,
        &[
            Message::SendToNc {
                text: b"from-nc".to_vec(),
            },
            Message::SendToRc {
                text: b"from-rc".to_vec(),
            },
            Message::SendToNc { text: Vec::new() },
            Message::SendToRc { text: Vec::new() },
        ],
    )
    .unwrap();

    let mut nc_boss_seen = until_nc(
        &mut nc_boss,
        "the NC line",
        |e| matches!(e, NcEvent::Message { id: 74, payload } if payload == b"from-nc"),
    );
    nc_boss_seen.extend(until_nc(
        &mut nc_boss,
        "the empty NC line",
        |e| matches!(e, NcEvent::Message { id: 74, payload } if payload.is_empty()),
    ));
    let mut nc_aide_seen = until_nc(
        &mut nc_aide,
        "the NC line",
        |e| matches!(e, NcEvent::Message { id: 74, payload } if payload == b"from-nc"),
    );
    nc_aide_seen.extend(until_nc(
        &mut nc_aide,
        "the empty NC line",
        |e| matches!(e, NcEvent::Message { id: 74, payload } if payload.is_empty()),
    ));

    let mut boss_seen = until_rc(
        &mut boss,
        "the RC line",
        |e| matches!(e, Event::RcChat(t) if t == "from-rc"),
    );
    boss_seen.extend(until_rc(
        &mut boss,
        "the empty RC line",
        |e| matches!(e, Event::RcChat(t) if t.is_empty()),
    ));
    let mut aide_seen = until_rc(
        &mut aide,
        "the RC line",
        |e| matches!(e, Event::RcChat(t) if t == "from-rc"),
    );
    aide_seen.extend(until_rc(
        &mut aide,
        "the empty RC line",
        |e| matches!(e, Event::RcChat(t) if t.is_empty()),
    ));

    let quiet = Duration::from_millis(300);
    nc_boss_seen.extend(drain_nc(&mut nc_boss, quiet));
    nc_aide_seen.extend(drain_nc(&mut nc_aide, quiet));
    boss_seen.extend(drain_rc(&mut boss, quiet));
    aide_seen.extend(drain_rc(&mut aide, quiet));

    for (who, lines) in [
        ("boss", nc_lines(&nc_boss_seen)),
        ("aide", nc_lines(&nc_aide_seen)),
    ] {
        assert!(
            lines.contains(&b"from-nc".as_slice()),
            "NC {who} missed the NC line: {lines:?}"
        );
        assert!(
            lines.contains(&b"".as_slice()),
            "NC {who} missed the empty NC line: {lines:?}"
        );
        assert!(
            !lines.contains(&b"from-rc".as_slice()),
            "NC {who} received the RC line: {lines:?}"
        );
    }
    for (who, lines) in [
        ("boss", rc_lines(&boss_seen)),
        ("aide", rc_lines(&aide_seen)),
    ] {
        assert!(
            lines.contains(&"from-rc"),
            "RC {who} missed the RC line: {lines:?}"
        );
        assert!(
            lines.contains(&""),
            "RC {who} missed the empty RC line: {lines:?}"
        );
        assert!(
            !lines.contains(&"from-nc"),
            "RC {who} received the NC line: {lines:?}"
        );
    }

    std::thread::sleep(quiet);
    let seen = player.seen.lock().unwrap();
    assert!(
        seen.iter().all(|(id, _)| *id != 74),
        "a player received an RC console line: {seen:?}"
    );
    assert!(
        seen.iter().all(|(_, p)| p != b"from-nc" && p != b"from-rc"),
        "a player received a staff line: {seen:?}"
    );
    let _ = std::fs::remove_dir_all(&w);
}
