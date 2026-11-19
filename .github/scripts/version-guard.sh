#!/usr/bin/env bash
# Version guard (VERSIONING.md): a PR may change the version in Cargo.toml only
# as a release PR. Run by .github/workflows/version-guard.yml on pull requests.
#
# Env: BASE_SHA, HEAD_SHA, PR_TITLE, PR_BODY, NOTES_DIR (where vX.Y.Z.md lives).
set -euo pipefail

read_version() {
  # The [workspace.package] version, else the [package] version.
  python3 -c '
import sys, tomllib
d = tomllib.loads(sys.stdin.read())
print(d.get("workspace", {}).get("package", {}).get("version")
      or d.get("package", {}).get("version") or "")'
}

old=$(git show "$BASE_SHA:Cargo.toml" | read_version)
new=$(git show "$HEAD_SHA:Cargo.toml" | read_version)
echo "Version on the base branch: ${old:-none}; in this PR: ${new:-none}"

if [ "$old" = "$new" ]; then
  echo "The version is unchanged: not a release PR. Nothing to check."
  exit 0
fi

fail=0
err() { echo "::error::$*"; fail=1; }

# 1. The next patch, minor or major (pre-releases: only higher).
python3 - "$old" "$new" <<'PY' || fail=1
import re, sys
old, new = sys.argv[1], sys.argv[2]
pat = re.compile(r'^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$')
mo, mn = pat.match(old or "0.0.0"), pat.match(new)
if not mn:
    print(f"::error::{new} is not a SemVer version (X.Y.Z or X.Y.Z-pre)."); sys.exit(1)
o = tuple(int(x) for x in mo.groups()[:3]); n = tuple(int(x) for x in mn.groups()[:3])
if mo.group(4) or mn.group(4):
    if n < o or (n == o and (mn.group(4) or "~") <= (mo.group(4) or "~")):
        print(f"::error::{new} is not higher than {old}."); sys.exit(1)
    sys.exit(0)
nexts = [(o[0], o[1], o[2] + 1), (o[0], o[1] + 1, 0), (o[0] + 1, 0, 0)]
if n not in nexts:
    want = ", ".join(".".join(map(str, v)) for v in nexts)
    print(f"::error::{old} can only go to {want} (VERSIONING.md), not {new}."); sys.exit(1)
PY

# 2. Marked as a release PR.
tag="v$new"
if ! printf '%s\n%s\n' "$PR_TITLE" "$PR_BODY" | grep -Eiq "release:? +${tag//./\\.}([^0-9A-Za-z.-]|$)"; then
  err "This PR changes the version to $new. Version bumps belong only in a release PR: put a line 'Release: $tag' in the PR body (or start the title with 'Release $tag'), or leave the version alone and add your change to the unreleased notes. See VERSIONING.md."
fi

# 3. Release notes for the version.
if ! git cat-file -e "$HEAD_SHA:$NOTES_DIR/$tag.md" 2>/dev/null; then
  err "No release notes: add $NOTES_DIR/$tag.md in this PR."
fi

# 4. Sibling Seihai crates pinned by a published tag, not a branch or a rev.
seen=" "
while IFS= read -r -d '' f; do
  while IFS= read -r line; do
    repo=$(printf '%s' "$line" | sed -nE 's#.*github\.com/squirelworks/([A-Za-z0-9_.-]+)".*#\1#p')
    ref=$(printf '%s' "$line" | sed -nE 's#.*[{,] *tag *= *"([^"]+)".*#\1#p')
    if [ -z "$ref" ]; then
      err "$f pins squirelworks/$repo by branch or rev, not a tag: $line. A release pins published tags (VERSIONING.md, 'Pinning seihai-core')."
      continue
    fi
    case "$seen" in *" $repo@$ref "*) continue ;; esac
    seen="$seen$repo@$ref "
    if ! git ls-remote --exit-code --tags "https://github.com/squirelworks/$repo" "refs/tags/$ref" >/dev/null 2>&1; then
      err "$f pins squirelworks/$repo at tag $ref, which is not published."
    fi
  done < <(git show "$HEAD_SHA:$f" | grep -E '^[^#]*git *= *"https://github.com/squirelworks/' || true)
done < <(git ls-tree -r -z --name-only "$HEAD_SHA" | grep -z -E '(^|/)Cargo\.toml$' | grep -z -v -E '(^|/)fuzz/' || true)

if [ "$fail" -ne 0 ]; then exit 1; fi
echo "Release PR for $tag: version, marker, notes and pins are in order."
