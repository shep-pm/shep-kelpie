#!/usr/bin/env python3
"""Score Claude models as implementers of small, well-specified Rust changes.

    implement.py run --model claude-opus-5-5 --effort low [--only id,id]   # one config, resumable
    implement.py launch                                                   # every config, <= 3 at once
    implement.py score                                                    # write SCORES-implementer.md

A run is one (model, effort, case). It gets a fresh worktree of the pinned
worker repo at the case's parent commit, where the case's tests do not exist
yet, and hands `claude -p` the case's spec on stdin. The model may edit files
and run `cargo check` / `cargo build`, nothing else. When it stops, the hidden
test patch goes onto whatever it left and the case's tests run exactly
(`cargo test -p <crate> --lib -- <names> --exact`). Pass means every hidden
test ran and passed.

Also recorded, unscored: the crate's whole lib suite on the same tree (so a
change that breaks an existing test is visible), whether the model touched the
file's test module or any other file, and a tool-use summary from the session
transcript, which is copied next to the result.

One JSON per run in results/<model>@<effort>/<case>.json; an existing file is
skipped, so a rerun resumes. Each config builds into its own
~/.kelpie/targets/<slug> and runs its cases one after another.
"""
import argparse
import json
import os
import re
import shutil
import signal
import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from make_cases import (  # noqa: E402
    TARGETS, add_worktree, apply_patch, load_cases, remove_worktree, run_tests, test_module_start,
)

RESULTS = HERE / "results"
PROJECTS = Path.home() / ".claude" / "projects"
CLAUDE_TIMEOUT_S = 1800
MAX_CONCURRENT = 3
MIN_FREE_GB = 40

CONFIGS = [
    ("claude-opus-5-5", "low"),
    ("claude-opus-5-5", "medium"),
    ("claude-sonnet-5", "medium"),
    ("claude-sonnet-5", "high"),
    ("claude-haiku-4-5-20251001", "low"),
    ("claude-fable-5-1", "medium"),
]

PROMPT = """You are working in a checkout of shep, a Rust process manager (a cargo workspace).

Task:
{spec}

Crate: {crate}
File to change: {file}

Edit source only: do not add or change tests. Finish when `cargo check -p {crate}` passes.
"""


def slug(model, effort):
    return f"{model}@{effort or 'default'}"


def weighted_units(model_usage):
    total = 0.0
    for usage in (model_usage or {}).values():
        total += ((usage.get("inputTokens") or 0) * 1
                  + (usage.get("cacheCreationInputTokens") or 0) * 2
                  + (usage.get("cacheReadInputTokens") or 0) * 0.1
                  + (usage.get("outputTokens") or 0) * 5)
    return total


def claude_cmd(model, effort):
    cmd = ["claude", "-p", "--model", model]
    if effort:
        cmd += ["--effort", effort]
    return cmd + ["--output-format", "json", "--permission-mode", "acceptEdits",
                  "--allowedTools", "Bash(cargo check*)", "Bash(cargo build*)"]


def call_claude(model, effort, prompt, cwd, env):
    """Run claude -p to completion. Returns (response dict, error or None, stderr tail, wall seconds)."""
    t0 = time.time()
    proc = subprocess.Popen(claude_cmd(model, effort), cwd=str(cwd), env=env, text=True,
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            start_new_session=True)
    try:
        stdout, stderr = proc.communicate(prompt, timeout=CLAUDE_TIMEOUT_S)
        error = None
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        stdout, stderr = proc.communicate()
        error = f"timeout after {CLAUDE_TIMEOUT_S}s"
    wall = time.time() - t0
    response = {}
    if stdout.strip():
        try:
            response = json.loads(stdout)
        except json.JSONDecodeError as exc:
            error = error or f"bad --output-format json: {exc}: {stdout[:500]}"
    if not error and proc.returncode != 0 and not response:
        error = (stderr or "").strip()[:2000] or f"exit {proc.returncode}"
    if not error and response.get("is_error"):
        error = str(response.get("result") or response.get("subtype") or "is_error")[:2000]
    return response, error, (stderr or "")[-2000:], wall


def test_module_text(text):
    start = test_module_start(text)
    return "" if start is None else "\n".join(text.split("\n")[start - 1:])


