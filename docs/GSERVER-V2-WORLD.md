# GServer-v2 world folders: the format, and how they move over

Roadmap #56: read GServer-v2-style world folders, so existing worlds move
over. This note describes the folder from its **sample data files only**,
the ones a GServer-v2 checkout ships under `bin/servers/default/`. No
source, script or build file of the fork was opened for it, and nothing
from the fork is copied into this repo. Each key below names the sample
file it was seen in. Where the samples do not say what a field means, this
note says so and the importer leaves the field alone.

`seihai-gserver import-v2` (`src/import.rs`) is built from this note.

## Folder layout

Seen in the sample server folder (`bin/servers/default/`):

| Path | What it holds | Seen in |
|---|---|---|
| `config/serveroptions.txt` | server settings, `key = value` | the file itself |
| `config/allowedversions.txt` | allowed client version tokens | the file itself |
| `config/foldersconfig.txt` | which files under `world/` a client may get, by kind | the file itself |
| `config/adminconfig.txt`, `config/ipbans.txt`, `config/rules.txt`, `config/rcmessage.txt`, `config/rchelp.txt`, `config/servermessage.html` | hub, IP bans, word rules, RC texts | file names (the bans and rules samples are empty) |
| `accounts/<account>.txt` | one account per file | `defaultaccount.txt`, `YOURACCOUNT.txt`, `(npcserver).txt` |
| `accounts/defaultaccount.txt` | the template a new account starts from | `bin/readme.txt`, step 2 |
| `world/` | levels, images, sounds, ganis and the rest, in any subfolder | `world/readme.txt` |
| `weapons/`, `npcs/`, `guilds/`, `logs/`, `translations/`, `documents/`, `serverflags.txt` | weapons, database NPCs, guilds, logs, texts, server flags | file names only |

## `config/serveroptions.txt`

One `key = value` per line. `#` starts a comment line; some values have a
tab and a `#` comment after them (the `triggerhack_*` lines). A key with a
commented-out line (`#startlevel = ...`) is unset. Doubled `''` inside a
comment is an apostrophe and does not matter here.

Keys the importer carries over, and where they go:

| Key | Meaning (from the sample's own comments) | Ours (`config/server.txt`) |
|---|---|---|
| `name` | the name in the server list | `name` |
| `description` | the description in the server list | `description` |
| `serverport` | the port players connect to | `port` |
| `maxplayers` | the most players at once | `max_players` |
| `startlevel`, `startx`, `starty` | where new players start; unset means "use `defaultaccount.txt`'s" | `start_level`, `start_x`, `start_y`, from `defaultaccount.txt`'s `LEVEL`, `X`, `Y` when unset |
| `playerlisticons` | the status names in the player list | `status_list` |
| `staff` | accounts that may use RC; `(Manager)` and the like are placeholders, not accounts | `STAFF 1` in each named account file; names in brackets are skipped |

Everything else is listed by the importer as not carried over, so the
admin sees what to set by hand. In particular `listip`/`listport` (this
server's `listserver` takes a host and port, and the admin should choose it
on purpose), `serverip`, `localip`, `serverinterface` and the many
gameplay switches this server does not have yet.

## `config/allowedversions.txt`

One entry per line. `//` starts a comment, also after an entry
(`GNW13110	// 1.41r1`). An entry is a version token (`GNW22122`) or a range
`FROM:TO`. In the sample every entry is commented out, which allows
nothing.

Ours uses `#` for comments and already reads tokens and `FROM:TO` ranges
(`src/allow.rs`). The importer rewrites `//` comments as `#` comments and
keeps every entry and every commented-out entry where it was.

## `accounts/<account>.txt`

The first line is `GRACC001`. Then one `KEY value` per line; a value may be
empty (`BOW `, `BANREASON `). `FOLDERRIGHT` and `WEAPON` repeat. There is
no password line in any sample (`bin/readme.txt` has accounts checked by
the listserver). Seen in `defaultaccount.txt` and `YOURACCOUNT.txt`:

| Key | Sample value | Ours |
|---|---|---|
| `NAME` | `YOURACCOUNT` | the file name; the importer uses the file name |
| `NICK` | `unknown` | `NICK` |
| `COMMUNITYNAME` | `YOURACCOUNT` | not carried |
| `LEVEL`, `X`, `Y` | `onlinestartlocal.nw`, `30.00`, `30.50` | `LEVEL`, `X`, `Y` |
| `Z` | `0.00` | not carried |
| `MAXHP`, `HP` | `3.00`, `3.00` | `MAXHP` (whole hearts), `HP` |
| `RUPEES`, `ARROWS`, `BOMBS` | `0`, `10`, `5` | `RUPEES`, `ARROWS`, `BOMBS` |
| `GLOVEP` | `1` | `GLOVE` |
| `SWORDP` and `SWORD` | `1`, `sword1.png` | `SWORD 1 sword1.png` |
| `SHIELDP` and `SHIELD` | `1`, `shield1.png` | `SHIELD 1 shield1.png` |
| `HEAD`, `BODY` | `head0.png`, `body.png` | `HEAD`, `BODY` |
| `COLORS` | `2,0,10,4,18` | `COLORS` |
| `SPRITE`, `STATUS` | `2`, `21` | `SPRITE`, `STATUS` |
| `AP` | `50` | `ALIGN` (alignment points, player prop 32) |
| `KILLS` | `0` | `KILLS` |
| `EMAIL` | empty | `EMAIL` |
| `COMMENTS` | empty | `COMMENT` (one line) |
| `IPRANGE` | `*.*.*.*`, or `0.0.0.0` in the template | `IPRANGE`; `0.0.0.0` is left out (ours: loopback only) |
| `FOLDERRIGHT` | `rw world/levels/*`, repeated | `FOLDER`; see below |
| `LOCALRIGHTS` | `1040383` (the staff sample), `0` | `RCRIGHTS` |
| `BANNED`, `BANREASON` | `0`, empty | a line in our `config/bans.txt` when `BANNED` is not 0 |
| `ANI`, `BOWP`, `BOW`, `MP`, `APCOUNTER`, `ONSECS`, `IP`, `LANGUAGE`, `DEATHS`, `RATING`, `DEVIATION`, `LASTSPARTIME`, `WEAPON`, `BANLENGTH`, `LASTFOLDER` | | not carried; listed by the importer |

`BANLENGTH` is empty in both samples, so its unit is not known, and an
imported ban has no end. The import reports it when it is set.

### Folder rights

`FOLDERRIGHT` paths are relative to the server folder (`world/levels/*`,
`config/*`). Ours are relative to our world folder, where levels live in
`levels/` and other client files in `files/`. The importer rewrites
`world/levels/...` to `levels/...` and keeps every other path as it is
(`config/*` and `accounts/*` mean the same folders in both).

## The `world/` folder

`world/readme.txt`: every level, image, sound and script goes in `world/`
or a subfolder of it. `config/foldersconfig.txt` then says which ones a
client may get, one `kind pattern` per line: `level *.nw`,
`level levels/*.graal`, `file images/*.png`, `head heads/*`,
`body bodies/*.png`, `sword swords/*`, `shield shields/*`, `file global/*`.
Patterns are relative to `world/`.

Moving `world/` over (levels to `levels/`, the rest to `files/`, with the
folders config as the guide) is the next slice and is not done by
`import-v2` yet; the importer says so.
