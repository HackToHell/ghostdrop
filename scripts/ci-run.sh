#!/usr/bin/env bash
# CI helper: runs a command, and if it fails, surfaces the tail of its output as a
# GitHub Actions error annotation (visible on the run summary page and via the API).
set -uo pipefail
log="$(mktemp)"
"$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
if [ "$status" -ne 0 ]; then
  tail -n 60 "$log" | python3 -c '
import sys
body = sys.stdin.read().replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
print(f"::error title=Command failed: {sys.argv[1]}::{body}")
' "$*"
fi
exit "$status"
