# Five client builds in one level, rerun, 2026-11-14 (issue #45)

The same five real builds as `five-clients-2026-11-12.md` (1.39, 2.22,
2.31, 5.24r4, 6.03r7) were together on one seihai-gserver level for about
five minutes. This rerun checks the 1.x fixes in `others::for_receiver`
(status value 4 and player art) and the scripts row of
`docs/CLIENT-PROFILES.md`.

Loopback only. The run was on the lab host, under Wine, inside a namespace
with only `lo`. Before any client started, a check confirmed that public
addresses could not be reached and DNS failed. All five clients came in
through the decrypting proxy, so every message was logged per client. No
public server and no real player account were used.

## Setup

- **Before:** `main` at b3f2c25. **After:** the #45 branch (eb9d8b2 plus
  docs). Both were built with `cargo build --release` and run with
  `--verbose`, in a throwaway world with `allowedversions *`.
- **Tokens:** 1.39 `GNW07080`, 2.22 `GNW03014`, 2.31 `GNW28015`, 5.24r4
  `G3D15010` and 6.03r7 `G3D0311C`. 5.24r4 and 6.03r7 came through the
  lab lobby's 178 warp, and 6.03r7's account was renamed by the proxy.
- **Two GS1 level NPCs in the shared level:**
  - a sign: `if (playerenters) { message lab45b; }`;
  - the door from seihai-core's `delivery` tests (`setimg door.png`; on
    touch `setimg dooropen.png; play door.wav; timeout = 1`).

## 1.39: what it got and drew

| Prop | before (`main`) | after |
|---|---|---|
| 8 sword | `3f 2a "sword1.png"` | `21` (preset 1) |
| 9 shield | `2b 2b "shield1.png"` | `21` (preset 1) |
| 11 head | `8d "head0.png"` | `20` (preset 0) |
| 18 status | `34` (20) | `30` (16) |

These values are the same in 1.39's own props and in every other player's.
2.22 still gets the names, `body.png` and status 20.

- **Before:** 1.39 drew the 2.22 player as a nickname with no body, and its
  own player had no head. This reproduces finding 1 of the 2026-11-12 run.
- **After:** 1.39 draws every other player with head, body, sword and
  shield, and its own player with a head. The nickname now sits right
  under each player.

## Grid (after)

The cells are arrives / body / moves / chat.

| viewer | 1.39 | 2.22 | 2.31 | 5.24r4 | 6.03r7 |
|---|---|---|---|---|---|
| 1.39 | — | ✓ / ✓ / ✓ / ✓ | ✓ / ✓ / ✓ / ✓ | ✓ / ✓ / ✓ / n/a | ✓ / ✓ / n/a / n/a |
| 2.22 | ✓ / ✓ / ✓ / ✓ | — | ✓ / ✓ / ✓ / ✓ | ✓ / ✓ / ✓ / n/a | ✓ / ✓ / n/a / n/a |
| 2.31 | ✓ / ✓ / ✓ / ✓ | ✓ / ✓ / ✓ / ✓ | — | ✓ / ✓ / ✓ / n/a | ✓ / ✓ / n/a / n/a |
| 5.24r4 | wire | wire | wire | — | wire |
| 6.03r7 | wire | wire | wire | wire | — |

"wire" means every message arrived but the build draws no players under
Wine (5.24r4 draws no tiles or players, and 6.03r7 is black). "n/a" means
that player could not do the action:
- 5.24r4's keys all landed as game keys, so it sent no chat.
- 6.03r7 took no input at all.

Chat from 1.39, 2.22 and 2.31 reached every other client once and was
drawn on 1.39, 2.22 and 2.31.

## Scripts row

| Build | Delivery | Ran |
|---|---|---|
| 1.39 | prop 1, GS1 text | **sign: yes.** It drew `lab45b` and sent `PLI_NPCPROPS` back |
| 2.22, 2.31 | prop 1, GS1 text | **sign: yes** (image and message drawn) |
| 5.24r4 | `PLO_NPCBYTECODE` (131) for both, no prop 1 | **door: yes.** It drew the sign and the closed door; walking into the door made it ask for `dooropen.png` and `door.wav`, which only the touch branch names |
| 6.03r7 | 131 for both | not shown (black under Wine, nothing answered) |

The door image did not draw on 1.39 or 2.x, and touching it there asked
for nothing. The lab art was a 2-bit palette PNG, and another image in
that format did not draw either, so the art is the likely cause and the
door branch on GS1 builds is not shown. The door was solid for 1.39 and
2.22.

## Server

There were no crashes and no disconnects. Two messages were logged as not
handled, once each:
- 47 at the 5.24r4 login;
- 38 after 5.24r4 fired the bow.

## Left

- Chat from 5.24r4 and 6.03r7: no input path under Wine.
- The door's touch branch on GS1 builds: rerun with plain 8-bit art.
- 6.03r7 running a script: it draws nothing under Wine.
