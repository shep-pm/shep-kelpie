#!/bin/sh
# Fails on a home folder path other than /Users/me or /home/me, or on a private
# name, in any tracked file. The names are a regex in PRIVATE_NAMES, so this
# file never spells them: CI reads it from a repository secret, a local run
# from the maintainer's shell. Unset, the names half is skipped, as it is on a
# fork's pull request, which has no secrets. The paths half always runs.
set -eu

status=0

paths=$(git grep -noE '/(Users|home)/[A-Za-z0-9_-]+' -- . ':!scripts/check-private.sh' | grep -vE ':/(Users|home)/me$' || true)
if [ -n "$paths" ]; then
    echo "home folder path other than /Users/me or /home/me:"
    echo "$paths"
    status=1
fi

if [ -z "${PRIVATE_NAMES:-}" ]; then
    echo "PRIVATE_NAMES is not set, so the private name check is skipped"
else
    names=$(git grep -niE "$PRIVATE_NAMES" -- . ':!scripts/check-private.sh' || true)
    if [ -n "$names" ]; then
        echo "private name:"
        echo "$names"
        status=1
    fi
fi

exit "$status"
