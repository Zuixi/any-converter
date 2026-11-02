//! Gemini CLI's model table and the experiment flags that pick from it
//! (magpie `internal/provider/google.go`: `geminiModels`, `geminiFlags`,
//! `flags`, `offered`).
//!
//! Code Assist decides which of these models an account is offered through
//! four experiments; [`GeminiFlags`] records them, [`offered`] applies
//! Gemini CLI's skip table, and [`flags_from_experiments`] parses the
//! experiment list out of the `loadCodeAssist` reply.

/// Gemini CLI's model table, in the CLI's order (magpie `geminiModels`).
pub const GEMINI_MODELS: &[(&str, &str)] = &[
    ("gemini-3.1-pro-preview", "Gemini 3.1 Pro Preview"),
    ("gemini-3-pro-preview", "Gemini 3 Pro Preview"),
    ("gemini-3-flash-preview", "Gemini 3 Flash Preview"),
    ("gemini-3.8-flash", "Gemini 3.8 Flash"),
    ("gemini-3.5-flash", "Gemini 3.5 Flash"),
    ("gemini-3.5-flash-lite", "Gemini 3.5 Flash Lite"),
    ("gemini-3.1-flash-lite", "Gemini 3.1 Flash Lite"),
    ("gemini-2.5-pro", "Gemini 2.5 Pro"),
    ("gemini-2.5-flash", "Gemini 2.5 Flash"),
    ("gemini-2.5-flash-lite", "Gemini 2.5 Flash Lite"),
];

/// The experiments Code Assist has the account in that decide which models
/// Gemini CLI offers it (magpie `geminiFlags`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GeminiFlags {
    /// Gemini 3.1 Pro launched to the account.
    pub gemini31: bool,
    /// The account has no Pro model.
    pub no_pro: bool,
    /// Gemini 3.8 Flash in place of 3.5 Flash.
    pub latest_flash: bool,
    /// Gemini 3.5 Flash Lite in place of 3.1 Flash Lite.
    pub latest_flash_lite: bool,
}

/// Gemini CLI's ids of those experiments (its `ExperimentFlags`).
pub const FLAG_GEMINI31_PRO: i64 = 45760185;
pub const FLAG_PRO_NO_ACCESS: i64 = 45768879;
pub const FLAG_LATEST_FLASH: i64 = 45842815;
pub const FLAG_LATEST_FLASH_LITE: i64 = 45827489;

/// The models those experiments offer the account (magpie `offered`):
/// previews only to an account with quota for one, 3.1 Pro in place of
/// 3 Pro once launched to it, and one Flash and one Flash Lite — the
/// latest where Google has rolled it out to the account.
///
/// `preview` is whether the account's model list carries a `-preview` id
/// (Gemini CLI derives it from `listModels`; this project reads no quota,
/// so the caller passes it). `gemini-2.5-flash-lite` is always offered.
pub fn offered(flags: GeminiFlags, preview: bool) -> Vec<(String, String)> {
    let skip = |id: &str| match id {
        "gemini-3.1-pro-preview" => !flags.gemini31 || !preview || flags.no_pro,
        "gemini-3-pro-preview" => flags.gemini31 || !preview || flags.no_pro,
        "gemini-3-flash-preview" => !preview,
        "gemini-3.8-flash" => !flags.latest_flash,
        "gemini-3.5-flash" => flags.latest_flash,
        "gemini-3.5-flash-lite" => !flags.latest_flash_lite,
        "gemini-3.1-flash-lite" => flags.latest_flash_lite,
        "gemini-2.5-pro" => flags.no_pro,
        // the CLI's 2.5 Flash is 3.8 Flash then
        "gemini-2.5-flash" => flags.latest_flash,
        _ => false,
    };
    GEMINI_MODELS
        .iter()
        .filter(|(id, _)| !skip(id))
        .map(|(id, name)| ((*id).to_string(), (*name).to_string()))
        .collect()
}

