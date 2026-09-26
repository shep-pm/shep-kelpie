#!/usr/bin/env python3
"""Score a local Ollama model as a findings checker and as a diff reviewer.

    calibrate.py --model qwen3.8:27b                 # both sets, resumable
    calibrate.py --model gpt-oss:20b --think low     # passes think through as-is
    calibrate.py --model devstral --set reviewer --only r01,c01
    calibrate.py --check-sets                        # apply every mutation, size every prompt, no model
    calibrate.py --score                             # one table over every model in results/

Two sets, both drawn from shep (sets/*.jsonl):

  checker   A finding plus the code it cites; the model says whether the
            problem is real. 20 real findings that held when verified, 6 real
            ones that did not, and 9 synthetic ones: a true finding with one
            checkable detail flipped. Scored as balanced accuracy, so a model
            that says "holds" to everything lands at 50%, not 57%.

  reviewer  Real shep commits (sets/diffs), clean and with a planted bug on an
            added line. Caught means a bug or risk finding that quotes the
            planted line or names one of its anchors. False alarms are
            bug-severity findings on the clean copies. Two tiers: easy bugs
            sit beside the removed line they break or contradict the subject,
            and Q caught all nine with no false alarm; hard bugs sit in new
            code or in call order, and the hard clean diffs are the shapes Q
            invents findings on.

Every case is one sample at the model's own default sampling, so a single
flipped verdict is noise. Compare totals, not cases.

Each request takes the same GPU lock qwen-review.sh does, one case at a time,
so a review round can slot in between cases instead of contending. A model
other than Q is unloaded when the run ends, so the VRAM goes back.
"""
import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))  # this copy's own local_llm.py sits beside it
from local_llm import (  # noqa: E402  (needs HERE on the path first)
    LOCK, LOCK_WAIT, NUM_CTX, GpuLock, chat, ollama_base, parse_json, unload, unload_others,
)

SETS = HERE / "sets"
RESULTS = HERE / "results"
REPO = Path(os.environ.get("CALIBRATE_REPO", Path.home() / "GitHub" / "pm2-rs"))
Q_MODEL = "qwen3.8:27b"

CHECKER_SCHEMA = {
    "type": "object",
    "properties": {
        "verdict": {"type": "string", "enum": ["holds", "fails", "unsure"]},
        "reason": {"type": "string"},
    },
    "required": ["verdict", "reason"],
}
REVIEWER_SCHEMA = {
    "type": "object",
    "properties": {
        "findings": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "severity": {"type": "string", "enum": ["bug", "risk", "nit"]},
                    "quote": {"type": "string"},
                    "what": {"type": "string"},
                },
                "required": ["severity", "quote", "what"],
            },
        }
    },
    "required": ["findings"],
}

CHECKER_PROMPT = """A reviewer reported the finding below about some Rust code. Findings like this are often wrong: a miscount, a misread of what the code does, an invariant the finding ignores, a hazard that cannot actually happen, or a claim about code that says something else. Decide whether the problem it describes is real in the code shown.

Finding (severity|location|problem|suggested fix):
{finding}

Code at commit {commit}, with line numbers:
{code}
{greps}
Answer with JSON only: {{"verdict": "holds" | "fails" | "unsure", "reason": "one or two sentences citing lines"}}.
holds: the problem is real as described. fails: it is not. unsure: the code shown cannot settle it."""

REVIEWER_PROMPT = """Review this commit to shep, a Rust process manager. Report only defects: code that does the wrong thing, changes behaviour the commit does not intend, drops behaviour the removed code had, or fails at runtime. Do not report style, naming, documentation or test-coverage suggestions.

Commit subject: {subject}

```diff
{diff}
```

Answer with JSON only: {{"findings": [{{"severity": "bug" | "risk" | "nit", "quote": "one line copied exactly from the diff", "what": "one sentence"}}]}}. An empty list is the right answer for a commit with no defect."""


def die(message):
    print(f"calibrate: {message}", file=sys.stderr)
    sys.exit(1)


def load(name):
    with open(SETS / f"{name}.jsonl") as fh:
        return [json.loads(line) for line in fh if line.strip()]


