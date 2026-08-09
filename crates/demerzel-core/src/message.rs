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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SealedPayload {
    /// Id of the adapter that produced the payload, e.g. "openai-codex".
    pub provider: String,
    pub data: serde_json::Value,
}

/// Token counts for one model response. Fields default on read, so a
/// session file written before a field existed still replays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
}

/// Why the model stopped. Non-exhaustive: a later milestone can add a
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
                    sealed: Some(SealedPayload {
                        provider: "openai-codex".into(),
                        data: json!({"encrypted_content": "abc123"}),
                    }),
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
    fn stop_reason_and_role_use_snake_case() {
        assert_eq!(
            serde_json::to_value(StopReason::EndTurn).unwrap(),
            json!("end_turn")
        );
        assert_eq!(serde_json::to_value(Role::User).unwrap(), json!("user"));
    }
}
