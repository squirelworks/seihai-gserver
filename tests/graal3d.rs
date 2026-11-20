//! The 3D mode over real sockets: a 5.12 logs in over TCP, opens the
//! Torque UDP link on the next port, goes through the mission start to its
//! control object, and loses the link when the TCP session ends.

mod common;

use seihai_core::net::{login_gserver_secure, random_key, wantfile_message};
use seihai_core::protocol::{ClientVersion, SessionState};
use seihai_gserver::graal3d::bits::{BitReader, BitWriter};
use seihai_gserver::graal3d::events::{Event, Events};
use seihai_gserver::graal3d::handshake::{self, PROTOCOL_24};
use seihai_gserver::graal3d::link::{Link, Rate};
use seihai_gserver::graal3d::objects::GameObject;
use seihai_gserver::server::serve;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn start_3d(world: &std::path::Path) -> (String, std::net::SocketAddr) {
    let (server, warnings) = seihai_gserver::load_world(world).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let server = Arc::new(server);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let g = server.graal3d.clone().expect("graal3d = true");
            let udp = seihai_gserver::start_graal3d(&server, &g, 0).await.unwrap();
            tx.send((l.local_addr().unwrap(), udp)).unwrap();
            let _ = serve(server, l).await;
        });
    });
    let (tcp, udp) = rx.recv().unwrap();
    (tcp.to_string(), udp)
}

fn recv(sock: &UdpSocket, until: Instant) -> Option<Vec<u8>> {
    let mut buf = [0u8; 2048];
    sock.set_read_timeout(Some(
        until
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1)),
    ))
    .unwrap();
    sock.recv(&mut buf).ok().map(|n| buf[..n].to_vec())
}

fn connect_request(seq: u32, digest: [u32; 4], account: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.u8(handshake::kind::CONNECT_REQUEST);
    w.u32(seq);
    for d in digest {
        w.u32(d);
    }
    w.string(b"GameConnection");
    w.u32(0);
    w.u32(0xffff_ffff);
    w.string(b"Graal3D");
    w.u32(24);
    w.u32(24);
    w.string(common::PASSWORD.as_bytes());
    w.u32(2);
    w.string(account);
    w.string(b"");
    w.into_bytes()
}

