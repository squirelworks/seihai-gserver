# seihai-gserver (new Rust gserver)

A clean-room gserver in Rust on `seihai-core`. The fork is a lab reference to diff
behaviour against, never a code source (see `00-rules.md`).

## Shape

- One binary, `seihai-gserver`. A world folder holds `config/`, `accounts/`,
  `levels/`, `weapons/`, `npcs/` (format to be ours; read GServer-v2-style world
  folders too, so existing worlds move over). `seihai-gserver import-v2`
  makes a world from a GServer-v2 server folder: settings, the version list,
  accounts, staff and bans (`src/import.rs`, from the format note
  `docs/GSERVER-V2-WORLD.md`, written from the fork's sample data files
  only), and `world/` by its `foldersconfig.txt` (levels to `levels/`,
  the rest to `files/`).
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
   Played with real 1.39, 2.05 rev 2, 2.17.1, 2.22 and 2.31 under Wine
   (`docs/lab/stage2-2026-11-04.md`). **Done.**
3. Client-side NPCs: NPC prop sync between players in a level (`npc-props-lab.md`),
   baddies (`baddies.md`), bombs, arrows, hurt and death. **Built** (3.1 to 3.4,
   `src/npcs.rs`, `src/baddies.rs`; lab run with real 2.05 rev 2 and 2.22 in
   `docs/lab/stage3-2026-11-05.md`). Open from that run: real clients did not
   take or send player hits; a player saved with 0 hearts logs back in with 0;
   `PLI_THROWCARRIED`'s shape needs a capture.
4. RC and NC endpoints for `seihai-rc` (issue #52, in slices). **Slice 1
   built** (`src/rc.rs`, `tests/rc_login.rs`): RC logins (types 1 and 6,
   and through `SEI00001`) for accounts with `STAFF`, from the account's
   `IPRANGE` (loopback only without one); refusals in the RC's framing; the
   welcome (`PLO_SIGNATURE`, RC chat) and a quiet idle link. **Slice 2
   built** (`tests/rc_players.rs`, with `seihai-rc` v0.1.0 as the client):
   the world task lists RCs under ids from the players' space; an RC gets
   `PLO_ADDPLAYER` for every player and other RC on login, then live
   `ADDPLAYER` on a login, level change or nickname change and
   `PLO_DELPLAYER` on a leave; "New RC: account" and RC chat lines
   (`nickname: text`, our own form) go to every RC. **Slice 3 built**
   (same test file): private messages (28) to players from the RC's id and
   nickname, admin messages to all (63) and to one (64) as
   `PLO_RC_ADMINMESSAGE`, and the disconnect (61), which kicks through the
   world task so the RCs see the leave. **Slice 4 built**
   (`src/rcaccounts.rs`, `tests/rc_accounts.rs`): account list (with `%`
   patterns and `adminlevel` conditions), get, set, add and delete; the
   dialog's e-mail, load only, admin level and admin worlds are kept in
   the account file and not acted on. **Slice 5 built** (`src/rcfiles.rs`,
   `tests/rc_files.rs`): the file browser (list 90, get 92, put 93 in a raw
   block) in the world folder within the account's `FOLDER` rights (`r` or
   `rw`, a folder and a mask; none sees nothing), plain files of at most
   60 KiB; move 96, delete 97 and rename 98 as seihai-rc sends them
   (not yet captured from a real RC). **Slice 6 built** (`rcaccounts.rs`): a player's staff rights
   (83/84 and 62: rights bits as `RCRIGHTS`, not acted on yet; the IP
   range as `IPRANGE`; folder rights as `FOLDER`, tokenized out) and
   comments (85/86 and 63, `COMMENT` lines). **Slice 7
   built** (`src/rcprops.rs`): a player's props by id (73) or account
   (74) as 72, from the account file, and a set (76) of an offline
   account's props and flags; a set on a player online is refused until
   it can go through the world. **The RC warp built** (82,
   `tests/rc_players.rs`): the player moves as on an NPC-server warp of
   stage 5, and the RCs get the new entry; a missing level is logged and
   the player stays. No real RC run yet; seihai-rc's lab notes still ask
   what a real client gets on a warp to another level. **The ban fields
   built** (`tests/rc_accounts.rs`): the account dialog's banned and
   reason, and the player ban (87 get as 64, 88 set), are the world's
   `config/bans.txt`; an RC ban has no end and names the RC's account as
   the admin, a ban set on a player online disconnects them, and their
   next login meets the ban. **Props of a player online built**
   (`tests/rc_accounts.rs`): a get shows what the world holds over the
   file, and a set goes to the player at once (as the NPC-server's
   `SET_PLAYER_PROPS`), replaces the world's flags and writes the file.
   Next: server options once a lab has their layouts, then NC.
