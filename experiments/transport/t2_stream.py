"""T2: one long-lived stream-json worker, held by this script as its runner.

Usage: python3 t2_stream.py RUN [IDLE_SECONDS]

Also records every event type the worker emits, for test R.
"""

import collections
import json
import os
import queue
import signal
import subprocess
import sys
import threading
import time
import uuid

from common import MODEL, WORKER_REPO, ledger, summarize_usage, turn_prompt, warm_up

BIGLINE = "/tmp/kelpie-bigline.txt"
BIGLINE_CHARS = 120_000


class Worker:
    """A `claude` child speaking stream-json on stdin and stdout."""

    def __init__(self, test, run, sid, resume=False):
        self.test, self.run, self.sid = test, run, sid
        args = [
            "claude", "-p",
            "--input-format", "stream-json",
            "--output-format", "stream-json",
            "--verbose",
            "--model", MODEL,
            "--allowedTools", f"Bash(cat {BIGLINE})",
            "--resume" if resume else "--session-id", sid,
        ]
        self.proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=str(WORKER_REPO),
            text=True,
            bufsize=1,
            start_new_session=True,
        )
        self.events = queue.Queue()
        self.types = collections.Counter()
        self.limit_samples = []
        self.max_line = 0
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.proc.stdout:
            self.max_line = max(self.max_line, len(line))
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                self.types["<unparsed>"] += 1
                continue
            key = ev.get("type", "?") + ("/" + ev["subtype"] if ev.get("subtype") else "")
            self.types[key] += 1
            text = line.lower()
            if ("rate_limit" in text or "ratelimit" in text) and len(self.limit_samples) < 5:
                self.limit_samples.append(sample_shape(ev))
            self.events.put((time.monotonic(), len(line), ev))
        self.events.put((time.monotonic(), 0, None))

    def send(self, text):
        msg = {"type": "user", "message": {"role": "user", "content": text}}
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def turn(self, label, text, timeout=600):
        """Send one user turn and wait for its result event."""
        t0 = time.monotonic()
        self.send(text)
        first = None
        longest = 0
        seen = collections.Counter()
        while True:
            try:
                ts, n, ev = self.events.get(timeout=timeout)
            except queue.Empty:
                return self.log(label, {"timeout": True})
            if ev is None:
                return self.log(label, {"worker_exited": True, "stderr_tail": self.stderr_tail()})
            longest = max(longest, n)
            key = ev.get("type", "?") + ("/" + ev["subtype"] if ev.get("subtype") else "")
            seen[key] += 1
            if first is None and ev.get("type") in ("assistant", "stream_event"):
                first = ts - t0
            if ev.get("type") == "result":
                return self.log(label, {
                    "wall_s": round(ts - t0, 2),
                    "first_event_s": round(first, 2) if first is not None else None,
                    "longest_line": longest,
                    "events": dict(seen),
                    "session_id": ev.get("session_id"),
                    "is_error": ev.get("is_error"),
                    "subtype": ev.get("subtype"),
                    "num_turns": ev.get("num_turns"),
                    "total_cost_usd_cumulative": ev.get("total_cost_usd"),
                    "result_head": (ev.get("result") or "")[:120],
                    **summarize_usage(ev.get("usage")),
                })

    def log(self, label, fields):
        rec = ledger(self.test, {"run": self.run, "turn": label, **fields})
        print(f"t2 r{self.run} {label}: ctx={rec.get('context')} w={rec.get('cache_write')} "
              f"r={rec.get('cache_read')} units={rec.get('units')} {rec.get('wall_s')}s "
              f"longest={rec.get('longest_line')}", flush=True)
        return rec

    def stderr_tail(self):
        try:
            return self.proc.stderr.read()[-400:]
        except Exception:
            return None

    def kill(self):
        os.killpg(self.proc.pid, signal.SIGKILL)
        self.proc.wait()

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=60)
        except Exception:
            self.kill()


def sample_shape(ev):
    """An event's keys and small scalar values, never its text content."""
    def shape(v, depth=0):
        if isinstance(v, dict) and depth < 3:
            return {k: shape(x, depth + 1) for k, x in v.items()}
        if isinstance(v, (int, float, bool)) or v is None:
            return v
        if isinstance(v, str) and len(v) < 40:
            return v
        return type(v).__name__
    return shape(ev)


def main():
    run = int(sys.argv[1])
    idle = int(sys.argv[2]) if len(sys.argv) > 2 else 600
    test = f"t2.run{run}"
    if not os.path.exists(BIGLINE) or os.path.getsize(BIGLINE) < BIGLINE_CHARS:
        with open(BIGLINE, "w") as f:
            f.write("x" * BIGLINE_CHARS + "\n")
    warm_up(test)
    sid = str(uuid.uuid4())
    w = Worker(test, run, sid)
    for i in range(10):
        w.turn(i + 1, turn_prompt(i))
    ledger(test, {"run": run, "phase": "idle", "seconds": idle})
    time.sleep(idle)
    w.turn("after_idle", "Reply with exactly: awake")
    w.turn("bigline", f"Run exactly this command with the Bash tool: cat {BIGLINE} "
                      "Then reply with exactly: done")
    w.turn("compact", "/compact")
    w.turn("after_compact", "Reply with exactly: compacted")
    # A crash mid-turn: send a turn that reads a file, kill the worker before
    # it answers, then resume the session in a fresh child.
    w.send(turn_prompt(0))
    time.sleep(5)
    w.kill()
    ledger(test, {"run": run, "phase": "killed_mid_turn", "types": dict(w.types),
                  "limit_samples": w.limit_samples, "max_line": w.max_line})
    w2 = Worker(test, run, sid, resume=True)
    w2.turn("after_crash_resume", "Reply with exactly: resumed")
    w2.close()
    ledger(test, {"run": run, "phase": "done", "types_after_resume": dict(w2.types),
                  "limit_samples_after_resume": w2.limit_samples})


if __name__ == "__main__":
    main()