def transcript_summary(session_id, dest):
    """Copy the session transcript beside the result and summarise its tool calls."""
    if not session_id:
        return None
    found = list(PROJECTS.glob(f"*/{session_id}.jsonl"))
    if not found:
        return {"transcript": None}
    src = found[0]
    shutil.copyfile(src, dest)
    tools, bash, edited = {}, [], []
    for line in src.read_text().splitlines():
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        message = entry.get("message") or {}
        if entry.get("type") != "assistant" or not isinstance(message.get("content"), list):
            continue
        for block in message["content"]:
            if block.get("type") != "tool_use":
                continue
            name = block.get("name")
            tools[name] = tools.get(name, 0) + 1
            args = block.get("input") or {}
            if name == "Bash":
                bash.append(str(args.get("command", ""))[:200])
            if name in ("Edit", "Write", "MultiEdit", "NotebookEdit"):
                edited.append(args.get("file_path"))
    # The project folder is named after the throwaway worktree, so nothing else lives in it.
    project = src.parent
    if all(p.name.startswith(session_id) for p in project.iterdir()):
        shutil.rmtree(project, ignore_errors=True)
    return {"transcript": dest.name, "tool_calls": tools, "bash": bash,
            "edited_paths": sorted(set(p for p in edited if p))}


def run_case(model, effort, case, out_dir, target_dir):
    base, wt = add_worktree(case["parent"])
    record = {"model": model, "effort": effort, "case": case["id"], "commit": case["commit"],
              "parent": case["parent"], "crate": case["crate"], "file": case["file"]}
    try:
        env = dict(os.environ, CARGO_TARGET_DIR=str(target_dir))
        prompt = PROMPT.format(spec=case["spec"], crate=case["crate"], file=case["file"])
        response, error, stderr, wall = call_claude(model, effort, prompt, wt, env)
        if error and effort and "effort" in (error + stderr).lower() and not response.get("num_turns"):
            record["effort_rejected"] = error[:500]
            effort = None
            response, error, stderr, wall = call_claude(model, None, prompt, wt, env)
        record["effort_passed"] = effort
        model_usage = response.get("modelUsage") or {}
        record.update({
            "error": error, "stderr_tail": stderr if error else None,
            "num_turns": response.get("num_turns"), "wall_s": round(wall, 1),
            "duration_ms": response.get("duration_ms"),
            "total_cost_usd": response.get("total_cost_usd"),
            "modelUsage": model_usage, "weighted_units": weighted_units(model_usage),
            "stop_reason": response.get("stop_reason"), "subtype": response.get("subtype"),
            "session_id": response.get("session_id"),
            "permission_denials": response.get("permission_denials"),
            "result_text": (response.get("result") or "")[:3000],
        })

        # What the model left, before the hidden tests go on.
        record["diff_stat"] = subprocess.run(["git", "diff", "--stat"], cwd=wt, capture_output=True,
                                             text=True).stdout
        diff = subprocess.run(["git", "diff"], cwd=wt, capture_output=True, text=True).stdout
        (out_dir / f"{case['id']}.diff").write_text(diff)
        status = subprocess.run(["git", "status", "--porcelain", "--untracked-files=all"], cwd=wt,
                                capture_output=True, text=True).stdout.splitlines()
        record["files_changed"] = [line[3:] for line in status]
        # cargo itself rewrites Cargo.lock at parents whose lock trails a version bump.
        record["cargo_lock_changed"] = "Cargo.lock" in record["files_changed"]
        record["other_files_changed"] = [p for p in record["files_changed"]
                                         if p not in (case["file"], "Cargo.lock")]
        parent_text = subprocess.run(["git", "show", f"HEAD:{case['file']}"], cwd=wt,
                                     capture_output=True, text=True).stdout
        model_text = (wt / case["file"]).read_text() if (wt / case["file"]).exists() else ""
        record["touched_test_module"] = test_module_text(parent_text) != test_module_text(model_text)
        record["test_fns_added"] = model_text.count("#[test]") - parent_text.count("#[test]")

        method = apply_patch(wt, HERE / case["test_patch"])
        record["test_patch_applied"] = method
        if method is None:
            record["hidden"] = {"passed": False, "failing": [], "missing": case["tests"],
                                "compile_error": False, "tail": "hidden test patch did not apply"}
        else:
            record["hidden"] = run_tests(wt, case["crate"], case["tests"], target_dir)
        record["passed"] = bool(record["hidden"]["passed"])
        record["failing_tests"] = (record["hidden"].get("failing", [])
                                   + record["hidden"].get("missing", []))

        # Unscored: does the change break anything else the crate already tested?
        if method is not None:
            suite = subprocess.run(["cargo", "test", "-p", case["crate"], "--lib"], cwd=wt,
                                   env=dict(env, CARGO_TERM_COLOR="never"), capture_output=True,
                                   text=True, timeout=1200)
            out = suite.stdout + suite.stderr
            summary = re.findall(r"test result: \w+\. (\d+) passed; (\d+) failed", out)
            record["crate_suite"] = {
                "exit": suite.returncode,
                "failed": [t for t, r in re.findall(r"^test (\S+) \.\.\. (FAILED)", out, re.M)],
                "passed_count": int(summary[0][0]) if summary else None,
            }
        record["transcript"] = transcript_summary(record.get("session_id"),
                                                  out_dir / f"{case['id']}.transcript.jsonl")
    finally:
        remove_worktree(base)
    return record


