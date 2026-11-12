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
| `src/accounts.rs` | `accounts/<name>.txt`, PBKDF2 passwords, opened chests and player flags | 1 |
| `src/login.rs` | the login line: type, key, token, account, password, tail | 1 |
| `src/secure.rs` | the Seihai secure login (`SEI00001`): spots C1, answers S1, opens C2 and rebuilds the classic login frame | 1 |
| `src/ratelimit.rs` | login scheduling: the cap on password checks at once (`password_checks`) and the per-address login rate (`login_rate`) | 1 |
| `src/codec.rs` | frames per generation (gen 2 zlib, gen 3 LCG byte, gen 5 XOR) | 1 |
| `src/wire.rs` | server-to-client message writers and the ids we use (`docs/packet-ids.md`) | 1 |
| `src/burst.rs` | the login burst per generation, own player props | 1 |
| `src/server.rs` | accept loop, one task per connection, the idle session | 1 |
| `src/world.rs` | the world task: who is online and player ids, the levels read so far, other players in a level, links between levels, gmap cells and crossings, opened chests and player flags, items on the floor, the account save on logout, NPCs, baddies and level leaders, bombs and hits, the 50 ms tick (`TICK`) | 2, 3 |
| `src/levels.rs` | level files (`.nw`, `.graal`, `.zelda`) through `seihai_core::assets`, the level name check, the link, sign and chest writers | 2 |
| `src/gmaps.rs` | `.gmap` files in `levels/` (`seihai_core::gmap::Gmap`), which builds play gmaps, a level's cell | 2 |
| `src/others.rs` | other players' props: kept in one form, sent as `PLO_OTHERPLPROPS` (8) per receiver's build; join (50 = 1), logout (50 = 0, 51); chest items given as own props (`PLO_PLAYERPROPS` 9) | 2 |
| `src/files.rs` | `WANTFILE`/`UPDATEFILE` served from `<world>/files/` and the `.gmap` files in `levels/`: the name check, the case-insensitive index, `PLO_FILE` per build, large files, packages | 2 |
| `src/gpak.rs` | `.gpak` / `.gpake`: who loads them, `config/gpakkeys.txt`, the `PLO_FILEDECRYPTKEY` (104) packet | 2 |
| `src/npcs.rs` | level NPCs: ids, props kept as wire bytes, the props each build reads, props a client may not set | 3 |
| `src/baddies.rs` | baddies from the level file, `PLO_BADDYPROPS`, hits handed to the level leader, death and respawn | 3 |
| `src/listlink.rs` | the listserver link over the encrypted channel (`SVLINK01`): register, player count with nickname and props, account check (`VERIACC2`), pong (`SVO_PING` 16), private messages, file start; two plain threads, reconnects every 15 s | link |
| `src/rcaccounts.rs` | accounts from an RC: list, get, set, add, delete on the account files (slice 4) | 4 |
| `src/rc.rs` | RC logins for `seihai-rc`: the `STAFF` and `IPRANGE` checks, the welcome (slice 1); the player list entries and RC chat lines the world task sends RCs (slice 2); private and admin messages and the disconnect (slice 3); NC still refused | 4 |
| `src/npclink.rs` | the link to `seihai-npcserver` (`PROTOCOL.md`): `Hub` (players and server flags for the link, the world's inbox), `serve_link`, `supervise` for `npcserver = auto` | 5 |
| `src/anticheat.rs` | the admin policy (`anticheat.*` settings), violation scores and kicks, the log; the speed, teleport, warp and message-rate checks (stage 2.9); item and damage checks to come (`docs/PLAN.md`, stage 7) | 2, 7 |

The skeleton modules hold the plan for their stage as doc comments and the
types the stage will fill in. They are compiled, so they stay in step with
the rest; nothing calls them yet.

## Data flow (stage 1)

```
TcpListener ──accept──▶ Conn task (server.rs)
   first frame: plain zlib ─▶ [SEI00001? secure.rs: S1 out, C2 in, rebuilt classic frame]
                           ─▶ login::parse ─▶ allow-list ─▶ accounts (spawn_blocking: PBKDF2)
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

Who is online (account to player id) is owned by the world task below: a
login asks it with `World::join` after the account check, and the
connection sends `Left` when it closes.

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
  `login::valid_account_name` before they name a file; level names pass
  `levels::valid_level_name` and file names `files::valid_name`, and a
  file is only ever looked up in the index of the world's own folders.

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
- `Joined` returns the level the player is in, read once by the world from
  `levels.rs` (the generated board only when the file is missing). The
  connection builds the burst from it, since its frames differ per
  generation. The other players in the level arrive through the outbox once the burst
  is out (item 3).

## The NPC-server link

```
World task ──Hub::joined / props / level / left / level_reloaded──▶ Hub (Mutex: players + link sender)
                                             │  only when an NPC-server is connected
                                             ▼
                     link task (serve_link) ──frames──▶ seihai-npcserver
World task ◀──WorldIn::FromNpc (0x40 to 0x4C), NpcGone── link task
                                             ▲
         supervise (npcserver = auto): starts seihai-npcserver with
         --gserver 127.0.0.1:<link port> --world <world> --once and the token in
         SEIHAI_NPCSERVER_TOKEN; restarts it with backoff; kills it on exit
```

- The wire format is `seihai_npcserver::link` (a git dependency pinned by
  tag), so both programs share one codec. `PROTOCOL.md` is a copy of the
  NPC-server repo's; change both together.
- Props on the link are in the 2.22 encoding: the login uses
  `burst::own_props(V222, ...)`, and client prop updates are re-emitted with
  the old bow slot (prop 10 before 2.1) dropped.
- The world task makes every `Hub` call, so the link hears of a level
  change in the same order the clients do. The NPC-server's messages reach
  the world through a weak sender (`Hub::attach_world`), so the world task
  still ends when the server drops it. Server NPCs sit in their level's
  NPC list under this server's ids; when the link closes they are deleted.

## Testing

- Unit tests next to the code; every writer against `seihai-core`'s reader.
- `tests/login_matrix.rs`: every profile logs in through `seihai_core::net`.
- `tests/secure_login.rs`: the same matrix through the secure login, plus
  a wrong pinned key and `require_secure_login`. Helpers are in
  `tests/common/`; each test file is its own binary so the matrices do not
  compete for the CPU (they time out when run side by side on 4 cores).
  Add a matrix test per stage.
- `tests/world_levels.rs`: the start level from `levels/` (board, links,
  signs, chests) for 1.39, 2.05, 2.22 and 2.31, and the blank board when
  the file is missing.
- `tests/other_players.rs`: a 2.05 and a 2.31 in one level see each other
  arrive, walk (X2/Y2 seen as half tiles) and log out.
- `tests/gmaps.rs`: a 2.22 on a 2x1 gmap is told the gmap at login, asks for
  its neighbour, walks east across the seam and back; a 2.05 in the west
  level gets it as a plain level and sees the 2.22 go and come back.
- `tests/level_warps.rs`: a 2.22 takes a link into another level and back,
  then in again with its cached copy's mod time (name only); a 1.39 takes the
  same link; a 2.05 left behind sees the player go and come back.
- `tests/file_serving.rs`: 1.39, 2.05, 2.22 and 2.31 ask for a small file,
  a missing one, a `.gmap`, a 150 KB one and a name with `..`, and check
  copies with old and future times; every answer and every byte is checked.
- `tests/chests.rs`, `tests/floor_items.rs`, `tests/save_on_stop.rs`:
  chests, flags and items on the floor stay with the account, including a
  save on stop while players are online.
- `tests/anticheat.rs`: a cheating client's teleport, fast step, unlinked
  warp and flood each get the configured action.
- `tests/level_npcs.rs`, `tests/baddies.rs`, `tests/bombs_arrows.rs`,
  `tests/hurt_death.rs`: stage 3.1 to 3.4 between two clients in a level.
- `tests/login_load.rs`, `tests/unserved_logins.rs`,
  `tests/quiet_weapon_ids.rs`, `tests/sample_world.rs`: login limits, RC/NC
  refused in their own framing, ids 25 and 33 accepted quietly, the sample
  world loads clean.
- `tests/rc_login.rs`: RC logins of type 6 (gen 5) and type 1 (zlib), and
  type 6 through `SEI00001`, for a `STAFF` account; a non-staff account,
  a wrong password and an address outside `IPRANGE` refused.
- `tests/rc_accounts.rs`: an RC lists, reads, sets, adds and deletes
  accounts with `seihai-rc`'s own messages.
- `tests/rc_players.rs`: two RCs (`seihai-rc`'s own client) and a player:
  the list on login, each RC listed to the other, RC chat, a nickname
  update and the leaves; then private and admin messages from an RC as two
  2.22 players read them, and a disconnect.
- `tests/listserver_link.rs`: the encrypted listserver link against a fake
  listserver on loopback.
- `tests/file_rtt.rs` (ignored; a lab measure): file round trips while a
  level full of players walks.
- `tests/npc_link.rs`: this gserver, the real NPC-server session code and real
  client logins (2.22, 2.05) on loopback.
- `tests/npc_messages.rs`: a stand-in NPC-server on the shared link codec
  sends `0x40` to `0x4C`; 2.05, 2.22 and 2.31 logins see server NPCs come,
  move and go, own props, a warp, weapons, server flags and a PM, and the
  link hears `PLAYER_PROPS`, `PLAYER_LEVEL` and `LEVEL_RELOADED`.
- `tools/lab-matrix.sh`: the real `seihai-mae` binary for every profile.
- `tools/lab-wine.md`: real Windows clients under Wine in a loopback-only
  namespace. Record each run in `docs/lab/`.

## Secure login (SEI00001)

Spec: Seihai `documentation/secure-login.md`. The crypto and the message
formats live in `seihai_core::protocol::seclogin` so the client and every
server share one definition; `secure.rs` only fits them to `Conn::run`.
After C2 the rebuilt classic frame goes through `login::parse`, the
allow-list (checked on the inner token) and the account store as any other
login, so nothing after the login knows the difference. The server key is
loaded by `load_secure_identity` (not `load_world`, so reading a world never
writes a file) from `secure_login_key` (default `config/seihai-login.key`).