5. NPC-server link for `seihai-npcserver`. **Built from source, lab run
   open** (`src/npclink.rs`, `src/world.rs`, `PROTOCOL.md`,
   `tests/npc_link.rs`, `tests/npc_messages.rs`,
   `docs/lab/npclink-2026-10-31.md`): `npcserver = off | external | auto`, the
   token handshake, level list, keep-alive; the world task feeds the `Hub`
   (player join, props, `PLAYER_LEVEL`, leave, `LEVEL_RELOADED` from a 1 s
   check of the loaded level files, the server flags for a new link) and
   applies `0x40` to `0x4C` (`world.rs` module notes say what each becomes).
   Dropped with a log line: the sound messages (no build reads a server
   sound) and `NPC_TRIGGER_ACTION` (`PLO_TRIGGERACTION` 48 needs a capture).
   Left: the lab run (`npcserver = auto` with seihai-npcserver's
   `examples/world`, real 2.05 rev 2 and 2.22), and `PM_TO_NPC` /
   `TRIGGER_ACTION`, which wait for the client messages they come from.
6. 3.x to 6.x: the Login lobby and server warp, GS2 bytecode delivery, and later our
   own GS2 compiler (gs2-parser is GPL, so it cannot be used).
7. Server-side anti-cheat (see "Milestone: server-side anti-cheat" below). Each
   check lands with the feature it guards (movement and warps with stage 2,
   items, hearts and gralats with stages 2 and 3), behind one policy layer that
   admins configure. No client-side anti-debugging.

## Out of band: Graal3D (February 2000)

Not a stage of this binary. Graal3D (`GR3DV002`) is a separate protocol
(3D floats, renumbered ids, peer UDP, inline `.x` level text, no board /
NPC / WANTFILE game). Hosting it is **G3D-9** in Seihai's
`documentation/plannedmilestones.md`: a sibling Rust host next to
`seihai-g3d`, ported from Graal_analysis `graal3d/reference-server/`. Do not
fold it into stages 1–7. Worlds are DirectX `.x` + local BMPs, not `.nw` /
`.graal`. The Torque 3D path of 3.x–5.x clients is also separate (held as
Ato Stage 4 until a capture exists).

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