def run(args):
    out_dir = RESULTS / slug(args.model, args.effort)
    out_dir.mkdir(parents=True, exist_ok=True)
    target_dir = TARGETS / slug(args.model, args.effort)
    only = set(args.only.split(",")) if args.only else None
    cases = [c for c in load_cases(include_backups=bool(only))
             if (only is None or c["id"] in only) and not (out_dir / f"{c['id']}.json").exists()]
    print(f"implement: {len(cases)} cases to run for {out_dir.name}", flush=True)
    for case in cases:
        record = run_case(args.model, args.effort, case, out_dir, target_dir)
        if record["error"] and not record.get("num_turns"):
            # The model never ran (usage limit, API or CLI failure): keep the
            # evidence but not as a result, so a rerun retries the case.
            stamp = time.strftime("%Y%m%d-%H%M%S")
            (out_dir / f"{case['id']}.infra-error-{stamp}.json").write_text(json.dumps(record, indent=1))
            print(f"{time.strftime('%H:%M:%S')} {case['id']} INFRA ERROR (not saved as a result): "
                  f"{record['error'][:300]}", flush=True)
            continue
        (out_dir / f"{case['id']}.json").write_text(json.dumps(record, indent=1))
        print(f"{time.strftime('%H:%M:%S')} {case['id']} passed={record['passed']} "
              f"turns={record['num_turns']} {record['wall_s']:.0f}s units={record['weighted_units']:.0f}"
              + (f" failing={record['failing_tests']}" if not record["passed"] else "")
              + (f" ERROR {record['error'][:300]}" if record["error"] else ""), flush=True)


def free_gb():
    out = subprocess.run(["df", "-k", str(Path.home())], capture_output=True, text=True).stdout
    return int(out.splitlines()[-1].split()[3]) / 1024 / 1024


def launch(args):
    queue = [c for c in CONFIGS if not args.only_config or slug(*c) in args.only_config.split(",")]
    active = {}
    RESULTS.mkdir(exist_ok=True)
    while queue or active:
        for s in list(active):
            proc, log = active[s]
            if proc.poll() is not None:
                print(f"[launch] {time.strftime('%H:%M:%S')} finished {s} rc={proc.returncode}", flush=True)
                log.close()
                del active[s]
        while queue and len(active) < MAX_CONCURRENT:
            gb = free_gb()
            df = subprocess.run(["df", "-h", str(Path.home())], capture_output=True, text=True).stdout
            print(f"[launch] {time.strftime('%H:%M:%S')} df -h ~: {df.splitlines()[-1]}", flush=True)
            if gb < MIN_FREE_GB:
                print(f"[launch] {gb:.0f} GB free, under {MIN_FREE_GB}: holding new configs", flush=True)
                break
            model, effort = queue.pop(0)
            s = slug(model, effort)
            log = open(RESULTS / f"{s}.log", "a")
            cmd = [sys.executable, str(HERE / "implement.py"), "run", "--model", model, "--effort", effort]
            print(f"[launch] {time.strftime('%H:%M:%S')} starting {s}", flush=True)
            active[s] = (subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, cwd=str(HERE)), log)
        time.sleep(20)
    print(f"[launch] {time.strftime('%H:%M:%S')} done", flush=True)


