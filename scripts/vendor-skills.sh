#!/bin/sh
# Vendors the mattpocock/skills that kelpie's steps run, at one pinned commit,
# into crates/kelpie/skills/. Run it from the repo's root with the commit to
# pin, then update SKILLS in crates/kelpie/src/skills/vendored.rs to match and
# run the tests. `blobs.txt` is `git ls-tree` at the pin, so the vendored
# copies can be checked against it without the network.
set -eu

commit=${1:?usage: scripts/vendor-skills.sh <upstream commit>}
upstream=https://github.com/mattpocock/skills
out=crates/kelpie/skills
skills="engineering/triage engineering/to-tickets engineering/to-spec
engineering/implement engineering/tdd engineering/code-review
engineering/diagnosing-bugs engineering/pr engineering/retro productivity/handoff"

clone=$(mktemp -d)
trap 'rm -rf "$clone"' EXIT
git clone --quiet "$upstream" "$clone"
git -C "$clone" checkout --quiet "$commit"

rm -rf "$out/LICENSE" "$out/skills"
mkdir -p "$out/skills"
cp "$clone/LICENSE" "$out/LICENSE"
paths=LICENSE
for skill in $skills; do
    mkdir -p "$out/skills/$(dirname "$skill")"
    cp -R "$clone/skills/$skill" "$out/skills/$skill"
    paths="$paths skills/$skill"
done
# shellcheck disable=SC2086 # the paths are split on purpose
git -C "$clone" ls-tree -r "$commit" -- $paths | awk '{ print $3 "  " $4 }' >"$out/blobs.txt"
printf '%s\n%s\n' "$upstream" "$(git -C "$clone" rev-parse "$commit")" >"$out/UPSTREAM"
