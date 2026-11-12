# Five client builds in one level, 2026-11-12 (issue #45)

Five real client builds, 1.39, 2.22, 2.31, 5.24r4 and 6.03r7, were on one
seihai-gserver level at the same time. Each one saw the others arrive,
move and chat, with three exceptions: 1.39 draws the other players with
no body, 5.24r4 shows a blank panel under Wine, and 6.03r7 shows a black
window under Wine. No client dropped or froze. The run was loopback only,
under Wine (`tools/lab-wine.md`), inside a network namespace with only
`lo`. Before any client started, a check confirmed that a public address
could not be reached from inside it. No public server and no real player
account were used.

## Setup

- Server: `main` (3c26e78) built with `cargo build --release` and run
  with `--verbose` in a throwaway world with `allowedversions *`.
- Each client had its own Wine prefix and Xvfb display.
- 1.39 connected directly.
- 2.22 and 2.31 came in through the lab listserver and the decrypting
  proxy.
- 5.24r4 and 6.03r7 dial the login server first. A lab lobby on loopback
  answered with `PLO_SERVERWARP` to the proxy in front of the gserver.

## Results

Each row is the viewer. Each cell is the column's player: arrives /
moves / chat. "wire" means the bytes arrived but the screen could not be
checked. "n/a" means the column's client could not do that action.

| viewer | 1.39 | 2.22 | 2.31 | 5.24r4 | 6.03r7 |
|---|---|---|---|---|---|
| 1.39 | — | name, no body / ✓ / ✓ | name, no body / ✓ / ✓ | name only / ✓ / n/a | name only / n/a / n/a |
| 2.22 | ✓ / ✓ / ✓ | — | ✓ / ✓ / ✓ | ✓ / ✓ / n/a | ✓ / n/a / n/a |
| 2.31 | ✓ / ✓ / ✓ | ✓ / ✓ / ✓ | — | ✓ / ✓ / n/a | ✓ / n/a / n/a |
| 5.24r4 | wire ✓ / ✓ / ✓ | wire ✓ / ✓ / ✓ | wire ✓ / ✓ / ✓ | — | wire ✓ |
| 6.03r7 | wire ✓ / ✓ / ✓ | wire ✓ / ✓ / ✓ | wire ✓ / ✓ / ✓ | wire ✓ | — |

- The positions sent matched what each client reported. The 2.22 and 1.39
  got half-tile X/Y. The 2.31, 5.24r4 and 6.03r7 got X2/Y2.
- Chat from 1.39, 2.22 and 2.31 reached every client and was drawn on
  1.39, 2.22 and 2.31.

## Findings

1. **1.39 draws other players as a nickname with no body, and its own
   player has no head.** The props carried `head0.png` and `body.png`. The
   server logged nothing unusual: the props were sent and the client did
   not draw them. Not fixed yet. A follow-up lab will try what 1.39 does
   draw.
2. **Two logins to one account.** 5.24r4 and 6.03r7 both log in as
   `guest` when no real login server is present, and the second was
   refused with "Account already in use.", as the server does for any
   account. For the run, a proxy renamed the second login.
3. **157 (`PLI_UPDATEGANI`) was logged as unhandled.** 5.24r4 and 6.03r7
   send it once after the login (`idle`), next to a `PLI_WANTFILE` for
   `idle.gani`. The `PLI_WANTFILE` already gets the file or a not-found.
   It is now read and not answered (`wire::ids::stage1_quiet`, test
   `quiet_weapon_ids::update_gani_157_is_not_unhandled`).
4. **5.24r4 stays on a blank panel under Wine.** It asked for
   `basepackage.gupd`, `arial.ttf`, `sprites.png`, `state.png` and other
   files, and got not-found for each. Adding a one-line `basepackage.gupd`
   did not change the panel. Its game state was live: it walked, and the
   other clients saw it move.
5. **6.03r7 is black under Wine/Xvfb**, as in earlier runs. It was sent
   every player and every chat line.
