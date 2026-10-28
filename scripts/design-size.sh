#!/bin/sh
# docs/design/ is read before code, so each file stays small enough to read
# whole: under 8 KB. The README's table lists each area index (files-*.md),
# and every other file is in exactly one index's table.
set -eu
cd "$(git rev-parse --show-toplevel)/docs/design"
bad=0
for f in *.md; do
  size=$(wc -c < "$f" | tr -d " ")
  if [ "$size" -gt 8192 ]; then
    echo "docs/design/$f is $size bytes, over 8192: split the area in two"
    bad=1
  fi
  case "$f" in
    README.md) ;;
    files-*.md)
      if ! grep -q "^| $f |" README.md; then
        echo "docs/design/$f is missing from the table in docs/design/README.md"
        bad=1
      fi ;;
    *)
      n=$(cat files-*.md | grep -c "^| $f |" || true)
      if [ "$n" -ne 1 ]; then
        echo "docs/design/$f is in $n index tables (docs/design/files-*.md), not one"
        bad=1
      fi ;;
  esac
done
exit "$bad"
