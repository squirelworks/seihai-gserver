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
graal3d_port = 14901        # optional; the default is the TCP port + 1
graal3d_shapecheck = skip   # optional; or warn, or off (see "Start-up check")
graal3d_pushterrain = false # optional; see "Each build's row"
graal3d_edit = off          # optional; staff: in-game world editing (see "Editing a world in game")
```

Lab only (see "Lab options"):

```text
graal3d_terrainall = false  # the 5.x ground to every GameObject-era build
graal3d_trace = g3d.tsv     # a trace file; empty: off
graal3d_route = mission     # or sharp3d
graal3d_mutate = off        # or <kind>[:<seed>]
graal3d_stringprefix = true # false: game packet strings without the prefix flag
```

The clients open the UDP link on the listed TCP port + 1, so a set
`graal3d_port` must be that. A 5.12 enters 3D through a warp only when the
server's name carries the `3 ` tag (it uses `name` as given), so a lab world
without a listserver names itself e.g. `3 Seihai Lab`.

With it on:

- the server registers at HQ level 4 (Graal3D), so the listserver tags its
  name `3 `, which 3D clients look for. A leading `3 ` in `name` is taken
  off, since the tag carries it. Mac 3.153 reads only the first tag: with the
  default bronze level the list said `H 3 name` and it did not see a 3D
  server;
- 3D-capable clients (3.153, 4.04r2, 4.20r6, 4.20r8, 5.x, 6.0x) get
  `PLO_SERVERLISTCONNECTED`, an answer to every file request (missing files
  are refused instead of left unanswered), and image lookups that try other
  extensions: a bare name, or a name with the wrong image extension, also
  finds the same stem as `.png`, `.jpg`, `.jpeg`, `.gif`, `.bmp`, `.tga` or
  `.dds`, in that order, for file requests and "is my copy current?" checks
  alike. The 5.x clients ask for Sky and terrain textures with no extension;
  3.153 asks for Sky textures as `.gif` (lab, 2026-11-25), so a `.png`
  answers it;
- a UDP connect request is held until the same account (or a guest) from the
  same IP is logged in over TCP, and the link is closed with a disconnect
  when that TCP session ends.
- TCP logins on the 6.03r7 token (Windows and Linux) may be linked too.
  Stock 6.03r7 is a 2D client and opens no UDP link (Graal_analysis T3D-121),
  so nothing changes for it; graal-client-repro's Frankenstein 6.037 logs in
  with that token and opens the 5.12 protocol-24 link beside it. Its first
  lab run (2026-11-26) reached login step 6 only with this.

## The world file

`config/graal3dworld.txt` says what every 3D client is sent. One entry per
line, `#` starts a comment. Lines not understood are printed at start.

```text
object <name> <shape> <x> <y> <z> [<axis x> <axis y> <axis z> <angle>]
sun <dir x> <dir y> <dir z> <r> <g> <b> <a> <ambient r> <g> <b> <a>
sky <textures> [<visible distance> [<fog r> <g> <b>]]
skyoptions <b1> <b2> <b3> <solid r> <g> <b>
playershape <shape>
movetype interpolate|rigid|list
spawn <x> <y> <z>
camera <x> <y> <z> [<axis x> <axis y> <axis z> <angle>]
tgecontrol player|camera
tgeplayershape <file.dts>
weapon3d <name> <file>
terrain <folder> [<square size>] [repeat]
water <x> <y> <z> <sx> <sy> <sz> [<surface texture> [<liquid type>]]
replicator <class> <x> <y> <z> <radius> <min> <max> <shapes> [<seed> [<x0> <y0> <x1> <y1>]]
objectfor <accounts> <name> <shape> <x> <y> <z> [...as object]
```

A sample:

```text
sun 0.57735 0.57735 -0.57735 0.8 0.8 0.8 1 0.3 0.3 0.35 1
sky sky_fr,sky_rt,sky_bk,sky_lf,sky_up,sky_dn 500
object house house.dif 4 4 0
playershape labcube.3ds
spawn 0 0 1
```