def git(*args):
    return subprocess.run(["git", "-C", str(REPO), *args], check=True,
                          capture_output=True, text=True).stdout


def render_code(case):
    blocks = []
    for spec in case["files"]:
        path, _, span = spec.partition(":")
        lines = git("show", f"{case['commit']}:{path}").split("\n")
        start, end = (int(n) for n in span.split("-")) if span else (1, len(lines))
        end = min(end, len(lines))
        body = "\n".join(f"{n:5} | {lines[n - 1]}" for n in range(start, end + 1))
        blocks.append(f"--- {path} (lines {start}-{end})\n{body}")
    return "\n\n".join(blocks)


def render_greps(case):
    out = []
    for grep in case.get("greps", []):
        hits = git("grep", "-n", "-F", grep["pattern"], case["commit"], "--", grep["path"])
        hits = "\n".join(line.split(":", 1)[1] for line in hits.splitlines()[:40])
        out.append(f"\nSearch: git grep -n -F '{grep['pattern']}' -- {grep['path']}\n{hits}\n")
    return "".join(out)


def mutate(case):
    diff = (SETS / "diffs" / case["diff"]).read_text()
    mutation = case.get("mutation")
    if not mutation:
        return diff, None
    find, nth = mutation["find"], mutation.get("nth", 1)
    count = diff.count(find)
    if count < nth:
        raise ValueError(f"{case['id']}: mutation matches {count} times, needs occurrence {nth}")
    at = -1
    for _ in range(nth):
        at = diff.index(find, at + 1)
    mutated = diff[:at] + mutation["replace"] + diff[at + len(find):]
    planted = mutation["replace"].strip().lstrip("+").strip() or None
    return mutated, planted


def prompt_for(set_name, case):
    if set_name == "checker":
        return CHECKER_PROMPT.format(finding=case["finding"], commit=case["commit"],
                                     code=render_code(case), greps=render_greps(case)), CHECKER_SCHEMA
    diff, _ = mutate(case)
    return REVIEWER_PROMPT.format(subject=case["subject"], diff=diff), REVIEWER_SCHEMA


def loaded_split(base, model):
    """How much of the model Ollama holds in VRAM right now, from /api/ps."""
    try:
        with urllib.request.urlopen(f"{base}/api/ps", timeout=10) as response:
            running = json.loads(response.read()).get("models", [])
    except (urllib.error.URLError, OSError, ValueError):
        return None
    for entry in running:
        if entry.get("name") == model or entry.get("model") == model:
            size = entry.get("size") or 0
            return {"size_gb": size / 1e9, "vram_gb": (entry.get("size_vram") or 0) / 1e9,
                    "gpu_share": (entry.get("size_vram") or 0) / size if size else None,
                    "context": entry.get("context_length")}
    return None


def normal(text):
    return re.sub(r"\s+", " ", (text or "").strip().lstrip("+-").strip())


def score_reviewer_case(case, parsed):
    findings = (parsed or {}).get("findings") or []
    bugs = [f for f in findings if f.get("severity") == "bug"]
    if case["kind"] == "clean":
        return {"caught": None, "false_alarms": len(bugs), "matched": None}
    _, planted = mutate(case)
    planted_n = normal(planted) if planted else ""
    anchors = [a.lower() for a in case["anchors"]]
    for finding in findings:
        if finding.get("severity") not in ("bug", "risk"):
            continue
        quote = normal(finding.get("quote"))
        by_quote = planted_n and len(quote) >= 6 and (quote in planted_n or planted_n in quote)
        text = f"{finding.get('quote', '')} {finding.get('what', '')}".lower()
        if by_quote or any(anchor in text for anchor in anchors):
            others = sum(1 for f in bugs if f is not finding)
            return {"caught": True, "false_alarms": others, "matched": finding}
    return {"caught": False, "false_alarms": len(bugs), "matched": None}


def slug(model, think):
    tag = re.sub(r"[^A-Za-z0-9._-]+", "_", model)
    return tag if think is None else f"{tag}@think-{str(think).lower()}"


def claude_slug(model, effort):
    return f"{model}@{effort or 'default'}"


CLAUDE_TIMEOUT_S = 900  # a per-call safety net; the 22:55 hard stop kills the tree from outside


