# seihai-gserver (new Rust gserver)

A clean-room gserver in Rust on `seihai-core`. The fork is a lab reference to diff
behaviour against, never a code source (see `00-rules.md`).

## Shape

- One binary, `seihai-gserver`. A world folder holds `config/`, `accounts/`,
  `levels/`, `weapons/`, `npcs/` (format to be ours; read GServer-v2-style world
  folders too, so existing worlds move over).
- Async networking (tokio). One task per connection; one world task that owns
  levels and players and runs a 50 ms tick (real 2.x clients send walk props on a
  100 ms cadence and expect replies within a frame).
- Protocol through `seihai-core`: framing per generation (gen 1/2 zlib, gen 3
  zlib + LCG byte, gen 4 bz2 + partial XOR, gen 5 XOR + zlib/bz2), version
  profiles, prop encoding. Where the core only has the client-to-server side of a
  message, add the other direction there (in the seihai repo), with tests from the
  captures, so client and server share one definition.
- Listserver link: registers like GServer-v2 does (see `03-listserver.md`), so it
  shows in the list and can verify accounts through the listserver.
- NPC-server link: a listening port for `seihai-npcserver`. With
  `npcserver = auto` in the config, the gserver starts the NPC-server itself.
- RC/NC: accepts RC logins (staff accounts) for `seihai-rc`.

## Stages (each passes the lab matrix before the next starts)

1. Core: listen, login for every generation, allow-list by version token, flat-file
   accounts, the login burst exactly as `login-burst.md` describes, then idle
   keep-alive. Test: every Seihai profile logs in and stays connected 60 s.
   **Done** (see "Stage 1 as built" below and `docs/lab/stage1-2026-10-31.md`).
2. World for 1.33 to 2.31: levels (`.nw`, `.graal`, `.zelda`), gmaps (see
   `gmap-order.md`, `bigmap-neighbours.md`), links and warps, signs, chests, items
   on the ground, other players' props, flags, `WANTFILE`/`UPDATEFILE` file serving.
3. Client-side NPCs: NPC prop sync between players in a level (`npc-props-lab.md`),
   baddies (`baddies.md`), bombs, arrows, hurt and death.
4. RC and NC endpoints for `seihai-rc`.
5. NPC-server link for `seihai-npcserver`. **Gserver to NPC-server half done,
   ahead of stage 2** (`src/npclink.rs`, `PROTOCOL.md`, `tests/npc_link.rs`,
   `docs/lab/npclink-2026-10-31.md`): `npcserver = off | external | auto`, the
   token handshake, level list, player join/props/leave, keep-alive. Left:
   `PLAYER_LEVEL` and `LEVEL_RELOADED` once the world task exists, and the
   NPC-server to gserver messages (drafted in `PROTOCOL.md`).
6. 3.x to 6.x: the Login lobby and server warp, GS2 bytecode delivery, and later our
   own GS2 compiler (gs2-parser is GPL, so it cannot be used).
7. Server-side anti-cheat (see "Milestone: server-side anti-cheat" below). Each
   check lands with the feature it guards (movement and warps with stage 2,
   items, hearts and gralats with stages 2 and 3), behind one policy layer that
   admins configure. No client-side anti-debugging.

## Behaviour to match (from lab findings)

- 2.05 needs `SETACTIVELEVEL` (156) and the level name before the board, or it
  stays on the loading screen.
- 2.05/2.17 send the login and `PLI_LANGUAGE` in one frame: read both.
- 1.33 to 1.39 send `PLI_PACKETCOUNT` (31) every 10 s; it is not a login.
- 1.33 to 1.37 upload each weapon with id 25: accept and ignore.
- On classic servers prop 10 is the bow, not a gani.
- `DEFAULTWEAPON` (43) goes out as 8 then 7, as on every public server type.
- Never log a player in hidden.
- Real 2.22 sends `gr.x`/`gr.y` FLAGSETs in gmap coordinates every 50 ms; handle the
  load.
- Public servers do not require the tamper check (39) at login. Do not require it.

