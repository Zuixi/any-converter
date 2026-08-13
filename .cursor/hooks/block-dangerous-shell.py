#!/usr/bin/env python3
"""Deny agent shell commands that can wipe a workspace (rm -rf, git clean -fdx, etc.)."""

from __future__ import annotations

import json
import re
import sys

# Recursive / force mass deletes
RM_RECURSIVE = re.compile(
    r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?rm\s+[^\n;|&]*?(?:-[^\s]*[rR][^\s]*|-[^\s]*[fF][^\s]*r|[^\n]*\s--recursive)",
    re.IGNORECASE,
)
# Any rm -rf / rm -fr style (including combined flags)
RM_RF = re.compile(
    r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?rm\s+[^\n;|&]*-(?:[a-zA-Z]*[rR][a-zA-Z]*[fF]|[a-zA-Z]*[fF][a-zA-Z]*[rR])\b",
)

# Other workspace-destroying patterns
OTHER_DANGEROUS = [
    (
        re.compile(
            r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?git\s+clean\s+[^\n;|&]*-[^\n;|&]*[fdx]",
            re.I,
        ),
        "Blocked git clean with force/directory/ignore flags.",
    ),
    (
        re.compile(r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?git\s+reset\s+--hard\b", re.I),
        "Blocked git reset --hard.",
    ),
    (
        re.compile(r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?find\b[^\n;|&]*\s-delete\b", re.I),
        "Blocked find -delete.",
    ),
    (
        re.compile(r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?find\b[^\n;|&]*\s-exec\s+rm\b", re.I),
        "Blocked find -exec rm.",
    ),
    (
        re.compile(r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?chmod\s+-R\s+[^\n;|&]*\s+/", re.I),
        "Blocked recursive chmod on absolute paths.",
    ),
    (
        re.compile(r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?chown\s+-R\s+[^\n;|&]*\s+/", re.I),
        "Blocked recursive chown on absolute paths.",
    ),
    (
        re.compile(r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?dd\s+if=", re.I),
        "Blocked dd if=.",
    ),
    (
        re.compile(r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?mkfs\.", re.I),
        "Blocked mkfs.",
    ),
]


def deny(reason: str) -> None:
    print(
        json.dumps(
            {
                "permission": "deny",
                "user_message": reason,
                "agent_message": (
                    reason
                    + " Use targeted deletes (single files via delete_file tool / rm without -r/-f), "
                    "or ask the human to run the destructive command manually."
                ),
            },
            ensure_ascii=False,
        )
    )
    sys.exit(0)


def allow() -> None:
    print(json.dumps({"permission": "allow"}))
    sys.exit(0)


def main() -> None:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        # failClosed is true — invalid input should block
        deny("Hook could not parse beforeShellExecution input; blocking for safety.")

    command = payload.get("command") or ""
    if not command.strip():
        allow()

    if RM_RF.search(command) or RM_RECURSIVE.search(command):
        deny(
            "Blocked dangerous recursive/force rm (e.g. rm -rf). "
            "This policy exists because an agent previously wiped the workspace."
        )

    # Bare `rm -r` / `rm -R` without f
    if re.search(
        r"(?:^|[;&|()\n]|&&|\|\|)\s*(?:sudo\s+)?rm\s+[^\n;|&]*-[^\s]*[rR]\b",
        command,
    ):
        deny("Blocked recursive rm (-r/-R). Agents must not recursively delete trees.")

    for pat, reason in OTHER_DANGEROUS:
        if pat.search(command):
            deny(reason)

    allow()


if __name__ == "__main__":
    main()
