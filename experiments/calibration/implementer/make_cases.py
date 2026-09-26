#!/usr/bin/env python3
"""Build and validate the implementer case set.

    make_cases.py build                 # write cases/<id>.test.patch, cases/<id>.src.patch, cases.jsonl
    make_cases.py validate [--only a,b] # prove each case: red at the parent with only the tests, green at the commit

Each case is one real commit to the pinned worker repo (~/.kelpie/repos/shep)
that changes one non-test source file in shep-core or shep-channel and adds
unit tests in that file's own `#[cfg(test)] mod tests`. The commit's diff is
split by position: a hunk whose first changed line sits inside the test module
goes to the hidden test patch, anything else to the source patch (kept only so
validation can prove the two halves recompose the commit exactly).

`validate` writes cases/<id>.validation.json with both runs' parsed results and
output tails. A case holds only if the listed tests fail on parent + test patch
and all pass on parent + test patch + source patch, and that last tree matches
the commit's file byte for byte.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
CASES = HERE / "cases"
REPO = Path(os.environ.get("IMPLEMENTER_REPO", Path.home() / ".kelpie" / "repos" / "shep"))
TARGETS = Path.home() / ".kelpie" / "targets"

# Specs are written from the commit's behaviour, not its code: what must hold,
# never how the commit did it. Where the hidden tests call an item that does
# not exist at the parent, the spec names that item's signature, since the
# tests cannot compile against anything else.
DEFS = [
    {
        "id": "wire-reply-id-shape",
        "commit": "59cdae0ac6",
        "crate": "shep-core",
        "file": "crates/shep-core/src/protocol/wire.rs",
        "kind": "bug fix",
        "tests": ["protocol::wire::tests::reply_id_is_none_for_a_future_progress_frame"],
        "spec": (
            "`reply_id(frame)` is meant to recover the request id from a frame that is a reply "
            "to a request, even when the rest of the reply does not decode, and to return None "
            "for anything that is not a reply. Today it returns Some for any JSON object that "
            "carries a numeric `id`, so a future server frame that is not a reply but happens to "
            "carry an `id` (a progress or flow-control frame, say, with an `id` and other fields "
            "of its own) would be mistaken for an undecodable reply. Make `reply_id` answer Some "
            "only for frames shaped like a reply, as the reply frame is defined in this crate's "
            "protocol module, and None for an id-bearing frame that is missing a reply's other "
            "required part. Replies whose result does not decode must still yield their id, and "
            "events and non-JSON input must still yield None."
        ),
    },
    {
        "id": "channel-non-utf8-frame",
        "commit": "4f8f5146e5",
        "crate": "shep-channel",
        "file": "crates/shep-channel/src/session.rs",
        "kind": "bug fix",
        "tests": ["session::tests::a_frame_that_is_not_utf8_is_malformed_and_recoverable"],
        "spec": (
            "`read_message` reads one newline-terminated JSON frame from the shepherd. The crate "
            "documents two error kinds on the receive path: `ChannelError::Malformed` means one "
            "bad line that the caller can skip by reading again, and `ChannelError::Io` means the "
            "transport itself failed. A line whose bytes are not valid UTF-8 currently surfaces as "
            "`Io`, so a caller that stops on `Io` stops on one garbage byte. A line that is not "
            "valid UTF-8 must instead be reported as `Malformed`, exactly like a line that is "
            "valid text but not valid JSON, and the whole bad line must be consumed so that the "
            "next call returns the frame on the following line. End of input and genuine read "
            "failures keep their current behaviour."
        ),
    },
    {
        "id": "outbox-zero-capacity",
        "commit": "20b7dcbd98",
        "crate": "shep-channel",
        "file": "crates/shep-channel/src/outbox.rs",
        "kind": "edge case",
        "tests": ["outbox::tests::a_zero_capacity_outbox_counts_the_drop_and_retains_nothing"],
        "spec": (
            "An `Outbox` built with a capacity of zero must be legal and must never retain a "
            "message. Today a lossy push into a zero-capacity outbox bumps the dropped counter "
            "but still queues the message, so the queue exceeds its own capacity and the counter "
            "no longer describes what was actually discarded. For a zero-capacity outbox, each "
            "`push_lossy` must count exactly one drop (the message being pushed) and leave the "
            "queue empty, so that after `close()` a `pop()` returns None. Behaviour for capacities "
            "of one or more (evict the oldest, count one drop) and for pushes after close must not "
            "change."
        ),
    },
    {
        "id": "env-u64-beyond-i64",
        "commit": "016d92ba6b",
        "crate": "shep-core",
        "file": "crates/shep-core/src/config/app.rs",
        "kind": "edge case",
        "tests": ["config::app::tests::env_reads_a_number_beyond_i64_max_without_wrapping"],
        "spec": (
            "An app config's `env` table accepts scalar values (strings, booleans, whole numbers, "
            "floats) and stores each one as a string. A whole number larger than `i64::MAX` is "
            "valid JSON and TOML, but deserializing an app config that contains one currently "
            "fails. Every unsigned 64-bit whole number, up to and including `u64::MAX`, must load "
            "and be stored as its exact decimal digits, with no wrap to a negative value and no "
            "error. Negative whole numbers down to `i64::MIN`, and every other value kind, must "
            "keep loading exactly as they do today, and serializing the config must still write "
            "each env value as a string."
        ),
    },
    {
        "id": "graph-dogs-last-with-cycle",
        "commit": "dda57085fd",
        "crate": "shep-core",
        "file": "crates/shep-core/src/config/graph.rs",
        "kind": "bug fix",
        "tests": ["config::graph::tests::a_dog_still_runs_last_when_the_flock_holds_a_cycle"],
        "spec": (
            "`plan` turns a flock's boot nodes into ordered stages. By default a dog that nothing "
            "depends on (and that is not marked to boot first) is meant to start after everything "
            "else, so that a metrics-style dog never reports on a flock still coming up. That "
            "holds for an acyclic graph, but when the graph contains a dependency cycle the plan "
            "puts those dogs ahead of the cyclic stage and ahead of the nodes that depend on the "
            "cycle. Such dogs must be the final stage of the plan whether or not the graph has a "
            "cycle. Nothing else about the plan changes: boot-first dogs, the ordinary order, the "
            "single stage holding every cyclic node, the stages after it, and the reported cycles "
            "stay as they are. Update the function's documentation of the stage order to match."
        ),
    },
    {
        "id": "paths-user-home",
        "commit": "683599568b",
        "crate": "shep-core",
        "file": "crates/shep-core/src/paths.rs",
        "kind": "small feature",
        "tests": [
            "paths::tests::home_is_read_first_on_every_platform",
            "paths::tests::nothing_named_resolves_nothing",
            "paths::tests::an_empty_value_counts_as_unset",
            "paths::tests::unix_ignores_the_windows_variables",
        ],
        # The commit's test helper names `OsString` bare and gets it from the
        # module's own import through `use super::*`. A correct implementation
        # that spells `std::ffi::OsString` in full would fail to compile the
        # hidden tests for a reason the spec cannot fairly demand, so the
        # hidden patch carries its own import.
        "patch_tweak": {
            "find": "+    /// A fake environment, so nothing here touches the process's own.\n",
            "replace": "+    use std::ffi::OsString;\n+\n"
                       "+    /// A fake environment, so nothing here touches the process's own.\n",
        },
        "spec": (
            "Add a public function to `paths.rs` that finds the user's home directory, with the "
            "signature `pub fn user_home(var: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> "
            "Option<std::path::PathBuf>`. It must read environment variables only through `var`, "
            "never from the process environment, in keeping with the rest of the module. On unix "
            "it returns `HOME` and ignores every other variable. On Windows it tries `HOME`, then "
            "`USERPROFILE`, then `HOMEDRIVE` and `HOMEPATH` joined as one string (they are two "
            "halves of a single path, such as `C:` and `\\Users\\name`), and the pair counts only "
            "when both halves are present. A variable set to the empty string counts as unset "
            "everywhere. When nothing resolves, it returns None. Document the function, including "
            "the platform behaviour."
        ),
    },
    # Backups, validated but not run unless a primary case fails validation.
    {
        "id": "normalize-colon-in-name",
        "commit": "605f59823e",
        "crate": "shep-core",
        "file": "crates/shep-core/src/config/normalize.rs",
        "kind": "small feature",
        "backup": True,
        "tests": ["config::normalize::tests::a_colon_in_a_name_is_refused_because_it_is_the_instance_separator"],
        "spec": (
            "A sheep name may not contain a colon, because the colon separates a name from an "
            "instance slot (`name:slot`) and is also illegal in a Windows file name, which a sheep "
            "name becomes part of. Normalizing an app whose name contains a colon must fail with "
            "the same error variant already used for names containing a path separator, carrying "
            "the name. The error's user-facing message must mention the colon among the refused "
            "characters and must not use an em or en dash. Names without a colon, such as "
            "`web-2`, keep normalizing as before, and the documentation of the refusal must be "
            "updated to match."
        ),
    },
    {
        "id": "secrets-all-slot-push",
        "commit": "b7d3f9a37c",
        "crate": "shep-core",
        "file": "crates/shep-core/src/secrets.rs",
        "kind": "bug fix",
        "backup": True,
        "tests": [
            "secrets::tests::an_all_slot_push_makes_a_genuinely_missing_key_permanent",
            "secrets::tests::an_all_slot_push_resolves_its_key_for_every_environment",
        ],
        "spec": (
            "A secret provider can push a namespace either for one environment or once for the "
            "every-environment slot (`ALL_ENVIRONMENTS`). Value lookup already falls back to that "
            "slot, but the check that decides whether a namespace has been pushed at all only "
            "looks at the view's own environment. So when a namespace was pushed only under the "
            "every-environment slot, a key missing from it resolves as the transient "
            "`MissingNamespace` rather than the permanent `MissingKey`. A namespace pushed under "
            "either the view's own environment or the every-environment slot must count as pushed, "
            "so a key absent from such a push resolves as `MissingKey`, and a key present in it "
            "resolves as found, for a view in any environment."
        ),
    },
]


def git(*args, cwd=REPO, check=True):
    return subprocess.run(["git", "-C", str(cwd), *args], check=check,
                          capture_output=True, text=True).stdout


def full_sha(rev):
    return git("rev-parse", rev).strip()


def test_module_start(text):
    """1-based line of the `#[cfg(test)]` that opens a `mod ... {`, or None."""
    lines = text.split("\n")
    for i, line in enumerate(lines):
        if not line.strip().startswith("#[cfg(test)]"):
            continue
        for follow in lines[i + 1:i + 5]:
            s = follow.strip()
            if not s or s.startswith("#[") or s.startswith("//"):
                continue
            if re.match(r"(pub(\([\w:]+\))?\s+)?mod\s+\w+\s*\{", s):
                return i + 1
            break
    return None


def split_diff(commit, path):
    """(header, test_hunks, src_hunks) of the commit's diff to one file."""
    diff = git("diff", f"{commit}^", commit, "--", path)
    new_start = test_module_start(git("show", f"{commit}:{path}"))
    old_start = test_module_start(git("show", f"{commit}^:{path}"))
    lines = diff.splitlines(keepends=True)
    first = next(i for i, l in enumerate(lines) if l.startswith("@@"))
    header, body = "".join(lines[:first]), lines[first:]
    hunks, cur = [], None
    for line in body:
        if line.startswith("@@"):
            cur = [line]
            hunks.append(cur)
        else:
            cur.append(line)
    tests, srcs = [], []
    for hunk in hunks:
        m = re.match(r"@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@", hunk[0])
        old_line, new_line = int(m[1]), int(m[2])
        first_new = first_old = None
        for line in hunk[1:]:
            if line.startswith("+"):
                first_new = new_line if first_new is None else first_new
                new_line += 1
            elif line.startswith("-"):
                first_old = old_line if first_old is None else first_old
                old_line += 1
            else:
                old_line += 1
                new_line += 1
        in_tests = ((first_new is None or (new_start and first_new > new_start))
                    and (first_old is None or (old_start and first_old > old_start)))
        (tests if in_tests else srcs).append("".join(hunk))
    return header, tests, srcs


