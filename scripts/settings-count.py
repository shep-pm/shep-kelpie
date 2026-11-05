#!/usr/bin/env python3
"""Fewer settings on this machine and in CI: the count of the settings keys
in `settings.example.toml` may not pass `.github/settings-baseline.txt`.

A key is a fact kelpie cannot detect or a safety limit; a judgement goes to
the project manager (`CODING_STANDARDS.md`, Fewer settings). A pull request
that adds a key runs `--update`, which shows the new count in the diff, and
says in its body why the project manager cannot decide it. The count may
shrink with no edit.

A key is a `name = value` line, live or commented out, under a table of the
dog's settings (`app.dogs.kelpie`, live or commented header). The same name
in the same table counts once, so a repeated `[[...]]` entry adds nothing.
"""
import os
import re
import subprocess
import sys

BASELINE = ".github/settings-baseline.txt"
EXAMPLE = "settings.example.toml"
PREFIX = "app.dogs.kelpie"
HEADER = re.compile(r"^\s*#?\s*\[\[?\s*([A-Za-z0-9_.\"-]+?)\s*\]\]?\s*$")
KEY = re.compile(r"^\s*#?\s*([A-Za-z_][A-Za-z0-9_-]*)\s*=\s*\S")


def repo_root():
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    )
    return out.stdout.strip()


def keys(text):
    """Every (table, key) the example sets or shows."""
    found, table = set(), None
    for line in text.split("\n"):
        header = HEADER.match(line)
        if header:
            table = header.group(1)
            continue
        key = KEY.match(line)
        if key and table and (table == PREFIX or table.startswith(PREFIX + ".")):
            found.add((table, key.group(1)))
    return found


def main():
    root = repo_root()
    baseline_path = f"{root}/{BASELINE}"
    with open(f"{root}/{EXAMPLE}") as handle:
        count = len(keys(handle.read()))
    if sys.argv[1:] == ["--update"]:
        os.makedirs(os.path.dirname(baseline_path), exist_ok=True)
        with open(baseline_path, "w") as handle:
            handle.write(f"{count}\n")
        return 0
    try:
        with open(baseline_path) as handle:
            baseline = int(handle.read().strip())
    except (FileNotFoundError, ValueError):
        baseline = 0
    if count > baseline:
        print(
            f"{EXAMPLE}: {count} settings, baseline {baseline}. A setting is a fact kelpie "
            f"cannot detect or a safety limit; a judgement goes to the project manager. If "
            f"this key is one, run `python3 scripts/settings-count.py --update` and say in "
            f"the pull request why the project manager cannot decide it."
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