/// Parse Code Assist's experiment list into [`GeminiFlags`] (magpie
/// `flags`). The `experiments` array is the one the `loadCodeAssist`
/// reply carries; its entries name the flag ids above, shaped like
/// `listExperiments` flags (`flagId` + `boolValue`). Missing or unknown
/// entries leave their flag off, as in the CLI.
pub fn flags_from_experiments(json: &serde_json::Value) -> GeminiFlags {
    let mut flags = GeminiFlags::default();
    let Some(experiments) = json
        .get("experiments")
        .and_then(serde_json::Value::as_array)
    else {
        return flags;
    };
    for entry in experiments {
        let Some(id) = entry.get("flagId").and_then(serde_json::Value::as_i64) else {
            continue;
        };
        let on = entry
            .get("boolValue")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        match id {
            FLAG_GEMINI31_PRO => flags.gemini31 = on,
            FLAG_PRO_NO_ACCESS => flags.no_pro = on,
            FLAG_LATEST_FLASH => flags.latest_flash = on,
            FLAG_LATEST_FLASH_LITE => flags.latest_flash_lite = on,
            _ => {}
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn flags(
        gemini31: bool,
        no_pro: bool,
        latest_flash: bool,
        latest_flash_lite: bool,
    ) -> GeminiFlags {
        GeminiFlags {
            gemini31,
            no_pro,
            latest_flash,
            latest_flash_lite,
        }
    }

    /// The offered model ids for a flag/preview combination.
    fn offered_ids(flags: GeminiFlags, preview: bool) -> Vec<String> {
        offered(flags, preview)
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    #[test]
    fn test_offered_no_flags_no_preview() {
        // Everything off: no previews, no 3.8 Flash, no 3.5 Flash Lite,
        // no 3.1 Pro (not launched), but the rest stay.
        assert_eq!(
            offered_ids(flags(false, false, false, false), false),
            vec![
                "gemini-3.5-flash",
                "gemini-3.1-flash-lite",
                "gemini-2.5-pro",
                "gemini-2.5-flash",
                "gemini-2.5-flash-lite",
            ]
        );
    }

    #[test]
    fn test_offered_all_flags_with_preview() {
        // Every experiment on, with previews: all three Pro models are
        // skipped (noPro), the preview Flash is offered, 3.8 replaces
        // 3.5 and 2.5 Flash, and 3.5 Flash Lite replaces 3.1 Flash Lite.
        assert_eq!(
            offered_ids(flags(true, true, true, true), true),
            vec![
                "gemini-3-flash-preview",
                "gemini-3.8-flash",
                "gemini-3.5-flash-lite",
                "gemini-2.5-flash-lite",
            ]
        );
    }

    #[test]
    fn test_offered_gemini31_swaps_pro() {
        // Launched to 3.1 Pro: the 3.1 preview replaces the 3 preview.
        let got = offered_ids(flags(true, false, false, false), true);
        assert!(
            got.contains(&"gemini-3.1-pro-preview".to_string()),
            "{got:?}"
        );
        assert!(
            !got.contains(&"gemini-3-pro-preview".to_string()),
            "{got:?}"
        );
        // Not launched: the 3 preview stays and 3.1 is out.
        let got = offered_ids(flags(false, false, false, false), true);
        assert!(got.contains(&"gemini-3-pro-preview".to_string()), "{got:?}");
        assert!(
            !got.contains(&"gemini-3.1-pro-preview".to_string()),
            "{got:?}"
        );
    }

    #[test]
    fn test_offered_latest_flash_swaps_flash() {
        // 3.8 Flash rolled out: it replaces 3.5 Flash and 2.5 Flash.
        let got = offered_ids(flags(false, false, true, false), false);
        assert!(got.contains(&"gemini-3.8-flash".to_string()), "{got:?}");
        assert!(!got.contains(&"gemini-3.5-flash".to_string()), "{got:?}");
        assert!(!got.contains(&"gemini-2.5-flash".to_string()), "{got:?}");
        // Not rolled out: 3.5 and 2.5 Flash stay, 3.8 is out.
        let got = offered_ids(flags(false, false, false, false), false);
        assert!(got.contains(&"gemini-3.5-flash".to_string()), "{got:?}");
        assert!(got.contains(&"gemini-2.5-flash".to_string()), "{got:?}");
        assert!(!got.contains(&"gemini-3.8-flash".to_string()), "{got:?}");
    }

    #[test]
    fn test_offered_latest_flash_lite_swaps_flash_lite() {
        // 3.5 Flash Lite rolled out: it replaces 3.1 Flash Lite.
        let got = offered_ids(flags(false, false, false, true), false);
        assert!(
            got.contains(&"gemini-3.5-flash-lite".to_string()),
            "{got:?}"
        );
        assert!(
            !got.contains(&"gemini-3.1-flash-lite".to_string()),
            "{got:?}"
        );
        // Not rolled out: 3.1 Flash Lite stays, 3.5 Flash Lite is out.
        let got = offered_ids(flags(false, false, false, false), false);
        assert!(
            got.contains(&"gemini-3.1-flash-lite".to_string()),
            "{got:?}"
        );
        assert!(
            !got.contains(&"gemini-3.5-flash-lite".to_string()),
            "{got:?}"
        );
    }

    #[test]
    fn test_offered_no_pro_drops_pro_models() {
        let got = offered_ids(flags(false, true, false, false), true);
        assert!(
            !got.contains(&"gemini-3-pro-preview".to_string()),
            "{got:?}"
        );
        assert!(!got.contains(&"gemini-2.5-pro".to_string()), "{got:?}");
        let got = offered_ids(flags(false, false, false, false), true);
        assert!(got.contains(&"gemini-2.5-pro".to_string()), "{got:?}");
    }

    #[test]
    fn test_offered_preview_off_hides_every_preview() {
        let got = offered_ids(flags(true, false, true, true), false);
        assert!(!got.iter().any(|id| id.ends_with("-preview")), "{got:?}");
        let got = offered_ids(flags(true, false, true, true), true);
        assert!(got.iter().any(|id| id.ends_with("-preview")), "{got:?}");
    }

    #[test]
    fn test_offered_always_keeps_flash_lite_fallback() {
        // gemini-2.5-flash-lite has no skip rule: every combination keeps it.
        for gemini31 in [false, true] {
            for no_pro in [false, true] {
                for latest_flash in [false, true] {
                    for latest_flash_lite in [false, true] {
                        for preview in [false, true] {
                            let got = offered_ids(
                                flags(gemini31, no_pro, latest_flash, latest_flash_lite),
                                preview,
                            );
                            assert!(
                                got.contains(&"gemini-2.5-flash-lite".to_string()),
                                "{got:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_offered_order_is_cli_order() {
        // The table order is preserved: every result is a subsequence.
        let got = offered(flags(true, false, true, true), true);
        let table = GEMINI_MODELS.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        let mut pos = 0;
        for (id, _) in &got {
            let at = table[pos..]
                .iter()
                .position(|t| t == id)
                .map(|p| p + pos)
                .unwrap();
            pos = at + 1;
        }
    }

    #[test]
    fn test_flags_from_experiments() {
        let json = serde_json::json!({
            "experiments": [
                {"flagId": 45760185, "boolValue": true},
                {"flagId": 45768879, "boolValue": true},
                {"flagId": 45842815, "boolValue": false},
                {"flagId": 45827489, "boolValue": true},
                {"flagId": 99999999, "boolValue": true},
                {"noFlagId": true},
                {"flagId": "not-a-number", "boolValue": true}
            ]
        });
        assert_eq!(
            flags_from_experiments(&json),
            flags(true, true, false, true)
        );
    }

    #[test]
    fn test_flags_from_experiments_absent_bool_is_off() {
        // magpie reads `BoolValue`; absent means off.
        let json = serde_json::json!({ "experiments": [{"flagId": 45842815}] });
        assert_eq!(flags_from_experiments(&json), GeminiFlags::default());
    }

    #[test]
    fn test_flags_from_experiments_missing_or_empty() {
        assert_eq!(
            flags_from_experiments(&serde_json::json!({})),
            GeminiFlags::default()
        );
        assert_eq!(
            flags_from_experiments(&serde_json::json!({ "experiments": [] })),
            GeminiFlags::default()
        );
        assert_eq!(
            flags_from_experiments(&serde_json::json!({ "experiments": "nope" })),
            GeminiFlags::default()
        );
    }
}
