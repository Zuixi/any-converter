#!/usr/bin/env bash
# Create a GitHub PR without Cursor Co-authored-by trailers, and without
# force-pushing to main.
#
# Usage:
#   scripts/pr-create.sh --title "feat: ..." --body "$(cat <<'EOF'
#   ## Summary
#   - ...
#   ## Test plan
#   - [ ] ...
#   EOF
#   )"
#
# Options:
#   --title TEXT          PR title (required)
#   --body TEXT           PR body (required unless --body-file)
#   --body-file PATH      Read PR body from file
#   --base BRANCH         Base branch (default: main)
#   --draft               Open as draft
#   --strip-head-trailer  If HEAD commit still has Cursor co-author, rewrite it
#                         with commit-tree (no force to main; only current branch)
#
# Safety:
#   - Refuses to run when current branch is main/master
#   - Never uses --force / --force-with-lease
#   - Relies on .githooks/commit-msg to strip trailers on future commits
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TITLE=""
BODY=""
BODY_FILE=""
BASE="main"
DRAFT=0
STRIP_HEAD=0

usage() {
  sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --title) TITLE="${2:-}"; shift 2 ;;
    --body) BODY="${2:-}"; shift 2 ;;
    --body-file) BODY_FILE="${2:-}"; shift 2 ;;
    --base) BASE="${2:-}"; shift 2 ;;
    --draft) DRAFT=1; shift ;;
    --strip-head-trailer) STRIP_HEAD=1; shift ;;
    -h|--help) usage ;;
    *) echo "Unknown arg: $1" >&2; usage ;;
  esac
done

if [[ -z "$TITLE" ]]; then
  echo "error: --title is required" >&2
  exit 1
fi

if [[ -n "$BODY_FILE" ]]; then
  BODY="$(cat "$BODY_FILE")"
fi
if [[ -z "$BODY" ]]; then
  echo "error: --body or --body-file is required" >&2
  exit 1
fi

branch="$(git branch --show-current)"
if [[ -z "$branch" ]]; then
  echo "error: detached HEAD; check out a branch first" >&2
  exit 1
fi
if [[ "$branch" == "main" || "$branch" == "master" ]]; then
  echo "error: refuse to open a PR from $branch — create a feature branch first:" >&2
  echo "  git switch -c feat/your-topic" >&2
  exit 1
fi

# Ensure commit-msg hook is active for this clone
hooks_path="$(git config --get core.hooksPath || true)"
if [[ "$hooks_path" != ".githooks" && "$hooks_path" != "$ROOT/.githooks" ]]; then
  echo "note: enabling core.hooksPath=.githooks for this repo"
  git config core.hooksPath .githooks
fi
chmod +x .githooks/commit-msg .githooks/pre-commit .githooks/pre-push 2>/dev/null || true

strip_cursor_trailer_from_head() {
  if ! git log -1 --format=%B | grep -q '^Co-authored-by: Cursor <cursoragent@cursor.com>$'; then
    return 0
  fi
  echo "stripping Cursor Co-authored-by from HEAD via commit-tree..."
  local tree parent new msg
  tree="$(git rev-parse 'HEAD^{tree}')"
  parent="$(git rev-parse 'HEAD^')"
  msg="$(git log -1 --format=%B | python3 -c '
import re,sys
text=sys.stdin.read()
text=re.sub(r"(?m)^Co-authored-by:\s*Cursor\s*<cursoragent@cursor\.com>\s*\n?","",text)
print(text.rstrip()+"\n", end="")
')"
  if git rev-parse --verify --quiet 'HEAD^' >/dev/null; then
    new="$(printf '%s' "$msg" | git commit-tree "$tree" -p "$parent" -F -)"
  else
    new="$(printf '%s' "$msg" | git commit-tree "$tree" -F -)"
  fi
  git update-ref "refs/heads/$branch" "$new"
}

if [[ "$STRIP_HEAD" -eq 1 ]]; then
  strip_cursor_trailer_from_head
fi

# Ordinary push only (never force) — avoids rewriting remote main by accident
git push -u origin HEAD

args=(pr create --title "$TITLE" --body "$BODY" --base "$BASE" --head "$branch")
if [[ "$DRAFT" -eq 1 ]]; then
  args+=(--draft)
fi

url="$(gh "${args[@]}")"
echo "$url"
