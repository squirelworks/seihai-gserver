# Anti-cheat with real clients, every check on kick, 2026-11-12 (issue #54)

Real 1.39, 2.05 rev 2, 2.17.1, 2.22 and 2.31 clients played on
seihai-gserver with every anti-cheat check set to kick on the first
violation. Two builds were kicked, both for honest play: a 2.22 that died
and respawned (warp check), and a 2.05 rev 2 after the server stalled for
3 s (speed check). Nothing else raised a line. PR #96 fixes both. The
run was loopback only, under Wine (`tools/lab-wine.md`), in a network
namespace with only `lo`. Before any client started, a check confirmed
that a public address could not be reached from inside it. No public
server and no real player account were used.

## Setup

- Server: `main` (0f53796) built with `cargo build --release` in the
  headless Debian lab VM, run with `--verbose` in a throwaway world with
  `allowedversions *`, a start level with a link to a second level and
  back, a chest and floor items.
- Every check on log, kick after the first:

  ```text
  anticheat = on
  anticheat.speed = log, kick after 1 in 60
  anticheat.teleport = log, kick after 1 in 60
  anticheat.warp = log, kick after 1 in 60
  anticheat.rate = log, kick after 1 in 60
  anticheat.items = log, kick after 1 in 60
  anticheat.hurt = log, kick after 1 in 60
  ```

- Each client had its own Wine prefix and Xvfb display. 1.39 and 2.05 rev
  2 drew only after their prefixes were switched to the GDI renderer.
- To measure speeds, the lab build printed one extra trace line per
  accepted position, switched on by an environment variable. That change
  was not committed.

## What was done

With each build: walking in all 8 directions, pushing into walls, the
link to the other level and back, opening the chest, taking floor items
(1.39 only: 2.x clients do not roll their own bush items, and the server
drops none), crossing gmap seams (2.17.1, 2.22), sword fights between
pairs, a 3 s `SIGSTOP` of the client mid-walk, and a 3 s stop of the
server mid-walk (5 s for 2.22).

## Results

| build | walk, link, chest, items, gmap, swords, client frozen 3 s | server stalled |
|---|---|---|
| 1.39 | no lines | 3 s: passed, 0.05 tiles of budget left |
| 2.05 rev 2 | no lines | 3 s: **kicked** (speed) |
| 2.17.1 | no lines | 3 s: passed, 1.02 tiles left |
| 2.22 | **kicked** (warp) after a death | 5 s: **kicked** (8 speed lines) |
| 2.31 | no lines | not run |

The two kicks:

```text
1794518698	h222	2.22	start.nw	warp	to start.nw 30 30.5 from 25.5 32; kicked
1794518915	a205d	2.05	start.nw	speed	1.0 tiles to 44.5 27.5; kicked
```

- A 2.22 that dies asks to warp to the start spot to respawn. There is no
  link there, so the warp check took it for a cheat.
- After a 3 s server stall the 2.05 rev 2's steps arrived together: 31
  tiles in one read, one more than the 30-tile budget.
- A frozen client does not catch up when it resumes, so it never tripped
  the speed check. Only a stall on the server or the network makes a
  burst.
- No `PLI_HURTPLAYER` (26) was sent in any fight, as in
  `docs/lab/hurt-2026-11-12.md`, so the hurt check had nothing to read.
- Deaths on 2.05 and 2.31 could not be set up, so only the 2.22 respawn
  warp was seen.

## Measured walking

| | |
|---|---|
| walk speed | about 10 tiles a second, one update every ~100 ms (2.31: half-tile steps every ~85 ms) |
| longest single step | 1.58 tiles in 101 ms (1.39, diagonal) |
| most in any 1 s of walking | 15.6 tiles |
| one read after a 3 s server stall | 31 tiles |
| one read after a 5 s server stall | 38 tiles |

## What changed (PR #96)

- The speed budget (`BURST`) went from 30 to 60 tiles, about 6 s of
  walking. That covers both stalls seen here. 24 tiles a second and the
  16-tile teleport limit stay, since walking never came near them.
- A dead player's warp (0 hearts) is not checked, so the respawn goes
  through.

These two fixes were tested with Seihai's own client code in
`tests/anticheat.rs`. They have not had a second real-client run yet.

The full lab notes, raw logs and scripts are on the lab machine.
