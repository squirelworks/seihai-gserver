# The per-connection client profile table (design, issue #45)

The world keeps one model of players, levels and NPCs. Each connection
writes that model out in the form its client build reads. This page lays
out the table those writes read from: what each row decides, where its
value comes from, who reads it today, and what is still a version check
somewhere else. It is a design. The only code that exists so far is
`src/profile.rs` and the `seihai-core` profiles it reads.

## Shape

At login, the connection resolves its build once (`ClientVersion`, or for
an unknown token the stand-in from `burst::shape_version`). It keeps one
value for the rest of the session:

```rust
/// Everything this connection's writes depend on. Built once at login.
pub struct ClientProfile {
    pub version: ClientVersion,
    /// The shared table: packet ids, props, files, levels, scripts.
    pub core: &'static seihai_core::protocol::VersionProfile,
    /// Server-only rules and lab overrides (`src/profile.rs`).
    pub server: ServerProfile,
}
```

`ServerProfile::of(version)` already exists. `ClientProfile` adds the
`&'static` core row next to it, so a writer is handed one value instead of
calling `version.profile()` again at each site (there are about 50 such
calls today). Code outside `profile.rs` never compares versions. When it
needs something new, a field is added: to `seihai-core` when it is a fact
about the client, and to `ServerProfile` when it is a server policy or a
lab override waiting to move into core.

There is one rule for overrides, the same as today: each override in
`ServerProfile` names the lab run that found it, and a unit test pins
every field to the core value except where an override is listed. When
core takes the correction, the override is removed.

## The rows

The **Source** column says where the value comes from: `core` is
`VersionProfile` in `seihai-core`, and `server` is `ServerProfile` here.
**Status** is one of these:

- **done:** every write goes through the field.
- **partial:** some sites still decide the same thing another way.
- **todo:** no field yet.

| Row | Decides at write time | Source | Read by today | Status |
|---|---|---|---|---|
| Packet ids | which server-to-client ids the build handles; others are dropped or swapped for a near one | core `protocol.plo` | `world.rs`, `burst.rs`, `files.rs`, `others.rs`, `ping.rs`, `server.rs` | done |
| Own props | which own-player props, and how many (below 2.1: the first 37) | core `props.own` | `others.rs`, `burst.rs` | done |
| Other players' props | which props of others, prop 10 as bow or gani | core `props.other`, server `prop10_bow` (2.05 override) | `others.rs` | done |
| NPC props | ids up to `npc_max`, prop 12 in the build's shape | core `props.npc_max`, `props.bow_prop10` | `npcs.rs` | partial: `npcs.rs` reads `bow_prop10` from core directly, not through `ServerProfile`, so the 2.05 override does not reach NPCs |
| Positions | half-tile X/Y before 2.3, pixel X2/Y2 from 2.3 | core `props.precise_pos` | `others.rs`, `burst.rs` | partial: level NPCs are always written as half tiles (`npcs.rs`, `half_tiles`) |
| Image names | `.gif` before 2.1, `.png` after; baddy images | core `files.image_ext`, server `baddy_image_ext` (2.05 override) | `baddies.rs`, `files.rs` | partial: NPC and player image props pass the world's name through unchanged |
| Files | `PLO_FILE` mod time, large files | core `files.*`, server `file_modtime` (2.05 override) | `files.rs` | done |
| Packages | `.gpak` / `.gpake` | server `loads_gpak`, `loads_gpake` | `gpak.rs` | partial: a build list in `profile.rs` (`GPAKE`), not a core field. Move it to core `files` once the 4.20r8 lab result is recorded there |
| Levels | which board form (GR-V1.0x, NW); gmaps as gmaps or as linked single levels | core `levels.*`, `GmapRules` | `levels.rs`, `gmaps.rs` | partial: `gmaps::plays_gmaps` reads `GmapRules`, not the profile row. Single-level gmap output for pre-gmap builds is not written yet |
| Frames | gen 5 long frames, round-down | core `protocol.gen5_*` | `codec.rs`, `server.rs` | done |
| Login burst | the burst per era, lobby or not | core `protocol.*`, `burst::shape_version` | `burst.rs`, `login.rs` | done |
| **Scripts** | GS1 text, GS2 bytecode or a stub, per NPC | core `scripts` through `seihai_core::scripting::delivery` | nothing yet: every build gets the GS1 text in prop 1 | todo (see below) |

