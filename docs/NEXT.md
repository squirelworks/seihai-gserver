# Roadmap status (issue #56)

Working mirror of [issue #56](https://github.com/squirelworks/seihai-gserver/issues/56)
against `main` at `571b2bb` (after #152). Tick boxes when the matching PR
merges. Lab items stay open until a real-client write-up lands under
`docs/lab/`.

## Where it stands

- **Stages 1–5** and **anti-cheat** are done and lab-run (see #56 body).
- **Stage 6** is built (#102, #98, #120, #121). Wire evidence: 5.24r4 and
  6.03r7 reached the game port through lobby `PLO_SERVERWARP` (178) in
  `docs/lab/five-clients-2026-11-14.md`. On-screen confirmation on a setup
  that draws (real Windows, or Wine with the base asset set) is still open.
- **Mixed-version worlds (#45):** design and profile plumbing are in
  (`docs/CLIENT-PROFILES.md`, #114, #118, #98). The five-client same-level
  lab checkbox on #56 remains open for a draw-confirmed rerun.
- **Dependencies:** direct `seihai-core` is `v0.4.0`; the temporary
  `[patch]` is gone (#151). NPC-server and RC pins already sit on that core
  (seihai-core#79).
- **Early OTHERPLPROPS (#132):** done. Builds before 1.33 get their
  own-prop set plus 50/51, with join (50) first, and prop 30 comes from the
  connection. A real 1.28 beta 2 draws a real 1.35 (lab 2026-11-18,
  `docs/lab/early-otherplprops-2026-11-18.md`).

## Open checkboxes

### Code / docs (non-lab)

- [ ] **Parity rows:** work through `docs/PARITY.md` `no` and `partial`
      entries (code or docs per row), then differential runs against the
      fork with seihai's `tools/difftest`.

### Lab (needs a lab VM / real clients)

- [ ] **#45:** five clients (1.39, 2.22, 2.31, 5.24, 6.03r7) in one level
      with draw-confirmed 5.x/6.x cells (asset staging on Wine, or a
      rendering Windows guest).
- [ ] **Stage 6 visual:** a real 5.x/6.x client through the lobby and
      server warp on a setup that draws.
- [ ] **Ping capture:** real 4.20r8 and 6.03r7 on loopback, 108 out and
      162 back with the same value, and no 108 to a 5.x client; write-up
      in Graal_analysis (moved here from #44).
- [ ] Re-run seihai-servertools' suite stress as stages land.

## Relay-era listener

An optional `relay` subcommand adds the account-free line/room/peer
transport separately from the central world server. Generated stream
regressions cover same-room pairing, unchanged peer data, explicit room
isolation and cleanup; they do not establish native client compatibility.
The first native acceptance candidate is Java Graal and **4th Graal alpha**
in the same `G2` level room. The historical Java/5th Graal result used a
room-renaming proxy; this listener does not rename rooms. Original-client
Windows admission remains separate. See
[ANCIENT-NETWORKING.md](ANCIENT-NETWORKING.md). Relay support does not
close #132.

## Research boundaries (unchanged)

- **Type 4 / generation 4:** login tokens and a synthetic framing contract
  exist; they do not establish an original-client exchange. Original
  2.19–2.21 clients are lost. Early 3.x artifacts must be traced on their
  own before any capture is proposed.
- **Profile overrides:** `src/profile.rs` keeps server-only decisions and
  recorded overrides (`docs/CLIENT-PROFILES.md`). Moving a field into core
  needs matching client and server semantics plus a consumer audit.
- **Level NPC output:** `Npc::from_level` still uses `half_tiles`; that
  does not settle native X2/Y2 versus X/Y precedence. Image-extension
  policy when only one file exists still needs a capture.
- **Lowered flags:** `world.rs` `client.gs1_` stripping and
  `tests/gs1_flags.rs` are an intentional compatibility bridge; keep them
  unless evidence justifies a change.

## Not in this repo

Graal3D hosting (G3D-9) is a separate host next to `seihai-g3d`, not a
stage of this binary.
