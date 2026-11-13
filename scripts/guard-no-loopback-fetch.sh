#!/usr/bin/env bash
# Guard: the desktop webview must never dial the embedded server directly.
#
# A browser origin (http://localhost:8420 in dev, tauri://localhost or
# http://tauri.localhost in production) crossing into the loopback server
# needs a CORS preflight the server does not answer, so any such fetch fails
# at runtime with "TypeError: Failed to fetch" (the Accounts/Agents bug this
# guard was born from). Management calls must go through
# apps/desktop/src/lib/management.ts (the server_request Tauri command).
set -euo pipefail

DESKTOP_SRC="$(cd "$(dirname "$0")/.." && pwd)/apps/desktop/src"

status=0
# fetch( ... http://127.0.0.1 / localhost / :3028 — direct or template-literal.
while IFS= read -r line; do
  echo "guard: loopback fetch in desktop webview: ${line}" >&2
  status=1
done < <(grep -rnE 'fetch\([^)]*(127\.0\.0\.1|localhost|:3028)' \
    --include='*.ts' --include='*.tsx' "$DESKTOP_SRC" || true)

if [ "$status" -ne 0 ]; then
  echo "guard: route management calls through src/lib/management.ts instead" >&2
  exit 1
fi
echo "guard: no loopback fetch in the desktop webview"
