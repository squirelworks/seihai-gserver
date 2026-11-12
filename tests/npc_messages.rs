//! Stage 5: what the NPC-server sends, applied on loopback. A stand-in
//! NPC-server speaks the link with `seihai_npcserver::link` (the codec the
//! real one uses); real client logins go through `seihai_core::net` and
//! read what they get with `SessionState`. Only `127.0.0.1` is used.

use seihai_core::net::{login_gserver, random_key};
use seihai_core::protocol::{split_messages, ChatKind, ClientVersion, SessionState};
use seihai_gserver::npclink::serve_link;
use seihai_gserver::server::{serve, Server};
use seihai_npcserver::link::{self, Message, LINK_VERSION};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const PASSWORD: &str = "labpw";
const TOKEN: &str = "lab-token-0123456789";
const START: &str = "onlinestartlocal.nw";

fn world(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "seihai-gserver-npcmsg-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("levels")).unwrap();
    std::fs::write(dir.join("levels").join(START), "GLEVNW01\n").unwrap();
    std::fs::write(dir.join("levels/house.nw"), "GLEVNW01\n").unwrap();
    std::fs::write(
        dir.join("config/server.txt"),
        format!("name = Link Lab\ncreate_accounts = true\nheartbeat = 1\nnpcserver = external\nnpcserver_token = {TOKEN}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("config/allowedversions.txt"), "*\n").unwrap();
    dir
}

fn start(world: &Path) -> (String, String, Arc<Server>) {
    let (server, warnings) = seihai_gserver::load_world(world).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let server = Arc::new(server);
    let (tx, rx) = std::sync::mpsc::channel();
    let s = server.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let game = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let link = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send((game.local_addr().unwrap(), link.local_addr().unwrap()))
                .unwrap();
            tokio::spawn(serve_link(s.npc.clone(), link));
            let _ = serve(s, game).await;
        });
    });
    let (g, l) = rx.recv().unwrap();
    (g.to_string(), l.to_string(), server)
}

