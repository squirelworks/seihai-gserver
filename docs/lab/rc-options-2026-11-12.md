# RC server options, folder config and server flags, 2026-11-12 (issue #52)

seihai-rc reads and saves the server options, folder config and server
flags on seihai-gserver (#89). Real RC2 2.05 reads all three, but under
Wine its dialogs crash before it can save, so the save layouts from a real
RC are still unconfirmed. The run was loopback only, inside a network
namespace with only `lo`, and every frame went through the decrypting
proxy. No public server and no real player account were used.

## Setup

- Server: #89's branch (c208304), release build, throwaway world, staff
  account `labrc`.
- Clients: seihai-rc `main` (443415d), and real RC2 2.05 (`GSERV025`)
  under 32-bit Wine.

## seihai-rc: all pass

| Request | Read | Save | Stored in |
|---|---|---|---|
| Server options | 51 → 76: `server.txt` as written, 95 lines | 52 with a changed description and `bogus_setting = 7`. It read back equal. RC chat got `server.txt line 96: unknown setting` and "saved, applies when the server restarts" | `config/server.txt` |
| Folder config | 53 → 77: empty at first | 54 with 3 lines. It read back equal | `config/foldersconfig.txt` |
| Server flags | 68 → 61: empty at first | 69 with `labflag=1`, `quest.done`, `multi=a,b`. It read back the same, sorted. An empty set gave "3 flag(s) before, 0 after" | `config/serverflags.txt` |

Wire layouts:
- 51, 53 and 68 are sent with an empty payload.
- 76/52 and 77/54 carry the file as one gtokenized line.
- 61/69 carry a GSHORT count, then a GCHAR length and the text for each
  flag.
- seihai-rc sends 51 just before each 52.

## Real RC2 2.05 under Wine

- Login, its local Options window and the File Browser all work.
- Server Flags, Folder Config and Server Options each send their read (68,
  53, 51, all empty, the same as seihai-rc) and get the reply. Then each
  one crashes with a page fault inside a Wine DLL.
- An empty flag list crashes the same way, so the crash is Wine's dialog
  and not the reply.
- 52, 54 and 69 from a real RC still need a run on real Windows.

## Found

- The login line called RC logins `GSERV025 unknown build`. It now says
  `RC` (`login::build_label`).