## Milestone: server-side anti-cheat

The server is the authority on what a player has and where they may be. A
client is only trusted for what it could really have done. Everything here
runs on the server and works with unmodified clients of every build. We do not
add client-side anti-debugging, packers or tamper checks: they break old builds
and Wine, and they do not stop a modified client anyway. (Public servers do not
require the tamper check (39) at login, and neither do we.)

### What is checked

- **Movement speed.** Track each player's last accepted position and time from
  their props. Flag a step that is longer than the build's walk speed allows
  over the elapsed time, with slack for lag bursts (clients batch steps after a
  stall). Use the precise position where the profile has it
  (`props.precise_pos`), and whole tiles otherwise. Gmaps count in gmap
  coordinates, so a step across a level edge is not a jump.
- **Teleporting.** A position change with no walking in between is accepted
  only when the server caused it: a warp it sent, a link the player stood on,
  a respawn, or an NPC or script warp from the NPC-server. Anything else is a
  teleport and is put back.
- **Warps and level changes.** `PLI_LEVELWARP` (0), `PLI_LEVELWARPMOD` (30) and
  `PLI_ADJACENTLEVEL` (35) are accepted only for a link the player touches, a
  gmap neighbour of the level they are in, or a warp the server or NPC-server
  sent. Level names pass the same name check as files (no paths out of the
  world folder).
- **Items, gralats and hearts.** The server keeps the real count of gralats,
  arrows, bombs, hearts and max hearts. A prop change that raises one is
  accepted only for a source the server knows: an item it put on the ground
  that this player picked up, a chest the player opened, a shop or script grant
  from the NPC-server, or healing it allows. Item pickups are matched against
  the level's item list and taken off it, so one item is not picked up twice.
  A refused change is answered with the server's value.
- **Damage and death.** Hurt from another player is checked against range and
  weapon (stage 3). A client that sets its own hearts above the server's count
  is put back.
- **Packet rate.** Count messages per connection in a sliding window, by kind.
  The limits must leave room for real traffic: 2.x walk props every 100 ms,
  and real 2.22 sends `gr.x`/`gr.y` FLAGSETs every 50 ms. Over the soft limit,
  messages are dropped and logged; over the hard limit the player is kicked.
  Chat, PMs and file requests get their own lower limits.
- **Malformed input.** Already in stage 1: unknown messages are counted, never
  kicked for. Broken gen-3 ciphers and impossible lengths still end the
  connection, with a `PLO_DISCMESSAGE` first.

### Policy (admin settings)

Each check reports a violation with a kind and a size (how far over). A policy
layer, not the check, decides what happens:

- per kind: `off`, `log`, `correct` (put the value or position back), `kick`,
  or `ban`, with a threshold (for example 3 speed flags in 30 s before `kick`);
- violation scores decay over time, so one lag spike never kicks anyone;
- staff accounts can be exempt per kind;
- bans by account (and later by machine field, where 5.00+ builds send one)
  are stored in the world folder with a reason, the admin and an end time;
- a log file in the world folder, one line per violation (time, account,
  build, level, kind, values), and a summary for `seihai-rc` once stage 4
  lands.

Sketch for `config/server.txt` (names to settle when it is built):

```
anticheat = on
anticheat_log = logs/anticheat.log
anticheat.speed = correct, kick after 5 in 60
anticheat.teleport = correct, kick after 3 in 60
anticheat.items = correct, log
anticheat.rate = kick at 200 per second
```

Defaults are `log` and `correct` only: no kicks or bans until an admin turns
them on, so a check that is wrong for some build never locks real players out.

### Where it lives

- `src/anticheat.rs`: the `Violation` type, the per-player score, and the
  policy from the settings. Checks call one function: `report(player, kind,
  size)`, which returns the action.
- Movement, warp and item checks live in the world task (`world.rs`,
  `levels.rs`), since only it knows positions, links and item lists.
- The packet-rate counter lives in the connection task (`server.rs`), before
  the message reaches the world.

