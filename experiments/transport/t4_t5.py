"""T4 (tools kelpie serves to a worker) and T5 (MCP channel push).

Usage: python3 t4_t5.py [t4a] [t4b] [t5]
Needs the koji runner registered with the pinned shepherd (test L does that).
"""

import json
import os
import queue
import subprocess
import sys
import threading
import time

from common import HERE, MODEL, RESULTS, WORKER_REPO, call_record, ledger, run_p, summarize_usage

SHEP = os.path.expanduser("~/.kelpie/bin/shep")
SHEP_ENV = {**os.environ, "SHEP_HOME": os.path.expanduser("~/.kelpie/shep")}
SERVER = str(HERE / "mcp_server.py")
TOOL = "mcp__kelpie__lease_status"
TOOL_PROMPT = ("Call the lease_status tool with project koji. "
               "Reply with exactly the value of the holding field in its answer, and nothing else.")


def stdio_config(*extra):
    return json.dumps({"mcpServers": {"kelpie": {"command": "python3", "args": [SERVER, *extra],
                                                 "env": {"KELPIE_MCP_LOG": str(RESULTS / "mcp_server.log")}}}})


def t4a(runs=3):
    for run in range(1, runs + 1):
        for label, extra in (("floor_without", []), ("floor_with", ["--mcp-config", stdio_config()])):
            for i in range(2):  # the second call reads a warm prefix
                result, wall, _ = run_p("Reply with exactly: ok", extra)
                if i == 1:
                    ledger("t4", {"part": "t4a", "run": run, "step": label, **call_record(result, wall)})
        result, wall, err = run_p(TOOL_PROMPT, ["--mcp-config", stdio_config(), "--allowedTools", TOOL])
        rec = ledger("t4", {"part": "t4a", "run": run, "step": "tool_call", **call_record(result, wall),
                            "stderr_tail": err[-300:] if result.get("is_error") else None})
        print(f"t4a r{run}: {rec['result_head']!r} turns={rec['num_turns']} {wall}s", flush=True)


class StreamWorker:
    """A stream-json worker whose host answers control requests itself."""

    def __init__(self, extra_args):
        self.proc = subprocess.Popen(
            ["claude", "-p", "--input-format", "stream-json", "--output-format", "stream-json",
             "--verbose", "--model", MODEL, *extra_args],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            cwd=str(WORKER_REPO), text=True, bufsize=1)
        self.q = queue.Queue()
        self.stderr = []
        self.handler = None
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=lambda: self.stderr.extend(self.proc.stderr), daemon=True).start()

    def _read(self):
        for line in self.proc.stdout:
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if ev.get("type") == "control_request" and self.handler:
                self.handler(self, ev)
                continue
            self.q.put((time.monotonic(), ev))
        self.q.put((time.monotonic(), None))

    def write(self, obj):
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def user(self, text):
        self.write({"type": "user", "message": {"role": "user", "content": text}})

    def until(self, pred, timeout):
        """Collect events until pred(event) or timeout. Returns (events, matched)."""
        seen, end = [], time.monotonic() + timeout
        while time.monotonic() < end:
            try:
                ts, ev = self.q.get(timeout=max(0.1, end - time.monotonic()))
            except queue.Empty:
                break
            if ev is None:
                return seen, None
            seen.append((ts, ev))
            if pred(ev):
                return seen, ev
        return seen, None

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=30)
        except Exception:
            self.proc.kill()


