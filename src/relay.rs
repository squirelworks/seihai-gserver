//! Optional byte-preserving relay-era lobby, separate from the central world.
//!
//! Wire facts: Graal_analysis protocol/relay-1998.md and the qualified
//! client-internals/relay-room-bridge.md captures. No game payload is decoded.
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub max_clients: usize,
    pub max_line_bytes: usize,
    pub local_levels: bool,
}
impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            max_clients: 128,
            max_line_bytes: 1024,
            local_levels: true,
        }
    }
}
struct Peer {
    abort: tokio::task::AbortHandle,
    generation: u64,
    room: Option<Vec<u8>>,
    name: Vec<u8>,
    available: bool,
    partners: BTreeSet<u16>,
    output: mpsc::Sender<Vec<u8>>,
}
enum Event {
    Line(u16, u64, Vec<u8>),
    Gone(u16, u64),
}
fn tagged(tag: u8, id: u16) -> Vec<u8> {
    format!("{}{:08}", char::from(tag), id).into_bytes()
}
fn send(peers: &BTreeMap<u16, Peer>, id: u16, line: Vec<u8>, failed: &mut BTreeSet<u16>) {
    if let Some(p) = peers.get(&id) {
        if p.output.try_send(line).is_err() {
            failed.insert(id);
        }
    }
}
fn leave(peers: &mut BTreeMap<u16, Peer>, id: u16, failed: &mut BTreeSet<u16>, unlist: bool) {
    let Some(me) = peers.get_mut(&id) else { return };
    let partners = std::mem::take(&mut me.partners);
    let room = me.room.clone();
    let was_available = me.available;
    if unlist {
        me.available = false;
    }
    for other in partners {
        if let Some(p) = peers.get_mut(&other) {
            p.partners.remove(&id);
        }
        send(peers, other, tagged(b'C', id), failed);
        send(peers, id, tagged(b'C', other), failed);
    }
    if unlist && was_available {
        for (&other, p) in peers.iter() {
            if other != id && room.is_some() && p.room == room && p.available {
                send(peers, other, tagged(b'-', id), failed);
            }
        }
    }
}
fn command(
    peers: &mut BTreeMap<u16, Peer>,
    id: u16,
    line: &[u8],
    local: bool,
    failed: &mut BTreeSet<u16>,
) {
    let Some((&op, body)) = line.split_first() else {
        return;
    };
    match op {
        b'N' => {
            let Some(split) = body.iter().position(|&b| b == b'#') else {
                return;
            };
            if split == 0 || body.contains(&0) {
                return;
            }
            leave(peers, id, failed, true);
            let room = body[..split].to_vec();
            let name = body[split + 1..].to_vec();
            let Some(me) = peers.get_mut(&id) else { return };
            me.room = Some(room.clone());
            me.name = name.clone();
            me.available = true;
            send(peers, id, tagged(b'I', id), failed);
            if local {
                send(peers, id, b"NR".to_vec(), failed);
                send(peers, id, b"NL".to_vec(), failed);
            }
            for (&other, p) in peers.iter() {
                if other != id && p.available && p.room.as_ref() == Some(&room) {
                    let mut announcement = tagged(b'+', other);
                    announcement.extend_from_slice(&p.name);
                    send(peers, id, announcement, failed);
                    let mut announcement = tagged(b'+', id);
                    announcement.extend_from_slice(&name);
                    send(peers, other, announcement, failed);
                }
            }
        }
        b'P' => {
            if body.len() != 8 || !body.iter().all(u8::is_ascii_digit) {
                return;
            }
            let Ok(other) = body
                .iter()
                .try_fold(0u16, |n, b| {
                    n.checked_mul(10)?.checked_add(u16::from(b - b'0'))
                })
                .ok_or(())
            else {
                return;
            };
            if other == id {
                return;
            }
            let Some(me) = peers.get(&id) else { return };
            let Some(target) = peers.get(&other) else {
                return;
            };
            if !me.available
                || !target.available
                || me.room.is_none()
                || me.room != target.room
                || me.partners.contains(&other)
            {
                return;
            }
            // Joining parties merges their peer graphs into one clique. Data
            // remains opaque; each newly linked pair receives confirmation.
            let mut party = me.partners.clone();
            party.extend(target.partners.iter().copied());
            party.insert(id);
            party.insert(other);
            let room = me.room.clone();
            let party: Vec<_> = party
                .into_iter()
                .filter(|p| peers.get(p).is_some_and(|p| p.room == room))
                .collect();
            for (i, &a) in party.iter().enumerate() {
                for &b in &party[i + 1..] {
                    if peers.get_mut(&a).unwrap().partners.insert(b) {
                        peers.get_mut(&b).unwrap().partners.insert(a);
                        send(peers, a, tagged(b'P', b), failed);
                        send(peers, b, tagged(b'P', a), failed);
                    }
                }
            }
        }
        b'D' => {
            if let Some(me) = peers.get(&id) {
                for &other in &me.partners {
                    send(peers, other, line.to_vec(), failed);
                }
            }
        }
        b'+' if body.is_empty() => {
            let Some(me) = peers.get_mut(&id) else { return };
            if me.available {
                return;
            }
            let Some(room) = me.room.clone() else { return };
            me.available = true;
            let name = me.name.clone();
            for (&other, p) in peers.iter() {
                if other != id && p.available && p.room.as_ref() == Some(&room) {
                    let mut announcement = tagged(b'+', other);
                    announcement.extend_from_slice(&p.name);
                    send(peers, id, announcement, failed);
                    let mut announcement = tagged(b'+', id);
                    announcement.extend_from_slice(&name);
                    send(peers, other, announcement, failed);
                }
            }
        }
        b'C' if body.is_empty() => leave(peers, id, failed, false),
        b'-' if body.is_empty() => {
            let Some(me) = peers.get_mut(&id) else { return };
            if !me.available {
                return;
            }
            me.available = false;
            let room = me.room.clone();
            for (&other, peer) in peers.iter() {
                if other != id && room.is_some() && peer.room == room && peer.available {
                    send(peers, other, tagged(b'-', id), failed);
                }
            }
        }
        _ => {}
    }
}
async fn connection(
    stream: TcpStream,
    id: u16,
    generation: u64,
    cap: usize,
    events: mpsc::Sender<Event>,
    mut output: mpsc::Receiver<Vec<u8>>,
) {
    let (mut reader, mut writer) = stream.into_split();
    let input = async {
        let mut pending = Vec::new();
        let mut chunk = [0u8; 512];
        loop {
            let n = reader.read(&mut chunk).await?;
            if n == 0 {
                return Ok::<(), io::Error>(());
            }
            for &byte in &chunk[..n] {
                if byte == b'\n' {
                    if pending.first() != Some(&b'D') && pending.last() == Some(&b'\r') {
                        pending.pop();
                    }
                    let line = std::mem::take(&mut pending);
                    if events
                        .send(Event::Line(id, generation, line))
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                } else {
                    if pending.len() >= cap {
                        return Ok(());
                    }
                    pending.push(byte);
                }
            }
        }
    };
    let writes = async {
        while let Some(line) = output.recv().await {
            writer.write_all(&line).await?;
            writer.write_all(b"\n").await?;
        }
        Ok::<(), io::Error>(())
    };
    tokio::select! { _ = input => {}, _ = writes => {} }
    let _ = events.send(Event::Gone(id, generation)).await;
}
/// Serve an explicitly supplied listener. Dropping this future aborts all its
/// connection tasks. Duplicate pairing is deliberately idempotent, unlike the
/// historical doubled-delivery bug. Observer/superuser modes are unsupported.
pub async fn run(listener: TcpListener, config: RelayConfig) -> io::Result<()> {
    if config.max_clients == 0
        || config.max_clients > 9999
        || config.max_line_bytes == 0
        || config.max_line_bytes > 65536
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid relay limits",
        ));
    }
    let (events, mut incoming) = mpsc::channel(128);
    let mut peers = BTreeMap::new();
    let mut tasks = JoinSet::new();
    let mut next = 1u16;
    let mut generation = 0u64;
    loop {
        let mut failed = BTreeSet::new();
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if peers.len() >= config.max_clients { drop(stream); continue }
                while peers.contains_key(&next) { next = if next == 9999 { 1 } else { next+1 }; }
                let id = next; next = if next == 9999 { 1 } else { next+1 };
                generation = generation.checked_add(1).ok_or_else(|| io::Error::other("relay connection generation exhausted"))?;
                // A rejoin can synchronously enqueue party-leave notices,
                // the complete roster and identity/mode lines before a
                // writer is polled.
                // Do not mistake that normal burst for a slow reader.
                let (output, receiver) = mpsc::channel(2 * config.max_clients + 4);
                let abort = tasks.spawn(connection(stream, id, generation, config.max_line_bytes, events.clone(), receiver));
                peers.insert(id, Peer {abort, generation, room:None, name:Vec::new(), available:false, partners:BTreeSet::new(), output});
            }
            Some(event) = incoming.recv() => match event {
                Event::Line(id, token, line) => {
                    if peers.get(&id).is_some_and(|p| p.generation == token) {
                        command(&mut peers, id, &line, config.local_levels, &mut failed);
                    }
                },
                Event::Gone(id, token) => {
                    if peers.get(&id).is_some_and(|p| p.generation == token) {
                        leave(&mut peers,id,&mut failed,true); peers.remove(&id);
                    }
                }
            },
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
        while let Some(id) = failed.pop_first() {
            leave(&mut peers, id, &mut failed, true);
            if let Some(peer) = peers.remove(&id) {
                peer.abort.abort();
            }
        }
    }
}
