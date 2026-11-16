# Relay-era listener

An optional `relay` subcommand adds the account-free line/room/peer transport
separately from the central world server. Generated stream regressions cover
same-room pairing, unchanged peer data, explicit room isolation and cleanup;
they do not establish native client compatibility. The first native acceptance
candidate is Java Graal and **4th Graal alpha** in the same `G2` level room.
The historical Java/5th Graal result used a room-renaming proxy; this listener
does not rename rooms. Original-client Windows admission and #132 remain
separate. See [ANCIENT-NETWORKING.md](ANCIENT-NETWORKING.md).

# Current dependency follow-through

## Research boundaries: intermediate clients and profile gaps

- **Type 4 / generation 4:** `src/login.rs` accepts the corresponding
  tokens and `src/codec.rs` implements a synthetic framing contract. Its
  tests do not establish an original-client exchange. Original 2.19–2.21
  clients are lost; no executable-dependent native experiment for those
  builds is ready. Historical server records and adjacent versions can
  constrain hypotheses, but cannot establish their native framing. Early
  3.x artifacts must be identified and traced independently before any
  capture is proposed; a result for one must not be transferred to the
  lost builds. Type-4 login framing is separate from Torque UDP gameplay.
  No default admission or framing behavior is changed by this note.
- **Profile overrides:** `src/profile.rs` combines shared core facts with
  server-only decisions and recorded overrides. Its tests deliberately
  pin the 2.05 file-time, image and prop-10 differences and the pre-switch
  ping exceptions. Moving a field into core requires matching client and
  server semantics plus a consumer audit; duplication alone is not proof
  that the override should be removed. See `docs/CLIENT-PROFILES.md`.
- **Level NPC output:** `Npc::from_level` in `src/npcs.rs` still converts
  coordinates through `half_tiles`; this does not establish the native
  precedence of NPC X2/Y2 versus X/Y. NPC image names still need a
  world-asset policy when only one extension exists. Capture those cases
  independently before changing serialization or rewriting image names.
- **Lowered flags:** `world.rs`'s `client.gs1_` stripping and
  `tests/gs1_flags.rs` are an explicit compatibility bridge for compiled
  GS1 flags. Preserve that contract unless evidence justifies an
  alternative; the prefix itself is not a demonstrated defect.

These are source-level findings at the post-#140 / post-#141 baseline, not
new native observations. #132's introduction ordering/property-layout
question remains open, separately from these profile and framing questions.


## Core v0.4.0 consumer update

Direct core is published `v0.4.0` (`eb7434ac1fbddc35d127ab4bbe8717e8bf1d64e7`).
The temporary `www.github.com` `[patch]` is gone: NPC-server is pinned at
`f0904910f581fb2f16b71dcc7b6e0b7f74b0bcb0` and RC at
`90cb87f31bfb9f81490aa7e6c8c6e082ecbab75d` (after seihai-rc#61), both already
on core `v0.4.0`, so the lock has one core without a patch
(squirelworks/seihai-core#79). Preserve the merged chat/trigger and
staff-line routing.

The source audit found no exhaustive match on core's scripting `Era` in
this consumer: level NPC preparation and NC fixtures select `Era::V217`;
the exhaustive matches in `rules.rs` concern the separate local rules enum.
Registering `V140` does not establish original-client behavior or script
argument semantics. Fresh Linux/Windows CI on this PR's resulting head is
required; earlier pin-update CI below is historical evidence only. No local
heavy compilation or new original-client run is claimed.

## Core pin update (#138)

The paired manifest and lockfile update moves `seihai-core` from tag
`v0.1.0`, commit `708e9e4c19e45bc3db85528e37013921fedb1f10`, to tag
`v0.2.16`, commit `8fb62fbfa2bc770412de8cc470e2f1269676667c`.

The `[patch."https://github.com/squirelworks/seihai-core"]` entry uses
`https://www.github.com/squirelworks/seihai-core` at the new tag. This is a
whole-dependency-graph replacement, not only the gserver's direct dependency:
the pinned NPC-server (`21de1cac293f3270ce3f634a01dac4a6bd0e0e8b`) and test RC
(`dd103226dca1bedbeb983bc523a17259b81f118f`) still request core `v0.1.0`, but
resolve to the same replacement. The lockfile contains one core package.
The alternate host avoids Cargo's same-source patch restriction; it is the
same repository, not a separate implementation.

The staff-line branch keeps this whole-graph replacement and its NPC-server
pin `1ebabde1cb49488b59d72861a29d21694cedc55e`. That revision still requests
core `v0.1.0` and resolves to the same one core package. The lockfile differs
from #138 by that pin alone.

Main after #137 (`6a1db9cbf7fe322cbc827998e867a4b149d36939`) was merged
normally into this branch at `955b8ab03841055153a0e3f69d4b4fddb3aecd0f`.
At that integrated head, [CI run 43657008370](https://github.com/squirelworks/seihai-gserver/actions/runs/43657008370)
passed formatting, strict Clippy and tests on Linux and Windows. Local
`cargo metadata --locked --offline --no-deps` and `git diff --check` also
passed. These checks establish generated-client and build/test compatibility,
not original-client or native protocol fidelity. No new original-client run
is claimed.

This note changes the PR head: fresh Linux and Windows checks on the resulting
commit are required before integration; the preceding green run is historical
evidence only. Independent SERVER review owns integration. Keep #139's staff
delivery changes separate. The remaining behavioral research is tracked in
#132 and roadmaps #45/#56; the core bump does not close those acceptance gaps.
