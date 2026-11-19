# The 3D mode (Torque UDP)

Graal 3.153 to 6.x clients have a 3D mode built on Torque networking. The
client logs in over TCP as usual, then opens a UDP link to the same host on
another port and plays a Torque mission there. This server can host that link.
It is off by default.

The wire facts come from Graal_analysis `torque/udp-session.md` and
`torque/protocol19-server.md`, the Seihai client's Ato crate, and Seihai's own
C++ gserver fork (used as a behaviour reference and for test vectors).

## Turning it on

In `config/server.txt`:

```text
graal3d = true
graal3d_port = 14901   # optional; the default is the TCP port + 1
```

With it on:

- the listing name starts with `3 `, which 3D clients look for;
- 3D-capable clients (3.153, 4.04r2, 4.20r6, 4.20r8, 5.x, 6.0x) get
  `PLO_SERVERLISTCONNECTED`, an answer to every file request (missing files
  are refused instead of left unanswered), and texture lookups without an
  extension (`.png`, `.jpg`, `.jpeg`, `.bmp`, `.gif`, `.dds` are tried);
- a UDP connect request is held until the same account (or a guest) from the
  same IP is logged in over TCP, and the link is closed with a disconnect
  when that TCP session ends.

## The world file

`config/graal3dworld.txt` says what every 3D client is sent. One entry per
line, `#` starts a comment. Lines not understood are printed at start.

```text
object <name> <shape> <x> <y> <z> [<axis x> <axis y> <axis z> <angle>]
sun <dir x> <dir y> <dir z> <r> <g> <b> <a> <ambient r> <g> <b> <a>
sky <textures> [<visible distance> [<fog r> <g> <b>]]
skyoptions <b1> <b2> <b3> <solid r> <g> <b>
playershape <shape>
spawn <x> <y> <z>
camera <x> <y> <z>
tgecontrol player|camera
tgeplayershape <file.dts>
weapon3d <name> <file>
```

A sample:

```text
sun 0.57735 0.57735 -0.57735 0.8 0.8 0.8 1 0.3 0.3 0.35 1
sky sky_fr,sky_rt,sky_bk,sky_lf,sky_up,sky_dn 500
object house house.dif 4 4 0
playershape labcube.3ds
spawn 0 0 1
```

`weapon3d` files are relative to the world folder and are sent as script
source over the text channel.

## What each protocol gets

| Torque protocol | Clients | Mission start | Objects | Control |
|---|---|---|---|---|
| 24 | 4.20r8, 5.x, 6.0x | phases 1 and 2 | GameObject, Sun, Sky, Camera | the player's own GameObject |
| 21 | 4.20r6 | phases 1 and 2 | as 24 | as 24 |
| 19 | 3.153, 4.04r2 | phases 1 to 3, datablocks, ghost-always | TGE Camera, Player, Sky | Player (with `tgeplayershape`) or Camera |

All protocols: the challenge and connect handshake, the notify link (acks,
keep-alives, 30 s timeout), guaranteed and unguaranteed events, ghost
creates, updates and deletes with lost creates resent, and the text channel
both ways (flags from the TCP world, `clientr.g3dpos`, weapons, and the
client's `s`/`w`/`t` lines, which go to the world as flags, weapons and
trigger actions). Protocol 19 moves are read and applied to the Player, and
each move is acked.

## Not done yet

- `weapon3d` scripts are sent as source, not compiled to bytecode, and the
  name-only form is not read.
- Server flags (`server.`) are not sent.
- The `graal3dstringprefix = false` mode is not ported.
- bzip2 text channel bodies are not decoded.
- No TerrainBlock, and no movetype on GameObjects.
- 3.153 has been seen to crash on the first frame in the C++ fork; the cause
  is unknown.
- No lab run with a real client yet; checked against recorded 5.12 packets
  and a loopback test (`tests/graal3d.rs`).

## Code

`src/graal3d/`: `bits`, `huffman`, `link` and `events` are the Torque wire
layer (shared in spirit with Ato; moving it into `seihai-core` is a
follow-up), `handshake` holds the protocol tables, `objects` and `tge` the
ghost and datablock layouts, `conn` one client's link, `scene` the world
file, and `service` the UDP socket and its tie to the TCP server.