### Tests and lab runs

- Unit tests per check with recorded real walking (every build in the matrix
  must produce zero flags in normal play, including gmap edges and lag).
- A cheating test client built on `seihai-core` in `tests/`: speed walking,
  teleports, warps to unlinked levels, gralat and heart raises, and floods.
  Each must be caught and get the configured action.
- A lab run with real 1.39, 2.05, 2.17, 2.22 and 2.31 under Wine walking,
  warping and picking up items with every check on `kick`: no false kicks.

## Stage 1 as built

- **Generations.** The login frame is plain zlib for every build. Type 5 is gen 5;
  type 0 is gen 2 when a known token follows the type byte directly, else gen 3
  (key byte, then the token). Known tokens are `seihai-core`'s profiles plus the
  allow-list's own. Gen-3 messages behind the login line in the same frame
  already carry their inserted byte; gen-5 iterators start at the seed with the
  next frame. 5.12 gets its rounded-down step count and compressed bodies. Other
  login types (RC, NC, NPC-server) are refused for now.
- **Allow-list.** `config/allowedversions.txt`: `*`, labels, tokens and ranges of
  either, plus GServer-v2-style `name = A:B` lines. No file allows every known build.
- **Accounts.** `accounts/<name>.txt`, our own `KEY value` format, PBKDF2-SHA256
  passwords, unknown keys kept. `create_accounts` makes one on first login (lab only).
  One login per account ("Account already in use.").
- **Burst.** Gen 2: props, warp, raw header / board / name, mod time, leader (the
  1.31 order). Later: signature, start message, status list, props, default
  weapons 8 then 7, warp, name, board, mod time, 156, leader, world time, 182.
  Each message only if the build handles its id; each prop only if the build's
  prop parser takes it. The board is generated (one tile) until stage 2 loads levels.
- **Idle session.** `WANTFILE` gets `FILESENDFAILED`, `UPDATEFILE` gets
  `FILEUPTODATE` (or `FILESENDFAILED` before 2.1); 1.21 reads neither and gets none.
  Language, packet count (31), weapon upload (25), tamper check, process list,
  show-image, flags, enter-level, neighbour and warp requests are read and not
  answered yet. The world time (42) goes out every 5 s to builds that read it; an
  idle timeout is available and off by default.
- **Not yet in core.** The server-to-client writers live in `src/wire.rs` and
  `src/burst.rs`, checked against `seihai-core`'s readers. They should move to
  `seihai-core` when it gets a server side.
- **No world task yet.** Stage 1 shares only who is online. The 50 ms world tick
  arrives with stage 2.

## Secure login (SEI00001), 2026-10-31

Done: `src/secure.rs`, `secure_login_key` / `require_secure_login`,
`tests/secure_login.rs` (every profile through the secure login), lab run in
`docs/lab/secure-login-2026-10-31.md`.

Next:

- [ ] Switch `seihai-core` from the PR rev to the tag once seihai's
      secure-login PR is merged and tagged (`v0.0.4`; Chiaki tags).
- Seihai only: real clients cannot be changed, so their logins stay in the
  clear (Squirrel, 2026-10-31).
- [x] The listserver link is encrypted (Squirrel, 2026-10-31: "encrypt
      it"): `src/listlink.rs`, `listserver_secure` on by default,
      `tests/listserver_link.rs`, lab run in
      `docs/lab/listserver-link-2026-10-31.md`.
- [x] `seihai-core` is on tag `v0.0.5`.
- [ ] Listserver link, later: send the player's nickname and props
      (`PLYRADD` sends only the id and type now), `SVO_PING`, and the
      server-to-server messages (PMs, `SVI_FILESTART`) when stage 3 needs
      them. `VERIACC2`'s "type" is always 0 (a player); RC logins will
      send their own once `rc.rs` lands.
- [ ] RC logins (`rc.rs`, type 1/6): the same handshake once `seihai-rc`
      speaks it.
- [ ] A `key-fingerprint` subcommand that prints the key without starting.