`movetype` gives every player's own GameObject a movetype (section 6). With
`interpolate` or `rigid`, a server move (a `g3dpos` trigger) goes out as
movement data (section 7: the position, the rotation as a quaternion, zero
velocities) in place of a transform, and 5.12 glides its object there; with
`list` the object goes out with an empty list and the transform still
carries the position, so the client jumps. Without the line no movetype is
sent, as before. This follows Graal_analysis
`protocol/lab-appe-hosta-2026-11-20d.md` (E#505), where a lab copy of this
server did the same on the real 5.12's control object; how other players'
clients show a player object with a movetype has not been seen yet.

A `.3ds` shape needs a material. One with only vertices and faces loads
without an error, but 5.12 and 4.20r6 draw none of it (lab 2026-11-20).

`weapon3d` files are relative to the world folder and are sent as script
source over the text channel.

## Terrain, water and ground cover (protocols 21 and 24)

These three go to 4.20r6, 4.20r8 and 5.x only. Protocol 19 (3.153, 4.04r2)
has stock TGE terrain, a different layout, and gets none of them. The wire
layouts are from Graal_analysis `torque/terrain-512.md` and
`torque/replicators-512.md`.

**Terrain.** `terrain world/island` streams a 256 x 256 terrain centred on
the origin, 8 metres a square unless the line gives a square size (1 to 64;
a huge one killed a live 5.12). `repeat` tiles it past its edges. The folder
(from the world folder) holds `heights<x>-<y>.dat`, x and y 0 to 15 with no
padding: each one 16 x 16 squares as a zlib stream, at most 1023 bytes, the
same file the client keeps in its own cache. Missing chunks are left out, and
files that are too long or do not inflate are named at start-up. One terrain
a world: a later `terrain` line replaces the first.

Each client gets a `TerrainBlock` ghost. When it has the block it sends the
stamp of every chunk it already holds, and the server sends every chunk whose
stamp differs, so a returning client downloads only what changed. The client
stamps its own re-encoding of each chunk, so write chunks in its form
(`seihai-servertools gen terrain DIR --stream` does), or every chunk goes out
again on every visit.

Put `heights.info` (one texture name a line, in material order) and its
textures in the file folders. Name the textures with their extension
(`g3d_grass.png`): the Linux 4.20 client given bare names saved them without
one and then could not load them. Terrain textures should be 256 x 256 24-bit.
On a first visit 5.12 fetches only `heights.info` and draws the terrain white;
it fetches the textures on the next visit.

**Water.** `water -1024 -1024 -19.5 2048 2048 20 water_surface 0` is a
`WaterBlock` from that corner, unrotated, of that size. Its surface is at
z + the z size (0.5 here), not at z. The liquid types are stock TGE's (0
water, 1 ocean, 2 river, 3 stagnant, 4 lava, ...); everything else takes
stock TGE 1.x's defaults.

**Ground cover.** `replicator DynamicShapeReplicator 0 0 0 4 1 3 rock.dts` ghosts a
`DynamicShapeReplicator`, `DynamicGrass` or `DynamicGrassReplicator`: cover
scattered on a grid of 8-unit cells around the client's own control object
(not around the replicator). The radius is in cells (1 to 64), min and max
are objects a cell (0 to 20), and the shapes are one comma-separated list
with no spaces. `DynamicGrass` and `DynamicGrassReplicator` are different:
they build their own flat ground pieces from a **texture** named in that list
(`replicator DynamicGrass 0 0 0 8 20 40 blades.png`), draw nothing for a
`.3ds` or `.dts`, and take up to 100 a cell. A real 5.12 grows no grass at a radius under 8
cells, so grass lines go out with at least 8. The start-up check reports a
grass line with a smaller radius or one that names a shape. A mostly transparent grass texture draws next to
nothing. The optional box `x0 y0 x1 y1` is the world x/y region a
cell's corner must lie in; without one, cover may grow anywhere. Cover is
placed by casting rays, so it needs real ground (streamed terrain or solid
objects). `scale=<n>` or `scale=<min>,<max>` anywhere after the class sizes
the copies (default 1; for the grass classes 3, since a grass quad at 1 is
only about 0.6 units). The rest is fixed: any turn about z, terrain only,
slopes up to 90 degrees, shown.

