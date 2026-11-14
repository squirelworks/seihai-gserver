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
**baddy respawn time** (60 s) and, on 1.3, **alignment increment time**
(1200 s). 1.28 beta 2 has a **player-killer flag timeout** (`pktimeout`)
in that place instead; it reads the setting and never uses it (see
"The player-killer flag" below).

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
| `PLI_BADDYHURT` (16) | To the leader as `PLO_BADDYHURT` (27), when the level has two players or more. |
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
taken while status bit 0x20 is on and the player has hearts left; see
"The player-killer flag"), gralats 9,999,999, arrows and bombs 99, alignment 0 to
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

## The player-killer flag (before 1.3)

Read from GServer 1.28 beta 2. The 1.27 readme lists "a new player
killing / alignment system"; the 1.28 beta 2 readme says the new
alignment system is not working yet ("no pk flags anymore"), and 1.3
brought alignment points. The servers before 1.3 kept a flag for it:

- **Prop 25 is the flag's time.** A `GSHORT` count of seconds from the
  player. Any count but 0 sets status bit 0x20 (prop 18), and the player
  (9) and the level (8) are told. The same count sent again starts it
  again.
- **Once a second** the count goes down; when it runs out, bit 0x20 is
  cleared and the player and the level are told again.
- **Hearts.** While bit 0x20 is on and the player has hearts left, the
  hearts the player sends (prop 2) are not taken. A player at 0 hearts
  is taken as before. GServer 1.3 keeps this check, but from 1.3 prop 25
  is the alignment clock (a count below 0 restarts it at the alignment
  time), so nothing on that server sets the bit.
- The `pktimeout` setting (300 s in the October 1999 server's
  `serverconfig.txt`) is read at start and never used: the time is the
  client's.

## The earlier servers

Three more servers from 1999 are in the archive, read only from their
strings and message switches, not run:

| Server | Builds it lets in | What differs |
|---|---|---|
| GServer from the 1.25 installer (August 1999) | `GSERV007` to `GSERV010`; "You need Graal version 1.2 rev1" | The same settings as 1.28 beta 2 (`pktimeout` included). |
| GServer of 20 October 1999 (also in the 1.27 installer) | `GSERV007` to `GSERV011`; "You need at least Graal version 1.27" | The same settings, with `pktimeout = 300` and `baddyrespawntime = 30` shipped. Handles 0 to 13, 15 to 29 and 51 to 64: no kill claim (14) and no 30. **Unlike 1.28 beta 2 and 1.3 it passes bombs, arrows, fire, thrown objects and floor items on to the level** (client messages 4, 5, 9 to 13, 24 and 27). |
| GServer from the 1.1.9 package | | No game settings in its dialog. |

So the "not handled at all" list above is new in 1.28 beta 2: the
October server passed those on, as later servers and this one do.

## What the 1.3 readme adds

- "You start at 50": a new account's alignment is 50.
- A player with 100 AP cannot be hurt by swords or NPC weapons, and a
  name is coloured by its alignment. Both are the client's.

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
| Private messages between players | not relayed | relayed; old builds and long texts answered as on GServer 1.3 |
| A new nickname or head | to the level | the same; `rules = 1999`: to everyone online |
| Prop limits | the anti-cheat's counted props | the same; `rules = 1999`: the limits below |
| Prop 25 from a build before 1.3 | kept | the same; `rules = 1999`: the player-killer flag above, with its hearts rule |
| A new account's AP | 0 | the same; `rules = 1999`: 50 |
| A second player claiming a carried NPC | kept | the same; `rules = 1999`: refused, sprite cut to its direction, both props sent back |
| A baddy hit in a level with one player | to the leader (the sender) | the same; `rules = 1999`: dropped |
| `PLI_PUTNPC` (21), `PLI_NPCDEL` (22) | dropped | `rules = 1999` or `put_npcs = yes`: the NPC with its script file from `files/`, to the level and later arrivals; 22 takes away only an NPC a client put (`src/putnpc.rs`) |

## Private messages, nicknames and prop limits

Read from GServer 1.3 and its December 1999 Linux port; 1.28 beta 2
matches where it was read.

- **Private messages.** A count, that many player ids and the text, each
  id one byte for the builds of the time. Every listed player online gets
  37 with the sender's id and the text; ids not online are passed over. A
  text over 1024 bytes is refused, and the sender gets an admin message
  (35), `Server message:`, then `Your message was too long.` When a
  listed player's build is too old to read a private message, the sender
  gets 37 with that player's id and `"Server message:","The player cannot
  read the message because he/she has an older Graal version."` (both
  servers turn such builds away at login, so in 1999 this never ran).
  This server does all of it in either rule set, with 1.21 as the build
  that reads no 37, and writes 37 to a one-byte-id build as the 1999
  servers did: the id and the text, without GServer-v2's labels.
- **Nickname and head.** A new nickname (prop 0) or head (11) goes as 8
  to every player online, not only to the level. Under `rules = 1999`
  this server does the same.
- **Prop limits.** Under `rules = 1999` this server cuts these down as the
  1999 servers do. The player is told (9) for max hearts over 20, hearts
  over the max, arrows and bombs over 99 and magic over 100. Without
  telling: a sword power from 5 to 9 becomes 4, a colour past 18 becomes
  18, a sprite from 132 becomes 0, and AP past 100 becomes 100. Glove and
  bomb power have no limit. Gralats stop at 9,999,999 there, more than a
  `GINT3` holds, so nothing changes here.

Not taken over: the 1999 servers drop a player who sends an unknown prop
index, and the Linux port keeps a nickname's guild only when its guild
list verifies it. Mixed-version worlds send props an old build may not
know, so the first would drop honest players; there is no guild list yet
for the second.

