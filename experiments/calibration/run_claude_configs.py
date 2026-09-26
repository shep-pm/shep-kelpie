#!/usr/bin/env python3
"""Launch every --backend claude config as its own calibrate.py process, at
most MAX_CONCURRENT running at once, queueing the rest until a slot frees.

    ./run_claude_configs.py <stop_launch_epoch>

After stop_launch_epoch (a unix timestamp), no new config is started; any
already running keeps going until it finishes or something else kills it
(the hard-deadline kill is done from outside this script, by pattern-matching
process command lines -- see SCORES.md notes).

Logs each config's calibrate.py stdout/stderr to results/<slug>.log.
"""
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"
RESULTS.mkdir(exist_ok=True)

STOP_LAUNCH_EPOCH = float(sys.argv[1]) if len(sys.argv) > 1 else float("inf")
MAX_CONCURRENT = 6

# (model, effort or None), in launch-priority order.
CONFIGS = [
    ("claude-opus-5-5", "low"),
    ("claude-opus-5-5", "medium"),
    ("claude-opus-5-5", "high"),
    ("claude-sonnet-5", "medium"),
    ("claude-sonnet-5", "high"),
    ("claude-sonnet-5", "xhigh"),
    ("claude-haiku-4-5-20251001", "low"),
    ("claude-fable-5-1", "medium"),
    ("claude-fable-5-1", "high"),
]


def slug(model, effort):
    return f"{model}@{effort or 'default'}"


def launch(model, effort):
    s = slug(model, effort)
    log = open(RESULTS / f"{s}.log", "a")
    cmd = [str(HERE / "calibrate.py"), "--backend", "claude", "--model", model]
    if effort:
        cmd += ["--effort", effort]
    print(f"[launcher] {time.strftime('%H:%M:%S')} starting {s}: {' '.join(cmd)}", flush=True)
    proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, cwd=str(HERE))
    return proc, log


def main():
    queue = list(CONFIGS)
    active = {}  # slug -> (Popen, filehandle)
    dropped = []

    while queue or active:
        for s in list(active):
            proc, log = active[s]
            if proc.poll() is not None:
                print(f"[launcher] {time.strftime('%H:%M:%S')} finished {s} rc={proc.returncode}", flush=True)
                log.close()
                del active[s]

        while queue and len(active) < MAX_CONCURRENT and time.time() < STOP_LAUNCH_EPOCH:
            model, effort = queue.pop(0)
            active[slug(model, effort)] = launch(model, effort)

        if queue and time.time() >= STOP_LAUNCH_EPOCH:
            dropped = [slug(m, e) for m, e in queue]
            print(f"[launcher] {time.strftime('%H:%M:%S')} stop-launch deadline hit, "
                  f"dropping unstarted: {dropped}", flush=True)
            queue = []

        time.sleep(5)

    print(f"[launcher] {time.strftime('%H:%M:%S')} done. dropped={dropped}", flush=True)


if __name__ == "__main__":
    main()