fn wait(what: &str, ok: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

/// A stand-in NPC-server: sends what the test says, keeps what it hears.
struct FakeNpc {
    out: TcpStream,
    heard: Arc<Mutex<Vec<Message>>>,
}

impl FakeNpc {
    fn connect(addr: &str) -> FakeNpc {
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
        let (h, mut r) = (heard.clone(), s.try_clone().unwrap());
        std::thread::spawn(move || {
            while let Ok(msgs) = link::read_frame(&mut r) {
                h.lock().unwrap().extend(msgs);
            }
        });
        let f = FakeNpc { out: s, heard };
        f.wait_for("WELCOME", |m| matches!(m, Message::Welcome { .. }));
        f
    }

    fn send(&mut self, m: Message) {
        link::write_frame(&mut self.out, &[m]).unwrap();
    }

    fn wait_for(&self, what: &str, f: impl Fn(&Message) -> bool) -> Message {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if let Some(m) = self.heard.lock().unwrap().iter().find(|m| f(m)) {
                return m.clone();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "the NPC-server heard no {what}: {:?}",
            self.heard.lock().unwrap()
        );
    }
}

/// A logged-in client whose frames a thread applies to its state; every
/// message id that arrived is kept too.
type Seen = Arc<Mutex<Vec<(u8, Vec<u8>)>>>;

struct Client {
    id: u16,
    st: Arc<Mutex<SessionState>>,
    ids: Seen,
    _sess: seihai_core::net::GserverSession,
}

impl Client {
    fn login(addr: &str, v: ClientVersion, account: &str, npc: &FakeNpc) -> Client {
        let mut sess = login_gserver(addr, v, random_key(), account, PASSWORD, true).unwrap();
        let rx = sess.conn.spawn_reader().unwrap();
        let mut s = SessionState::for_version(v);
        s.file_has_modtime = seihai_gserver::files::has_modtime(v);
        let st = Arc::new(Mutex::new(s));
        let ids = Arc::new(Mutex::new(Vec::new()));
        let (st2, ids2) = (st.clone(), ids.clone());
        std::thread::spawn(move || {
            while let Ok(Ok(body)) = rx.recv() {
                ids2.lock()
                    .unwrap()
                    .extend(split_messages(&body).into_iter().map(|m| (m.id, m.payload)));
                st2.lock().unwrap().apply_body(&body);
            }
        });
        let want = account.to_string();
        let joined = npc.wait_for(
            "PLAYER_JOINED",
            |m| matches!(m, Message::PlayerJoined { account, .. } if account == want.as_bytes()),
        );
        let Message::PlayerJoined { player_id, .. } = joined else {
            unreachable!()
        };
        let c = Client {
            id: player_id,
            st,
            ids,
            _sess: sess,
        };
        c.until("the level", |s| s.current_level_name().is_some());
        c
    }

    fn until(&self, what: &str, f: impl Fn(&SessionState) -> bool) {
        wait(what, || f(&self.st.lock().unwrap()));
    }

    fn got(&self, id: u8) -> Vec<Vec<u8>> {
        self.ids
            .lock()
            .unwrap()
            .iter()
            .filter(|(i, _)| *i == id)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

/// NPC prop stream (2.22): image and position in half tiles.
fn npc_place(image: &str, x2: u8, y2: u8) -> Vec<u8> {
    let mut p = vec![0x20, 0x20 + image.len() as u8];
    p.extend(image.as_bytes());
    p.extend([0x20 + 2, 0x20 + x2, 0x20 + 3, 0x20 + y2]);
    p
}

#[test]
fn server_npcs_come_change_and_go() {
    let w = world("npcs");
    let (game, link, _server) = start(&w);
    let mut npc = FakeNpc::connect(&link);
    let a = Client::login(&game, ClientVersion::V222, "npca", &npc);
    let b = Client::login(&game, ClientVersion::V205, "npcb", &npc);

    npc.send(Message::NpcAdded {
        npc_id: 900,
        level: START.as_bytes().to_vec(),
        props: npc_place("door.png", 60, 61),
    });
    let shown = |c: &Client, image: &'static str| {
        c.until("the server NPC", move |s| {
            s.server_npcs
                .values()
                .any(|(_, p)| p.image.as_deref() == Some(image))
        })
    };
    shown(&a, "door.png");
    shown(&b, "door.png");
    let id = *a.st.lock().unwrap().server_npcs.keys().next().unwrap();
    {
        let st = a.st.lock().unwrap();
        let p = &st.server_npcs[&id].1;
        assert_eq!((p.x, p.y), (Some(30.0), Some(30.5)));
        assert!(
            p.script.is_none(),
            "a server NPC's script stays on the NPC-server"
        );
    }

    // A move goes to both; a late arrival gets the NPC where it is now.
    npc.send(Message::NpcProps {
        npc_id: 900,
        props: vec![0x20 + 2, 0x20 + 20],
    });
    for c in [&a, &b] {
        c.until("the NPC move", move |s| {
            s.server_npcs
                .get(&id)
                .is_some_and(|(_, p)| p.x == Some(10.0))
        });
    }
    let late = Client::login(&game, ClientVersion::V231, "npcc", &npc);
    late.until("the NPC on arrival", move |s| {
        s.server_npcs
            .get(&id)
            .is_some_and(|(_, p)| p.x == Some(10.0))
    });

    // Deleted: PLO_NPCDEL (29) with the id, to everyone there.
    npc.send(Message::NpcDeleted { npc_id: 900 });
    let mut want = Vec::new();
    seihai_gserver::wire::gint3(id, &mut want);
    for c in [&a, &b, &late] {
        wait("PLO_NPCDEL", || c.got(29).contains(&want));
    }

    // One more, then the link closes: it goes too.
    npc.send(Message::NpcAdded {
        npc_id: 901,
        level: START.as_bytes().to_vec(),
        props: npc_place("sign.png", 10, 10),
    });
    shown(&a, "sign.png");
    let n = a.got(29).len();
    npc.out.shutdown(std::net::Shutdown::Both).unwrap();
    wait("PLO_NPCDEL after the link closed", || a.got(29).len() > n);
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn player_props_warps_weapons_flags_and_pms() {
    let w = world("player");
    let (game, link, _server) = start(&w);
    let mut npc = FakeNpc::connect(&link);
    let a = Client::login(&game, ClientVersion::V222, "plya", &npc);

    // SET_PLAYER_PROPS: hearts and rupees; PLAYER_PROPS goes back.
    let mut props = vec![0x20 + 2, 0x20 + 5, 0x20 + 3];
    seihai_gserver::wire::gint3(250, &mut props);
    npc.send(Message::SetPlayerProps {
        player_id: a.id,
        props,
    });
    a.until("the new hearts and rupees", |s| {
        s.own_cur_power == Some(5) && s.own_rupees == Some(250)
    });
    let pid = a.id;
    npc.wait_for("PLAYER_PROPS", move |m| {
        matches!(m, Message::PlayerProps { player_id, props } if *player_id == pid && props.first() == Some(&(0x20 + 2)))
    });

    // WARP_PLAYER: into another level, with no link to it.
    npc.send(Message::WarpPlayer {
        player_id: a.id,
        level: b"house.nw".to_vec(),
        x: 20,
        y: 24,
    });
    a.until("the warp", |s| {
        s.current_level_name().as_deref() == Some("house.nw")
            && s.player_x == Some(10.0)
            && s.player_y == Some(12.0)
    });
    npc.wait_for("PLAYER_LEVEL", move |m| {
        matches!(m, Message::PlayerLevel { player_id, level } if *player_id == pid && level == b"house.nw")
    });

    // ADD_WEAPON: the bow is a default weapon; another name goes as itself.
    for name in ["bow", "Lantern"] {
        npc.send(Message::AddWeapon {
            player_id: a.id,
            weapon: name.as_bytes().to_vec(),
        });
    }
    // The login burst sent 8 and 7; the bow is a third 43.
    wait("PLO_DEFAULTWEAPON", || a.got(43).len() >= 3);
    a.until("the weapon", |s| {
        s.weapons.iter().any(|w| w.name == "Lantern")
    });

    // SERVER_FLAG: set for everyone online and later logins, then unset.
    npc.send(Message::ServerFlag {
        name: b"server.door".to_vec(),
        set: true,
        value: b"open".to_vec(),
    });
    a.until("the server flag", |s| {
        s.flags.get("server.door").map(String::as_str) == Some("open")
    });
    let b = Client::login(&game, ClientVersion::V205, "plyb", &npc);
    b.until("the server flag at login", |s| {
        s.flags.get("server.door").map(String::as_str) == Some("open")
    });
    npc.send(Message::ServerFlag {
        name: b"server.door".to_vec(),
        set: false,
        value: Vec::new(),
    });
    wait("PLO_FLAGDEL", || {
        a.got(31).contains(&b"server.door".to_vec())
    });

    // PM: a private message, named NPC-Server.
    npc.send(Message::Pm {
        player_id: a.id,
        text: b"Welcome to the lab".to_vec(),
    });
    a.until("the PM", |s| {
        s.chat_log.iter().any(|l| {
            l.kind == ChatKind::Private && l.text == "Welcome to the lab" && l.from == "NPC-Server"
        })
    });

    // Sounds and the NPC trigger action have no client message: nothing
    // breaks, and the link stays up.
    npc.send(Message::PlaySound {
        level: b"house.nw".to_vec(),
        file: b"door.wav".to_vec(),
        x: 10,
        y: 10,
        looped: false,
    });
    npc.send(Message::Ping { nonce: 7 });
    npc.wait_for("PONG", |m| matches!(m, Message::Pong { nonce: 7 }));
    let _ = std::fs::remove_dir_all(&w);
}

#[test]
fn a_changed_level_file_is_reloaded() {
    let w = world("reload");
    let (game, link, _server) = start(&w);
    let npc = FakeNpc::connect(&link);
    // The login reads the start level.
    let _a = Client::login(&game, ClientVersion::V222, "rela", &npc);
    let f = std::fs::File::options()
        .write(true)
        .open(w.join("levels").join(START))
        .unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(3600))
        .unwrap();
    npc.wait_for(
        "LEVEL_RELOADED",
        |m| matches!(m, Message::LevelReloaded { name } if name == START.as_bytes()),
    );
    let _ = std::fs::remove_dir_all(&w);
}
