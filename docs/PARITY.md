# Parity with GServer-v2

This page lists every GServer-v2 feature area and says where this server
stands on it. It answers "can this server replace the fork for my world
yet?" and is the checklist for the differential lab runs against the fork
(seihai's `tools/difftest`). The roadmap item is #56, "Parity".

Where the original 1999 servers were read, `ORIGINAL-SERVERS.md` says what
they do and `rules = 1999` follows them.

It is written from behaviour only. The fork
(`squirelworks/seihai-gserver-v2`) was read to list its feature areas and
the client messages it acts on. No code, table or comment was taken from
it, as `AGENTS.md` requires.

## Status words

- **lab**: built, covered by tests here, and seen working with real clients in
  a recorded lab run (`docs/lab/`).
- **tests**: built and covered by loopback tests, with no real-client run yet.
- **partial**: some of the area works. The note says what is missing.
- **no**: not started. The client's messages are read and dropped
  (`message N not handled` with `--verbose`).
- **out**: out of scope for this binary. The note says where it lives.

The "Shown by" column names the test file under `tests/` (or the module
test) and the lab notes.

## Connections and accounts

| Area | Status | Shown by | Notes |
|---|---|---|---|
| Logins for every encryption generation, the allowed-versions list | lab | `login_matrix.rs`, `unserved_logins.rs`; `lab/stage1-2026-10-31.md` | |
| The login burst per era | lab | `login_load.rs`; `lab/stage1`, `lab/stage2` | |
| Flat-file accounts, saving on logout and on stop | lab | `save_on_stop.rs` | |
| Listserver link: registering, the player count, account checks | lab | `listserver_link.rs`; `lab/listserver-link-2026-10-31.md`, `lab/listlink-stage3-2026-11-01.md` | |
| Secure login (`SEI00001`) | lab | `secure_login.rs`; `lab/secure-login-2026-10-31.md` | Seihai's own. GServer-v2 has its own secure login, which is a different scheme |
| Bans: account, address, machine | tests | `items_bans.rs`, `rc_accounts.rs`, `machine_bans.rs` | Machine bans use the machine fields that 5.00+ send on the login line. Staff accounts are not kept out by a machine or address ban (#99) |
| Ping (`PLO_PING` 108 / `PLI_PONG` 162) | tests | `ping.rs` | The lab capture is open (#56, "Lab") |
| Word filter | no | | |
| UPnP port forwarding | no | | Low priority: `seihai-servertools setup` covers going public |
| Login lobby and server warp (`PLO_SERVERWARP` 178, `PLI_SERVERWARP`) for 5.x/6.x | partial | `lobby.rs` | `lobby_port` in `config/server.txt` (off by default) answers a 4.x–6.x lobby build's login with 178 to this server. `PLI_SERVERWARP` (41) asks the listserver (`SERVERINFO`) and passes its answer on as 178 (`listserver_link.rs`). No real-client run (#102) |

## World

| Area | Status | Shown by | Notes |
|---|---|---|---|
| Levels: `.nw`, `.graal`, `.zelda`; links and warps | lab | `world_levels.rs`, `level_warps.rs`; `lab/stage2-2026-11-04.md` | |
| Gmaps and adjacent levels (`PLI_ADJACENTLEVEL`) | lab | `gmaps.rs`; `lab/stage2` | Gmaps as linked single levels for pre-gmap builds are not written (`CLIENT-PROFILES.md`) |
| Signs, chests (`PLI_OPENCHEST`) | lab | `chests.rs`; `lab/stage2` | |
| Items on the ground (`PLI_ITEMADD`, `ITEMDEL`, `ITEMTAKE`) | lab | `floor_items.rs`; `lab/stage2` | |
| Player flags and server flags (`PLI_FLAGSET`, `FLAGDEL`) | lab | `lab/stage2` | Server flags come from the NPC-server (stage 5) |
| Level changes on disk reloaded | tests | `npc_messages.rs` (`LEVEL_RELOADED`) | |
| Board changes (`PLI_BOARDMODIFY`, `PLI_REQUESTUPDATEBOARD`, `PLO_BOARDMODIFY2`) | partial | `board_changes.rs` | `PLI_BOARDMODIFY` is passed on to the level and sent to later arrivals; 2x2 changes grow back after 15 s (`rules = 1999`: 7.5 to 12.5 s). On a gmap, players in that cell or next to it get 186 with the cell, arrivals and neighbour loads included. `PLI_REQUESTUPDATEBOARD` (130) gets the kept changes in its rectangle; no server's answer to 130 is known, so this is this server's choice. A level keeps at most 256 changes (#106). The board check (`anticheat.board`, off by default) refuses blocks that match none of the world's `config/boardrules.txt` (#107); the bush cut is captured from 2.05 and 2.22 (the sample world has the 2.22 line); pushing a stone on 2.22 sends no board change, and dug holes are not captured, so a world with them needs its own lines |
| Horses (`PLI_HORSEADD`, `HORSEDEL`) | tests | `original_rules.rs`, `horses.rs` | As the 1999 servers keep them (`ORIGINAL-SERVERS.md`): shown to arrivals, gone after `horse_lifetime` |
| `PLI_ENTERLEVEL` (newer builds' level entry) | no | | |
| Level groups (`gr.setlevelgroup`) | no | | |

## Players and combat

| Area | Status | Shown by | Notes |
|---|---|---|---|
| Own and other players' props, per receiver's build | lab | `other_players.rs`; `lab/five-clients-2026-11-12.md` | 1.39 draws others with no body (that lab note) |
| Chat (prop 12) | lab | `lab/five-clients-2026-11-12.md` | |
| `PLI_TOALL` (to everyone online, `PLO_TOALL` 13) | tests | `original_rules.rs` | |
| Bombs, arrows, fire spy, explosions | lab | `bombs_arrows.rs`; `lab/stage3-2026-11-05.md` | |
| Player hits, hurt and death, `PLI_CLAIMPKER` | lab | `hurt_death.rs`; `lab/hurt-2026-11-12.md` | The kill cost is GServer-v2's; `rules = 1999` takes the 1999 servers' (`original_rules.rs`). One-byte claims from 1.21 to 1.35: tests only. Deaths and alignment over time were saved for real 1.35 and 1.39 (`lab/rules-1999-2026-11-13.md`) |
| Carry and throw (`PLI_THROWCARRIED`) | no | | Its shape needs a capture of a real throw |
| `PLI_HITOBJECTS`, `PLI_SHOOT`, `PLI_SHOOT2` | no | | |
| `PLI_SHOWIMGPLAYER` | no | | |
| Private messages between players (`PLI_PRIVATEMESSAGE`) | partial | `listserver_link.rs`, `private_messages.rs` | From RCs, the listserver and players online. 1.21 to 1.35 read and write one-byte ids. A target whose build reads no 37 makes the sender get a server message, and a text over 1024 bytes an admin message (GServer 1.3). PMs to players on other servers from a game client are not sent on yet |
| Guilds (`gr.setguild` and the rest) | no | | |
| Player groups (`gr.setgroup`, `gr.setplayergroup`) | no | | |

## NPCs, baddies and scripts

| Area | Status | Shown by | Notes |
|---|---|---|---|
| Level NPCs and their props (`PLI_NPCPROPS`) | lab | `level_npcs.rs`; `lab/stage3-2026-11-05.md` | |
| Level NPC scripts per client build: GS1 text, GS2 bytecode or none | tests | `npcs.rs` unit tests | No original client has run the bytecode yet (#45) |
| Baddies (`PLI_BADDYPROPS`, `BADDYHURT`) | lab | `baddies.rs`; `lab/stage3` | `PLI_BADDYADD` is not handled |
| NPCs a client puts or deletes (`PLI_PUTNPC`, `PLI_NPCDEL`) | no | | |
| Trigger actions (`PLI_TRIGGERACTION`, `gr.*` commands) | no | | `PLO_TRIGGERACTION` needs a capture (#53) |
| Weapons: `PLI_WEAPONADD`, `PLI_TOWEAPON` | partial | `weapons.rs`, `npc_messages.rs` | `PLI_WEAPONADD` gives default weapons and level-NPC weapons (`toweapons`), kept in `weapons/` and on the account. GS1 weapons go only to builds that run GS1. `PLI_TOWEAPON` (25, 1.21 to 1.37) keeps the weapon when the world has it or an NPC in the level gives it; the uploaded script is never used (`weapons.rs`) |
| GS2 weapons and the weapon cache (`PLO_WEAPONHASH` 193, `PLI_UPDATESCRIPT` 158, 132) | lab | `weapons.rs`; `tests/weapons.rs`; lab comment on #120 | To builds that run GS2 and read 193 and 132 behind `PLO_RAWDATA` (4.04r2 to 4.20r8, the Mac 5.12): 33 with name and image, 193 with the CRC-32 of the bytecode; a 158 for a held weapon gets the bytecode in 132. Real 4.20r8 takes this server's 193 and 132, writes its cache and asks no more on the next login (lab, 2026-11-13). Not yet seen: the weapon running (the lab world had no stock art) |
| `PLO_NPCWEAPONSCRIPT` (140) | no | | GServer-v2 never sent it to 4.20r8. 5.00r7, 5.12 and 5.24 on still get no scripted weapon |
| `PLI_NPCWEAPONDEL` | no | | |
| Classes and GS2 script updates (`PLI_UPDATECLASS`, `UPDATEGANI`) | no | | `PLI_UPDATESCRIPT` is answered for weapons (above) |
| Server-side scripts (the NPC-server) | tests | `npc_link.rs`, `npc_messages.rs`, `nc.rs`; `lab/npclink-2026-10-31.md`, `lab/nc-2026-11-12.md` | A separate program, `seihai-npcserver`, runs GS1. GServer-v2 runs GS2 server scripts in-process; there is no GS2 server engine here |

## The 1999 rules (`rules = 1999`)

What the original servers do (`ORIGINAL-SERVERS.md`) and where this server stands on each rule. Real-client runs: `lab/rules-1999-2026-11-13.md` (#110).

| Rule | v2 (default) | 1999 | Status | Shown by | Notes |
|---|---|---|---|---|---|
| Horses kept per level, sent to arrivals, gone after `horse_lifetime` | same | same | partial | `original_rules.rs`; `lab/rules-1999-2026-11-13.md` | A real 1.39's dropped horse got `PLO_HORSEDEL` 10.04 s later (v2). A later arrival getting it: tests only |
| `PLI_TOALL` to everyone else online | same | same | tests | `original_rules.rs` | No real-client way to send it is known yet |
| Kill claim from 1.21 to 1.35 (one `GCHAR`) | counted, GServer-v2's cost | counted only when the victim has 10+ AP; cost (AP+10)/20 | tests | `original_rules.rs` | |
| Deaths counted from the dead bit and saved | yes | yes | lab (v2) | `original_rules.rs`; `lab/rules-1999-2026-11-13.md` | `DEATHS 1` saved for real 1.35 and 1.39 |
| Alignment +1 per `alignment_time` online, saved | yes | yes | lab | `original_rules.rs`; `lab/rules-1999-2026-11-13.md` | Saved under both rule sets |
| A leader's own mode 9 starts the baddy respawn | yes | yes | tests | `baddies.rs`, `original_rules.rs` | |
| Bush regrowth | 15 s | 0.75 to 1.25 x `respawn_time` | tests | `board_changes.rs`, `original_rules.rs` | The real-client cut was not captured in the 2026-11-13 run |
| A dead leader hands off | no | yes | tests | `original_rules.rs` | |
| Baddy changes from a follower | kept | dropped | tests | `baddies.rs` | |
| Swamp soldier and hare start modes | 0 | 6 and 7 | tests | `baddies.rs` | |
| A new nickname or head | to the level | to everyone online | tests | `original_rules.rs` | |
| Prop limits (max hearts 20, gralats 9,999,999, arrows and bombs 99, alignment 0 to 100) | the anti-cheat's counted props | the 1999 limits | tests | `original_rules.rs` | |
| The player-killer flag: prop 25 from a build before 1.3 sets status bit 0x20 for that many seconds; hearts not taken while it is on | prop 25 kept | flag and hearts rule | tests | `original_rules.rs` | Read from 1.28 beta 2. No 1.2x client has been run against it |
| A new account's AP | 0 | 50 | tests | `original_rules.rs` | The 1.3 readme |
| A second player claiming a carried NPC | kept | refused | tests | `original_rules.rs` | |
| A baddy hit with one player in the level | to the leader | dropped | tests | `rules.rs` | |

## Files and packages

| Area | Status | Shown by | Notes |
|---|---|---|---|
| `PLI_WANTFILE`, `PLI_UPDATEFILE`, large files | lab | `file_serving.rs`, `file_rtt.rs`; `lab/stage2` | |
| `PLI_VERIFYWANTSEND` | no | | |
| Packages (`.gpak`, `.gpake`, `PLI_UPDATEPACKAGEREQUESTFILE`) | partial | `gpak.rs` unit tests; real clients 2026-11-06 (`gpak.rs` notes) | Packages are served and load on 4.20r8 to 6.03r7. The update-package request is not handled |
| `foldersconfig.txt` | partial | `import_v2.rs` | Read by `import-v2` to lay out a world; not a live setting |

## Staff tools

| Area | Status | Shown by | Notes |
|---|---|---|---|
| RC logins, player list, chat, admin messages, disconnect | tests | `rc_login.rs`, `rc_players.rs` | Per-slice runs with `seihai-rc` are tracked in seihai-rc#27 |
| RC accounts, rights, comments, bans | tests | `rc_accounts.rs` | |
| RC file browser | tests | `rc_files.rs` | Move, delete and rename are not captured from a real RC |
| Server options, folder config, server flags from RC | partial | `rc_server.rs`; `lab/rc-options-2026-11-12.md` | Real RC2 2.05 reads all three. Its save dialogs crash under Wine, so saving is shown with seihai-rc only |
| NC: NPC Control | lab | `nc.rs`; `lab/nc-2026-11-12.md` | |
| `PLI_PROCESSLIST` | no | | |

## Anti-cheat

| Area | Status | Shown by | Notes |
|---|---|---|---|
| Item, heart and gralat checks, hurt range, warps | lab | `anticheat.rs`; `lab/anticheat-2026-11-12.md` | Off by default. Policy bans (which also ban the offender's machine ids) and the staff exemption came in #96 and #99 |
| `PLI_TAMPERCHECK` | no | | Not required at login, on purpose (`PLAN.md`, "Behaviour to match") |

## Elsewhere

| Area | Status | Notes |
|---|---|---|
| Graal3D (`GR3DV002`) and the Torque 3D path | out | A separate host next to `seihai-g3d` (`PLAN.md`, "Out of band") |
| The listserver itself | out | `seihai-listserver` |
| Translations | out | No plan yet |

## What parity needs next

From the rows above, in the roadmap's order:

1. **Lab runs** for the lobby, weapons and board changes, which are built
   and covered by loopback tests only.
2. **Weapons:** a real 4.20r8 running a weapon from this server (the cache
   check passed; the lab world lacked stock art). 140 for 5.24 on waits
   for a capture of the fork sending it.
3. **Board changes:** a lab capture of a dug hole, for the sample
   `boardrules.txt` (#107).
4. **Trigger actions**, once `PLO_TRIGGERACTION` has a capture.
5. Carry and throw, guilds and the word filter.

Each row moves to **lab** only with a recorded run. A differential run
against the fork is the stronger evidence: same world, same client, same
steps, and the two wire logs compared with `tools/difftest`.
