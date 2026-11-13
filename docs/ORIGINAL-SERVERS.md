# What the original 1999 servers do

The archive holds two original Graal servers: **GServer 1.28 beta 2**
(`GServerv.exe`, 23 November 1999, shipped with the 1.28 beta 2 client) and
**GServer 1.3** (`GServer13.exe`, 29 November 1999, shipped with the 1.3
test client). Until now they had only been run in the lab (a login, the
start dialog). This page is what reading them showed: how they handle each
client message, what they keep, and their timers. It is written as plain
behaviour, as `AGENTS.md` asks; no code or addresses from them are here.

Everything else in this server's rules comes from GServer-v2, a later
reimplementation, measured in the lab. Where the two differ, `rules = 1999`
in `config/server.txt` picks the 1999 behaviour (`src/rules.rs`).

## The two builds

Both are the same program (a Delphi 4 tray application, "GServer2"). 1.3 adds
kill claims and alignment points; 1.28 beta 2 has neither. 1.3 knows the
tokens `GSERV007` to `GSERV014` but turns every build before 1.3 away with
"You need at least Graal version 1.3 to play on this server."; 1.28 beta 2
says the same for 1.28 beta 2.

The start dialog (also `serverconfig.txt`) has five settings: the port
(14804), **respawn time** for bushes (10 s), **horse lifetime** (10 s),
**alignment increment time** (1200 s) and **baddy respawn time** (60 s).

A one-second timer runs every game clock. Each socket is read on a second,
faster timer.

## Client messages

| Message | What the 1999 servers do |
|---|---|
| `PLI_LEVELWARP` (0), `PLI_LEVELWARPMOD` (30, 1.3 only) | Leave the level, move, enter the new one. |
| `PLI_BOARDMODIFY` (1) | The tiles go into the server's own copy of the board and to the others in the level (`PLO_BOARDMODIFY` 7). Only a 2x2 change grows back, after a random whole number of seconds between 0.75 and 1.25 times the respawn time (7.5 to 12.5 s by default); the old tiles then go to the level. Larger changes stay. A player who enters later gets the board with the changes already in it. |
| `PLI_PLAYERPROPS` (2) | Applied with limits (below). |
| `PLI_NPCPROPS` (3) | Passed to the others in the level, then kept. |
| `PLI_TOALL` (6) | To every other player online as `PLO_TOALL` (13), the sender's id first. |
| `PLI_HORSEADD` (7) | `x*2, y*2, dir, image`. Kept for the horse lifetime, replacing a horse on the same spot. **Not** passed on at once; arrivals get it. |
| `PLI_HORSEDEL` (8) | The horse on that spot is removed. Nobody is told. |
| `PLI_FIRESPY` (10) | Read and dropped. |
| `PLI_CLAIMPKER` (14, 1.3 only) | One `GCHAR`, the killer's id. See "Kills" below. |
| `PLI_BADDYPROPS` (15) | **Only from the level's leader**; passed to the others in the level, then kept. |
| `PLI_BADDYHURT` (16) | To the leader as `PLO_BADDYHURT` (27). |
| `PLI_BADDYADD` (17) | A new baddy that does not come back. |
| `PLI_FLAGSET`, `PLI_FLAGDEL` (18, 19) | The account's flags. |
| `PLI_OPENCHEST` (20) | Kept on the account. |
| `PLI_PUTNPC` (21), `PLI_NPCDEL` (22) | An NPC from a script file, sent to the level; removed. |
| `PLI_WANTFILE` (23) | The name with `.gif` added, from the server folder, if under 64,500 bytes; else "failed" (30). |
| `PLI_WEAPONADD` (25), 29 | Weapons on the account. |
| `PLI_PRIVATEMESSAGE` (28) | One `GCHAR` count, one `GCHAR` per id. A build too old to answer gets the sender "The player cannot answer because he/she has an older version." |
| 51 to 64 | The RC's messages: accounts, disconnect, admin messages, player props. |

**Not handled at all:** `PLI_BOMBADD` (4), `PLI_BOMBDEL` (5), `PLI_ARROWADD`
(9), `PLI_THROWCARRIED` (11), `PLI_ITEMADD` (12), `PLI_ITEMDEL` (13), 24
and 27. These servers pass on no bombs, arrows, fire or floor items. Later
servers do, and this server keeps doing so.