A 5.12 grows cover only from the shapes and ground it has when it reads the
replicator, and grows it again on each update. On a first visit those files
arrive later, so the server sends each replicator's whole update again at
the end of the mission start and 5, 15 and 30 seconds after it (and after a
world switch). On a real 5.12 (lab, 2026-11-26) a `DynamicShapeReplicator`
scattered its shape over streamed terrain; `DynamicGrass` and
`DynamicGrassReplicator` drew nothing with a `.3ds` shape, as expected; with
a texture they draw (Graal_analysis `torque/replicators-512.md`).

## Start-up check

At start-up the server looks up every shape and texture the world file names
(object and player shapes, replicator shapes, Sky and water textures, and with
a `terrain` line `heights.info` and every texture it lists) and logs the ones
the file folders do not have. Then it checks them for the faults that crashed
or blanked a real client (Graal_analysis
`torque/3d-fuzz-content-512-2026-11-25.md`):

- a `.3ds` must hold at least one material (with none, 5.12 and 4.20r6 draw
  nothing), every chunk must fit in its parent, every vertex and face list
  must hold what it claims, and every face must use a vertex of its mesh. A
  cut file, a face count past the list and a face index past the mesh each
  killed 5.12;
- a `.dts` must be version 24, the only one 5.12 drew; the memory buffer its
  header claims must fit the file, its section starts must lie inside that
  buffer, and its node, object, decal and sub-shape counts must fit it. A cut
  file or a huge size word killed 5.12, and a bad login player shape killed
  4.04r2 at login;
- a PNG must claim 1 to 8192 pixels a side (30000 x 30000 killed 5.12). A
  16-bit PNG, an image over 1024 a side, BMP, TGA, DDS and empty files get a
  warning: 5.12 draws them white or not at all. Terrain textures other than
  256 x 256 24-bit get a warning too.

With `graal3d_shapecheck = skip` (the default) an object with a bad shape is
left out of the world, a bad `playershape` is not used, and a bad
`tgeplayershape` is not used either, so protocol 19 gets a Camera. `warn`
logs and sends them anyway, for labs that send broken shapes on purpose;
`off` checks nothing. A bad texture cannot be left out of the world; under
`skip` it is logged as an error and refused when a 3D client asks for it (the
client is told the server has no such file, which it survives: an oversized
terrain PNG fetched by name crashed 5.12). The same goes for a bad shape.

The check also says when `heights.info` lists a name with a space or comma (the
client reads one name per line), when the `heights.info` the clients get is not
the one in the terrain's folder, and when the world has a second `terrain`
line (one TerrainBlock a world; the later line wins).

## Several players

The first player stands on the world's `spawn`. Each later one takes the
first point no linked player stands on, of 8 points 2 units out, then 16 at 4
units, and so on, so two players never start on one point.

A `g3dpos` trigger moves the player's own object on every screen. The client
goes on reporting where it was for a while; those reports are set aside until
it reports somewhere else (and for two seconds whatever it reports).

When a player leaves, its object is deleted on every other client, and the
log says on how many.

A 3D client draws only the objects it is ghosted, so a 2D (TCP only) player in
a 3D player's level is ghosted to it as `G2DPlayer_<account>`: a GameObject
with the world's `playershape` at the player's tile position, Torque (x, -y),
at the spawn's height, with its nickname and chat. The levels are checked 5
times a second; the object moves and talks as the player does and goes when
the player leaves the level. Protocol 19 gets a stock Player in its place with
`tgeplayershape` (and `tgecontrol player`), else nothing.

Every 3D player is also an ordinary TCP player, and its TCP position follows
its 3D object: Torque (x, y) is map tile (x, -y), set as the player's own
classic `X`/`Y` props whenever it moves half a tile or more. So serverside
scripts and 2D players in its level see where it is. Positions off the 64 x 64
map are left alone.

A 5.12 in 3D mode runs no GS1 level NPCs or baddies, so a level led by a 3D
player stopped its 2D players' level NPC timeouts (lab, 2026-11-26). A 2D
player in the level leads it instead: when the leader links 3D, the first 2D
player there leads, and a 2D player coming into a level a 3D player leads
takes the lead. A 3D player leads only a level with no 2D player in it. The
5.12 shows a 2D player's nickname and chat to scripts (`G2DPlayer_<account>.nickname`
and `.chat`) but does not draw them.

## More worlds and level warps

