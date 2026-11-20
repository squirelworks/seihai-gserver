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

- the server registers at HQ level 4 (Graal3D), so the listserver tags its
  name `3 `, which 3D clients look for. A leading `3 ` in `name` is taken
  off, since the tag carries it. Mac 3.153 reads only the first tag: with the
  default bronze level the list said `H 3 name` and it did not see a 3D
  server;
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
camera <x> <y> <z> [<axis x> <axis y> <axis z> <angle>]
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

A `.3ds` shape needs a material. One with only vertices and faces loads
without an error, but 5.12 and 4.20r6 draw none of it (lab 2026-11-20).

`weapon3d` files are relative to the world folder and are sent as script
source over the text channel.

## What each protocol gets

| Torque protocol | Clients | Mission start | Objects | Control |
|---|---|---|---|---|
| 24 | 4.20r8, 5.x, 6.0x | phases 1 and 2 | GameObject, Sun, Sky, Camera | the player's own GameObject |
| 21 | 4.20r6 | phases 1 and 2 | as 24, but every Sky padded to 7 texture names, and a plain Sky when the world has none (as 19) | as 24 |
| 19 | 3.153, 4.04r2 | phases 1 to 3, datablocks, ghost-always | TGE Camera, Player, Sky | Player (with `tgeplayershape`) or Camera |

All protocols: the challenge and connect handshake, the notify link (acks,
keep-alives, 30 s timeout), guaranteed and unguaranteed events, ghost
creates, updates and deletes with lost creates resent, and the text channel
both ways (flags from the TCP world, `clientr.g3dpos`, weapons, and the
client's `s`/`w`/`t` lines, which go to the world as flags, weapons and
trigger actions). Protocol 19 moves are read and applied to the Player, and
each move is acked.

## The camera (protocols 21 and 24)

Every protocol 21/24 client gets a Camera ghost of its own,
`G3DCamera_<account>`, named as its camera once the client has its create.
The 5.x client draws its 3D view only through a Camera: with just its control
object the 3D window stays black. It sits 0 -5 2 from the spawn point unless
the world file has a `camera` line, and it faces the spawn point unless that
line gives a rotation.

Rotations (objects and the camera) are an axis and an angle in radians, and
the clients turn the opposite way from the right-hand rule: a positive angle
about +x tilts the view down, a positive angle about +z turns it right
(forward, +y, swings toward +x). The default camera for `spawn 0 0 1` is the
same as `camera 0 -5 2 1 0 0 0.38`.

## Not done yet

- `weapon3d` scripts are sent as source, not compiled to bytecode, and the
  name-only form is not read.
- Server flags (`server.`) are not sent.
- The `graal3dstringprefix = false` mode is not ported.
- bzip2 text channel bodies are not decoded.
- No TerrainBlock, and no movetype on GameObjects.
- 3.153 has been seen to crash on the first frame in the C++ fork; the cause
  is unknown.
- The Linux 4.20r6 (`G3D22126`) links as protocol 24, so the protocol 21
  path has not been run; no Windows 4.20r6 was found.
- 3.151, 3.153 and 4.20r8 have not been run against this server.

## Lab runs

2026-11-19, in a headless VM:

| Client | Result |
|---|---|
| 4.04r2, native Linux (protocol 19) | 3 of 3 runs: linked, datablocks loaded, Sky and Player ghosts, mission start done, control object delivered. The client logged "Initial Control Object" and "Graal3D has been activated!". The link held 96 s. |
| 4.04r2, Windows under Wine | Crashes while starting 3D, before sending anything (a Wine limit). |
| 5.12, Windows under Wine (protocol 24) | Linked, assets downloaded, ghosts and mission start done, control object delivered, link held 97 s. Stays on "Starting 3D mode...", the same as the C++ fork. In 2 of 5 runs the client never opened TCP to the 3D server after the lobby warp (a client-side race; holding the lobby link open 20 s avoided it for 4.x). |