def claude_schema_suffix(schema):
    return ("\n\nRespond with only a single JSON object matching this JSON schema, and no "
            "other text (no markdown fences, no commentary):\n" + json.dumps(schema))


def run_claude_case(model, effort, set_name, case, cwd):
    """One case through `claude -p`. Never raises: failures land in record['error']."""
    prompt, schema = prompt_for(set_name, case)
    prompt = prompt + claude_schema_suffix(schema)
    cmd = ["claude", "-p", "--model", model, "--output-format", "json", "--max-turns", "1"]
    if effort:
        cmd += ["--effort", effort]
    t0 = time.time()
    response, error, stdout = {}, None, ""
    try:
        proc = subprocess.run(cmd, input=prompt, capture_output=True, text=True,
                              cwd=str(cwd), timeout=CLAUDE_TIMEOUT_S)
        stdout = proc.stdout
        if proc.returncode != 0 or not stdout.strip():
            error = (proc.stderr or "").strip()[:2000] or f"exit {proc.returncode}"
    except subprocess.TimeoutExpired:
        error = f"timeout after {CLAUDE_TIMEOUT_S}s"
    except OSError as exc:
        error = str(exc)
    wall = time.time() - t0
    if not error:
        try:
            response = json.loads(stdout)
        except json.JSONDecodeError as exc:
            error = f"bad --output-format json: {exc}"
    if not error and response.get("is_error"):
        error = str(response.get("result") or "is_error=true")[:2000]

    result_text = response.get("result") or ""
    parsed = parse_json(result_text)
    usage = response.get("usage") or {}
    input_t = usage.get("input_tokens") or 0
    cache_creation = usage.get("cache_creation_input_tokens") or 0
    cache_read = usage.get("cache_read_input_tokens") or 0
    output_t = usage.get("output_tokens") or 0
    weighted_units = input_t * 1 + cache_creation * 2 + cache_read * 0.1 + output_t * 5

    record = {
        "id": case["id"], "set": set_name, "model": model, "backend": "claude", "effort": effort,
        "error": error, "parsed": parsed, "content": result_text[:4000],
        "usage": usage, "total_cost_usd": response.get("total_cost_usd"),
        "duration_ms": response.get("duration_ms"), "wall_s": wall,
        "weighted_units": weighted_units,
        "stop_reason": response.get("stop_reason") or response.get("subtype"),
    }
    if set_name == "checker":
        verdict = (parsed or {}).get("verdict")
        record.update(label=case["label"], origin=case["origin"], verdict=verdict,
                      correct=verdict == case["label"])
        outcome = f"{verdict} (label {case['label']})"
    else:
        record.update(kind=case["kind"], **score_reviewer_case(case, parsed))
        outcome = (f"caught={record['caught']} false_alarms={record['false_alarms']}"
                   if case["kind"] == "bug" else f"clean, false_alarms={record['false_alarms']}")
    return record, outcome


def run_claude(args):
    effort = args.effort
    out = RESULTS / claude_slug(args.model, effort)
    out.mkdir(parents=True, exist_ok=True)
    only = set(args.only.split(",")) if args.only else None
    sets = ["checker", "reviewer"] if args.set == "all" else [args.set]
    todo = [(s, c) for s in sets for c in load(s)
            if (only is None or c["id"] in only) and not (out / f"{c['id']}.json").exists()]
    print(f"calibrate: {len(todo)} cases to run for {out.name}", flush=True)
    started = time.time()
    with tempfile.TemporaryDirectory(prefix="calibrate-claude-") as cwd:
        def worker(item):
            set_name, case = item
            record, outcome = run_claude_case(args.model, effort, set_name, case, cwd)
            (out / f"{case['id']}.json").write_text(json.dumps(record, indent=1))
            print(f"{case['id']} {record['wall_s']:.0f}s {outcome}"
                  + (f" ERROR {record['error']}" if record["error"] else ""), flush=True)

        with ThreadPoolExecutor(max_workers=3) as pool:
            list(pool.map(worker, todo))
    print(f"calibrate: finished {out.name} in {(time.time() - started) / 60:.1f} min", flush=True)


def parse_think(value):
    if value is None:
        return None
    return {"true": True, "false": False}.get(value.lower(), value)


