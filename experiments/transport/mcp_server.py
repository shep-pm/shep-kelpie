"""A stdio MCP server for tests T4 and T5.

Serves one tool, lease_status, which relays to a project runner with
`shep trigger <project> status`. With --channel-push SECONDS it also declares
the claude/channel capability and pushes one channel message that many
seconds after initialization, to see whether it wakes an idle session.
"""

import json
import os
import subprocess
import sys
import threading
import time

SHEP = os.path.expanduser("~/.kelpie/bin/shep")
ENV = {**os.environ, "SHEP_HOME": os.path.expanduser("~/.kelpie/shep")}
PUSH_AFTER = None
if "--channel-push" in sys.argv:
    PUSH_AFTER = float(sys.argv[sys.argv.index("--channel-push") + 1])
LOG = open(os.environ.get("KELPIE_MCP_LOG", "/dev/null"), "a", buffering=1)
lock = threading.Lock()


def send(msg):
    with lock:
        sys.stdout.write(json.dumps(msg) + "\n")
        sys.stdout.flush()
    LOG.write(json.dumps({"t": time.time(), "out": msg.get("method") or "response"}) + "\n")


def lease_status(project):
    proc = subprocess.run([SHEP, "trigger", project, "status", "--format", "json"],
                          capture_output=True, text=True, env=ENV, timeout=30)
    return proc.stdout.strip() or proc.stderr.strip()


def push_later():
    time.sleep(PUSH_AFTER)
    send({"jsonrpc": "2.0", "method": "notifications/claude/channel",
          "params": {"content": "kelpie: the GPU lease for koji is free. Reply with exactly: woken by channel",
                     "meta": {"source": "kelpie"}}})


def handle(msg):
    method, mid = msg.get("method"), msg.get("id")
    LOG.write(json.dumps({"t": time.time(), "in": method}) + "\n")
    if method == "initialize":
        caps = {"tools": {}}
        if PUSH_AFTER is not None:
            caps["experimental"] = {"claude/channel": {}}
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"),
            "capabilities": caps,
            "serverInfo": {"name": "kelpie", "version": "0.0.0"}}})
    elif method == "notifications/initialized":
        if PUSH_AFTER is not None:
            threading.Thread(target=push_later, daemon=True).start()
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [{
            "name": "lease_status",
            "description": "Ask kelpie whether a project holds the GPU lease.",
            "inputSchema": {"type": "object",
                            "properties": {"project": {"type": "string"}},
                            "required": ["project"]}}]}})
    elif method == "tools/call":
        args = msg.get("params", {}).get("arguments", {})
        text = lease_status(args.get("project", "koji"))
        send({"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": text}]}})
    elif method == "ping":
        send({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": f"no method {method}"}})


def main():
    for line in sys.stdin:
        try:
            handle(json.loads(line))
        except json.JSONDecodeError:
            continue


if __name__ == "__main__":
    main()