`config/graal3dworld.txt` is the default world. Each
`config/graal3dworlds/<name>.txt` is another, in the same format. A 3D player
is in the world named after its 2D level (`cave.nw` or `levels/cave.nw` is
world `cave`, any case), else in the default one, so a warp on the TCP side
(a link, or a serverside `setlevel2`) takes its 3D client along. A client
script can also ask: `triggerserver(<kind>, <weapon>, "g3dworld", "cave")`, or
`"default"` to go back.

The switch needs no relink. The client is sent deletes for every ghost of the
old world (objects, Sky, terrain, other players), then the new world's, as on
a first link; its own object and Camera stay and move to the new `spawn`
(or the first free spot around it) and `camera`. Weapons of the new world's
`weapon3d` lines are sent (those the old world had too are left running), and the terrain files are pushed again with
`graal3d_pushterrain` (once per account and world). The client gets
`clientr.g3dworld` (`default` or the name) and a new `clientr.g3dpos`. Each
player sees only the players in its own world. Clients ask for
`heights.info` by its bare name, so all worlds with a `terrain` line share
one `heights.info`.

## Objects for some players only

`objectfor one,two door door.3ds 2 2 0` is an `object` line that only the
accounts `one` and `two` are sent. A client script can hide any world object
from its own player, and bring it back, by name:
`triggerserver(<kind>, <weapon>, "g3dhide", "door")` and `"g3dshow"`. The
object is deleted on, or sent again to, that client only. Hidden objects
come back on a relink or a world switch.

## Editing a world in game

The 5.12 client has Torque's `WorldEditor`, `TerrainEditor` and
`EditManager`, and a `weapon3d` can create them, but they work only on
objects the script made itself, `TerrainEditor` cannot attach to the
server's terrain, and nothing is saved: against a remote server their sync
does nothing (Graal_analysis `torque/3d-batch-w-rows-2026-11-26.md`,
T3D-126). So in-game editing goes through the server.

With `graal3d_edit = staff`, a client script of a `STAFF` account (not a
guest) sends:

```text
triggerserver("gui", <weapon>, "g3dedit", "move", <name>, x, y, z [, ax, ay, az, angle])
triggerserver("gui", <weapon>, "g3dedit", "add", <name>, <shape>, x, y, z [, ax, ay, az, angle])
triggerserver("gui", <weapon>, "g3dedit", "remove", <name>)
triggerserver("gui", <weapon>, "g3dedit", "list")
```

The edit applies to the world the player is in: every client in that world
sees the object move, appear or go at once (a removed object's later
objects are deleted and sent again under their new keys), and the world
file (`config/graal3dworld.txt` or `config/graal3dworlds/<name>.txt`) is
saved. Only the object's own line changes, keeping its comment, accounts
(`objectfor`) and scale; a new object goes after the last object line; the
old file is kept as `<file>.bak-<unix seconds>` and the new one written
through a temporary file and a rename. A move without a turn keeps the
object's turn. Names and shapes are one word without `#`, `,`, `:` or `"`;
a shape is a `.3ds`, `.dts` or `.dif`.

The client gets `clientr.g3dedit` back: `ok <op> <name>` or `error <why>`
(off, not staff, no such object, a name taken, a bad number). `list` also
sets `clientr.g3dedit_objects` to every object as
`name:shape:x:y:z:ax:ay:az:angle`, space separated. The server log names
the account, the edit and the backup. Everyone else's edits are refused,
and the option is off by default.

`docs/examples/g3deditor.gs2` is an editor weapon built on this: N and B
pick an object, the arrows and Page Up/Down move it, Q and E turn it, C
copies it, Delete removes it, and the camera follows the selected object.
Give it to staff as a `weapon3d` line (`weapon3d G3DEditor
weapons/g3deditor.gs2`, the file copied into the world folder). Terrain is
not edited in game yet; use `seihai-edit3d` from seihai-servertools on the
world folder for terrain, and for objects offline.

## What each protocol gets

| Torque protocol | Clients | Mission start | Objects | Control |
|---|---|---|---|---|
| 24 | 4.20r8, 5.x, 6.0x | phases 1 and 2 | GameObject, Sun, Sky, Camera | the player's own GameObject |
| 21 | 4.20r6 | phases 1 and 2 | as 24, but every Sky padded to 7 texture names, and a plain Sky when the world has none (as 19) | as 24 |
| 19 | 3.153, 4.04r2 | phases 1 to 3, datablocks, ghost-always | TGE Camera, Player, Sky | Player (with `tgeplayershape`) or Camera |