## Own props

The server keeps every prop and limits some, telling the client the value it
kept (`PLO_PLAYERPROPS` 9): max hearts 20, hearts up to the max (and none
taken while dead), gralats 9,999,999, arrows and bombs 99, alignment 0 to
100. Sword 1 to 4 and shield 1 to 3 become the stock images.

- **Deaths.** When the status (prop 18) gets its dead bit (8), the death
  count goes up by one.
- **A dead leader.** If the player who died leads the level (runs its
  baddies) and others are there, the next player leads (`PLO_ISLEADER` 10)
  and the dead one goes to the back of the line.
- **Carrying an NPC.** A second player claiming an NPC that someone already
  carries is refused.

The nickname and head go to every player online, in every level. A few props
go to the level at once (hearts, sword, shield, colours, the carried NPC,
alignment); the position and sprite go out batched.

## Timers, once a second

1. Cut bushes count down and grow back.
2. Horses count down the horse lifetime; then the level gets `PLO_HORSEDEL`
   (18) with the spot. The 1.3 lab run saw this at 9.5 to 9.8 s.
3. A baddy the leader set dead (mode 9) counts down the baddy respawn time
   and then comes back at its spawn point with its type's power and start
   mode. There is no server step from dying (mode 5) to dead: the leader's
   client says mode 9.
4. Each player's time online goes up, and every alignment increment time
   (20 minutes) their alignment rises by one, up to 100.

## Baddies

| Type | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |
|---|---|---|---|---|---|---|---|---|---|---|
| Power | 2 | 3 | 4 | 3 | 2 | 1 | 1 | 6 | 12 | 8 |
| Start mode | 0 | 0 | 0 | 0 | 6 | 7 | 0 | 0 | 0 | 0 |
| Image | gray | blue | red | blue | gray | hare | octopus | gold | lizardon | dragon |

The powers and images match this server's. The start modes do not: a swamp
soldier (4) starts in mode 6 and a hare (5) in mode 7, for level baddies and
when they come back. GServer-v2 starts every type in mode 0.

## Kills

A kill claim counts only when the **victim** has 10 alignment points or more.
Then the killer loses `(victim's AP + 10) / 20`, rounded half to even
(so 1 to 6 points), never going under 0, and the killer's kill count goes up.
A victim under 10 AP costs nothing and is not counted. GServer-v2 instead
takes `2 * (1 + killer's AP / 20)` from every killer.

## Entering a level

The board (or nothing, when the client's copy is current), links, signs and
the mod time; then each chest, opened or not for this account; each horse
(`PLO_HORSEADD` 17); each baddy; each NPC; and the players there both ways.
The first player in leads. Players in other levels hear the new level and
position.

## What this server took over

| | Before | Now |
|---|---|---|
| Horses | not handled | kept per level, `PLO_HORSEADD` to arrivals, `PLO_HORSEDEL` when the `horse_lifetime` (10 s) is up |
| `PLI_TOALL` | dropped | to everyone else online |
| Kill claims from 1.21 to 1.35 | read as a `GSHORT` id, so dropped | one `GCHAR` id |
| Deaths | not counted | counted from the dead bit, saved as `DEATHS` |
| Alignment | never rose | +1 per `alignment_time` (1200 s) online, up to 100 |
| A leader's own mode 9 | stopped the baddy from ever coming back | starts the `baddy_respawn_time` |
| Bush regrowth | 15 s | 15 s; `rules = 1999`: 0.75 to 1.25 times `respawn_time` (10 s) |
| Kill cost | GServer-v2's | the same; `rules = 1999`: the 1999 rule above |
| A dead leader | stays leader | the same; `rules = 1999`: hands off |
| Baddy changes from a follower | kept | the same; `rules = 1999`: dropped |
| Swamp soldier and hare start modes | 0 | the same; `rules = 1999`: 6 and 7 |

Still open: the private-message reply to old builds, the nickname and head
of players in other levels, and the server-side prop limits (the anti-cheat
covers the counted ones).