def recount(hunk_text):
    lines = hunk_text.splitlines(keepends=True)
    old = sum(1 for l in lines[1:] if l.startswith((" ", "-")) or l == "\n")
    new = sum(1 for l in lines[1:] if l.startswith((" ", "+")) or l == "\n")
    m = re.match(r"@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)", lines[0], re.S)
    lines[0] = f"@@ -{m[1]},{old} +{m[2]},{new} @@{m[3]}"
    return "".join(lines)


def changed_lines(hunks):
    return sum(1 for h in hunks for l in h.splitlines()[1:] if l[:1] in "+-")


def build(_args):
    CASES.mkdir(exist_ok=True)
    rows = []
    for d in DEFS:
        commit = full_sha(d["commit"])
        parent = full_sha(f"{commit}^")
        touched = git("show", "--name-only", "--format=", commit).split()
        header, tests, srcs = split_diff(commit, d["file"])
        if not tests or not srcs:
            sys.exit(f"{d['id']}: split found {len(tests)} test and {len(srcs)} source hunks")
        test_patch = header + "".join(tests)
        tweak = d.get("patch_tweak")
        if tweak:
            if test_patch.count(tweak["find"]) != 1:
                sys.exit(f"{d['id']}: patch tweak anchor matches {test_patch.count(tweak['find'])} times")
            test_patch = test_patch.replace(tweak["find"], tweak["replace"])
            test_patch = header + "".join(
                recount(h) for h in re.split(r"(?m)^(?=@@ )", test_patch[len(header):]) if h)
        (CASES / f"{d['id']}.test.patch").write_text(test_patch)
        (CASES / f"{d['id']}.src.patch").write_text(header + "".join(srcs))
        rows.append({
            "id": d["id"], "commit": commit, "parent": parent, "crate": d["crate"],
            "file": d["file"], "kind": d["kind"], "backup": d.get("backup", False),
            "tests": d["tests"],
            "test_patch": f"cases/{d['id']}.test.patch",
            "subject": git("log", "-1", "--format=%s", commit).strip(),
            "commit_files": touched,
            "src_changed_lines": changed_lines(srcs), "test_changed_lines": changed_lines(tests),
            "patch_tweak": bool(tweak),
            "spec": d["spec"],
        })
        print(f"{d['id']}: src {changed_lines(srcs)} lines in {len(srcs)} hunks, "
              f"tests {changed_lines(tests)} lines in {len(tests)} hunks, files {touched}")
    with open(HERE / "cases.jsonl", "w") as fh:
        for row in rows:
            fh.write(json.dumps(row) + "\n")


