# Current dependency follow-through

## Core v0.4.0 consumer update

Both the direct core dependency and the whole-graph `www.github.com` patch
move from `v0.2.16` (`8fb62fbfa2bc770412de8cc470e2f1269676667c`) to published
`v0.4.0` (`eb7434ac1fbddc35d127ab4bbe8717e8bf1d64e7`). The patch continues
to unify the pinned RC (`dd103226dca1bedbeb983bc523a17259b81f118f`) and
NPC-server (`1ebabde1cb49488b59d72861a29d21694cedc55e`) dependencies with the
direct core. Preserve the merged chat/trigger and staff-line routing.

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
