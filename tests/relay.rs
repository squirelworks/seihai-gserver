//! Generated loopback clients validate the relay transport, not native-client fidelity.
use seihai_gserver::relay::{self, RelayConfig};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{tcp::OwnedReadHalf, tcp::OwnedWriteHalf, TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;

struct Client {
    input: BufReader<OwnedReadHalf>,
    output: OwnedWriteHalf,
    id: Vec<u8>,
}
impl Client {
    async fn connect(addr: std::net::SocketAddr, room: &str) -> Self {
        let (read, write) = TcpStream::connect(addr).await.unwrap().into_split();
        let mut c = Self {
            input: BufReader::new(read),
            output: write,
            id: Vec::new(),
        };
        c.send(format!("N{room}#def\n").as_bytes()).await;
        for _ in 0..3 {
            let line = c.line().await;
            if line.starts_with(b"I") {
                c.id = line[1..].to_vec();
            } else {
                assert!(
                    line == b"NR" || line == b"NL",
                    "unexpected join line {line:?}"
                );
            }
        }
        assert_eq!(c.id.len(), 8);
        assert!(c.id.iter().all(u8::is_ascii_digit));
        c
    }
    async fn send(&mut self, bytes: &[u8]) {
        self.output.write_all(bytes).await.unwrap();
    }
    async fn line(&mut self) -> Vec<u8> {
        let mut line = Vec::new();
        let n = timeout(
            Duration::from_secs(3),
            self.input.read_until(b'\n', &mut line),
        )
        .await
        .expect("relay reply deadline")
        .unwrap();
        assert!(n > 0, "unexpected relay EOF");
        assert_eq!(line.pop(), Some(b'\n'));
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        line
    }
    async fn until(&mut self, expected: &[u8]) {
        for _ in 0..12 {
            if self.line().await == expected {
                return;
            }
        }
        panic!("expected relay line {expected:?}");
    }
}
struct Server(JoinHandle<std::io::Result<()>>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn start(config: RelayConfig) -> (std::net::SocketAddr, Server) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (addr, Server(tokio::spawn(relay::run(listener, config))))
}
fn tagged(tag: u8, id: &[u8]) -> Vec<u8> {
    [vec![tag], id.to_vec()].concat()
}
async fn pair(a: &mut Client, b: &mut Client) {
    let mut request = tagged(b'P', &b.id);
    request.push(b'\n');
    a.send(&request).await;
    a.until(&tagged(b'P', &b.id)).await;
    b.until(&tagged(b'P', &a.id)).await;
}

#[tokio::test]
async fn exact_room_pairing_preserves_bytes_and_deduplicates_requests() {
    let (addr, _server) = start(RelayConfig::default()).await;
    let mut java = Client::connect(addr, "G2level13.graal").await;
    let mut alpha = Client::connect(addr, "G2level13.graal").await;
    let mut fifth = Client::connect(addr, "G3level13.graal").await;
    pair(&mut java, &mut alpha).await;
    // A foreign-room request must not create a party. A following legitimate
    // request and payload provide a positive progress barrier.
    let mut foreign = tagged(b'P', &fifth.id);
    foreign.push(b'\n');
    java.send(&foreign).await;
    let mut duplicate = tagged(b'P', &alpha.id);
    duplicate.push(b'\n');
    java.send(&duplicate).await;
    java.send(b"D\x80\xfe chat\nDsecond\n").await;
    alpha.until(b"D\x80\xfe chat").await;
    assert_eq!(alpha.line().await, b"Dsecond");
    // If either packet leaked across rooms it precedes this freshly paired
    // same-room message in the foreign client's ordered stream.
    let mut other = Client::connect(addr, "G3level13.graal").await;
    assert_eq!(
        fifth.line().await,
        [tagged(b'+', &other.id), b"def".to_vec()].concat()
    );
    pair(&mut other, &mut fifth).await;
    other.send(b"Dforeign-control\n").await;
    assert_eq!(fifth.line().await, b"Dforeign-control");
    java.send(b"C\n").await;
    alpha.until(&tagged(b'C', &java.id)).await;
}

#[tokio::test]
async fn fragmented_join_and_disconnect_remove_the_old_peer() {
    let (addr, _server) = start(RelayConfig::default()).await;
    let mut first = Client::connect(addr, "Graal8").await;
    let stream = TcpStream::connect(addr).await.unwrap();
    let (read, write) = stream.into_split();
    let mut second = Client {
        input: BufReader::new(read),
        output: write,
        id: Vec::new(),
    };
    second.send(b"NGra").await;
    second.send(b"al8#def\n").await;
    // Announcement order relative to I/status is intentionally unconstrained.
    for _ in 0..4 {
        let line = second.line().await;
        if line.starts_with(b"I") {
            second.id = line[1..].to_vec();
        }
    }
    assert_eq!(second.id.len(), 8);
    pair(&mut first, &mut second).await;
    let id = second.id.clone();
    drop(second);
    first.until(&tagged(b'-', &id)).await;
    let mut replacement = Client::connect(addr, "Graal8").await;
    pair(&mut first, &mut replacement).await;
    replacement.send(b"Dreplacement\n").await;
    first.until(b"Dreplacement").await;
}

#[tokio::test]
async fn unterminated_oversized_line_closes_without_waiting_for_newline() {
    let config = RelayConfig {
        max_line_bytes: 32,
        ..RelayConfig::default()
    };
    let (addr, _server) = start(config).await;
    let stream = TcpStream::connect(addr).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut input = BufReader::new(read);
    write.write_all(&[b'N'; 33]).await.unwrap();
    let mut line = Vec::new();
    // An implementation may supply its bounded identity/error line first.
    for _ in 0..3 {
        let result = timeout(Duration::from_secs(3), input.read_until(b'\n', &mut line))
            .await
            .expect("oversize disconnect deadline");
        match result {
            Ok(0) | Err(_) => return,
            Ok(_) => line.clear(),
        }
    }
    panic!("oversized peer remained connected");
}

#[tokio::test]
async fn active_connection_limit_rejects_new_socket_without_evicting_existing_peer() {
    let config = RelayConfig {
        max_clients: 1,
        ..RelayConfig::default()
    };
    let (addr, _server) = start(config).await;
    let mut active = Client::connect(addr, "Graal8").await;
    let mut excess = BufReader::new(TcpStream::connect(addr).await.unwrap());
    let mut bytes = Vec::new();
    let closed = timeout(Duration::from_secs(3), excess.read_until(b'\n', &mut bytes))
        .await
        .expect("connection cap deadline");
    assert!(
        matches!(closed, Ok(0) | Err(_)),
        "over-limit socket was admitted: {bytes:?}"
    );
    // Rejoining the admitted socket is a positive liveness barrier after refusal.
    active.send(b"NGraal8#def\n").await;
    let active_id = active.id.clone();
    active.until(&tagged(b'I', &active_id)).await;
}

#[tokio::test]
async fn cancelling_relay_future_closes_its_active_connections() {
    let (addr, server) = start(RelayConfig::default()).await;
    let mut active = Client::connect(addr, "Graal8").await;
    server.0.abort();
    let mut bytes = Vec::new();
    let closed = timeout(
        Duration::from_secs(3),
        active.input.read_until(b'\n', &mut bytes),
    )
    .await
    .expect("relay cancellation cleanup deadline");
    assert!(
        matches!(closed, Ok(0) | Err(_)),
        "cancelled relay retained a live connection"
    );
}

#[tokio::test]
async fn data_ending_in_carriage_return_is_forwarded_byte_for_byte() {
    let (addr, _server) = start(RelayConfig::default()).await;
    let mut sender = Client::connect(addr, "G2level13.graal").await;
    let mut receiver = Client::connect(addr, "G2level13.graal").await;
    pair(&mut sender, &mut receiver).await;
    sender.send(b"D\x80\xfe\r\n").await;
    let mut wire = Vec::new();
    timeout(
        Duration::from_secs(3),
        receiver.input.read_until(b'\n', &mut wire),
    )
    .await
    .expect("binary data deadline")
    .unwrap();
    assert_eq!(wire, b"D\x80\xfe\r\n");
}

#[tokio::test]
async fn leaving_party_then_relisting_keeps_identity_and_can_pair_again() {
    let (addr, _server) = start(RelayConfig::default()).await;
    let mut first = Client::connect(addr, "Graal8").await;
    let mut second = Client::connect(addr, "Graal8").await;
    pair(&mut first, &mut second).await;
    let retained_id = first.id.clone();
    first.send(b"C\n-\n").await;
    second.until(&tagged(b'-', &retained_id)).await;
    // C left the party before withdrawal; this data must never arrive.
    first.send(b"Dunlisted-leak\n+\n").await;
    let announcement = [tagged(b'+', &retained_id), b"def".to_vec()].concat();
    assert_eq!(second.line().await, announcement);
    // Relisting an already available session must not allocate a new identity
    // or add another announcement. The confirmed party and data are barriers.
    first.send(b"+\n").await;
    let mut request = tagged(b'P', &second.id);
    request.push(b'\n');
    first.send(&request).await;
    // Relisting can announce existing peers to the relisted side, but it must
    // never send a second identity assignment.
    let expected = tagged(b'P', &second.id);
    let mut confirmed = false;
    for _ in 0..4 {
        let line = first.line().await;
        assert!(!line.starts_with(b"I"), "relisting replaced identity");
        if line == expected {
            confirmed = true;
            break;
        }
        assert!(
            line.starts_with(b"+") || line.starts_with(b"C"),
            "unexpected relist line {line:?}"
        );
    }
    assert!(confirmed, "relisted party was not confirmed");
    assert_eq!(second.line().await, tagged(b'P', &retained_id));
    first.send(b"Drelisted\n").await;
    assert_eq!(second.line().await, b"Drelisted");
}

#[tokio::test]
async fn joining_a_partner_joins_the_whole_party_with_single_delivery() {
    let (addr, _server) = start(RelayConfig::default()).await;
    let mut first = Client::connect(addr, "Graal8").await;
    let mut second = Client::connect(addr, "Graal8").await;
    pair(&mut first, &mut second).await;
    let mut third = Client::connect(addr, "Graal8").await;
    // Third must consume both original peers' announcements before requesting
    // the party. Client::connect consumed only I and the two level notices.
    let mut request = tagged(b'P', &first.id);
    request.push(b'\n');
    third.send(&request).await;
    first.until(&tagged(b'P', &third.id)).await;
    second.until(&tagged(b'P', &third.id)).await;
    third.until(&tagged(b'P', &first.id)).await;
    third.until(&tagged(b'P', &second.id)).await;
    third.send(b"Dthird-a\nDthird-b\n").await;
    for peer in [&mut first, &mut second] {
        assert_eq!(peer.line().await, b"Dthird-a");
        assert_eq!(peer.line().await, b"Dthird-b");
    }
    first.send(b"Dfirst-a\nDfirst-b\n").await;
    for peer in [&mut second, &mut third] {
        assert_eq!(peer.line().await, b"Dfirst-a");
        assert_eq!(peer.line().await, b"Dfirst-b");
    }
}

#[tokio::test]
async fn withdrawing_availability_does_not_leave_the_existing_party() {
    let (addr, _server) = start(RelayConfig::default()).await;
    let mut first = Client::connect(addr, "Graal8").await;
    let mut second = Client::connect(addr, "Graal8").await;
    pair(&mut first, &mut second).await;
    first.send(b"-\nDstill-partner\n").await;
    second.until(&tagged(b'-', &first.id)).await;
    assert_eq!(second.line().await, b"Dstill-partner");
    second.send(b"Dreply\n").await;
    assert_eq!(first.line().await, b"Dreply");
}

#[tokio::test]
async fn joining_a_room_with_more_than_sixty_four_peers_receives_the_complete_roster() {
    let config = RelayConfig {
        max_clients: 80,
        ..RelayConfig::default()
    };
    let (addr, _server) = start(config).await;
    let mut existing = Vec::new();
    for _ in 0..65 {
        existing.push(Client::connect(addr, "G2level13.graal").await);
    }
    let mut newcomer = Client::connect(addr, "G2level13.graal").await;
    let mut expected: std::collections::BTreeSet<Vec<u8>> = existing
        .iter()
        .map(|peer| [tagged(b'+', &peer.id), b"def".to_vec()].concat())
        .collect();
    for _ in 0..65 {
        let announcement = newcomer.line().await;
        assert!(
            expected.remove(&announcement),
            "missing, repeated or unexpected roster entry {announcement:?}"
        );
    }
    assert!(expected.is_empty());
    // The full roster must leave the newly joined session usable, not merely
    // flush buffered announcements just before an eviction closes it.
    for _ in 0..65 {
        assert!(existing[0].line().await.starts_with(b"+"));
    }
    pair(&mut newcomer, &mut existing[0]).await;
    newcomer.send(b"Dafter-full-roster\n").await;
    existing[0].until(b"Dafter-full-roster").await;
    // Grow the party to every existing peer, using newcomer confirmations as
    // the barrier before testing its combined disconnect/rejoin reply burst.
    let mut party_request = tagged(b'P', &newcomer.id);
    party_request.push(b'\n');
    for peer in existing.iter_mut().skip(1) {
        peer.send(&party_request).await;
    }
    let mut confirmations: std::collections::BTreeSet<Vec<u8>> = existing
        .iter()
        .skip(1)
        .map(|peer| tagged(b'P', &peer.id))
        .collect();
    for _ in 0..64 {
        assert!(confirmations.remove(&newcomer.line().await));
    }
    assert!(confirmations.is_empty());
    newcomer.send(b"NG2level13.graal#def\n").await;
    let mut departures: std::collections::BTreeSet<Vec<u8>> =
        existing.iter().map(|peer| tagged(b'C', &peer.id)).collect();
    let mut roster: std::collections::BTreeSet<Vec<u8>> = existing
        .iter()
        .map(|peer| [tagged(b'+', &peer.id), b"def".to_vec()].concat())
        .collect();
    let mut identity = 0;
    let mut remote_notice = 0;
    let mut local_notice = 0;
    for _ in 0..133 {
        let line = newcomer.line().await;
        match line.first() {
            Some(b'C') => assert!(departures.remove(&line)),
            Some(b'+') => assert!(roster.remove(&line)),
            Some(b'I') => {
                assert_eq!(line, tagged(b'I', &newcomer.id));
                identity += 1;
            }
            _ if line == b"NR" => remote_notice += 1,
            _ if line == b"NL" => local_notice += 1,
            _ => panic!("unexpected rejoin line {line:?}"),
        }
    }
    assert!(departures.is_empty());
    assert!(roster.is_empty());
    assert_eq!((identity, remote_notice, local_notice), (1, 1, 1));
}
