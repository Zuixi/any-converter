//! The API an upstream speaks — magpie's `Protocol` — and the legacy name a
//! provider's configuration may still use for it.

use any_converter_core::convert::Format;
use serde::{Deserialize, Serialize};

/// The API an upstream speaks. Mirrors magpie's `Protocol` minus Copilot
/// (which is an auth flavor returning Chat or Responses per model, not a
/// protocol of its own: internal/provider/plugins.go copilotAPI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpstreamApi {
    #[serde(rename = "chat")]
    Chat,
    #[serde(rename = "claude")]
    Claude,
    #[serde(rename = "responses")]
    Responses,
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "code_assist")]
    CodeAssist,
    /// Deserialization-only shim for legacy `api = "copilot"` configs;
    /// neither [`Self::api_for`] nor [`copilot_api`] ever returns it.
    #[serde(rename = "copilot")]
    Copilot,
}

impl UpstreamApi {
    /// The client-format the wire is shaped as. [`Self::Copilot`] is not
    /// reachable here (documented as OpenAIChat, mirroring magpie's
    /// `copilotAPI` fallthrough).
    pub fn to_format(self) -> Format {
        match self {
            UpstreamApi::Chat | UpstreamApi::Copilot => Format::OpenAIChat,
            UpstreamApi::Claude => Format::Claude,
            UpstreamApi::Responses => Format::OpenAIResponses,
            UpstreamApi::Gemini | UpstreamApi::CodeAssist => Format::Gemini,
        }
    }

    /// The path appended to the base (magpie `pathOf`, gateway.go:7520).
    /// The `model` is only used by [`Self::Gemini`], whose path carries it.
    pub fn path(self, model: &str, streaming: bool) -> String {
        match self {
            UpstreamApi::Chat => "/v1/chat/completions".to_string(),
            UpstreamApi::Claude => "/v1/messages".to_string(),
            UpstreamApi::Responses => "/v1/responses".to_string(),
            UpstreamApi::Gemini if streaming => {
                format!("/v1beta/models/{model}:streamGenerateContent")
            }
            UpstreamApi::Gemini => format!("/v1beta/models/{model}:generateContent"),
            UpstreamApi::CodeAssist if streaming => {
                "/v1internal:streamGenerateContent?alt=sse".to_string()
            }
            UpstreamApi::CodeAssist => "/v1internal:generateContent".to_string(),
            // Unreachable via `api_for`; Chat's path keeps the match total.
            UpstreamApi::Copilot => "/v1/chat/completions".to_string(),
        }
    }

    pub fn from_format(f: Format) -> Self {
        match f {
            Format::OpenAIChat => UpstreamApi::Chat,
            Format::Claude => UpstreamApi::Claude,
            Format::OpenAIResponses => UpstreamApi::Responses,
            Format::Gemini => UpstreamApi::Gemini,
        }
    }

    /// The wire a plugin model is spoken to with, by its AI SDK package
    /// (magpie pluginProtocol, internal/provider/plugins.go:200).
    pub fn from_npm(npm: &str) -> UpstreamApi {
        match npm {
            "@ai-sdk/anthropic" | "@ai-sdk/google-vertex/anthropic" => UpstreamApi::Claude,
            "@ai-sdk/openai" | "@ai-sdk/azure" => UpstreamApi::Responses,
            "@ai-sdk/google" => UpstreamApi::CodeAssist,
            // Copilot dispatches per model (P7); until then it — and every
            // unknown package, including @ai-sdk/openai-compatible — speaks
            // OpenAI Chat.
            _ => UpstreamApi::Chat,
        }
    }
}

/// The wire a Copilot model is served on (magpie `copilotAPI`): gpt-5 and
/// up — except `gpt-5-mini` — answer `/responses`; everything else
/// (`claude-*`, earlier gpts) answers `/chat/completions`.
///
/// Parsed by hand instead of adding a regex dependency: magpie's `^gpt-(\d+)`
/// is `strip_prefix("gpt-")` followed by the leading digit run.
pub fn copilot_api(model: &str) -> UpstreamApi {
    let Some(rest) = model.strip_prefix("gpt-") else {
        return UpstreamApi::Chat;
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let Ok(version) = digits.parse::<u32>() else {
        return UpstreamApi::Chat;
    };
    if version >= 5 && !model.starts_with("gpt-5-mini") {
        UpstreamApi::Responses
    } else {
        UpstreamApi::Chat
    }
}

/// Legacy `wire` key values from pre-`UpstreamApi` configs.
///
/// Read-only compatibility shim: [`ProviderConfig::effective_api`] converts
/// it to [`UpstreamApi`] once. New configs should use the `api` key instead;
/// nothing else in the codebase reads this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireCompat {
    GeminiApi,
    CodeAssist,
}

impl From<WireCompat> for UpstreamApi {
    fn from(wire: WireCompat) -> Self {
        match wire {
            WireCompat::GeminiApi => UpstreamApi::Gemini,
            WireCompat::CodeAssist => UpstreamApi::CodeAssist,
        }
    }
}
