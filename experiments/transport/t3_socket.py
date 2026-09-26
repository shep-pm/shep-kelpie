"""T3: a background session (`claude --bg`) steered over its messaging socket.

Usage, from the repo root (the local permission rule matches this form):
    python3 experiments/transport/t3_socket.py run [default|accept]

Reads the peer token only of the session this script itself started, found
by the session id `claude --bg` printed.
"""

import glob
import json
import os
import socket
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import MODEL, WORKER_REPO, ledger, summarize_usage  # noqa: E402

TEST = "t3"
# `claude --bg` refuses a folder whose trust prompt was never accepted, and
# accepting it is the maintainer's call, so T3 runs in the already-trusted
# home folder. Its floor lacks the worker repo's CLAUDE.md.
BG_CWD = os.path.expanduser("~")


def agents():
    out = subprocess.run(["claude", "agents", "--json", "--all"], capture_output=True, text=True,
                         stdin=subprocess.DEVNULL, timeout=60).stdout
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return []


def find_session(short_id, started_after_ms):
    for a in agents():
        blob = json.dumps(a)
        if (short_id and short_id in blob) or (a.get("kind") == "background"
                                               and (a.get("startedAt") or 0) >= started_after_ms
                                               and a.get("cwd") == BG_CWD):
            return a
    return None


def transcript(session_id):
    paths = glob.glob(os.path.expanduser(f"~/.claude/projects/*/{session_id}.jsonl"))
    if not paths:
        return []
    out = []
    with open(paths[0]) as f:
        for line in f:
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return out


def assistant_texts(entries):
    texts = []
    for e in entries:
        if e.get("type") == "assistant":
            for block in (e.get("message", {}).get("content") or []):
                if isinstance(block, dict) and block.get("type") == "text":
                    texts.append((e.get("timestamp"), block.get("text", "")[:80], e.get("message", {}).get("usage")))
    return texts


def run(mode):
    settings = ["--settings", json.dumps({"crossSessionInbound": "accept"})] if mode == "accept" else []
    t_start_ms = int(time.time() * 1000)
    bg = subprocess.run(["claude", "--bg", "--model", MODEL, *settings, "Reply with exactly: ready"],
                        capture_output=True, text=True, stdin=subprocess.DEVNULL, cwd=BG_CWD,
                        timeout=120)
    ledger(TEST, {"mode": mode, "step": "bg_start", "rc": bg.returncode,
                  "stdout": bg.stdout[-300:], "stderr": bg.stderr[-300:]})
    import re
    m = re.search(r"backgrounded\s*\W\s*([0-9a-f]{6,})", bg.stdout)
    short_id = m.group(1) if m else ""

    sess = None
    for _ in range(60):
        sess = find_session(short_id, t_start_ms - 1000)
        if sess and sess.get("status") not in ("busy", "starting") and sess.get("sessionId"):
            break
        time.sleep(2)
    ledger(TEST, {"mode": mode, "step": "session", "found": bool(sess),
                  "fields": {k: sess.get(k) for k in ("pid", "kind", "status", "waitingFor", "sessionId")} if sess else None})
    if not sess:
        return
    pid, sid = sess.get("pid"), sess.get("sessionId")
    before = assistant_texts(transcript(sid))

    reg_path = os.path.expanduser(f"~/.claude/sessions/{pid}.json")
    try:
        reg = json.load(open(reg_path))
    except OSError:
        reg = {}
    sock_path = reg.get("messagingSocketPath")
    keys = glob.glob(os.path.expanduser(f"~/.claude/sessions/{pid}.*.key"))
    token = json.load(open(keys[0])).get("peerToken") if keys else None
    ledger(TEST, {"mode": mode, "step": "registry", "socket": bool(sock_path), "token_found": bool(token),
                  "peer_features": reg.get("peerFeatures")})
    if not sock_path:
        return

    t0 = time.monotonic()
    reply = None
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(sock_path)
        if token:
            s.sendall((json.dumps({"type": "auth", "token": token}) + "\n").encode())
        s.sendall((json.dumps({"type": "user", "message": {"role": "user",
                   "content": "Reply with exactly: woken by socket"}}) + "\n").encode())
        s.settimeout(5)
        try:
            reply = s.recv(4096).decode(errors="replace")[:300]
        except socket.timeout:
            reply = None
        s.close()
    except OSError as exc:
        reply = f"socket error: {exc}"

    woke_after, usage, statuses = None, None, []
    for _ in range(90):
        a = find_session(short_id, t_start_ms - 1000) or {}
        statuses.append(a.get("status"))
        texts = assistant_texts(transcript(sid))
        new = [t for t in texts if t not in before]
        if any("woken by socket" in t[1] for t in new):
            woke_after = round(time.monotonic() - t0, 2)
            usage = [t[2] for t in new if "woken" in t[1]][-1]
            break
        time.sleep(1)
    ledger(TEST, {"mode": mode, "step": "inject", "socket_reply": reply, "woke": woke_after is not None,
                  "woke_after_s": woke_after, "statuses_seen": sorted(set(filter(None, statuses))),
                  **summarize_usage(usage)})
    print(f"t3 {mode}: woke={woke_after is not None} after={woke_after}s reply={reply!r}", flush=True)

    for verb in ("stop", "rm"):
        r = subprocess.run(["claude", verb, short_id], capture_output=True, text=True,
                           stdin=subprocess.DEVNULL, timeout=60)
        ledger(TEST, {"mode": mode, "step": verb, "rc": r.returncode, "out": (r.stdout + r.stderr)[-200:]})


if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] != "run":
        sys.exit("usage: t3_socket.py run [default|accept]")
    run(sys.argv[2] if len(sys.argv) > 2 else "default")
