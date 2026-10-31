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
