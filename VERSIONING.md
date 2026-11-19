# Versioning and releases

This file says when seihai-gserver makes a release, what the numbers mean, and what
1.0.0 would take. The first section is specific to this repo. The rest is the
same in every Seihai repo.

## This repo

- **Current version:** 0.1.0.
- **Where the version lives:** `version` under `[package]` in `Cargo.toml`.
- **Public surface** (the things a version number makes promises about):
  - what each client version sees on the wire, and RC/NC behaviour;
  - the NPC-server link (`PROTOCOL.md`);
  - the world folder layout, account files and config keys (`rules = 1999` and the rest);
  - the command line (`account`, `import-v2`, `key-fingerprint` and the flags).

  Internal modules, tests, lab write-ups and `docs/` are not part of the surface.
- **Unreleased notes:** `docs/releases/unreleased.md`.
- **Release notes:** `docs/releases/vX.Y.Z.md`.
- **How a release is made:** After the release PR merges, someone runs `.github/workflows/release.yml` by hand with the version. It builds every target and makes a **draft**, and the owner presses Publish.
- **Next milestone:** [#165](https://github.com/squirelworks/seihai-gserver/issues/165) (0.2.0).
- **1.0.0 criteria:** [#166](https://github.com/squirelworks/seihai-gserver/issues/166).
- **Long-term roadmap:** [#56](https://github.com/squirelworks/seihai-gserver/issues/56).
- **Check:** `.github/workflows/version-guard.yml` checks every PR that changes the version (see below).

## The suite-wide rules

These rules are the same in every Seihai repo (seihai, seihai-core,
seihai-gserver, -listserver, -npcserver, -rc, -servertools, the GPL forks
seihai-gserver-v2 and seihai-gs2lib, Graal_analysis and graal-client-repro).
Only the section above is specific to this repo. A change to these rules goes
to every repo in the same round.

### Does every PR need a release?

No. A release is a deliberate step, not a side effect of a green PR.

- Most PRs never trigger a release: refactors, tests, CI, docs, lab
  write-ups and research notes. Neither do features that are still part-way
  through a milestone.
- A PR that changes the public surface (above) adds one line to the
  unreleased notes and **leaves the version alone**.
- The version changes only in a **release PR**: the PR that bumps the
  version and turns the unreleased notes into `vX.Y.Z` notes. This can be a
  PR of its own, or the PR that ticks the milestone's last box, so a release
  doesn't cost an extra CI run. Its body carries a line `Release: vX.Y.Z`.
- A merged release PR gives a **draft** GitHub release. The tag only exists
  once the owner presses Publish. Nobody pushes a release tag by hand.

The `version-guard` check (in the repos that keep their version in a file)
fails a PR that changes the version without `Release: vX.Y.Z` in its body or
title, without the notes file, with a version that isn't the next patch, minor
or major, or with a sibling Seihai crate pinned by branch or bare commit
instead of a published tag.

### When to release

A release happens when one of these is true, and not on a timer:

1. **A milestone is done.** Every box in this repo's open "0.N.0 roadmap"
   issue is ticked. This gives a minor release.
2. **A fix can't wait.** A user or another repo needs a fix in a published
   build. This gives a patch release, and the milestone stays open.
3. **A consumer is releasing.** Another repo is about to release, and it needs
   a published tag of this one to pin (see "Pinning seihai-core" below). This
   gives a patch, or a minor if breaking changes are queued.

Small fixes are batched. Five fixes merged in a day are one patch release,
not five.

### The numbers before 1.0: 0.MINOR.PATCH

The numbers follow SemVer as Cargo reads it for 0.x versions. A consumer that
asks for `0.6` accepts any 0.6.z, and never 0.7.

- **0.0.z** is a repo's first period: any release can change anything. A repo
  leaves it with its first 0.1.0.
- **PATCH (0.6.0 to 0.6.1)** covers backward-compatible changes: bug fixes,
  lab-confirmed corrections, and new items or options that don't change what
  existing callers, configs, worlds or scripts see.
- **MINOR (0.6.z to 0.7.0)** covers either of these:
  - **a breaking change** to the public surface: something removed or
    renamed, a changed signature, a new field in a struct built by literal,
    a new variant in an exhaustive enum, a wire, config, file or CLI change a
    user must adapt to, or a behaviour change a consumer has to handle. The
    notes list these under "Breaking changes".
  - **a finished milestone**, even if nothing breaks. The minor number is how
    the suite says "this is a meaningful step".

**When is a repo ready for the next 0.N.0?** When its "0.N.0 roadmap" issue
is fully ticked: each item is merged, lab-run where the item says so, CI is
green on Linux, Windows and macOS at the release commit, and the notes are
written. Breaking changes that land on `main` before then wait and ship with
that release. If a breaking change has to go out sooner, it goes out as the
next 0.N.0, and the roadmap issue is renamed for the number after it.

### What 1.0.0 means

1.0.0 is a promise, not a size: **a user can rely on the public surface, and
only a 2.0.0 may break it.** A repo reaches 1.0.0 when every box in its
"1.0.0 criteria checklist" issue is ticked. Every checklist has the same four
parts:

1. **Scope:** the repo does the job the suite roadmap ([squirelworks/seihai#353](https://github.com/squirelworks/seihai/issues/353),
   "End goal") gives it, for every in-scope client from 1.x to 6.x. Each
   claim is backed by a lab run with real clients, recorded in the repo.
2. **Stability:** the public surface is documented in one place, and it is
   built so that adding a client build later is not a breaking change (for a
   library, `#[non_exhaustive]` enums and constructors; for files and configs,
   migrations). The last 0.x minor goes one whole cycle with no breaking change
   queued.
3. **Quality:** CI is green on Linux, Windows and macOS. Every decoder of
   untrusted input (network, world files, saved files) is fuzzed, with no open
   crash. No crash or data-loss bug is open. Release archives ship for all
   three platforms.
4. **Dependencies:** it builds on seihai-core 1.x. So core reaches 1.0 first,
   then the servers, the tools and the client.

After 1.0.0, it's plain SemVer: MAJOR breaks, MINOR adds, PATCH fixes.

### Pinning seihai-core (and other Seihai crates)

A version pin is only needed where a build has to be repeatable by someone
else, which means a release.

- **Local and lab test builds:** don't edit `Cargo.toml`. Build against a
  local seihai-core checkout, or core's `main`, with an override that isn't
  committed. For example, put this in a git-ignored `.cargo/config.toml`:

  ```toml
  [patch."https://github.com/squirelworks/seihai-core"]
  seihai-core = { path = "../seihai-core/crates/seihai-core" }
  seihai-draw = { path = "../seihai-core/crates/seihai-draw" }
  ```

  Then run `cargo update -p seihai-core -p seihai-draw` once, so the lock
  file takes the local copy. Cargo ignores a patch the lock file doesn't
  match, and only says so in a warning. Before committing, drop the lock-file
  change (`git checkout Cargo.lock`). There's no commit and no PR.
- **A repo's `main` between releases:** depend on core with
  `branch = "main"`. `Cargo.lock` still records the exact commit, so CI stays
  repeatable. A PR that needs newer core runs `cargo update -p seihai-core`
  (and `-p seihai-draw`) inside that same PR. That means no separate
  "bump the pin" PR, and no core tag is needed. Crates that end up in the same
  lock file (rc pulls in listserver, gserver pulls in npcserver and rc) must
  all name core the same way. If they don't, Cargo builds two cores and the
  types don't match ([squirelworks/seihai-core#79](https://github.com/squirelworks/seihai-core/issues/79)). If they all use `main`,
  that problem goes away.
- **A release:** a release PR pins every Seihai git dependency by a
  **published tag** (`tag = "vX.Y.Z"`), never a branch or a bare `rev`. That
  way the release's source archive builds the same anywhere. `version-guard`
  enforces this. Right after the release, `main` can go back to
  `branch = "main"`.

Repos still on a tag or a rev today move to `branch = "main"` the next time
they need a newer core. They don't need a PR just for that.

### Notes and changelogs

- Between releases: one line per user-visible change in this repo's
  unreleased notes (see the top section), written in the PR that makes the
  change.
- At release: the release PR renames or moves those lines into the version's
  notes file. The notes should say what is new, what it's for, what breaks
  (under "Breaking changes") and what is known not to work. A generated list
  of PR titles is not notes.

### Pre-releases and forks

- A lab build that someone else has to run can be a pre-release
  (`vX.Y.Z-rc.N`, a draft marked pre-release). Most lab builds don't need one:
  build from the branch.
- The GPL forks follow upstream's number: `<upstream version>-seihai.N`.
  N counts our releases on top of that upstream version, and goes back to 1
  when upstream's version changes. The forks don't have a 1.0 of their own.
  Their end state is retirement to a lab reference ([squirelworks/seihai#353](https://github.com/squirelworks/seihai/issues/353),
  phase 7).
