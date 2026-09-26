#!/usr/bin/env bash
# Waits until the given epoch (22:55 EDT), then kills every process this
# calibration run spawned: the launcher, every calibrate.py --backend claude
# config process, and any `claude -p ... --output-format json --max-turns 1`
# subprocess still in flight. Partial configs are expected and fine.
set -u
DEADLINE="${1:?usage: hard_stop.sh <epoch-seconds>}"

until [ "$(date +%s)" -ge "$DEADLINE" ]; do
  sleep 5
done

echo "=== hard stop firing at $(date) ==="

echo "--- pattern: run_claude_configs.py ---"
pgrep -fl "run_claude_configs.py" || echo "(none)"
pkill -f "run_claude_configs.py" 2>/dev/null

echo "--- pattern: calibrate.py --backend claude ---"
pgrep -fl "calibrate.py --backend claude" || echo "(none)"
pkill -f "calibrate.py --backend claude" 2>/dev/null

echo "--- pattern: claude -p ... --output-format json --max-turns 1 ---"
pgrep -fl -- "--output-format json --max-turns 1" || echo "(none)"
pkill -f -- "--output-format json --max-turns 1" 2>/dev/null

sleep 2
echo "--- survivors after kill ---"
pgrep -fl "run_claude_configs.py" || true
pgrep -fl "calibrate.py --backend claude" || true
pgrep -fl -- "--output-format json --max-turns 1" || true
echo "=== hard stop done at $(date) ==="
