"""A compact work-split run: one reading-heavy work item, three strategies.

Usage: python3 ws_mini.py STRATEGY RUN
Strategies: inline, crew, crew_nohooks, phased, inline_cbm.

The work item reviews crates/shep-daemon/src/supervisor/ (54 files, about
22k lines) in the pinned worker repo. Cost comes from `modelUsage`, which
counts subagents too.
"""

import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "../transport"))
from common import MODEL, WORKER_REPO, ledger  # noqa: E402

TEST = "ws_mini"
MODULE = os.environ.get("KELPIE_WS_MODULE", "crates/shep-daemon/src/supervisor")
BRIEF = (
    f"Review every .rs file under {MODULE} for three risks: an error that is silently discarded, "
    "a lock or guard held across an .await, and a child process that can be left unreaped. "
    "Report each finding on its own line as `path:line: claim`, at most 20 findings, strongest first, "
    "then one final line `FINDINGS: <n>`. Read the code; do not guess from file names."
)
WEIGHTS = {"inputTokens": 1.0, "cacheCreationInputTokens": 2.0, "cacheReadInputTokens": 0.1, "outputTokens": 5.0}


def files():
    out = subprocess.run(["git", "ls-files", f"{MODULE}/*.rs", f"{MODULE}/**/*.rs"],
                         capture_output=True, text=True, cwd=str(WORKER_REPO)).stdout.split()
    return sorted(set(out))


def run_p(prompt, extra):
    t0 = time.monotonic()
    proc = subprocess.run(["claude", "-p", "--output-format", "json", "--model", MODEL, *extra],
                          input=prompt, capture_output=True, text=True, cwd=str(WORKER_REPO), timeout=3600)
    wall = round(time.monotonic() - t0, 1)
    try:
        return json.loads(proc.stdout), wall
    except json.JSONDecodeError:
        return {"parse_error": True, "stderr": proc.stderr[-400:]}, wall


def units(model_usage):
    total, per = 0.0, {}
    for model, u in (model_usage or {}).items():
        v = sum((u.get(k) or 0) * w for k, w in WEIGHTS.items())
        per[model] = round(v)
        total += v
    return round(total), per


def record(strategy, run, step, result, wall):
    total, per = units(result.get("modelUsage"))
    text = result.get("result") or ""
    rec = ledger(TEST, {
        "strategy": strategy, "run": run, "step": step, "module": MODULE, "wall_s": wall,
        "session_id": result.get("session_id"), "num_turns": result.get("num_turns"), "is_error": result.get("is_error"),
        "subtype": result.get("subtype"), "total_cost_usd": result.get("total_cost_usd"),
        "units_all_models": total, "units_by_model": per,
        "last_call_context": sum((result.get("usage") or {}).get(k, 0) or 0 for k in
                                 ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens")),
        "subagent_stats": result.get("subagent_stats"),
        "findings_line": next((l for l in text.splitlines()[::-1] if l.startswith("FINDINGS")), None),
        "report": text[-6000:],
    })
    print(f"{strategy} r{run} {step}: units={total} turns={rec['num_turns']} {wall}s "
          f"{rec['findings_line']}", flush=True)
    return rec


def main():
    strategy, run = sys.argv[1], int(sys.argv[2])
    fs = files()
    if strategy == "inline":
        result, wall = run_p(BRIEF + " Do all the reading yourself; do not use the Agent tool.",
                             ["--disallowedTools", "Agent"])
        record(strategy, run, "all", result, wall)
    elif strategy == "inline_cbm":
        # Same as inline, plus the codebase-memory-mcp server: a pre-built graph
        # of this repo the worker can query instead of guessing where to look.
        cbm_config = os.path.join(os.path.dirname(os.path.abspath(__file__)), "cbm-mcp-config.json")
        result, wall = run_p(
            BRIEF + " Do all the reading yourself; do not use the Agent tool. A codebase-memory-mcp "
            "server is also connected (tools named mcp__cbm__*): it holds a pre-built code graph of "
            "this repository (search_graph, query_graph, trace_path, get_code_snippet, "
            "get_file_outline, get_architecture, search_code, and related read tools). You may use it "
            "to navigate the codebase — find callers, trace paths, look up symbols — instead of "
            "guessing from file names, but still read the actual source before reporting a finding.",
            ["--disallowedTools", "Agent", "--mcp-config", cbm_config])
        record(strategy, run, "all", result, wall)
    elif strategy in ("crew", "crew_nohooks"):
        # The maintainer's interactive hooks block review dispatches from a
        # worker, so crew_nohooks runs with every hook off.
        extra = ["--settings", json.dumps({"disableAllHooks": True})] if strategy == "crew_nohooks" else []
        result, wall = run_p(
            BRIEF + " Split the files into four groups of similar size and review them with four "
            "subagents in parallel (Agent tool, model sonnet), one group each, giving each the same "
            "brief. Then merge, deduplicate and rank their findings yourself. Your final message must "
            "repeat the full merged, deduplicated list of findings (one per line, `path:line: claim`) "
            "and the `FINDINGS: <n>` line in full — do not refer back to a subagent's report or an "
            "earlier message instead of restating them.", extra)
        record(strategy, run, "all", result, wall)
    elif strategy == "phased":
        half = len(fs) // 2
        first, second = fs[:half], fs[half:]
        a, wall = run_p(
            BRIEF + " This session covers only these files: " + " ".join(first) +
            ". End with a section `HANDOFF:` that a fresh session will read before it reviews the "
            "remaining files: your findings so far and anything it must know, in under 400 words.",
            ["--disallowedTools", "Agent"])
        rec = record(strategy, run, "phase_a", a, wall)
        handoff = a.get("result") or ""
        b, wall = run_p(
            BRIEF + " A previous session already reviewed part of the module and left this handoff:\n\n"
            + handoff[-4000:] + "\n\nThis session covers only these files: " + " ".join(second) +
            ". Merge the previous findings with yours in the final report.",
            ["--disallowedTools", "Agent"])
        record(strategy, run, "phase_b", b, wall)
    else:
        sys.exit(f"unknown strategy {strategy}")


if __name__ == "__main__":
    main()
