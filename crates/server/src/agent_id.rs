//! Normalize a raw `User-Agent` string into a stable agent id.

/// Known agents and their lowercase user-agent prefixes, in match order.
pub(crate) const KNOWN_AGENTS: &[(&str, &[&str])] = &[
    ("claude-code", &["claude-cli/", "claude-code/"]),
    ("codex", &["codex", "acp-extension-codex"]),
    ("gemini-cli", &["gemini-cli/", "google-genai-sdk/"]),
    ("opencode", &["opencode"]),
    ("cursor", &["cursor"]),
    ("cline", &["cline/", "cline"]),
    ("roo-code", &["roo-", "roocoder"]),
    ("aider", &["aider"]),
    ("pi", &["pi/"]),
];

/// Map a `User-Agent` to a known agent id, or fall back to its product name.
pub(crate) fn agent_of(user_agent: &str) -> String {
    let trimmed = user_agent.trim();
    let lowered = trimmed.to_lowercase();

    for (id, prefixes) in KNOWN_AGENTS {
        if prefixes.iter().any(|p| lowered.starts_with(p)) {
            return (*id).to_string();
        }
    }

    match trimmed
        .split_whitespace()
        .next()
        .and_then(|token| token.split('/').next())
        .filter(|product| !product.is_empty())
    {
        Some(product) => product.to_string(),
        None => "other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_agents() {
        let cases: &[(&str, &str)] = &[
            ("claude-cli/2.1.0 (external, cli)", "claude-code"),
            ("claude-code/1.0.0", "claude-code"),
            ("codex_cli_rs/0.159.2 (Mac OS 26.6.0; arm64)", "codex"),
            ("Codex Desktop/0.162.3", "codex"),
            ("acp-extension-codex/1.0", "codex"),
            ("gemini-cli/0.11.0", "gemini-cli"),
            ("google-genai-sdk/1.2.3", "gemini-cli"),
            ("opencode/0.4.0", "opencode"),
            ("Cursor/1.2.3", "cursor"),
            ("cline/3.1.0", "cline"),
            ("Roo-Code/1.0", "roo-code"),
            ("roocoder/0.2", "roo-code"),
            ("aider/0.86.0", "aider"),
            ("pi/0.9.0", "pi"),
        ];

        for (ua, expected) in cases {
            assert_eq!(agent_of(ua), *expected, "user-agent: {ua}");
        }
    }

    #[test]
    fn test_unknown_and_edge_cases() {
        assert_eq!(agent_of("Python-httpx/0.27.0"), "Python-httpx");
        assert_eq!(agent_of(""), "other");
        assert_eq!(agent_of("   "), "other");
        assert_eq!(agent_of("Claude-CLI/2.1"), "claude-code");
    }
}