def sdk_handler(w, ev):
    """Answer the CLI's control requests: MCP messages for the in-process server."""
    req = ev.get("request", {})
    rid = ev.get("request_id")
    if req.get("subtype") == "mcp_message":
        msg = req.get("message", {})
        method, mid = msg.get("method"), msg.get("id")
        w.mcp_calls.append((time.monotonic(), method))
        if method == "initialize":
            resp = {"jsonrpc": "2.0", "id": mid, "result": {
                "protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"),
                "capabilities": {"tools": {}}, "serverInfo": {"name": "kelpie", "version": "0.0.0"}}}
        elif method == "tools/list":
            resp = {"jsonrpc": "2.0", "id": mid, "result": {"tools": [{
                "name": "lease_status", "description": "Ask kelpie whether a project holds the GPU lease.",
                "inputSchema": {"type": "object", "properties": {"project": {"type": "string"}},
                                "required": ["project"]}}]}}
        elif method == "tools/call":
            project = msg.get("params", {}).get("arguments", {}).get("project", "koji")
            out = subprocess.run([SHEP, "trigger", project, "status", "--format", "json"],
                                 capture_output=True, text=True, env=SHEP_ENV, timeout=30).stdout
            resp = {"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": out}]}}
        elif mid is None:
            resp = {"jsonrpc": "2.0", "result": {}, "id": 0}
        else:
            resp = {"jsonrpc": "2.0", "id": mid, "result": {}}
        w.write({"type": "control_response",
                 "response": {"subtype": "success", "request_id": rid, "response": {"mcp_response": resp}}})
    else:
        w.other_requests.append(req.get("subtype"))
        w.write({"type": "control_response",
                 "response": {"subtype": "error", "request_id": rid, "error": "not handled by kelpie"}})


def is_result(ev):
    return ev.get("type") == "result"


def t4b(runs=2):
    for run in range(1, runs + 1):
        w = StreamWorker(["--allowedTools", TOOL])
        w.mcp_calls, w.other_requests = [], []
        w.handler = sdk_handler
        w.write({"type": "control_request", "request_id": f"init-{run}",
                 "request": {"subtype": "initialize", "sdkMcpServers": ["kelpie"]}})
        _, init = w.until(lambda e: e.get("type") == "control_response", 60)
        ledger("t4", {"part": "t4b", "run": run, "step": "initialize",
                      "response": (json.dumps(init)[:300] if init else None)})
        for step, text in (("trivial", "Reply with exactly: ok"), ("tool_call", TOOL_PROMPT)):
            t0 = time.monotonic()
            w.user(text)
            seen, res = w.until(is_result, 300)
            rec = ledger("t4", {"part": "t4b", "run": run, "step": step,
                                "wall_s": round(time.monotonic() - t0, 2),
                                "result_head": (res or {}).get("result", "")[:120] if res else None,
                                "num_turns": (res or {}).get("num_turns"),
                                "mcp_calls": [m for _, m in w.mcp_calls],
                                "other_requests": w.other_requests,
                                **summarize_usage((res or {}).get("usage"))})
            print(f"t4b r{run} {step}: {rec.get('result_head')!r} ctx={rec.get('context')} "
                  f"mcp={rec['mcp_calls']}", flush=True)
        w.close()
        if not w.mcp_calls:
            ledger("t4", {"part": "t4b", "run": run, "stderr_tail": "".join(w.stderr)[-600:]})


def t5(push_after=25, wait=90):
    config = stdio_config("--channel-push", str(push_after))
    debug = str(RESULTS / "t5.debug.log")
    w = StreamWorker(["--mcp-config", config, "--debug-file", debug,
                      "--dangerously-load-development-channels", "server:kelpie"])
    w.user("Reply with exactly: ready")
    _, first = w.until(is_result, 180)
    t_idle = time.monotonic()
    seen, woke = w.until(is_result, wait)
    types = [e.get("type") + ("/" + e["subtype"] if e.get("subtype") else "") for _, e in seen]
    rec = ledger("t5", {"step": "channel_push", "first_result": (first or {}).get("result", "")[:80] if first else None,
                        "woke": woke is not None,
                        "woke_after_s": round(seen[-1][0] - t_idle, 2) if woke else None,
                        "woke_result": (woke or {}).get("result", "")[:120] if woke else None,
                        "events_while_idle": types[:30],
                        **summarize_usage((woke or {}).get("usage")),
                        "stderr_tail": "".join(w.stderr)[-800:]})
    print(f"t5: woke={rec['woke']} after={rec['woke_after_s']} result={rec['woke_result']!r}", flush=True)
    w.close()


def main():
    parts = sys.argv[1:] or ["t4a", "t4b", "t5"]
    subprocess.run([SHEP, "start", "koji"], env=SHEP_ENV, capture_output=True, cwd=str(HERE))
    time.sleep(1.5)
    for part in parts:
        {"t4a": t4a, "t4b": t4b, "t5": t5}[part]()


if __name__ == "__main__":
    main()
