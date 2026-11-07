# Packet ids this server names

`src/wire.rs` module `ids` is the list. A layout marked there is what this
server sends or reads. Client ranges come from the Seihai notes
([packet ids](https://github.com/squirelworks/Graal_analysis/blob/main/protocol/packet-ids.md),
[the 2026-11-06 pass](https://github.com/squirelworks/Graal_analysis/blob/main/protocol/unknown-ids-2026-11-06.md)).
This file does not restate those notes.

## Id 25 was the wrong name

25 is `PLI_WEAPONUPLOAD`: the weapon upload from 1.21 to 1.37 (name, image,
full script). `PLI_WEAPONADD` is 33, which 1.38 and later send. Both are
accepted and not answered until weapons are a stage, and neither is logged
as unhandled.

## Named, not sent yet

| Id | Name | Layout |
|---|---|---|
| C→S 158 | `PLI_UPDATESCRIPT` | the script name; the client asks when its cache does not match |
| C→S 162 | `PLI_PING` | game clients: `!` and a GSHORT capped at 28671; an RC sends it empty |
| S→C 104 | `PLO_FILEKEY` | file name, one byte, two GINT5; the DES key, sent before the file |
| S→C 108 | `PLO_PING` | sub byte, GSHORT. Sub 0 asks for 162. Sub 1 is not sent |
| S→C 109 | `PLO_DOWNLOADURL` | GINT5 mod time, local name, URL |
| S→C 140 | `PLO_NPCWEAPONSCRIPT` | header (type, name, cache flag, two GINT5 key words), then the body |
| S→C 168 | `PLO_LOGGEDIN` | empty, after the login burst |
| S→C 193 | `PLO_WEAPONCRC` | GINT5 CRC32, weapon name |
| S→C 194 | `PLO_CLEARWEAPONS` | empty |
| S→C 197 | `PLO_LOADSCRIPT` | the same key and the CRC32 of the plain body; no body |

## No constant

Server to client 60, 81, 83 and 110: no client in the notes handles them.
124's layout is unknown (an RC logs that player flags arrived and reads
nothing). 132 and 133 are a script-registration area whose bytes are unread.
181 is a player id and a toggle; which flag it sets is unknown. 108 sub 1
is stored by the client and nothing that reads it was traced.
