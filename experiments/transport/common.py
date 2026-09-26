"""Shared helpers for the transport test series.

Every worker call appends one JSON line to results/<test>.jsonl. Paths are
kept repo-relative or symbolic so the ledgers can be committed.
"""

import datetime
import json
import os
import pathlib
import re
import subprocess
import time

HERE = pathlib.Path(__file__).resolve().parent
RESULTS = HERE / "results"
WORKER_REPO = pathlib.Path(
    os.environ.get("KELPIE_WORKER_REPO", os.path.expanduser("~/.kelpie/repos/shep"))
)
MODEL = os.environ.get("KELPIE_MODEL", "sonnet")

# Weighted units: cache read 0.1, one-hour cache write 2, output 5, input 1.
# A five-minute cache write is priced at 1.25.
WEIGHTS = {"input": 1.0, "write_1h": 2.0, "write_5m": 1.25, "read": 0.1, "output": 5.0}

# Ten shep source files of roughly 3k tokens each, read one per turn so
# context grows at a steady rate.
TURN_FILES = [
    "crates/shep-channel/src/dispatch.rs",
    "crates/shep-cli/src/lookout/edits.rs",
    "crates/shep-cli/src/output/flock.rs",
    "crates/shep-cli/src/serve/path.rs",
    "crates/shep-cli/src/welcome.rs",
    "crates/shep-client/src/spawn.rs",
    "crates/shep-core/src/config/daemon/sections.rs",
    "crates/shep-core/src/secrets/resolve_mod.rs",
    "crates/shep-daemon/src/fake/fake_process.rs",
    "crates/shep-daemon/src/runner/log_path_security.rs",
]


def turn_prompt(i):
    return (
        f"Read {TURN_FILES[i]} with the Read tool, then reply with exactly one line: "
        "the name of the first function it defines. No other text."
    )


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


_VERSION = None


def claude_version():
    global _VERSION
    if _VERSION is None:
        out = subprocess.run(["claude", "--version"], capture_output=True, text=True)
        _VERSION = out.stdout.strip()
    return _VERSION


def units(usage):
    """Weighted units for one call's `usage` object."""
    if not usage:
        return None
    cc = usage.get("cache_creation") or {}
    w1h = cc.get("ephemeral_1h_input_tokens")
    w5m = cc.get("ephemeral_5m_input_tokens")
    total_write = usage.get("cache_creation_input_tokens", 0) or 0
    if w1h is None and w5m is None:
        w1h, w5m = total_write, 0
    return round(
        (usage.get("input_tokens", 0) or 0) * WEIGHTS["input"]
        + (w1h or 0) * WEIGHTS["write_1h"]
        + (w5m or 0) * WEIGHTS["write_5m"]
        + (usage.get("cache_read_input_tokens", 0) or 0) * WEIGHTS["read"]
        + (usage.get("output_tokens", 0) or 0) * WEIGHTS["output"],
        1,
    )


def context(usage):
    """Input-side tokens of one call: what the model read."""
    if not usage:
        return None
    return sum(
        usage.get(k, 0) or 0
        for k in ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens")
    )


def ledger(test, record):
    RESULTS.mkdir(parents=True, exist_ok=True)
    record = {"test": test, "ts": now(), "claude": claude_version(), "model": MODEL, **record}
    with open(RESULTS / f"{test}.jsonl", "a") as f:
        f.write(json.dumps(record) + "\n")
    return record


def summarize_usage(usage):
    if not usage:
        return {}
    cc = usage.get("cache_creation") or {}
    return {
        "input": usage.get("input_tokens"),
        "cache_write": usage.get("cache_creation_input_tokens"),
        "cache_write_1h": cc.get("ephemeral_1h_input_tokens"),
        "cache_write_5m": cc.get("ephemeral_5m_input_tokens"),
        "cache_read": usage.get("cache_read_input_tokens"),
        "output": usage.get("output_tokens"),
        "context": context(usage),
        "units": units(usage),
    }


def run_p(prompt, extra_args=(), cwd=None, timeout=900):
    """One `claude -p` call with the prompt on stdin. Returns (result, wall_s, stderr)."""
    args = ["claude", "-p", "--output-format", "json", "--model", MODEL, *extra_args]
    t0 = time.monotonic()
    proc = subprocess.run(
        args,
        input=prompt,
        capture_output=True,
        text=True,
        cwd=str(cwd or WORKER_REPO),
        timeout=timeout,
    )
    wall = round(time.monotonic() - t0, 2)
    try:
        result = json.loads(proc.stdout)
    except json.JSONDecodeError:
        result = {"parse_error": True, "stdout_head": proc.stdout[:500]}
    return result, wall, proc.stderr[-2000:]


def call_record(result, wall):
    """The fields every ledger line carries for one call."""
    return {
        "session_id": result.get("session_id"),
        "wall_s": wall,
        "duration_ms": result.get("duration_ms"),
        "duration_api_ms": result.get("duration_api_ms"),
        "ttft_ms": result.get("ttft_ms"),
        "num_turns": result.get("num_turns"),
        "is_error": result.get("is_error"),
        "subtype": result.get("subtype"),
        "total_cost_usd_cumulative": result.get("total_cost_usd"),
        "result_head": (result.get("result") or "")[:120],
        **summarize_usage(result.get("usage")),
    }


_USAGE_RE = {
    "week_pct": re.compile(r"Current week \(all models\):\s*(\d+)% used"),
    "session_pct": re.compile(r"Current session:\s*(\d+)% used"),
    "week_resets": re.compile(r"Current week \(all models\):.*?resets ([^\n(]+)"),
}


def usage_snapshot():
    """Account utilization through the headless `/usage` command. Costs no tokens."""
    proc = subprocess.run(
        ["claude", "-p", "/usage", "--output-format", "json"],
        input="",
        capture_output=True,
        text=True,
        timeout=120,
    )
    try:
        text = json.loads(proc.stdout).get("result", "")
    except json.JSONDecodeError:
        text = proc.stdout
    snap = {"ts": now()}
    for key, rx in _USAGE_RE.items():
        m = rx.search(text)
        snap[key] = (int(m.group(1)) if key.endswith("pct") else m.group(1).strip()) if m else None
    return snap


def warm_up(test, extra_args=()):
    """One fresh trivial call so every run starts with the prefix cache warm."""
    result, wall, _ = run_p("Reply with exactly: ok", extra_args)
    return ledger(test, {"phase": "warmup", **call_record(result, wall)})