def score(_args):
    cases = load_cases(include_backups=False)
    ids = [c["id"] for c in cases]
    rows = []
    for model, effort in CONFIGS:
        s = slug(model, effort)
        recs = {}
        for cid in ids:
            path = RESULTS / s / f"{cid}.json"
            if path.exists():
                recs[cid] = json.loads(path.read_text())
        rows.append((s, recs))

    def cell(rec):
        if rec is None:
            return "-"
        if rec.get("error") and not rec.get("passed"):
            return "ERR"
        return "pass" if rec["passed"] else "FAIL"

    lines = ["# Implementer calibration", "",
             "Six small, well-specified changes from shep's history (`cases.jsonl`). Each run starts "
             "from the case's parent commit with its tests absent, gives `claude -p` the spec, and then "
             "applies the hidden test patch and runs those tests exactly. One sample per cell, so a "
             "single flipped cell is noise: compare totals.", "",
             "## Pass/fail by case", "",
             "| config | " + " | ".join(ids) + " |",
             "|---|" + "---|" * len(ids)]
    for s, recs in rows:
        lines.append(f"| {s} | " + " | ".join(cell(recs.get(cid)) for cid in ids) + " |")
    lines += ["", "## Per config", "",
              "| config | passed | runs | total units | total cost USD | median wall s | median turns | denied tool calls |",
              "|---|---|---|---|---|---|---|---|"]
    for s, recs in rows:
        done = list(recs.values())
        if not done:
            lines.append(f"| {s} | - | 0 | - | - | - | - | - |")
            continue
        passed = sum(1 for r in done if r["passed"])
        units = sum(r.get("weighted_units") or 0 for r in done)
        cost = sum(r.get("total_cost_usd") or 0 for r in done)
        walls = [r["wall_s"] for r in done if r.get("wall_s") is not None]
        turns = [r["num_turns"] for r in done if r.get("num_turns") is not None]
        denied = sum(len(r.get("permission_denials") or []) for r in done)
        lines.append(f"| {s} | {passed}/{len(ids)} | {len(done)} | {units:,.0f} | {cost:.2f} | "
                     f"{statistics.median(walls):.0f} | {statistics.median(turns) if turns else '-'} | {denied} |")
    lines += ["", "Units are input x1 + cache creation x2 + cache read x0.1 + output x5, summed over "
              "every model in `modelUsage` (the sibling harness's weighting). Wall is the `claude -p` "
              "call alone, not the hidden test run.", "", "## Flags (generated)", ""]
    for s, recs in rows:
        for cid, r in recs.items():
            flags = []
            if r.get("error"):
                flags.append(f"error: {r['error'][:160]}")
            if r.get("touched_test_module"):
                flags.append(f"touched the test module ({r.get('test_fns_added', 0):+d} #[test])")
            if r.get("other_files_changed"):
                flags.append(f"changed other files: {r['other_files_changed']}")
            if r.get("test_patch_applied") not in ("git apply", None):
                flags.append(f"hidden patch needed `{r['test_patch_applied']}`")
            if r.get("test_patch_applied") is None:
                flags.append("hidden patch did not apply")
            if r.get("hidden", {}).get("compile_error"):
                flags.append("hidden tests did not compile")
            suite = r.get("crate_suite") or {}
            if suite.get("failed"):
                flags.append(f"crate suite failures: {suite['failed']}")
            if r.get("effort_rejected"):
                flags.append("--effort rejected, reran without it")
            if flags:
                lines.append(f"- {s} / {cid}: " + "; ".join(flags))
    if lines[-1] == "":
        lines.append("None: no run errored, touched its test module or another file, needed a fuzzy "
                     "hidden-patch apply, failed to compile the hidden tests, or broke the crate's suite.")
    # Everything from "## Notes" down is hand-written: keep it across regenerations.
    existing = HERE / "SCORES-implementer.md"
    if existing.exists() and "\n## Notes" in existing.read_text():
        lines += ["", "## Notes" + existing.read_text().split("\n## Notes", 1)[1].rstrip()]
    (HERE / "SCORES-implementer.md").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--model", required=True)
    r.add_argument("--effort")
    r.add_argument("--only")
    lp = sub.add_parser("launch")
    lp.add_argument("--only-config", help="comma-separated slugs to launch, default all")
    sub.add_parser("score")
    args = parser.parse_args()
    {"run": run, "launch": launch, "score": score}[args.cmd](args)


if __name__ == "__main__":
    main()