#[test]
fn a_512_reaches_its_control_object_and_leaves_with_its_tcp_session() {
    let world = common::world("graal3d", "G3D14097\n");
    std::fs::write(
        world.join("config/server.txt"),
        "create_accounts = true\nheartbeat = 1\ngraal3d = true\n",
    )
    .unwrap();
    std::fs::write(
        world.join("config/graal3dworld.txt"),
        "sun 0.57735 0.57735 -0.57735 0.8 0.8 0.8 1 0.3 0.3 0.35 1\n\
         sky sky_fr,sky_rt,sky_bk,sky_lf,sky_up,sky_dn 500\n\
         object house house.dif 4 4 0\n\
         playershape labcube.3ds\n\
         spawn 0 0 1\n",
    )
    .unwrap();
    let (tcp, udp) = start_3d(&world);

    // The UDP side may start first: the request is not answered until the
    // TCP login is done.
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.connect(udp).unwrap();
    sock.send(&[0x1a, 0, 4, 0, 0]).unwrap();
    let resp = recv(&sock, Instant::now() + Duration::from_secs(5)).expect("challenge response");
    let mut r = BitReader::new(&resp);
    assert_eq!(r.u8().unwrap(), handshake::kind::CHALLENGE_RESPONSE);
    assert_eq!(r.u32().unwrap(), 0x400);
    let mut digest = [0; 4];
    for d in &mut digest {
        *d = r.u32().unwrap();
    }
    let request = connect_request(0x400, digest, b"lab3d");
    sock.send(&request).unwrap();
    assert!(
        recv(&sock, Instant::now() + Duration::from_millis(300)).is_none(),
        "no answer before the TCP login"
    );

    let v = ClientVersion::V512;
    let mut sess = login_gserver_secure(
        &tcp,
        v,
        random_key(),
        "lab3d",
        common::PASSWORD,
        true,
        true,
        wantfile_message,
        None,
    )
    .unwrap();
    let rx = sess.conn.spawn_reader().unwrap();
    let mut st = SessionState::for_version(v);
    common::read_until(&rx, &mut st, Duration::from_secs(10), |s| {
        s.board.is_some() && s.current_level_name().is_some()
    })
    .unwrap();

    // Now the connect request is accepted, with protocol 24's table.
    let deadline = Instant::now() + Duration::from_secs(5);
    let accept = loop {
        sock.send(&request).unwrap();
        if let Some(p) = recv(&sock, Instant::now() + Duration::from_millis(200)) {
            if p[0] == handshake::kind::CONNECT_ACCEPT {
                break p;
            }
        }
        assert!(Instant::now() < deadline, "no accept");
    };
    assert_eq!(accept, handshake::connect_accept(0x400, &PROTOCOL_24));

    // The link: answer the two mission phases until the control object.
    let classes = PROTOCOL_24.class_table();
    let mut link = Link::new(0x400);
    let mut events = Events::new();
    let mut first = true;
    let mut ghosts: Vec<String> = Vec::new();
    let mut acked = Vec::new();
    let mut control = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while control.is_none() {
        assert!(
            Instant::now() < deadline,
            "no control object; ghosts {ghosts:?}"
        );
        let max = std::mem::replace(&mut first, false).then_some(Rate::CLIENT_512_MAX);
        let mut w = link.data(None, max).unwrap();
        w.flag(true);
        for _ in 0..3 {
            w.f32(0.0);
        }
        w.flag(false);
        w.flag(false);
        w.flag(false);
        let seq = link.last_sent();
        events.write(&mut w, seq, &classes, 3000);
        sock.send(&w.into_bytes()).unwrap();

        let Some(p) = recv(&sock, Instant::now() + Duration::from_millis(500)) else {
            continue;
        };
        let Ok(got) = link.receive(&p) else {
            continue;
        };
        for n in &got.notifies {
            events.notify(*n);
        }
        let Some(mut d) = got.data else {
            continue;
        };
        let b = &mut d.body;
        if b.flag().unwrap() && b.flag().unwrap() {
            b.u32().unwrap();
            let index = b.int(12).unwrap();
            GameObject::default().read(b, &mut Vec::new()).unwrap();
            control = Some(index);
        }
        // Its own Camera, named once its create is in.
        if b.flag().unwrap() {
            b.int(12).unwrap();
            assert!(!b.flag().unwrap(), "no camera packet data");
        }
        assert!(!b.flag().unwrap(), "no fov");
        for ev in events.read(b, &classes).unwrap() {
            if let Some(c) = events.command(&ev) {
                let ack: &[u8] = match c.name.as_slice() {
                    b"MissionStartPhase1" => b"MissionStartPhase1Ack",
                    b"MissionStartPhase2" => b"MissionStartPhase2Ack",
                    _ => continue,
                };
                if !acked.contains(&ack) {
                    acked.push(ack);
                    events.send_command(ack, &[]);
                }
            }
            let _: Event = ev;
        }
        if b.flag().unwrap() {
            let width = b.int(4).unwrap();
            while b.flag().unwrap() {
                let index = b.int(width).unwrap();
                if b.flag().unwrap() {
                    continue; // deleted
                }
                let class = b.int(12).unwrap();
                let len = b.int(14).unwrap() as usize;
                let start = b.position();
                let name = PROTOCOL_24.objects[class as usize];
                if name == "GameObject" {
                    let mut o = GameObject::default();
                    o.read(b, &mut Vec::new()).unwrap();
                    ghosts.push(format!("{index} {}", String::from_utf8_lossy(&o.name)));
                } else {
                    ghosts.push(format!("{index} {name}"));
                }
                b.seek(start + len).unwrap();
            }
        }
    }
    assert_eq!(acked.len(), 2, "both phases were sent");
    // The Sky is sent again once the mission is done (the client drops
    // the first one while it loads), so each name is counted once.
    ghosts.sort();
    ghosts.dedup();
    assert_eq!(
        ghosts,
        vec!["0 Sun", "1 Sky", "2 house", "3 G3DPlayer_lab3d", "4 Camera"],
        "the world, the player's own object and its Camera"
    );
    assert_eq!(control, Some(3), "its own object is its control object");

    // The TCP session ends; the 3D link is closed with a disconnect.
    drop(rx);
    drop(sess);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let p = recv(&sock, deadline).expect("a disconnect");
        if p[0] == handshake::kind::DISCONNECT {
            break;
        }
    }
    let _ = std::fs::remove_dir_all(&world);
}
