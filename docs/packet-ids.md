# Packet ids this server names

`src/wire.rs` module `ids` is the list. A layout marked there is what this
server sends or reads. Client ranges come from the Seihai notes
([packet ids](https://github.com/squirelworks/Graal_analysis/blob/main/protocol/packet-ids.md),
[the 2026-11-06 pass](https://github.com/squirelworks/Graal_analysis/blob/main/protocol/unknown-ids-2026-11-06.md)).
This file does not restate those notes.

Names follow upstream gs2lib's `IEnums.h` since 2026-11-11, as the rest of the
suite does. Seven ids had Seihai names before that; a search for the old name
still finds this table:

| Id | Name | Seihai name until 2026-11-11 |
|---|---|---|
| C→S 25 | `PLI_TOWEAPON` | `PLI_WEAPONUPLOAD` |
| C→S 162 | `PLI_PONG` | `PLI_PING` |
| S→C 104 | `PLO_FILEDECRYPTKEY` | `PLO_FILEKEY` |
| S→C 109 | `PLO_HTTPDOWNLOAD` | `PLO_DOWNLOADURL` |
| S→C 168 | `PLO_ONLINEMODE` | `PLO_LOGGEDIN` |
| S→C 193 | `PLO_WEAPONHASH` | `PLO_WEAPONCRC` |
| S→C 194 | `PLO_RESETCLIENT` | `PLO_CLEARWEAPONS` |

## Id 25 was the wrong name

25 is `PLI_TOWEAPON`: the weapon upload from 1.21 to 1.37 (name, image,
full script). `PLI_WEAPONADD` is 33, which 1.38 and later send. Both are
read (`weapons.rs`); 25's script is never used.

## Sent

| Id | Name | Layout |
|---|---|---|
| S→C 104 | `PLO_FILEDECRYPTKEY` | file name, byte 1, two GINT5; the DES key, before a `.gpake` |
| S→C 131 | `PLO_NPCBYTECODE` | behind `PLO_RAWDATA` (the count covers the id byte to the end): GINT3 NPC id, then the GS2 bytecode file, ending in the compiler's `\n`. Sent right after the NPC's `PLO_NPCPROPS`, in the same frame, to builds whose profile runs GS2; the client drops it for an NPC it has not been sent. Layout from seihai's `documentation/gs2-receive.md`; no capture of a real 131 yet |
| S→C 132 | `PLO_UNKNOWN132` | behind `PLO_RAWDATA`: `GCHAR` length and a weapon name, then the weapon's GS2 bytecode file whole, its own `\n` included, then the block's closing `\n`, which the client drops before caching the rest (a real 4.20r8 asked again on every login when the file's `\n` was cut; lab, 2026-11-13). The answer to 158 (`weapons.rs`). GServer-v2 sent this to real 4.20r8 (lab, 2026-11-13; the note gives it without framing, and `PLO_RAWDATA` is used here because the bytecode holds line feeds); core calls it `UNKNOWN132` |
| S→C 178 | `PLO_SERVERWARP` | comma text `0,name,host,port`: the lobby's own, or the listserver's `SVI_SERVERINFO` answer to a `PLI_SERVERWARP` (41) passed on unchanged |
| S→C 186 | `PLO_BOARDMODIFY2` | `GCHAR` cell column and row, then `PLO_BOARDMODIFY`'s block; no trailer |
| S→C 193 | `PLO_WEAPONHASH` | GINT5 CRC-32 (zlib) of the whole bytecode file 132 carries, then the weapon name; after the weapon's 33, which then has no script prop |

## Read

| Id | Name | Layout |
|---|---|---|
| C→S 158 | `PLI_UPDATESCRIPT` | the weapon name, as the rest of the message; answered with 132 for a weapon the player holds |

## Named, not sent yet

| Id | Name | Layout |
|---|---|---|
| C→S 162 | `PLI_PONG` | game clients: `!` and a GSHORT capped at 28671; an RC sends it empty |
| S→C 108 | `PLO_PING` | sub byte, GSHORT. Sub 0 asks for 162. Sub 1 is not sent |
| S→C 109 | `PLO_HTTPDOWNLOAD` | GINT5 mod time, local name, URL |
| S→C 140 | `PLO_NPCWEAPONSCRIPT` | header (type, name, cache flag, two GINT5 key words), then the body. GServer-v2 did not send it to 4.20r8 (lab, 2026-11-13) |
| S→C 168 | `PLO_ONLINEMODE` | empty, after the login burst |
| S→C 194 | `PLO_RESETCLIENT` | empty |
| S→C 197 | `PLO_LOADSCRIPT` | the same key and the CRC32 of the plain body; no body |

## No constant

Server to client 60, 81, 83 and 110: no client in the notes handles them.
124's layout is unknown (an RC logs that player flags arrived and reads
nothing). 133's bytes are unread.
181 is a player id and a toggle; which flag it sets is unknown. 108 sub 1
is stored by the client and nothing that reads it was traced.
