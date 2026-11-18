# Lab, 2026-11-18: real 1.28 beta 2 draws other players (#132)

Loopback only, in the Debian lab VM, under user-space Wine inside a
`bwrap --unshare-net` namespace whose only interface is `lo`. A logging relay
sat in front of each server. One client per Xvfb screen. Raw captures and
screenshots stay in the private lab folder; this page has the derived result.

## Clients and servers

| | |
|---|---|
| Viewer | 1.28 beta 2 (`GSERV013`, the `graal128b2` install), with the in-memory UDP receive fix from `live-trace-early.md` applied after start |
| Other player | 1.35 (`GNW13040`, `graal135`) |
| Control server | the original GServer 1.28 (`GServerv.exe` from the 1.28 beta 2 zip), two 1.28 beta 2 clients |
| Server under test | seihai-gserver main `0afe61d` (with #152), then this branch |
| Levels | `house1.graal` (from the 1.28 beta 2 install) and the `door45.nw` lab level |

## Results

| Run | What the 1.28 beta 2 drew |
|---|---|
| main `0afe61d` (prop 20 in, #152) | nobody, on `.nw` and on `.graal` |
| props cut to 0–31 (no 32), 50 last | nobody |
| props 0–31, 50 last, server prop 30 | nobody |
| **50 first**, props 0–31, server prop 30 | **the 1.35, named, at its position, following it as it walks**; the 1.35 still draws the 1.28 beta 2 |
| original GServer 1.28, two 1.28 beta 2 | each draws the other (positive control) |

## What GServer 1.28 sends

Decoded from the control run (ids +32 as on the wire):

- To the player already in the level, about the newcomer: `8` with head (11)
  only; then `8` with nickname (0), head (11), x (15), y (16) and level (20);
  then `8` beginning with **50 = 1** followed by 2, 8, 9, 10, 13, 21, 24,
  **30**, **31**, 15, 16, 17, 18, 19.
- To the newcomer, about the player there: `8` with 0, 11, 15, 16, 20 before
  the warp (14), and after the level the same 50-first message as above.
- Prop 30 (`IPADDR`) is a GINT5 of the IPv4 address read as a little-endian
  u32: `20 28 20 20 9f` for 127.0.0.1. Prop 31 is the client's UDP port, a
  GINT3, as the client sent it.
- A failed second login still sends the first player `8` with 51 for the
  refused id.

## Cause

Before 1.33 there is no separate parser for other players: message 8 goes
through the own-prop parser (0–31 in 1.28 beta 2), which also takes 50 and 51
and stops at the first id outside. The server sent the 1.33-shaped stream,
whose prop 32 came before the position and 50, so the parse ended there.
Cutting the stream to 0–31 was not enough: with 50 last the player was still
never drawn. With 50 first, as GServer 1.28 orders it, it is drawn at once.

## Change

- `others::other_props`: for 1.21 to 1.32, the build's own-prop set plus 50
  and 51.
- `others::message`: for those builds the tail (50/51) goes first.
- Prop 30 is filled from the connection's IPv4 address at join
  (`Props::set_ipaddr`); a client-sent 30 is ignored.
- Tests: `early_builds_get_join_first_and_no_prop_past_their_parser`,
  `client_sent_ipaddr_is_ignored`.

## Not checked

- Whether prop 30 alone is needed for the draw (it was in every passing run;
  GServer 1.28 sends it, so it stays).
- Peer UDP between two 1.28 beta 2 on this server (both walking needs the
  DirectInput patch from `live-trace-early.md`).
- 1.21 (`GSERV007`), 1.3 test, 1.31 and 1.32 as viewers: same rule by the
  shared parser, not run.