Anti-cheat is off by default (Squirrel, issue #54, 2026-11-12): a check can
only guess, and a wrong guess hurts honest players more than it stops a
cheat. Turned on, every check only logs until the admin picks `correct` or
a kick for it. Bans are moderation, not anti-cheat, and work either way.

Landed in stage 2.9 (`src/anticheat.rs`): the policy layer (`off`, `log`,
`correct`, `kick after N in S`; settings `anticheat`, `anticheat_log`,
`anticheat_rate`, `anticheat.speed|teleport|warp|rate`), the log, and the
speed, teleport, warp and message-rate checks. Defaults: speed `log`, the
rest `correct`, no kicks. Item, heart and gralat checks and bans are next.
The speed limits (24 tiles a second against a 10-tile walk, a 30-tile
budget for steps read in a backlog, 16 tiles for a teleport) are a first
guess for the lab run to tune. After a warp the client may send the spot it
left once more; that position is dropped, not counted.

Landed next (issue #54): the items check (`anticheat.items`, Kind `items`)
on the counted props (max hearts, hearts, gralats, arrows, bombs): a raise
the server did not give (floor item taken first, a pre-1.38 build's own
take, a chest, the NPC-server, hearts back to the max after a death) is
logged, and with `correct` put back. Its default is `log`, not `correct`:
classic level scripts run on the client and can pay gralats the server
never sees. Bans by account in `config/bans.txt` (account, end time,
admin, reason), read at every login (game and RC), with `seihai-gserver
ban` / `unban`. Then the hurt check (`anticheat.hurt`): a
`PLI_HURTPLAYER` from more than 24 tiles from the victim. #50 showed that
real 2.05 and 2.22 clients do not send 26 for a sword: the victim's client
hurts itself from the attacker's `sword` gani, so there is no hit message
to check a weapon against. Everything is off by default since then. Still
open, and only worth it if an admin asks: bans by machine field and the
speed figures from a lab run. A player saved dead logs in at 0 hearts, as beta4 does (issue
#51, lab run 2026-11-12); `revive_dead = true` gives full hearts instead.

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
  prop parser takes it. The board is generated (one tile) until stage 2 loads levels
  (done in 2.2: the start level comes from `levels/`, with its links, signs and
  chests; the generated board is only for a missing file).
- **Idle session.** `WANTFILE` gets `FILESENDFAILED`, `UPDATEFILE` gets
  `FILEUPTODATE` (or `FILESENDFAILED` before 2.1); 1.21 reads neither and gets none.
  (Since 2.7 the world's files are served: `src/files.rs`.)
  Language, packet count (31), weapon upload (25), tamper check, process list,
  show-image and enter-level requests are read and not answered yet (warp
  requests are answered since 2.4, neighbour requests since 2.5, flags and
  chests since 2.6). The world time (42) goes out every 5 s to builds that read it; an
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

- [x] Switch `seihai-core` from the PR rev to a tag (now `v0.0.5`, below).
- Seihai only: real clients cannot be changed, so their logins stay in the
  clear (Squirrel, 2026-10-31).
- [x] The listserver link is encrypted (Squirrel, 2026-10-31: "encrypt
      it"): `src/listlink.rs`, `listserver_secure` on by default,
      `tests/listserver_link.rs`, lab run in
      `docs/lab/listserver-link-2026-10-31.md`.
- [x] `seihai-core` is on tag `v0.0.6`, and `seihai-npcserver` `v0.1.1` uses the same tag, so only one copy is built.
- [x] Listserver link: answer the listserver's ping (99) with an empty
      `SVO_PING` (16), and do not send 16 on its own. `src/listlink.rs`,
      `tests/listserver_link.rs`. No real client: the packet is only
      between this gserver and a listserver.
- [x] Listserver link: `PLYRADD` sends the nickname and props after
      the id and type (account, nickname, level, x, y, and alignment,
      player prop 32, from the account's `ALIGN`, default 0).
      Private messages are `SVO_PMPLAYER` / `SVI_PMPLAYER` (29). File
      start is `SVI_FILESTART` (2) and `SVI_FILESTART3` (15), then
      `SVI_FILEDATA3` (16) and `SVI_FILEEND3` (17). `src/listlink.rs`,
      `tests/listserver_link.rs`. No real client: these packets are only
      between this gserver and a listserver. `VERIACC2`'s type byte is
      the login's account type (`login.rs`: 0 for 1.21 to 2.17, 5 for
      2.22 and later). RC logins send 1 or 6 (`rc.rs`); NC (3) will send
      its own once it is served.
- [x] RC logins (`rc.rs`, type 1/6): the same handshake
      (`tests/rc_login.rs`, with Seihai's own handshake code standing in
      for `seihai-rc`).
- [ ] A `key-fingerprint` subcommand that prints the key without starting.
