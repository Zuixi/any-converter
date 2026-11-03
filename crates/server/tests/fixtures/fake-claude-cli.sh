#!/usr/bin/env bash
# A fake `claude` CLI for any-converter's Claude sign-in tests (P15).
# The real `claude` command line is never run by the test suite.
#
# Three roles, picked by the call shape:
#   1. BROWSER shim: called with exactly one https:// page argument while
#      ANY_CONVERTER_OPENED_URL is set (the real any-converter binary does
#      the same, magpie TookOpenedURL) — writes the page there, exits.
#   2. `auth status --json`: prints a fixed account status.
#   3. `auth login --claudeai`: the sign-in, chosen by FAKE_CLAUDE:
#        browser — calls $BROWSER once with the page URL, then waits for a
#                   stdin line, writes the signed-in files, exits 0;
#        print   — prints the page URL, same stdin wait and files;
#        fail    — prints one error line and exits 1.
#      FAKE_CLAUDE_PORT overrides the callback port in the page URL.
set -euo pipefail

# (1) Browser shim: the page `claude auth login` asks its browser to open.
if [[ -n "${ANY_CONVERTER_OPENED_URL:-}" && $# -eq 1 && $1 == https://* ]]; then
  umask 077
  printf '%s' "$1" >"$ANY_CONVERTER_OPENED_URL"
  chmod 600 "$ANY_CONVERTER_OPENED_URL"
  exit 0
fi

# (2) `claude auth status --json`.
if [[ "${1:-}" == "auth" && "${2:-}" == "status" && "${3:-}" == "--json" ]]; then
  printf '%s\n' '{"email":"me@example.com","orgName":"Acme","subscriptionType":"pro"}'
  exit 0
fi

dir="${CLAUDE_CONFIG_DIR:?CLAUDE_CONFIG_DIR required}"
mode="${FAKE_CLAUDE:-print}"
port="${FAKE_CLAUDE_PORT:-34567}"
url="https://claude.example/auth?redirect_uri=http%3A%2F%2Flocalhost%3A${port}%2Fcallback&code=abc&state=s1"

case "$mode" in
  browser)
    "$BROWSER" "$url"
    ;;
  print)
    printf '%s\n' "$url"
    ;;
  fail)
    printf '%s\n' "claude exploded"
    exit 1
    ;;
  *)
    printf 'unknown FAKE_CLAUDE=%s\n' "$mode" >&2
    exit 2
    ;;
esac

# The code the page showed (code#state), one line on stdin.
IFS= read -r line || exit 0
if [[ "$line" != *"#"* ]]; then
  printf 'expected code#state, got: %s\n' "$line" >&2
  exit 3
fi

cat >"$dir/.credentials.json" <<'EOF'
{"claudeAiOauth":{"accessToken":"tok-access","refreshToken":"tok-refresh","expiresAt":2000000000000,"subscriptionType":"pro"}}
EOF
cat >"$dir/.claude.json" <<'EOF'
{"oauthAccount":{"emailAddress":"me@example.com","organizationName":"Acme"}}
EOF
exit 0
