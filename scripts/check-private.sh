#!/bin/sh
# Fails on a home folder path other than /Users/me, or on the name of a private
# project, in any tracked file. The name is spelled with a bracket so this file
# does not match itself.
set -eu

status=0

paths=$(git grep -noE '/Users/[A-Za-z]+' -- . ':!scripts/check-private.sh' | grep -v ':/Users/me$' || true)
if [ -n "$paths" ]; then
    echo "home folder path other than /Users/me:"
    echo "$paths"
    status=1
fi

names=$(git grep -niE '[h]azel' -- . ':!scripts/check-private.sh' || true)
if [ -n "$names" ]; then
    echo "private project name:"
    echo "$names"
    status=1
fi

exit "$status"