2026-11-20, the same world (no `camera` line), real 5.12 (protocol 24):

| Server | Where | Result |
|---|---|---|
| main before this change | Windows 10 VM, 2 runs | Logged in, control object delivered, the "Lab 3D" window black. No "Starting 3D mode..." box. |
| main with `camera 0 -5 2` added to the world | Windows 10 VM | Sky and fog drawn. |
| with the default Camera | Windows 10 VM, 4 runs | Sky and fog drawn every time. Camera ghost in seq 2, control object delivered in seq 3. |
| with the default Camera | Wine (Debian VM) | 3D view drawn, the Wine box still on top. |

2026-11-20, the default Camera facing the spawn (`spawn 0 0 1`, objects at
the origin), loopback only:

| Client | Shape | Result |
|---|---|---|
| 5.12, Windows 10 VM | `labcube.3ds` (no material) | Sky and fog, no cube from any camera, also with the sky behind it. |
| 5.12, Windows 10 VM | the same cube with one red material | Cube drawn and lit by the sun. |
| 5.12, Windows 10 VM, right-handed facing angle (first try) | red cube | View tilted up into the sky box, no cube. |
| 5.12, Windows 10 VM, angle negated (this change) | red cube | Cube centred, seen from above. |
| 4.20r6 Linux (`G3D22126`), native in a Debian VM | red cube | Logged in, linked, cube centred. With `labcube.3ds` no cube. |
| 4.20r6 Mac PPC (`G3D27076`, protocol 21), Tiger VM (QEMU, software renderer), main at 368c3d6, no `sky` line | red cube | Logged in, linked as protocol 21, 4 ghosts, shape downloaded, mission start done, control object taken, then a crash on the first 3D frame (3 of 3 runs). Mac 3.153 crashed the same way on protocol 19. |
| the same, with a six-name `sky` line | red cube | The same crash. |
| the same, with a seven-name `sky` line | red cube | No crash: the 3D view is drawn through the default Camera and the link holds. |
| lab build of this change (protocol 21 gets protocol 19's Skies), no `sky` line | red cube | A plain Sky is sent, "Graal3D has been activated!", no crash, the link holds until quit. One harmless "Could not locate texture: g3d_sky". |

The 4.20r6 Mac reads the scene's Sky, and that Sky's seventh texture name,
on its first 3D frame without checking either, as 3.153 and 4.04r2 do. The
camera, the control object and the ghost order needed no change. The cube
itself could not be checked there: the Tiger VM has only Apple's software
renderer, which drew the sky above the horizon and one flat colour below it
in every run, with or without the cube. That needs a Mac with real OpenGL.

What the runs and breakpoints showed (hardware breakpoints through the VM's
debugger; a debugger inside the guest makes the client exit):

- The "Starting 3D mode..." box belongs to the launcher, `Graal.exe`, not to
  the Torque engine. The launcher shows it while it loads `Graal3DEngine.dll`
  and hides it right after. No packet decides this. Under Wine the hidden
  window stays on screen; on Windows 10 it goes away.
- The 5.12 login finished on the old build too: the client took the control
  object (login step 5) and ran the step 6 switch to the 3D GUI with no file
  download pending. Its `console.log` never prints the step 5 line.
- The Torque "Connecting..." window is another one. `PLO_SERVERLISTCONNECTED`
  (190) closes it, and the login burst already sends it.
- The 2026-11-12 C++ fork run that drew the view had a `camera` line. The
  2026-11-19 world did not.

The log counts moves: `first move received`, then every hundredth, and the
total when the link closes.

## Code

`src/graal3d/`: `bits`, `huffman`, `link` and `events` are the Torque wire
layer (shared in spirit with Ato; moving it into `seihai-core` is a
follow-up), `handshake` holds the protocol tables, `objects` and `tge` the
ghost and datablock layouts, `conn` one client's link, `scene` the world
file, and `service` the UDP socket and its tie to the TCP server.
