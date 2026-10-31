# The gserver to NPC-server link (link version 1)

This is the protocol between `seihai-gserver` and `seihai-npcserver`. The same
file lives in both repos; change it in both, in the same week, and bump the link
version when a change is not backward compatible.

The original 2001 NPC-server's message ids are not known, so this link is our
own. It borrows the gen-5 frame layout (length, compression byte, body) so both
programs can share framing code with the client protocol, and adds its own
message ids and a length on every message so either side can skip a message it
does not know.

Status: milestone 1 (handshake, level list, player props, keep-alive) is
specified and implemented in `seihai-npcserver`. Message ids marked *reserved*
are planned, not specified; do not send them yet.

## Roles and transport

- The **gserver listens**, the **NPC-server connects**. TCP.
- Default address: `127.0.0.1:14810`. The gserver binds to `127.0.0.1` unless its
  config says otherwise. The link is not encrypted, so an NPC-server on another
  machine should come in over an SSH tunnel or a VPN, not an open port.
- One NPC-server per gserver. If a second one authenticates, the gserver keeps
  the first and rejects the second with `REJECT "already connected"`.

## Frames

Each frame is:

| Field | Size | Notes |
|---|---|---|
| length | `u16be`, or `FF FF` + `u32be` | Length of everything after it. The long form is used when the length is `0xFFFF` or more, as in 6.x client frames. |
| compression | `u8` | `0x02` none, `0x04` zlib, `0x06` bz2 (the gen-5 values) |
| body | rest | The messages, compressed as the byte says |

There is **no XOR cipher** on the link. Senders choose the compression; a good
rule is none under 48 bytes, zlib above. Receivers must accept all three.
A frame may hold any number of whole messages; a message never spans frames.
A frame or decompressed body over 16 MiB is an error and closes the link.

## Messages

Every message is:

| Field | Size |
|---|---|
| id | `u8` |
| length | `u32be`, the length of `data` |
| data | `length` bytes |

A receiver skips a message whose id it does not know (it may log it once). A
message whose data is shorter than its fields need is a protocol error and
closes the link. Extra bytes after the known fields are ignored, so a later
version may append fields to a message without a new id.

Field types used below:

| Type | Encoding |
|---|---|
| `u8`, `u16`, `u32` | big-endian |
| `str` | `u16be` length + bytes. Account and level names are bytes as Graal stores them (usually ASCII or Latin-1), not guaranteed UTF-8. |
| `blob` | `u32be` length + bytes |

## Connection sequence

```
NPC-server                         gserver
    | ---- HELLO(version, token) ---> |
    | <--- WELCOME / REJECT --------- |   REJECT: gserver closes
    | <--- LEVEL_LIST --------------- |
    | <--- PLAYER_JOINED x N -------- |   one per player online now
    | <--- live updates ------------- |   PLAYER_JOINED / PLAYER_PROPS / PLAYER_LEFT / LEVEL_RELOADED
    | <--> PING / PONG -------------- |
    | ---- BYE (optional) ----------> |
```

- The NPC-server sends `HELLO` first. The gserver closes a connection that has
  not sent a valid `HELLO` within 10 seconds.
- After `WELCOME` the gserver sends the full `LEVEL_LIST`, then a
  `PLAYER_JOINED` for every player already online, then live updates.
- Keep-alive: a side that has sent nothing for 30 seconds sends `PING`. A side
  that has received nothing for 90 seconds closes the link.

## Message ids

### 0x00 to 0x0F: control (both directions)

| Id | Name | Direction | Fields |
|---|---|---|---|
| `0x01` | `HELLO` | NPC → GS | `link_version: u16`, `token: str`, `program: str` |
| `0x02` | `WELCOME` | GS → NPC | `link_version: u16`, `server_name: str`, `program: str` |
| `0x03` | `REJECT` | GS → NPC | `reason: str` |
| `0x04` | `PING` | both | `nonce: u32` |
| `0x05` | `PONG` | both | `nonce: u32`, the nonce of the `PING` it answers |
| `0x06` | `BYE` | both | `reason: str`. The sender closes after it. |

- `link_version` is `1` for this document. The gserver answers a version it
  cannot speak with `REJECT "link version N not supported"`. `WELCOME` carries
  the version the gserver will speak, which is never higher than the one asked.
- `program` is free text for logs, such as `seihai-npcserver 0.1.0`.
- `token` is the shared secret (see "Authentication"). The gserver compares it
  in constant time and answers a wrong one with `REJECT "bad token"`. It never
  logs the token.

### 0x10 to 0x1F: levels (GS → NPC)

| Id | Name | Fields |
|---|---|---|
| `0x10` | `LEVEL_LIST` | `count: u16`, then `count` × `name: str`. The whole list; it replaces any earlier one. |
| `0x11` | `LEVEL_RELOADED` | `name: str`. The level file changed on disk and the gserver reloaded it. |
| `0x12` to `0x1F` | *reserved* | Level temporary state: bombs, explosions, bush removal, dropped items |

Level names are file names relative to the world's `levels/` folder, with
`/` as the separator, for example `onlinestartlocal.nw` or `gmaps/house1.nw`.

