#!/usr/bin/env python3
"""Fewer settings on this machine and in CI: the count of the settings keys
in `settings.example.toml` (a project's) and `kelpie-settings.example.toml`
(the dog's own) may not pass `.github/settings-baseline.txt`, a line per file.

A key is a fact kelpie cannot detect or a safety limit; a judgement goes to
the project manager (`CODING_STANDARDS.md`, Fewer settings). A pull request
that adds a key runs `--update`, which shows the new count in the diff, and
says in its body why the project manager cannot decide it. The count may
shrink with no edit.

A key is a `name = value` line, live or commented out, under a table of the
file's settings (live or commented header): `app.dogs.kelpie` in a project's
file and `kelpie` in the dog's. The same name in the same table counts once,
so a repeated `[[...]]` entry adds nothing.
"""
import os
import re
import subprocess
import sys

BASELINE = ".github/settings-baseline.txt"
# Each example file and the table its settings sit in, itself and below.
FILES = (("settings.example.toml", "app.dogs.kelpie"), ("kelpie-settings.example.toml", "kelpie"))
HEADER = re.compile(r"^\s*#?\s*\[\[?\s*([A-Za-z0-9_.\"-]+?)\s*\]\]?\s*$")
KEY = re.compile(r"^\s*#?\s*([A-Za-z_][A-Za-z0-9_-]*)\s*=\s*\S")


def repo_root():
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True
    )
    return out.stdout.strip()


def keys(text, prefix):
    """Every (table, key) the example sets or shows."""
    found, table = set(), None
    for line in text.split("\n"):
        header = HEADER.match(line)
        if header:
            table = header.group(1)
            continue
        key = KEY.match(line)
        if key and table and (table == prefix or table.startswith(prefix + ".")):
            found.add((table, key.group(1)))
    return found


def counts(root):
    """{file: key count} for each example file the repo has."""
    found = {}
    for name, prefix in FILES:
        try:
            with open(f"{root}/{name}") as handle:
                found[name] = len(keys(handle.read(), prefix))
        except FileNotFoundError:
            continue
    return found


def parse_baseline(text):
    return {name: int(count) for name, count in (line.split("\t") for line in text.split("\n") if line)}


def main():
    root = repo_root()
    baseline_path = f"{root}/{BASELINE}"
    found = counts(root)
    if sys.argv[1:] == ["--update"]:
        os.makedirs(os.path.dirname(baseline_path), exist_ok=True)
        with open(baseline_path, "w") as handle:
            handle.write("".join(f"{name}\t{count}\n" for name, count in found.items()))
        return 0
    try:
        with open(baseline_path) as handle:
            baseline = parse_baseline(handle.read())
    except (FileNotFoundError, ValueError):
        baseline = {}
    failed = False
    for name, count in found.items():
        was = baseline.get(name, 0)
        if count > was:
            failed = True
            print(
                f"{name}: {count} settings, baseline {was}. A setting is a fact kelpie cannot "
                f"detect or a safety limit; a judgement goes to the project manager. If this key "
                f"is one, run `python3 scripts/settings-count.py --update` and say in the pull "
                f"request why the project manager cannot decide it."
            )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