def load_cases(include_backups=True):
    with open(HERE / "cases.jsonl") as fh:
        rows = [json.loads(l) for l in fh if l.strip()]
    return [r for r in rows if include_backups or not r["backup"]]


# ---- shared with implement.py -------------------------------------------------

TEST_LINE = re.compile(r"^test (\S+) \.\.\. (ok|FAILED|ignored)", re.M)


def run_tests(worktree, crate, tests, target_dir, timeout=1200):
    """Run the named tests exactly. Returns a dict; `passed` needs every name ok."""
    cmd = ["cargo", "test", "-p", crate, "--lib", "--", *tests, "--exact"]
    env = dict(os.environ, CARGO_TARGET_DIR=str(target_dir), CARGO_TERM_COLOR="never")
    t0 = time.time()
    try:
        proc = subprocess.run(cmd, cwd=str(worktree), env=env, capture_output=True,
                              text=True, timeout=timeout)
        out, rc = proc.stdout + proc.stderr, proc.returncode
    except subprocess.TimeoutExpired as exc:
        out = (exc.stdout or b"").decode(errors="replace") if isinstance(exc.stdout, bytes) else (exc.stdout or "")
        rc = "timeout"
    seen = dict(TEST_LINE.findall(out))
    missing = [t for t in tests if t not in seen]
    failing = [t for t in tests if seen.get(t) == "FAILED"]
    compile_error = rc not in (0, "timeout") and not seen
    passed = rc == 0 and not missing and not failing and all(seen.get(t) == "ok" for t in tests)
    return {
        "cmd": " ".join(cmd), "exit": rc, "seconds": round(time.time() - t0, 1),
        "passed": passed, "results": {t: seen.get(t, "not run") for t in tests},
        "failing": failing, "missing": missing, "compile_error": compile_error,
        "tail": "\n".join(out.strip().splitlines()[-40:]),
    }


