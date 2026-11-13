# Parity with GServer-v2

This page lists every GServer-v2 feature area and says where this server
stands on it. It answers "can this server replace the fork for my world
yet?" and is the checklist for the differential lab runs against the fork
(seihai's `tools/difftest`). The roadmap item is #56, "Parity".

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
| Login lobby and server warp (`PLO_SERVERWARP` 178, `PLI_SERVERWARP`) for 5.x/6.x | no | | Stage 6. The five-client run used a lab lobby in front of this server |

## World

| Area | Status | Shown by | Notes |
|---|---|---|---|
| Levels: `.nw`, `.graal`, `.zelda`; links and warps | lab | `world_levels.rs`, `level_warps.rs`; `lab/stage2-2026-11-04.md` | |
| Gmaps and adjacent levels (`PLI_ADJACENTLEVEL`) | lab | `gmaps.rs`; `lab/stage2` | Gmaps as linked single levels for pre-gmap builds are not written (`CLIENT-PROFILES.md`) |
| Signs, chests (`PLI_OPENCHEST`) | lab | `chests.rs`; `lab/stage2` | |
| Items on the ground (`PLI_ITEMADD`, `ITEMDEL`, `ITEMTAKE`) | lab | `floor_items.rs`; `lab/stage2` | |
| Player flags and server flags (`PLI_FLAGSET`, `FLAGDEL`) | lab | `lab/stage2` | Server flags come from the NPC-server (stage 5) |
| Level changes on disk reloaded | tests | `npc_messages.rs` (`LEVEL_RELOADED`) | |
| Board changes (`PLI_BOARDMODIFY`, `PLI_REQUESTUPDATEBOARD`) | no | | Bushes, rocks and digging go back to normal on the client only |
| Horses (`PLI_HORSEADD`, `HORSEDEL`) | no | | |
| `PLI_ENTERLEVEL` (newer builds' level entry) | no | | |
| Level groups (`gr.setlevelgroup`) | no | | |

## Players and combat

| Area | Status | Shown by | Notes |
|---|---|---|---|
| Own and other players' props, per receiver's build | lab | `other_players.rs`; `lab/five-clients-2026-11-12.md` | 1.39 draws others with no body (that lab note) |
| Chat (prop 12) | lab | `lab/five-clients-2026-11-12.md` | |
| Bombs, arrows, fire spy, explosions | lab | `bombs_arrows.rs`; `lab/stage3-2026-11-05.md` | |
| Player hits, hurt and death, `PLI_CLAIMPKER` | lab | `hurt_death.rs`; `lab/hurt-2026-11-12.md` | |
| Carry and throw (`PLI_THROWCARRIED`) | no | | Its shape needs a capture of a real throw |
| `PLI_HITOBJECTS`, `PLI_SHOOT`, `PLI_SHOOT2` | no | | |
| `PLI_SHOWIMGPLAYER` | no | | |
| Private messages between players (`PLI_PRIVATEMESSAGE`) | partial | `listserver_link.rs` | From RCs and the listserver; player-to-player PMs are not relayed yet |
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
| Weapons: `PLI_WEAPONADD`, `PLI_TOWEAPON` | partial | `quiet_weapon_ids.rs` | Read and not answered. Default weapons and NPC-server weapons are sent (`npc_messages.rs`) |
| Weapon scripts and the weapon cache (`PLO_NPCWEAPONSCRIPT` 140, `PLO_WEAPONHASH` 193) | no | | Named in `packet-ids.md`, not sent |
| `PLI_NPCWEAPONDEL` | no | | |
| Classes and GS2 script updates (`PLI_UPDATECLASS`, `UPDATESCRIPT`, `UPDATEGANI`) | no | | |
| Server-side scripts (the NPC-server) | tests | `npc_link.rs`, `npc_messages.rs`, `nc.rs`; `lab/npclink-2026-10-31.md`, `lab/nc-2026-11-12.md` | A separate program, `seihai-npcserver`, runs GS1. GServer-v2 runs GS2 server scripts in-process; there is no GS2 server engine here |

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

1. **Stage 6:** the login lobby and server warp. Without them, 5.x and 6.x
   clients need a lab lobby in front of this server.
2. **Weapons:** `PLI_WEAPONADD`, weapon scripts and the weapon cache. Most
   worlds rely on weapons.
3. **Board changes:** `PLI_BOARDMODIFY` and `PLI_REQUESTUPDATEBOARD`.
4. **Trigger actions**, once `PLO_TRIGGERACTION` has a capture.
5. Carry and throw, horses, guilds and the word filter.

Each row moves to **lab** only with a recorded run. A differential run
against the fork is the stronger evidence: same world, same client, same
steps, and the two wire logs compared with `tools/difftest`.
