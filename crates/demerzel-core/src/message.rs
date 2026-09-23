use serde::{Deserialize, Serialize};

/// Who produced a message. Tool results travel as content blocks inside
/// user messages; they are not a separate role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

/// One turn of conversation history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

/// One block of message content. The serialized shape is part of the
/// session-file format; changing it is a schema change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    /// Model reasoning. `summary` is readable text. A sealed payload is
    /// never the only carrier of meaning.
    Reasoning {
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sealed: Option<SealedPayload>,
    },
}

/// Opaque provider state attached to a reasoning block. The harness
/// stores it verbatim and returns it to the same provider only.
/// "Sealed" means opaque to the harness, not encrypted.
///
/// The identity fields bind the payload to the instance and the model
/// that produced it. A field that is absent binds nothing, so a
/// payload written before the fields existed replays as before.
/// Non-exhaustive: build one with [`SealedPayload::new`], so a later
/// identity field stays additive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SealedPayload {
    /// Id of the adapter that produced the payload, e.g. "openai-codex".
    pub provider: String,
    /// Name of the provider instance that produced the payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Id of the model that produced the payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub data: serde_json::Value,
}

impl SealedPayload {
    /// A payload from adapter `provider`, with no identity fields.
    pub fn new(provider: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            provider: provider.into(),
            instance: None,
            model: None,
            data,
        }
    }

    /// The payload bound to one instance and one model.
    pub fn with_identity(mut self, instance: impl Into<String>, model: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self.model = Some(model.into());
        self
    }

    /// Whether the payload may go back on the wire to adapter
    /// `provider`, as instance `instance`, for model `model`: the
    /// adapter must match, and so must every identity field the
    /// payload carries.
    pub fn replays_to(&self, provider: &str, instance: &str, model: &str) -> bool {
        self.provider == provider
            && self.instance.as_deref().is_none_or(|own| own == instance)
            && self.model.as_deref().is_none_or(|own| own == model)
    }
}

/// Token counts for one model response. Fields default on read, so a
/// session file written before a field existed still replays.
///
/// `input_tokens` is the whole prompt. The two cache counts are its
/// parts: the tokens served from the prompt cache, and the tokens
/// written into it. A provider without a write count reports zero.
/// Cost derives from the three parts and the model; the harness
/// carries no price table.
///
/// The session file's `usage` record flattens these fields beside
/// its clock, so the names `at` and `duration_ms` are reserved by the
/// record and cannot be counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

/// Why the model stopped. Non-exhaustive: a future release can add a
/// reason without breaking adapter or interface crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopReason {
    EndTurn,
    ToolUse,
    /// A token limit ended the turn: the output cap or the context
    /// window.
    MaxTokens,
    /// The model or its safety layer declined the request. The turn
    /// produced no usable answer; the harness must not report it as
    /// completed.
    Refusal,
    Aborted,
}

impl StopReason {
    /// The reason's serialized name, the one source for the
    /// vocabulary the session file and the trace share.
    pub const fn as_str(self) -> &'static str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::ToolUse => "tool_use",
            StopReason::MaxTokens => "max_tokens",
            StopReason::Refusal => "refusal",
            StopReason::Aborted => "aborted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_message() -> Message {
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "reading the file".into(),
                },
                ContentBlock::Reasoning {
                    summary: "check the manifest first".into(),
                    sealed: Some(SealedPayload::new(
                        "openai-codex",
                        json!({"encrypted_content": "abc123"}),
                    )),
                },
                ContentBlock::ToolUse {
                    id: "call_1".into(),
                    name: "read".into(),
                    input: json!({"path": "Cargo.toml"}),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: "a1b2c3|[workspace]".into(),
                    is_error: false,
                },
            ],
        }
    }

    #[test]
    fn message_round_trips_through_json() {
        let message = full_message();
        let text = serde_json::to_string(&message).unwrap();
        let back: Message = serde_json::from_str(&text).unwrap();
        assert_eq!(message, back);
    }

    #[test]
    fn serialized_shape_is_stable() {
        // The session-file format depends on these exact shapes.
        let value = serde_json::to_value(full_message()).unwrap();
        assert_eq!(value["role"], "assistant");
        assert_eq!(value["content"][0]["type"], "text");
        assert_eq!(value["content"][1]["type"], "reasoning");
        assert_eq!(value["content"][1]["sealed"]["provider"], "openai-codex");
        assert_eq!(value["content"][2]["type"], "tool_use");
        assert_eq!(value["content"][3]["type"], "tool_result");
        assert_eq!(value["content"][3]["is_error"], false);
    }

    #[test]
    fn reasoning_without_sealed_payload_omits_the_field() {
        let block = ContentBlock::Reasoning {
            summary: "plain".into(),
            sealed: None,
        };
        let value = serde_json::to_value(&block).unwrap();
        assert!(value.get("sealed").is_none());
        let back: ContentBlock = serde_json::from_value(json!({
            "type": "reasoning",
            "summary": "plain",
        }))
        .unwrap();
        assert_eq!(block, back);
    }

    #[test]
    fn a_payload_replays_only_where_its_identity_matches() {
        let bare = SealedPayload::new("openai-compat", json!({}));
        assert!(bare.replays_to("openai-compat", "any", "any-model"));
        assert!(!bare.replays_to("anthropic", "any", "any-model"));

        let bound = bare.with_identity("local", "qwen3");
        assert!(bound.replays_to("openai-compat", "local", "qwen3"));
        assert!(!bound.replays_to("openai-compat", "hosted", "qwen3"));
        assert!(!bound.replays_to("openai-compat", "local", "qwen3-coder"));
        assert!(!bound.replays_to("anthropic", "local", "qwen3"));
    }

    #[test]
    fn identity_fields_round_trip_and_stay_absent_when_unset() {
        let bare = SealedPayload::new("anthropic", json!({"type": "thinking"}));
        let value = serde_json::to_value(&bare).unwrap();
        assert!(value.get("instance").is_none());
        assert!(value.get("model").is_none());
        // A payload from before the fields reads with neither.
        let old: SealedPayload =
            serde_json::from_value(json!({"provider": "anthropic", "data": {"type": "thinking"}}))
                .unwrap();
        assert_eq!(old, bare);

        let bound = bare.with_identity("work", "claude-sonnet-5");
        let value = serde_json::to_value(&bound).unwrap();
        assert_eq!(value["instance"], "work");
        assert_eq!(value["model"], "claude-sonnet-5");
        assert_eq!(
            serde_json::from_value::<SealedPayload>(value).unwrap(),
            bound
        );
    }

    #[test]
    fn stop_reason_and_role_use_snake_case() {
        assert_eq!(
            serde_json::to_value(StopReason::EndTurn).unwrap(),
            json!("end_turn")
        );
        assert_eq!(serde_json::to_value(Role::User).unwrap(), json!("user"));
    }

    #[test]
    fn a_stop_reason_name_is_its_serialized_form() {
        for stop in [
            StopReason::EndTurn,
            StopReason::ToolUse,
            StopReason::MaxTokens,
            StopReason::Refusal,
            StopReason::Aborted,
        ] {
            assert_eq!(serde_json::to_value(stop).unwrap(), json!(stop.as_str()));
        }
    }
}