def add_worktree(rev, prefix="impl-"):
    base = Path(tempfile.mkdtemp(prefix=prefix))
    wt = base / "wt"
    git("worktree", "add", "--detach", str(wt), rev)
    return base, wt


def remove_worktree(base):
    wt = Path(base) / "wt"
    git("worktree", "remove", "--force", str(wt), check=False)
    git("worktree", "prune", check=False)
    shutil.rmtree(base, ignore_errors=True)


def apply_patch(worktree, patch):
    """Apply with git, then with context 1, then with patch(1) fuzz. Returns the method or None."""
    patch = str(Path(patch).resolve())
    tries = [
        ("git apply", ["git", "apply", patch]),
        ("git apply -C1", ["git", "apply", "-C1", patch]),
        ("patch --fuzz=3", ["patch", "-p1", "--fuzz=3", "--no-backup-if-mismatch", "-i", patch]),
    ]
    for name, cmd in tries:
        if subprocess.run(cmd, cwd=str(worktree), capture_output=True).returncode == 0:
            return name
    return None


# ---- validation ---------------------------------------------------------------

def validate(args):
    only = set(args.only.split(",")) if args.only else None
    target = TARGETS / "validate"
    for case in load_cases():
        if only and case["id"] not in only:
            continue
        base, wt = add_worktree(case["parent"], prefix="impl-validate-")
        record = {"id": case["id"], "commit": case["commit"], "parent": case["parent"]}
        try:
            method = apply_patch(wt, HERE / case["test_patch"])
            record["test_patch_applied"] = method
            if method != "git apply":
                raise RuntimeError(f"test patch does not apply cleanly at the parent: {method}")
            record["parent_with_tests"] = run_tests(wt, case["crate"], case["tests"], target)
            src = HERE / f"cases/{case['id']}.src.patch"
            method = apply_patch(wt, src)
            record["src_patch_applied"] = method
            if method != "git apply":
                raise RuntimeError(f"source patch does not apply cleanly: {method}")
            recomposed = (wt / case["file"]).read_text()
            expected = git("show", f"{case['commit']}:{case['file']}")
            tweak = next(d for d in DEFS if d["id"] == case["id"]).get("patch_tweak")
            if tweak:  # the commit's file plus exactly the lines the tweak adds
                strip = lambda s: "".join(l[1:] for l in s.splitlines(keepends=True))  # noqa: E731
                expected = expected.replace(strip(tweak["find"]), strip(tweak["replace"]), 1)
            record["recomposes_commit_file"] = recomposed == expected
            record["commit_with_tests"] = run_tests(wt, case["crate"], case["tests"], target)
            red = not record["parent_with_tests"]["passed"]
            green = record["commit_with_tests"]["passed"]
            record["holds"] = red and green and record["recomposes_commit_file"]
        except Exception as exc:  # noqa: BLE001  (record and move on)
            record["error"] = str(exc)
            record["holds"] = False
        finally:
            remove_worktree(base)
        (CASES / f"{case['id']}.validation.json").write_text(json.dumps(record, indent=1))
        p, c = record.get("parent_with_tests", {}), record.get("commit_with_tests", {})
        print(f"{case['id']}: holds={record['holds']} "
              f"parent={p.get('results')} compile_error={p.get('compile_error')} "
              f"commit={c.get('results')} recomposes={record.get('recomposes_commit_file')}"
              + (f" ERROR {record['error']}" if record.get("error") else ""), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("build")
    v = sub.add_parser("validate")
    v.add_argument("--only")
    args = parser.parse_args()
    {"build": build, "validate": validate}[args.cmd](args)


if __name__ == "__main__":
    main()