def run(args):
    base = ollama_base()
    think = parse_think(args.think)
    out = RESULTS / slug(args.model, think)
    out.mkdir(parents=True, exist_ok=True)
    only = set(args.only.split(",")) if args.only else None
    sets = ["checker", "reviewer"] if args.set == "all" else [args.set]
    todo = [(s, c) for s in sets for c in load(s)
            if (only is None or c["id"] in only) and not (out / f"{c['id']}.json").exists()]
    print(f"calibrate: {len(todo)} cases to run for {out.name}", flush=True)
    started = time.time()
    try:
        for done, (set_name, case) in enumerate(todo, 1):
            prompt, schema = prompt_for(set_name, case)
            max_tokens = 12000 if set_name == "checker" else 16000
            with GpuLock(f"calibrate {args.model} {case['id']}"):
                unload_others(base, args.model)
                t0 = time.time()
                try:
                    response = chat(base, args.model, prompt, schema, think, max_tokens)
                    error = response.get("error")
                except (urllib.error.URLError, TimeoutError, OSError) as exc:
                    response, error = {}, str(exc)
                wall = time.time() - t0
                split = loaded_split(base, args.model)
            message = response.get("message") or {}
            content = message.get("content") or ""
            parsed = parse_json(content)
            record = {
                "id": case["id"], "set": set_name, "model": args.model, "think": think,
                "error": error, "parsed": parsed, "content": content[:4000],
                "thinking_chars": len(message.get("thinking") or ""),
                "done_reason": response.get("done_reason"),
                "prompt_tokens": response.get("prompt_eval_count"),
                "eval_tokens": response.get("eval_count"),
                "load_s": (response.get("load_duration") or 0) / 1e9,
                "prompt_s": (response.get("prompt_eval_duration") or 0) / 1e9,
                "eval_s": (response.get("eval_duration") or 0) / 1e9,
                "wall_s": wall, "num_ctx": NUM_CTX, "loaded": split,
            }
            if set_name == "checker":
                verdict = (parsed or {}).get("verdict")
                record.update(label=case["label"], origin=case["origin"], verdict=verdict,
                              correct=verdict == case["label"])
                outcome = f"{verdict} (label {case['label']})"
            else:
                record.update(kind=case["kind"], **score_reviewer_case(case, parsed))
                outcome = (f"caught={record['caught']} false_alarms={record['false_alarms']}"
                           if case["kind"] == "bug" else f"clean, false_alarms={record['false_alarms']}")
            (out / f"{case['id']}.json").write_text(json.dumps(record, indent=1))
            print(f"[{done}/{len(todo)}] {case['id']} {wall:.0f}s {outcome}"
                  + (f" ERROR {error}" if error else ""), flush=True)
    finally:
        if args.model != Q_MODEL:
            try:
                unload(base, args.model)
            except (urllib.error.URLError, OSError):
                print("calibrate: could not unload the model; check `ollama ps` on the box", file=sys.stderr)
    print(f"calibrate: finished {out.name} in {(time.time() - started) / 60:.1f} min", flush=True)


