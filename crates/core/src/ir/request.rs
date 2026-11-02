//! Request IR: one call to a model, whichever API it arrived in.
//!
//! Ported from magpie's `internal/gateway/ir.go` — see
//! `docs/design/ir-hub-conversion.md` for field-by-field provenance.
//! Readers (`formats/<fmt>/read.rs`) turn wire types into these; builders
//! (`formats/<fmt>/build.rs`) turn them back into one format's wire type.
//! Invariants builders may rely on and nothing else:
//! * `system` holds every system/developer instruction, never a message role;
//! * a `ToolCall` part's `id` matches the `call_id` of the `ToolResult`
//!   that answers it (pairing/repair is the *target* format's builder job);
//! * `Sealed` and `Thinking.signature` cross the hub untouched: only the
//!   sealing API's builder may write them back out (magpie `Part.Sealed`).

use serde_json::Value;

use crate::convert::Format;

/// One turn. `system` never appears here; it lives on [`IrRequest::system`].
#[derive(Debug, Clone, PartialEq)]
pub struct IrMessage {
    pub role: Role,
    pub parts: Vec<IrPart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One block of a message (magpie `Part`).
#[derive(Debug, Clone, PartialEq)]
pub enum IrPart {
    Text(String),
    Image {
        url: String,
        media_type: String,
        /// base64, when the client inlined it
        data: Option<String>,
        /// the detail the client asked for (low, high, auto, Responses' original)
        detail: Option<String>,
    },
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    /// The output that answers a `ToolCall`; `content` holds text and images.
    ToolResult {
        call_id: String,
        is_error: bool,
        content: Vec<IrPart>,
    },
    Thinking {
        text: String,
        /// Anthropic's thinking signature (or Gemini's thoughtSignature):
        /// dropping it makes the next turn's replay fail upstream.
        signature: Option<Signature>,
    },
    /// Reasoning an upstream sealed as its own API wrote it — Anthropic's
    /// `redacted_thinking` block, a Responses reasoning item with its
    /// `encrypted_content`. Carried byte-for-byte; any other format sees
    /// reasoning with no text.
    Sealed {
        by: Format,
        raw: Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    /// the API that signed it; only that format's builder may replay it
    pub by: Format,
    pub value: String,
}

/// A function the model may call (magpie `Tool`).
#[derive(Debug, Clone, PartialEq)]
pub struct IrTool {
    pub name: String,
    pub description: String,
    /// JSON schema of the arguments
    pub schema: Value,
    /// the client asked for its arguments held to the schema
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum IrToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sampling {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub stop: Vec<String>,
}

/// How the client asked the model to reason. Readers keep the shape the
/// client used; `converters::reasoning` maps between them when a builder
/// needs the other one.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Reasoning {
    #[default]
    Off,
    /// Anthropic `thinking.budget_tokens`
    Budget(u32),
    /// OpenAI `reasoning_effort` / Responses `reasoning.effort`
    Effort(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IrRequest {
    pub model: String,
    pub system: Vec<IrPart>,
    pub messages: Vec<IrMessage>,
    pub tools: Vec<IrTool>,
    pub tool_choice: IrToolChoice,
    pub sampling: Sampling,
    pub stream: bool,
    pub reasoning: Reasoning,
    /// Fields no IR type models, kept by provenance so two vendors'
    /// same-named fields never cross (design doc §6).
    pub extra: NamespacedExtra,
}

/// `(owner format, field) -> value` bag. A builder takes only entries its
/// own format owns, plus the passlist of semantics-free fields.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NamespacedExtra {
    fields: Vec<(Format, String, Value)>,
}

/// Fields any format may hand any other: no vendor semantics attached.
const PASSLIST: &[&str] = &["user", "stream_options", "parallel_tool_calls"];

impl NamespacedExtra {
    pub fn put(&mut self, owner: Format, field: &str, value: Value) {
        if let Some(slot) = self
            .fields
            .iter_mut()
            .find(|(f, name, _)| *f == owner && name == field)
        {
            slot.2 = value;
        } else {
            self.fields.push((owner, field.to_string(), value));
        }
    }

    /// Take this format's own entries plus the passlist, removing them from
    /// the bag. Dropped entries (other vendors') are the caller's to log.
    pub fn take(&mut self, target: Format) -> Vec<(String, Value)> {
        let mut taken = Vec::new();
        let mut rest = Vec::new();
        for (owner, name, value) in std::mem::take(&mut self.fields) {
            if owner == target || PASSLIST.contains(&name.as_str()) {
                taken.push((name, value));
            } else {
                rest.push((owner, name, value));
            }
        }
        self.fields = rest;
        taken
    }

    /// What a builder refused to carry on, for a debug log.
    pub fn dropped(&self) -> impl Iterator<Item = (&Format, &str)> {
        self.fields.iter().map(|(f, name, _)| (f, name.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_crosses_only_same_vendor_or_passlist() {
        let mut extra = NamespacedExtra::default();
        extra.put(Format::Claude, "metadata", serde_json::json!({"u": "x"}));
        extra.put(Format::OpenAIChat, "stream_options", serde_json::json!({}));

        let claude_gets = extra.take(Format::Claude);
        assert_eq!(
            claude_gets
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["metadata", "stream_options"]
        );
        assert_eq!(extra.dropped().count(), 0, "passlist entry was taken");

        let mut extra = NamespacedExtra::default();
        extra.put(Format::Gemini, "safetySettings", serde_json::json!([]));
        let chat_gets = extra.take(Format::OpenAIChat);
        assert!(chat_gets.is_empty());
        assert_eq!(
            extra.dropped().next().map(|(f, _)| *f),
            Some(Format::Gemini)
        );
    }

    #[test]
    fn extra_put_replaces_same_field() {
        let mut extra = NamespacedExtra::default();
        extra.put(Format::Claude, "k", serde_json::json!(1));
        extra.put(Format::Claude, "k", serde_json::json!(2));
        assert_eq!(extra.take(Format::Claude)[0].1, serde_json::json!(2));
    }
}