All protocols: the challenge and connect handshake, the notify link (acks,
keep-alives, 30 s timeout), guaranteed and unguaranteed events, ghost
creates, updates and deletes with lost creates resent, and the text channel
both ways (flags from the TCP world, `server.` flags included, though a
5.12 script reads only `serverr.` values (see "Server flags on a 5.12"),
`clientr.g3dpos`, weapons, and the client's `s`/`w`/`t` lines, which go to the world as flags, weapons and
trigger actions). Protocol 19 moves are read and applied to the Player, and
each move is acked. Every other linked player reaches a protocol 19 client
as a stock TGE Player, and a protocol 19 player's moves reach the others
(as a TGE Player on protocol 19, as its GameObject on protocols 21 and 24).

## Client scripts (`weapon3d`)

A `weapon3d` script goes to the client as GS2 bytecode, compiled at link
time by seihai-core's own compiler (the text after a `//#CLIENTSIDE` line,
GS2 unless it starts with `//#GS1`, in legacy mode, which takes the loose
forms live servers let through). A script that does not compile is logged and
sent as source. Without a file, the server weapon of that name is used. The
5.x client's 3D key bindings call `moveForward2`, `moveBackward2`,
`moveLeft2`, `moveRight2` and `jump`, which only such a script defines; a
script moves the player's own object with `triggerserver(<kind>, <weapon>,
"g3dpos", x, y, z)`. Any other `triggerserver("weapon", ...)` or
`triggerserver("gui", ...)` reaches the named server weapon's serverside, as
from a 2D client.

## Each build's row

At link time the server picks the client's row of seihai-core's
`VersionProfile::three_d` (Graal_analysis `version-profiles/three-d.toml`):
the login token's own row (the Linux 4.20 `G3D22126` has one), else its
build's profile, else the protocol's default build (19: 4.04r2, 21: 4.20r6,
24: 5.12). The log names the row and how many of its values are assumed rather
than read. The 5.x `TerrainBlock`, `WaterBlock` and ground cover go only to
builds whose row takes them, and terrain chunks only to those; a build that
asks for Sky textures as `.gif` (3.153) is named in the log with the names it
will ask for, and a build that crashes without a Sky gets a warning when the
world has none.

`graal3d_pushterrain = true` sends `heights.info` and its textures over TCP
right after a link, once per account a run, to builds that take the terrain
and fetch its textures (5.12 fetched the textures only on its second visit, so
a first visit drew the terrain white). A refused texture is never pushed, and
a name listed twice goes once.

## The camera (protocols 21 and 24)

Every protocol 21/24 client gets a Camera ghost of its own,
`G3DCamera_<account>`, named as its camera once the client has its create.
The 5.x client draws its 3D view only through a Camera: with just its control
object the 3D window stays black. It sits 0 -5 2 from the spawn point unless
the world file has a `camera` line, and it faces the spawn point unless that
line gives a rotation.

A `camera` line's x, y and z are an offset from the `spawn` point, not a
world position: `spawn 2 -6 1` with `camera 0 -5 2` puts the camera at
2 -11 3. This is on purpose: moving the spawn moves the camera with it, and
a world switch puts the camera at the new world's spawn plus its offset.

The Camera and the client's own object are created before the world's
objects. A 5.12 gets about 140 creates a second, so on a big world the view
comes up first and the objects fill in.

Rotations (objects and the camera) are an axis and an angle in radians, and
the clients turn the opposite way from the right-hand rule: a positive angle
about +x tilts the view down, a positive angle about +z turns it right
(forward, +y, swings toward +x). The default camera for `spawn 0 0 1` is the
same as `camera 0 -5 2 1 0 0 0.38`.

## Changing files while players are on

A 5.12 asks for a 3D file (a texture, a shape, a terrain chunk) only the
first time it needs it in a session. A file changed on the server while it
is linked keeps its old copy, even after a world switch that ghosts the
objects again. A new link picks up every change: the textures and shapes
are sent again, and the terrain request gets only the changed chunks (lab,
T3D-155, 2026-11-26, real 5.12). The server does not push a changed file, so
a player sees new textures, shapes or terrain after relinking. `g3dedit`
changes only object lines, which reach linked clients at once.

## Server flags on a 5.12

A 5.12 script reads `serverr.x` flags and bare names, but a `server.x`
flag's value reads empty there: the key is kept (`getstringkeys("server.")`
lists it) and the value is blanked by the client. A lab capture (T3D-133,
2026-11-26, real 5.12) found the value intact on the wire on every path the
server sends it: the TCP `PLO_FLAGSET`, the 3D `events` line and the 3D
`set` text. The TCP flag alone was enough to give the empty key. seihai-core
compiles `server.x` reads the same way the official compiler does. So send
a value a 3D script must read as `serverr.x=…` in `config/serverflags.txt`.

## Not done yet

- World switches and per-player objects have loopback tests only.
- In-game editing (`g3dedit`) and the example editor weapon have loopback
  tests only; whether a 5.12 redraws a moved world object from its
  transform update has not been seen yet.
- A movetype on other players' screens was seen only from a 2D client
  (the sprite follows), with `interpolate`.
- Movement data never carries a velocity, and a `list` never carries
  entries: what the client does with them is not known.
- The Sharp3D route and the mutations have loopback tests only.
- No Windows 4.20r6 has been found; protocol 21 has been run only with the
  Mac 4.20r6 (below). The Mac 3.153 has not been run against this server
  since the Sky fix, because its Tiger VM cannot reach a loopback server.

## Lab options

Never on a public server.

- `graal3d_trace = <file>` appends one tab-separated line (milliseconds,
  account, protocol, what) for each link, each event and control object
  change a client sends, each flag line sent (`text out: …`), each 2D player shown or gone, each terrain push and
  each mutation.
- `graal3d_terrainall = true` sends the 5.x ground to every GameObject-era
  client, whatever its row says.
- `graal3d_route = sharp3d` gives protocol 24 clients the Sharp3D route: the
  class table of a server a real 5.12 joined and drew (objects Sun,
  GameObject, Camera, Sky; events BigDataStart, BigDataChunk,
  ConnectionMessage, GhostAlwaysObject), the world as GhostAlwaysObjectEvents,
  no mission start commands (the start is over once the client has every
  object), and its Camera as the control object, named in every packet.
- `rawobject <class> <fields...>` and `rawevent [late] <class> <fields...>` in
  the world file ghost an object or send a guaranteed event of any class the
  client's table has, built from fields: `f0`/`f1`, `i<N>:<v>`, `u8:`, `u16:`,
  `u32:`, `f32:<v>`, `str:<text>` (`%20` a space, `-` empty), `hex:<bytes>`,
  `file:<path>` and `zfile:<path>` (zlib), the byte fields with an optional
  length prefix (`file@10:<path>`). `late` events go after the mission start.
- `graal3d_mutate = <kind>[:<seed>]`: one seeded mutation a link.
  `accept-protocol` and `accept-classtable` change the connect accept; the
  others go in one packet 2 seconds after the mission start: `event-unknown-class`,
  `event-bad-length`, `ghost-uncreated`, `ghost-double-delete`,
  `ghost-index-width`, `control-wrong-class`, `control-deleted`. The log and
  the trace get what changed and the packet's bytes.

  A seed gives the same packet on every run and build of this server, for
  the same world and the same client. The generator is fixed (a test pins
  it): the state starts at seed x 0x9E3779B9 (wrapping) with its low bit
  set, each draw is xorshift32 (x ^= x << 13; x ^= x >> 17; x ^= x << 5),
  and "draw mod n" picks among n. Each kind draws in this order:

  | Kind | Draws |
  |---|---|
  | `accept-protocol` | the protocol: one of p-1, p+1, 0, 0xFFFFFFFF, p+100 |
  | `accept-classtable` | the change (mod 3): drop the last 1 + (draw mod 3) event classes; add `SeihaiLabEvent`; or rename object class (draw mod count) `SeihaiLabObject` |
  | `control-deleted` | the index: the first free one + (draw mod 8) |
  | `control-wrong-class` | one of the created ghosts of another class, in ghost order |
  | `event-unknown-class` | the bit count (mod 64), the class id: the table's size + (draw mod (4096 - size)), then one draw per bit (its low bit) |
  | `event-bad-length` | the bit count (mod 64), the class (mod the table), the length error (1, 8, -1 or 16383), then one draw per bit |
  | `ghost-uncreated` | the index: the first free one + (draw mod 8), the bit count (mod 64), then one draw per bit |
  | `ghost-double-delete` | the index, as above |
  | `ghost-index-width` | the index (drawn, unused), then the width: (needed + 1 + (draw mod 14)) mod 16, one more if that is the right one |

  seihai-gserver-v2 draws from `std::mt19937` in another order, so its seeds
  do not carry over.
- `graal3d_stringprefix = false` writes and reads strings in game packets
  without Torque's "shares a prefix" flag, for every link. Every Graal
  client seen so far reads the flag, so the default is true.

## Lab runs

2026-11-19, in a headless VM:

| Client | Result |
|---|---|
| 4.04r2, native Linux (protocol 19) | 3 of 3 runs: linked, datablocks loaded, Sky and Player ghosts, mission start done, control object delivered. The client logged "Initial Control Object" and "Graal3D has been activated!". The link held 96 s. |
| 4.04r2, native Linux, with Ato (`--version 4.04r2`) on the same world (2026-11-20, #188) | Each client got the other's Player as a third ghost. The real 4.04r2 drew Ato's octahedron and saw it walk away as Ato held Up. Both links held, and neither side had an unread packet. |
| 4.04r2, Windows under Wine | Crashes while starting 3D, before sending anything (a Wine limit). |
| 5.12, Windows under Wine (protocol 24) | Linked, assets downloaded, ghosts and mission start done, control object delivered, link held 97 s. Stays on "Starting 3D mode...", the same as the C++ fork. In 2 of 5 runs the client never opened TCP to the 3D server after the lobby warp (a client-side race; holding the lobby link open 20 s avoided it for 4.x). |

2026-11-26, main at 5daf3b9, headless VMs, loopback:

| Client | Result |
|---|---|
| 5.12 and 5.24r4, Windows under Wine | Sky, terrain (textured on the first visit, with `graal3d_pushterrain`), objects and 2D players both ways. A compiled `weapon3d`'s `g3dpos` trigger moves the player, and the 2D client sees it move. No panic or client crash; the link held. |
| 5.12, `movetype interpolate` | The player glides to each `g3dpos` target; a 2D client's sprite glides with it. |
| 5.12, `movetype rigid` | Glides with a small lag; the client echoes no object section. |
| 5.12, `movetype list` | Jumps exactly onto each target. |
| 5.12, a `server.labflag=on` server flag | Sent as an `events` `s` line and as a `set` text; the 3D script reads it empty. |
| 4.20r8 (`G3D20126`), Windows under Wine | Linked as protocol 24, control object delivered, sky, water and objects drawn, terrain untextured (its row takes no terrain files), link held about 90 s. It draws no 2D players. |
| `G3D27015`, native Linux (protocol 19, the 3.153 row) | Linked, Camera control object, mission start done, sky drawn, link held until quit. No first-frame crash. |

2026-11-26, PR #208 and #209, real 5.12 in a Windows 10 VM, loopback:

| Test | Result |
|---|---|
| Level warp (T3D-135), on #208 | No relink; sky and spawn switch; `clientr.g3dworld` follows. Objects of the new world under a key still being deleted were missing (fixed in #209). |
| Level warp, on #209 | Every switch deletes the old world's objects and creates the new one's, both by the `g3dworld` trigger and by a TCP warp to a level named like the world file. A `weapon3d` both worlds have runs once. |
| Objects for some players (T3D-136), two clients | An `objectfor` object reaches only its account; `g3dhide` / `g3dshow` hide and show it for that client only. |
| Ground cover (T3D-048), on #208 | A `DynamicShapeReplicator` scatters over streamed terrain once its files are cached; a first visit showed none. The grass classes drew nothing with a `.3ds`. |
| Ground cover, on #209, cold cache | The cover appears about 5 seconds after the mission start (the first resend) and stays. |

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
ghost and datablock layouts, `ground` the terrain, water and ground cover,
`assets` the start-up check, `caps` each build's row, `raw` the lab raw
fields, `mutate` the lab mutations, `conn` one client's link, `scene` the
world file, and `service` the UDP socket and its tie to the TCP server. Most
of the 3D side is ported from the 3D add-on the Seihai project wrote for
seihai-gserver-v2, at the owner's direction (2026-11-25).
