#!/usr/bin/env python3
"""Tests for settings-count.py, through its command line: each test builds a
small git repo, runs the script in it and reads the exit code and the output."""
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "settings-count.py")
BASELINE = ".github/settings-baseline.txt"
EXAMPLE = "settings.example.toml"

BASE = """\
# the Flockfile entry is not a setting
[[app]]
name = "shep"

[app.dogs.kelpie.git]
# prose with a = sign is not a key, and neither is `merging = "ask"` in text
merging = "ask"
# remote = "owner/name"

[app.dogs.kelpie.ci]
block = true
"""


class Repo:
    """A throwaway git repo with a settings example and an optional baseline."""

    def __init__(self, example, baseline=None):
        self.dir = tempfile.TemporaryDirectory()
        self.root = self.dir.name
        subprocess.run(["git", "init", "-q", self.root], check=True)
        self.write(EXAMPLE, example)
        if baseline is not None:
            self.write(BASELINE, baseline)

    def write(self, path, text):
        full = os.path.join(self.root, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as handle:
            handle.write(text)

    def read(self, path):
        with open(os.path.join(self.root, path)) as handle:
            return handle.read()

    def run(self, *args):
        return subprocess.run(
            [sys.executable, "-I", SCRIPT, *args],
            cwd=self.root,
            capture_output=True,
            text=True,
        )


class SettingsCount(unittest.TestCase):
    def test_counts_live_and_commented_keys_under_the_dog_tables_only(self):
        repo = Repo(BASE)
        self.assertEqual(repo.run("--update").returncode, 0)
        self.assertEqual(repo.read(BASELINE), "3\n")

    def test_passes_at_the_baseline(self):
        result = Repo(BASE, "3\n").run()
        self.assertEqual((result.returncode, result.stdout), (0, ""))

    def test_a_new_key_fails_and_names_the_baseline_and_the_rule(self):
        repo = Repo(BASE + 'fix_attempts = -1\n', "3\n")
        result = repo.run()
        self.assertEqual(result.returncode, 1)
        self.assertIn("4 settings, baseline 3", result.stdout)
        self.assertIn("--update", result.stdout)

    def test_update_clears_a_new_key(self):
        repo = Repo(BASE + '# base = "next"\n', "3\n")
        self.assertEqual(repo.run().returncode, 1)
        self.assertEqual(repo.run("--update").returncode, 0)
        self.assertEqual(repo.run().returncode, 0)
        self.assertEqual(repo.read(BASELINE), "4\n")

    def test_a_smaller_count_passes_with_no_edit(self):
        result = Repo(BASE, "9\n").run()
        self.assertEqual((result.returncode, result.stdout), (0, ""))

    def test_a_key_repeated_in_one_array_table_counts_once(self):
        hooks = "# [[app.dogs.kelpie.hooks]]\n# point = \"a\"\n# [[app.dogs.kelpie.hooks]]\n# point = \"b\"\n"
        repo = Repo(BASE + hooks, "4\n")
        self.assertEqual(repo.run().returncode, 0)

    def test_the_same_name_in_another_table_counts_again(self):
        repo = Repo(BASE + "[app.dogs.kelpie.worker]\nblock = 1\n", "3\n")
        self.assertEqual(repo.run().returncode, 1)

    def test_a_missing_baseline_fails(self):
        self.assertEqual(Repo(BASE).run().returncode, 1)


if __name__ == "__main__":
    unittest.main()