### 0x20 to 0x2F: players (GS → NPC)

| Id | Name | Fields |
|---|---|---|
| `0x20` | `PLAYER_JOINED` | `player_id: u16`, `account: str`, `level: str`, `props: blob` |
| `0x21` | `PLAYER_PROPS` | `player_id: u16`, `props: blob` |
| `0x22` | `PLAYER_LEFT` | `player_id: u16` |
| `0x23` | `PLAYER_LEVEL` | `player_id: u16`, `level: str`. The player entered another level. |
| `0x24` to `0x2F` | *reserved* | PMs to NPCs, `server.` flag add and remove, game time |

`player_id` is the id the gserver gives the player's connection, the same one
it puts in `PLO_OTHERPLPROPS`. It is unique while the player is online and may
be reused after `PLAYER_LEFT`.

`props` is a Graal player prop stream: `(GCHAR id, value)*`, the same bytes the
gserver would send a 2.22 client after the player id in `PLO_OTHERPLPROPS`
(prop 10 in the 2.1 GANI form). `PLAYER_JOINED` carries every prop the gserver
knows; `PLAYER_PROPS` carries only the props that changed. An NPC-server reads
props until the first id it does not know and keeps what it has, as clients do.

### 0x40 to 0x7F: NPC-server to gserver (*reserved*, milestone 2)

Planned: NPC props, player props changed by scripts, PM replies, weapon grants
(`addweapon`, `toweapons`), warps (`setlevel2`), `server.` flag changes.

#### Draft: milestone 2 (not final, do not send yet)

A starting point for milestone 2. Change it freely until it is implemented on
both sides; then move it into the tables above and drop "draft".

| Id | Name | Direction | Fields |
|---|---|---|---|
| `0x40` | `NPC_ADDED` | NPC → GS | `npc_id: u32`, `level: str`, `props: blob` |
| `0x41` | `NPC_PROPS` | NPC → GS | `npc_id: u32`, `props: blob`, only what changed |
| `0x42` | `NPC_DELETED` | NPC → GS | `npc_id: u32` |
| `0x43` | `SET_PLAYER_PROPS` | NPC → GS | `player_id: u16`, `props: blob` |
| `0x44` | `WARP_PLAYER` | NPC → GS | `player_id: u16`, `level: str`, `x: u16`, `y: u16` (half tiles) |
| `0x45` | `ADD_WEAPON` | NPC → GS | `player_id: u16`, `weapon: str` |
| `0x46` | `SERVER_FLAG` | NPC → GS | `name: str`, `set: u8` (1 set, 0 unset), `value: str` |
| `0x47` | `PM` | NPC → GS | `player_id: u16`, `text: str` |
| `0x24` | `PM_TO_NPC` | GS → NPC | `player_id: u16`, `text: str` |
| `0x25` | `SERVER_FLAGS` | GS → NPC | `count: u16`, then `count` × `name=value: str`; the whole set, sent after `LEVEL_LIST` |
| `0x26` | `TRIGGER_ACTION` | GS → NPC | `player_id: u16`, `level: str`, `x: u16`, `y: u16`, `action: str` |

`npc_id` is the NPC-server's id for a server-side NPC, unique while the link is
up. NPC prop streams use the same encoding the gserver sends a 2.22 client in
`PLO_NPCPROPS` after the NPC id. The gserver relays `NPC_ADDED`/`NPC_PROPS` to
the players in that level and answers `SET_PLAYER_PROPS` with the usual
`PLAYER_PROPS` once it has applied them.

### 0x80 to 0xFF: NPC Control (*reserved*)

Planned: the NPC list, classes and weapons for `seihai-rc`, relayed by the
gserver.

## Authentication

The token is a shared secret of 16 to 256 bytes. Neither program ever puts it
on a command line (other users can read command lines) or in a log.

- **`npcserver = auto` (the easy setup).** The gserver makes a new random token
  each time it starts, starts the NPC-server itself and hands the token over in
  the environment. Nobody configures the link.
- **Manual (NPC-server started by hand or on another machine).** Put the same
  token in the gserver's config (`npcserver_token`) and in a file the
  NPC-server reads with `--token-file`.

## Starting the NPC-server from the gserver (`npcserver = auto`)

With `npcserver = auto` the gserver:

1. listens on the link port on `127.0.0.1`,
2. makes a random token (32 bytes, hex),
3. starts the `seihai-npcserver` binary that sits next to its own binary (or
   the path in `npcserver_path`) with:

   ```
   seihai-npcserver --gserver 127.0.0.1:<link port> --world <world folder> --once
   ```

   and the environment variable `SEIHAI_NPCSERVER_TOKEN=<token>`,
4. restarts it, with a growing delay up to 30 seconds, if it exits while the
   gserver is running, and stops it when the gserver stops (it sends `BYE` and
   closes the link; `--once` makes the NPC-server exit when the link closes).

`npcserver = off` (the default until milestone 2 ships) turns the link off, and
`npcserver = external` listens for an NPC-server that someone starts by hand.

## Changes

| Link version | Date | Change |
|---|---|---|
| 1 | 2026-10-31 | First version: control, levels, players (milestone 1) |
