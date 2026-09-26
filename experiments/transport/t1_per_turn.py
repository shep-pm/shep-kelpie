"""T1: one `claude -p` process per turn.

Usage: python3 t1_per_turn.py [variant ...]
Variants: baseline (three runs), sys_every, sys_first, nohooks, crash.
"""

import glob
import json
import os
import signal
import subprocess
import sys
import time
import uuid

from common import (
    HERE,
    MODEL,
    WORKER_REPO,
    call_record,
    ledger,
    run_p,
    turn_prompt,
    usage_snapshot,
    warm_up,
)

TEST = "t1"
TURNS = 10
# About 5k tokens of real instructions, for the system-prompt variants.
SYS_FILE = str((HERE / "../../docs/design-log.md").resolve())
NOHOOKS = ["--settings", json.dumps({"disableAllHooks": True})]


def transcript_lines(sid):
    paths = glob.glob(os.path.expanduser(f"~/.claude/projects/*/{sid}.jsonl"))
    if not paths:
        return None
    with open(paths[0]) as f:
        return sum(1 for _ in f)


def turn_args(variant, turn, sid):
    args = ["--session-id", sid] if turn == 0 else ["--resume", sid]
    if variant == "sys_every" or (variant == "sys_first" and turn == 0):
        args += ["--append-system-prompt-file", SYS_FILE]
    if variant == "nohooks":
        args += NOHOOKS
    return args


def one_turn(variant, run, turn, sid):
    result, wall, err = run_p(turn_prompt(turn), turn_args(variant, turn, sid))
    rec = ledger(
        TEST,
        {"variant": variant, "run": run, "turn": turn + 1, **call_record(result, wall)},
    )
    if result.get("session_id") and result["session_id"] != sid:
        ledger(TEST, {"variant": variant, "run": run, "note": "session id changed", "got": result["session_id"]})
    if rec.get("is_error") or result.get("parse_error"):
        ledger(TEST, {"variant": variant, "run": run, "turn": turn + 1, "stderr_tail": err[-400:]})
    print(f"{variant} r{run} t{turn + 1}: ctx={rec.get('context')} w={rec.get('cache_write')} "
          f"r={rec.get('cache_read')} units={rec.get('units')} {wall}s", flush=True)
    return rec


def run_variant(variant, run):
    warm_up(TEST, NOHOOKS if variant == "nohooks" else ())
    sid = str(uuid.uuid4())
    for turn in range(TURNS):
        if variant == "crash" and turn == 5:
            crash_turn(run, sid)
        one_turn(variant, run, turn, sid)


def crash_turn(run, sid):
    """Kill a worker mid-turn, as a runner crash would, and record what survives."""
    before = transcript_lines(sid)
    # stream-json output for this one call, so the kill lands on the first
    # tool call, genuinely mid-turn, rather than after a fixed sleep.
    proc = subprocess.Popen(
        ["claude", "-p", "--output-format", "stream-json", "--verbose",
         "--model", MODEL, "--resume", sid],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=str(WORKER_REPO),
        text=True,
        start_new_session=True,
    )
    proc.stdin.write(turn_prompt(5))
    proc.stdin.close()
    t0 = time.monotonic()
    killed_on = None
    for line in proc.stdout:
        if '"tool_use"' in line:
            killed_on = "first tool_use"
            break
    alive = proc.poll() is None
    if alive:
        os.killpg(proc.pid, signal.SIGKILL)
    proc.wait()
    ledger(TEST, {"variant": "crash", "run": run, "turn": 6, "phase": "kill_point",
                  "killed_on": killed_on, "after_s": round(time.monotonic() - t0, 2)})
    time.sleep(1)
    after = transcript_lines(sid)
    ledger(TEST, {"variant": "crash", "run": run, "turn": 6, "phase": "killed",
                  "alive_at_kill": alive, "transcript_lines_before": before,
                  "transcript_lines_after": after})
    print(f"crash r{run}: killed turn 6 (alive={alive}), transcript {before} -> {after}", flush=True)


def main():
    variants = sys.argv[1:] or ["baseline", "sys_every", "sys_first", "nohooks", "crash"]
    ledger(TEST, {"phase": "usage_before", **usage_snapshot()})
    for variant in variants:
        runs = 3 if variant == "baseline" else 1
        for run in range(1, runs + 1):
            run_variant(variant, run)
    ledger(TEST, {"phase": "usage_after", **usage_snapshot()})


if __name__ == "__main__":
    main()