def score():
    rows = []
    for folder in sorted(p for p in RESULTS.iterdir() if p.is_dir()) if RESULTS.exists() else []:
        records = [json.loads(p.read_text()) for p in sorted(folder.glob("*.json"))]
        # Re-derive reviewer matches from the stored answers, so a corrected
        # anchor in sets/ re-scores every model without re-running one.
        cases = {c["id"]: c for c in load("reviewer")}
        for r in records:
            if r["set"] == "reviewer" and r["id"] in cases:
                r.update(score_reviewer_case(cases[r["id"]], r["parsed"]))
        checker = [r for r in records if r["set"] == "checker"]
        reviewer = [r for r in records if r["set"] == "reviewer"]

        def rate(subset, want):
            hits = [r for r in subset if r["label"] == want]
            return (sum(r["correct"] for r in hits) / len(hits)) if hits else None

        holds = rate(checker, "holds")
        fails_real = rate([r for r in checker if r["origin"] == "real"], "fails")
        fails_synth = rate([r for r in checker if r["origin"] == "synthetic"], "fails")
        fails_all = rate(checker, "fails")
        balanced = (holds + fails_all) / 2 if holds is not None and fails_all is not None else None
        unsure = sum(r.get("verdict") == "unsure" for r in checker)
        unparsed = sum(r["parsed"] is None for r in records)
        tiers = {c["id"]: c.get("tier", "easy") for c in load("reviewer")}

        def caught(tier):
            cases = [r for r in reviewer if r["kind"] == "bug" and tiers.get(r["id"]) == tier]
            return f"{sum(bool(r['caught']) for r in cases)}/{len(cases)}" if cases else "-"

        def alarms(tier):
            cases = [r for r in reviewer if r["kind"] == "clean" and tiers.get(r["id"]) == tier]
            return f"{sum(r['false_alarms'] for r in cases)} in {len(cases)}" if cases else "-"

        bugs = [r for r in reviewer if r["kind"] == "bug"]
        evals = [r for r in records if r.get("eval_s")]
        tok_s = (sum(r["eval_tokens"] or 0 for r in evals) / sum(r["eval_s"] for r in evals)) if evals else None
        shares = [r["loaded"]["gpu_share"] for r in records if r.get("loaded") and r["loaded"].get("gpu_share")]
        contexts = sorted({r.get("num_ctx") for r in records if r.get("num_ctx")})
        units = [r["weighted_units"] for r in records if r.get("weighted_units") is not None]
        walls = [r["wall_s"] for r in records if r.get("wall_s") is not None]
        rows.append({
            "model": folder.name,
            "ctx": "/".join(f"{c // 1024}k" for c in contexts) or "-",
            "gpu": min(shares) if shares else None,
            "n": f"{len(checker)}+{len(reviewer)}",
            "check bal": balanced, "holds": holds, "fails real": fails_real, "fails synth": fails_synth,
            "unsure": unsure,
            "caught easy": caught("easy"), "caught hard": caught("hard"),
            "alarms easy": alarms("easy"), "alarms hard": alarms("hard"),
            "alarms extra": sum(r["false_alarms"] for r in bugs) if bugs else "-",
            "unparsed": unparsed,
            "s/case": (sum(r["wall_s"] for r in records) / len(records)) if records else None,
            "tok/s": tok_s,
            "units": (sum(units) if units else None),
            "med s/case": (statistics.median(walls) if walls else None),
        })
    if not rows:
        print("calibrate: no results yet")
        return
    cols = list(rows[0])

    def cell(value):
        if isinstance(value, float):
            return f"{value:.0%}" if value <= 1 else f"{value:.0f}"
        return "-" if value is None else str(value)

    widths = {c: max(len(c), *(len(cell(r[c])) for r in rows)) for c in cols}
    print("  ".join(c.ljust(widths[c]) for c in cols))
    for r in rows:
        print("  ".join(cell(r[c]).ljust(widths[c]) for c in cols))


def check_sets():
    for set_name in ("checker", "reviewer"):
        for case in load(set_name):
            prompt, _ = prompt_for(set_name, case)
            extra = ""
            if set_name == "reviewer" and case.get("mutation"):
                _, planted = mutate(case)
                extra = f"  planted: {planted!r}"
            print(f"{set_name:8} {case['id']}  ~{len(prompt) // 4:>6} tokens{extra}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--model")
    parser.add_argument("--think", help="true, false, or a level such as low/medium/high; omitted means the model default")
    parser.add_argument("--backend", choices=["ollama", "claude"], default="ollama",
                        help="ollama (default): local Ollama box. claude: `claude -p` subprocess, no GPU lock")
    parser.add_argument("--effort", help="claude backend only: low/medium/high/xhigh, passed to `claude -p --effort`")
    parser.add_argument("--set", choices=["checker", "reviewer", "all"], default="all")
    parser.add_argument("--only", help="comma-separated case ids")
    parser.add_argument("--score", action="store_true")
    parser.add_argument("--check-sets", action="store_true")
    args = parser.parse_args()
    if args.score:
        return score()
    if args.check_sets:
        return check_sets()
    if not args.model:
        die("--model is required to run")
    if args.backend == "claude":
        run_claude(args)
    else:
        run(args)


if __name__ == "__main__":
    main()
