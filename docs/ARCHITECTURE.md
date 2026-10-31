# seihai-gserver architecture

How the server is put together, and where each later stage plugs in. Read
`AGENTS.md` first (licences, no real servers, loopback-only testing) and
`docs/PLAN.md` for the stage list.

## Layout

| file | what it owns | stage |
|---|---|---|
| `src/main.rs` | the command line: run a world, `account` to set passwords | 1 |
| `src/lib.rs` | `load_world` (settings + allow-list), `run` (bind and serve) | 1 |
| `src/config.rs` | `config/server.txt`, every setting with its default | 1 |
| `src/allow.rs` | `config/allowedversions.txt` | 1 |
| `src/accounts.rs` | `accounts/<name>.txt`, PBKDF2 passwords | 1 |
| `src/login.rs` | the login line: type, key, token, account, password, tail | 1 |
| `src/codec.rs` | frames per generation (gen 2 zlib, gen 3 LCG byte, gen 5 XOR) | 1 |
| `src/wire.rs` | server-to-client message writers and the ids we use | 1 |
| `src/burst.rs` | the login burst per generation, own player props | 1 |
| `src/server.rs` | accept loop, one task per connection, the idle session | 1 |
| `src/world.rs` | the world task: levels, players, the 50 ms tick (skeleton) | 2 |
| `src/levels.rs` | level files (`.nw`, `.graal`, `.zelda`), gmaps, links, signs, chests (skeleton) | 2 |
| `src/files.rs` | `WANTFILE`/`UPDATEFILE` file serving from the world folder (skeleton) | 2 |
| `src/listlink.rs` | the listserver link: register, player count, account check (skeleton) | later |
| `src/rc.rs` | RC and NC logins for `seihai-rc` (skeleton) | 4 |
| `src/npclink.rs` | the link to `seihai-npcserver` (`PROTOCOL.md`): `Hub` (players for the link), `serve_link`, `supervise` for `npcserver = auto` | 5 (first half) |
| `src/anticheat.rs` | violations, scores and the admin policy (planned; see `docs/PLAN.md`, stage 7) | 7 |

The skeleton modules hold the plan for their stage as doc comments and the
types the stage will fill in. They are compiled, so they stay in step with
the rest; nothing calls them yet.

## Data flow (stage 1)

```
TcpListener ──accept──▶ Conn task (server.rs)
   first frame: plain zlib ─▶ login::parse ─▶ allow-list ─▶ accounts (spawn_blocking: PBKDF2)
                                 │
                                 ▼
                        Codec::new(gen, key, version)          codec.rs
                                 │
          burst::login_burst(cfg, version, gen, player) ─▶ Codec::encode ─▶ socket
                                 │
   loop { socket ─▶ Codec::decode ─▶ Codec::messages ─▶ Session::handle ─▶ reply frame
          heartbeat tick ─▶ PLO_NEWWORLDTIME
          idle timeout (off by default) }
```

Shared state is one `Mutex<Online>` (account to player id). Stage 2 replaces
it with the world task below.

## Rules the code follows

- **Per build, from the profile.** What a client gets is decided by its
  `seihai-core` profile, not by version comparisons: a message goes out only
  if `protocol.plo` has its id; an own prop only if `props.own` has it (the
  client's prop parser stops at the first unknown id); prop 10 is left out
  where `props.bow_prop10` is set. New senders should follow the same rule
  (`burst::Out::put` is the pattern).
- **Tokens decide the build.** `login::version_for` maps token and generation
  to the oldest profile with that token. Unknown tokens that the allow-list
  names get a stand-in profile per generation (`burst::shape_version`).
- **Writers are checked against the client's readers.** Every writer in
  `wire.rs`/`burst.rs` has a test that feeds its bytes to `seihai-core`'s
  parser for every build (`SessionState::for_version`, `parse_player_props_in`).
  Do the same for each new message. When `seihai-core` grows a server side,
  move the writers there.
- **Never kick for an unknown message.** Count it, log it once with
  `--verbose`, carry on. Kick only for a broken cipher (three gen-3 bytes in a
  row that do not check out) or a refused login, always with a
  `PLO_DISCMESSAGE` first.
- **Files and names are untrusted.** Account names pass
  `login::valid_account_name` before they name a file. Level and file names
  from clients will need the same treatment in `files.rs`.

## The world task (stage 2)

One tokio task owns every level and player; connection tasks talk to it over
channels:

```
Conn task ──WorldIn::{Joined, Props, LevelWarp, WantFile, Left, ...}──▶ World task
Conn task ◀──Outbox (mpsc of message bodies)─────────────────────────── World task
```

- The world runs a 50 ms tick (`world::TICK`): apply inputs, move baddies
  and NPC timers (stage 3), then flush each player's outbox as one frame. Real
  2.x clients send walk props every 100 ms and expect answers within a frame.
- A connection keeps its `Codec` and only encodes what the world sends it, so
  generation differences stay in the connection task and the world speaks in
  messages.
- The burst moves into the world: `Joined` returns the level the player is in,
  the board from `levels.rs` instead of the generated one, and the other
  players in it.

## The NPC-server link

```
Conn task ──Hub::joined / props / left──▶ Hub (Mutex: players + link sender)
                                             │  only when an NPC-server is connected
                                             ▼
                     link task (serve_link) ──frames──▶ seihai-npcserver
                                             ▲
         supervise (npcserver = auto): starts seihai-npcserver with
         --gserver 127.0.0.1:<link port> --world <world> --once and the token in
         SEIHAI_NPCSERVER_TOKEN; restarts it with backoff; kills it on exit
```

- The wire format is `seihai_npcserver::link` (a git dependency pinned by
  commit), so both programs share one codec. `PROTOCOL.md` is a copy of the
  NPC-server repo's; change both together.
- Props on the link are in the 2.22 encoding: the login uses
  `burst::own_props(V222, ...)`, and client prop updates are re-emitted with
  the old bow slot (prop 10 before 2.1) dropped.
- When the world task arrives (stage 2), the `Hub` calls move into it, and it
  adds `PLAYER_LEVEL` on warps and relays the NPC-server's NPC props to the
  players in each level.

## Testing

- Unit tests next to the code; every writer against `seihai-core`'s reader.
- `tests/login_matrix.rs`: every profile logs in through `seihai_core::net`.
  Add a matrix test per stage (stage 2: walk to a link and get the next level).
- `tests/npc_link.rs`: this gserver, the real NPC-server session code and real
  client logins (2.22, 2.05) on loopback.
- `tools/lab-matrix.sh`: the real `seihai-mae` binary for every profile.
- `tools/lab-wine.md`: real Windows clients under Wine in a loopback-only
  namespace. Record each run in `docs/lab/`.