## The scripts row

Today `Npc::from_level` (`src/npcs.rs`) puts the level NPC's GS1 source in
prop 1 for every receiver. A 5.x or 6.x client is sent GS1 text, which it
does not run. Pre-2.1 builds are also sent 2.1 GS1, which they cannot
parse.

seihai-core#21 adds `seihai_core::scripting::delivery`:

- `Prepared::gs1(source, era)` compiles the script to GS2 bytecode once.
- `Prepared::for_client(version)` returns a `Choice`, which is a
  `Delivery` plus a `confirmed` flag. The `Delivery` is one of:
  - `Gs1Text(&str)` when the build runs GS1 and the script parses in its
    vocabulary.
  - `Gs2Bytecode(&[u8])` when the build runs GS2 (it handles
    `PLO_NPCBYTECODE`, 3.153 on) and the script compiled.
  - `Stub(Why)` when neither fits.
- `confirmed` is false when the choice rests on an `unknown` in the
  profile (2.1 to 2.16, 3.04).

The plan for the server:

1. **Keep the source and the compiled form apart.** A level NPC stores a
   `Prepared` next to its props. Prop 1 is not stored as wire bytes any
   more; it is written per receiver.
2. **Write per receiver.** In `Npc::message_on(receiver, …)`, the script
   is written from `prepared.for_client(receiver.version)`:
   - `Gs1Text`: prop 1 as today.
   - `Gs2Bytecode`: no prop 1. After the props, send `PLO_NPCBYTECODE`
     (131) with the NPC id and the bytes. The layout is a wire fact and
     goes in `docs/packet-ids.md` before any code.
   - `Stub`: no script. Log the `Why` once per NPC and build, so a server
     owner can see which builds do not get the script.
3. **Compile once, when the level is first read.** Compile errors go to
   the server log with the level and NPC. The NPC still goes to the GS1
   builds that read the source.
4. **Keep the client-set rule.** A client still may not set prop 1 (`SERVER_ONLY`).
   Bytecode is only ever sent by the server.
5. **Server NPCs stay as they are.** NPCs from the NPC-server keep their
   script on the NPC-server (`Npc::from_link`).
6. **Bump the core tag.** The gserver pins seihai-core `v0.0.11`. The
   delivery module ships in the first tag after seihai-core#21. Bumping
   brings v0.0.12 to v0.0.14, so the bump is its own PR with the test
   suite run.

This row's acceptance is a lab run, not a unit test. Put one door NPC
(the script in the `delivery` tests) in a level. Then 1.39, 2.22, 2.31,
5.24r4 and 6.03r7 join together, and each one opens the door. That is the
same five-client setup as `docs/lab/five-clients-2026-11-12.md`. The
5.24r4 and 6.03r7 results depend on those builds drawing under Wine,
which that run did not manage. Until then, a wire check is the evidence:
`PLO_NPCBYTECODE` arrived and the client answered with the expected
`PLI_NPCPROPS`.

## Order of work

1. **`ClientProfile` at login.** Pass it to writers instead of the
   `ClientVersion`. This is mechanical and changes no bytes; the existing
   tests show that.
2. **Fix the partial rows.** NPC prop 12 through `ServerProfile`. Level
   NPC positions per `precise_pos`. Image extensions on NPC and player
   image props. Move the package list into core. Each is a small change
   with its own test.
3. **The scripts row.** First the tag bump, then steps 1 to 5 above, then
   the lab run.
4. **Gmaps as single levels** for pre-gmap builds. This is the largest
   row, and it needs its own design: the cell links, warps at the edges,
   and what an old client sees of players in the next cell.

## Known limits

- GS2-only scripts (GUI, classes, objects) have no GS1 form. Builds before
  3.153 get a stub for them.
- 3.x Torque clients do not join the 2D world.
- A GS1 construct the compiler refuses gets a stub on GS2-only builds. The
  list is `gs1_command_lowering` in seihai-core and the open items on
  seihai-core#6.
