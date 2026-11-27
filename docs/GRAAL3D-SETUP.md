# Setting up seihai-gserver as a 3D server

This is a short, practical guide to running seihai-gserver 0.2.0 as a Graal 3D
server: what to download, what to put in the config, where the world goes, how
staff edit it, and how to check it with a 5.12 client. It assumes the
listserver and the rest of the community stack are already running; it only
covers the game server.

The full reference is [GRAAL3D.md](GRAAL3D.md). Every key and world line
below matches the 0.2.0 source (`src/config.rs`, `src/graal3d/scene.rs`).

## 1. Get the program

Download the archive for your system from the
[v0.2.0 release](https://github.com/squirelworks/seihai-gserver/releases/tag/v0.2.0)
and check it against `SHA256SUMS`:

| System | Archive |
|---|---|
| Linux x86_64 | `seihai-gserver-linux-x86_64.tar.gz` |
| Windows x86_64 | `seihai-gserver-windows-x86_64.zip` |
| macOS Intel | `seihai-gserver-macos-x86_64.tar.gz` |
| macOS Apple Silicon | `seihai-gserver-macos-aarch64.tar.gz` |

The macOS builds are not signed, so the first start may need right-click,
Open. To build it yourself instead: `git clone`, `git checkout v0.2.0`,
`cargo build --release`; the program is `target/release/seihai-gserver`.

## 2. Lay out the world folder

A world folder is everything the server serves. Start from a copy of
[`world/`](../world/) and add the 3D parts, so it looks like this:

```text
myworld/
  config/
    server.txt            settings (section 3)
    allowedversions.txt   which client builds may log in (`*` = all served builds)
    graal3dworld.txt      the default 3D world
    graal3dworlds/        more 3D worlds, one <name>.txt each (optional)
    serverflags.txt       server flags (optional; see section 8)
  accounts/               one <name>.txt per account
  levels/                 the 2D levels and gmaps (the 3D client still logs in through one)
  files/                  everything clients download: shapes, textures, terrain, weapons
```

Shapes (`.3ds`, `.dts`, `.dif`) and textures go anywhere under `files/`; the
server finds them by name at any depth. Folder paths inside the world file
(`terrain`, `regions`, `weapon3d`) are relative to the world folder, so a
terrain under `files/3d/isle_terrain` is written `terrain files/3d/isle_terrain`.

## 3. Settings for 3D mode (`config/server.txt`)

seihai-gserver reads `config/server.txt` (one `key = value` a line, `#` for
comments). There is no `serveroptions.txt`; that file and its underscore-free
spellings (`graal3dscope`, ...) belong to seihai-gserver-v2.

A 3D server needs only one line. The rest are the ones worth knowing:

```text
name = My 3D World
bind = 0.0.0.0              # default 127.0.0.1: loopback only
port = 14802                # TCP; the 3D link is UDP on port + 1 (14803)

graal3d = true              # the only required 3D line (default false)
graal3d_pushterrain = true  # push heights.info and terrain textures at login,
                            # so a 5.12's first visit is not white ground
graal3d_scope = auto        # off (default), on, or auto: send world objects
                            # only near each player (section 6)
graal3d_scopeauto = 2000    # auto: only for worlds with more objects than this
graal3d_scoperadius = 1100  # world units (default 1100)
graal3d_scopehysteresis = 200 # world units (default 200)
graal3d_edit = staff        # off (default) or staff: in-game editing (section 5)
graal3d_shapecheck = skip   # skip (default), warn or off: what to do with
                            # shapes/textures known to crash a client
graal3d_sendms = 50         # least ms between two 3D packets to one client,
                            # 10 to 1000 (default 50)
```

Leave `graal3d_port` unset. The clients always open UDP on the listed TCP
port + 1, so any other value just breaks the link.

The listserver side is the usual `listserver = <listserver-host>:<port>`
(plus `listserver_hq_password` and `listserver_accounts` if your listserver
needs them). With `graal3d = true` the server registers at HQ level 4, so the
listserver tags its name `3 `; do not type the `3 ` yourself (a leading one
is taken off). A 5.12 enters 3D only on a server listed with that tag.

Open both TCP `port` and UDP `port + 1` in the firewall.

Lab-only keys (`graal3d_trace`, `graal3d_terrainall`, `graal3d_route`,
`graal3d_mutate`, `graal3d_stringprefix`) are for protocol work; leave them
out of a real server.

## 4. Point it at a 3D world

`config/graal3dworld.txt` is the default world. A 3D player is put in the
world named after its 2D level (`cave.nw` is world
`config/graal3dworlds/cave.txt`), else in the default one, so a 2D warp takes
the 3D client along.

### The world file

One entry a line. The lines you will use most:

```text
sun 0.57735 0.57735 -0.57735 0.8 0.8 0.8 1 0.3 0.3 0.35 1
sky sky_fr,sky_rt,sky_bk,sky_lf,sky_up,sky_dn 1000 0.6 0.7 0.9
playershape player.3ds
spawn 24 -24 1
camera 0 -6 3                         # an offset from spawn, not a position
terrain files/3d/island 8             # 256 x 256 squares, 8 units a square
water -1024 -1024 -19.5 2048 2048 20 water_surface 0
object house1 house.dts 40 -10 0
object tree1 tree.3ds 20 -5 0 0 0 1 0.6   # axis x y z, angle in radians
replicator DynamicGrass 0 0 0 8 20 40 blades.png
weapon3d Walker weapons/walker.gs2
```

Each `heights<x>-<y>.dat` chunk (x and y 0 to 15) is 16 x 16 squares as a
zlib stream, at most 1,023 bytes; GRAAL3D.md, "Terrain, water and ground
cover", has the layout. Always give a world a `sun` (see section 8). A `.3ds` must have a material or
it draws nothing. Terrain textures go in the terrain folder's `heights.info`,
one name a line with the extension (`g3d_grass.png`), 256 x 256, 24-bit.

#### Regions

For a big overworld, use a `regions` line in place of `terrain`:

```text
regions files/3d/regions 8x8          # 8 x 8 regions of 8 chunks (1,024 units) each
regions files/3d/regions 4x4 16       # 4 x 4 regions of 16 chunks
regions files/3d/regions 16x16 8 8    # last number: square size (default 8)
```

The folder holds one subfolder a region, `<i>-<j>` (i east, j north), each
with its `heights<x>-<y>.dat` chunks and an optional `region.txt` of
`object`, `objectfor`, `water` and `replicator` lines in the region's own
coordinates. `heights.info` and the terrain textures go in the regions folder
itself. Turn `graal3d_scope` on for any regions world. The start-up log says
how many objects the densest spot has against the 3,584 a client can hold.

#### Interiors and portals

Put a house or cave in the same world, far from everything else (at least
about 1,500 units), and join its door to the outside:

```text
object house_inside house_in.3ds 0 -1600 0
portal 32 -63.5 0 5   31.5 -1660 0      # door outside -> room
portal 32 -1663.5 0 5 31.5 -26.5 0      # room door -> outside
```

`portal <x> <y> <z> <radius> <to x> <to y> <to z>`: while a player stands
within `radius` of the point, the objects round the `to` point are sent too,
so the room is there when a walker script moves the player through. Portals
do nothing without `graal3d_scope`. A separate world behind a level warp
works too, but a switch is slower (terrain resent, a second lighting pass).

## 5. Staff accounts and in-game editing

Make the account, then mark it staff in its account file:

```text
GRAAL_PW=choose-a-password seihai-gserver account alice --password-env GRAAL_PW --world myworld
```

then add `STAFF 1` on its own line in `myworld/accounts/alice.txt` and restart.

With `graal3d_edit = staff`, a staff account's 3D client script can move, add,
remove and list world objects with `g3dedit` triggers; every client in the
world sees the change at once, and the world file is saved with a backup
(`<file>.bak-<unix seconds>`). Region objects are edited in their
`region.txt`, not in game.

A real 5.12 always logs in as a guest, so it signs in for editing from the
script: `triggerserver("gui", <weapon>, "g3dedit", "login", <account>, <password>)`.
The password is checked against the account file (or the listserver with
`listserver_accounts`), the account must be `STAFF`, and three wrong
passwords lock that connection until it reconnects. The 3D link is not
encrypted, so use editing logins on a trusted network only.

The ready-made editor weapon is
[`docs/examples/g3deditor.gs2`](examples/g3deditor.gs2). Copy it to
`files/weapons/` (or anywhere in the world folder) and add it to the world:

```text
weapon3d G3DEditor files/weapons/g3deditor.gs2
```

In game: L signs in (account, Enter, password, Enter), N and B pick an
object, the arrows and Page Up/Down move it, Q and E turn it, C copies it,
Delete removes it. Terrain is not editable in game; edit its chunk files and restart.

## 6. Big worlds: the 4,096 limit

A 5.12 holds 4,096 objects ("ghosts") at most, and never draws anything more
than about 1,000 units from its camera. For any world over about 2,000
objects set `graal3d_scope = auto` (or `on`): objects are sent only within
`graal3d_scoperadius` of the player and removed past radius + hysteresis. A
scoped client holds at most 3,584 world objects, nearest first. The defaults
(1100 / 200) came from lab runs; players that move much faster than walking
need a larger radius.

## 7. Run it and check it with a 5.12

```text
seihai-gserver --world myworld
```

(`--bind` and `--port` override the settings; `--verbose` logs unhandled
messages.)

Read the start-up log before connecting anyone. It lists:

- shapes and textures the world names that `files/` does not have;
- files the start-up check refuses (a `.3ds` with no material, a `.dts`
  that is not version 24, oversized or 16-bit PNGs, and so on);
- terrain chunks found or bad, `heights.info` problems, a second `terrain`
  line;
- for regions, the region count and the densest spot against 3,584;
- a spawn in the origin cell when the world has ground cover.

Then log in with a Windows 5.12 through your listserver and pick the server.
What you should see:

1. The client logs in over TCP (as a guest) and is warped to the server.
2. It opens UDP on port + 1. Its console shows "Phase 1: Download Ghost
   Objects", then "Phase 2: Mission Lighting", then "Initial Control Object".
3. Once its file requests are answered the 3D view appears. On a big world
   the view comes up first and objects fill in (about 130 a second at
   `graal3d_sendms = 50`).

The 5.12 does not walk by itself: the world needs a walker `weapon3d`
(it calls `moveForward2`, `moveBackward2`, `moveLeft2`, `moveRight2` and
`jump`, and moves the player with a `g3dpos` trigger; see GRAAL3D.md,
"Client scripts"). Play it in a window, not full
screen. On Windows with no GPU driver the client has only OpenGL 1.1; install
a driver.

## 8. Troubleshooting

| What you see | Likely cause and fix |
|---|---|
| Server not shown as 3D, or the client never tries 3D | `graal3d` is off, or the listserver is not getting HQ level 4. Check the start-up log and the listserver's tag (`3 `). |
| "The connection to the Graal3D server couldn't be established." | UDP port + 1 is closed or `graal3d_port` is set to something else; or the TCP login came from another address. |
| Stuck at "Connecting..." | a file request went unanswered; check the log for missing files. |
| Black 3D view | no Camera reached the client; on a world past 4,096 objects, turn on `graal3d_scope`. |
| White terrain on the first visit | turn on `graal3d_pushterrain`, or relink. |
| White textures | BMP, TGA, DDS, 16-bit PNG or over 1,024 a side; use 8-bit PNG or JPEG. |
| A shape does not show | `.3ds` with no material, or `.dts` not version 24; see the start-up check. |
| Terrain unshaded | it arrived after lighting; the server handles this, so relink and check the log for chunk errors. |
| Every chunk downloads every visit | chunks not written in the client's encoding; write chunks in the client's own re-encoding (GRAAL3D.md, "Terrain"). |
| No grass or ground cover | the player is in the origin cell (see limits), the grass line names a shape instead of a texture, or there is no ground under it. |
| Client freezes after a warp | the target world has no terrain, water or Sun. |
| A script reads `server.x` as empty | a 5.12 blanks `server.` flags; send them as `serverr.x` in `config/serverflags.txt`. |
| An edited texture or shape does not show | clients fetch a file once per session; they see it after relinking. To roll a file back, give it a fresh modification time. |
| Edits refused | `graal3d_edit` is off, the account has no `STAFF 1`, the 5.12 did not sign in with `g3dedit login`, or the object is a region object. |

## 9. Known limits (from the 5.12 client, not the server)

- **4,096 objects a client.** Use scoping; a scoped client gets at most
  3,584 world objects. Very dense towns are limited by the client's frame
  rate.
- **Terrain is a 256 x 256 grid** of 16 x 16 chunks (each chunk 16 x 16
  squares, at most 1,023 bytes compressed), one terrain a world, square size
  1 to 64 (use 8). Larger terrains (`terrain <folder> <size> repeat <w>x<h>`,
  up to 256 x 256 chunks) and regions slide that grid round the player;
  put a border of sea or flat chunks round the edge.
- **No ground cover in the origin cell.** A 5.12 grows no grass or shape
  cover while the player stands with x and y both between -8 and 8. Keep the
  spawn at least 8 units from 0 0 in x or y. Grass also fades at about
  radius × 8 units from the camera, so a far camera needs a larger radius.
- **Give every world a Sun.** A world switch into a world with no terrain,
  water or Sun freezes a 5.12.
- **No client collision.** Walls are the walker script's job.
- **Changed files show only on a new link.**
- Distance scoping and fast `graal3d_sendms` were tried on loopback only;
  keep `graal3d_sendms` at 50 over a real network until tried.
