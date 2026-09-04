//! The transcript replay on resume: the session file's past turns as
//! the entries the full-screen interface shows before the
//! `continuing session` notice. The plain interface prints the notice
//! alone.
//!
//! The replay is capped: the last [`MAX_MESSAGES`] messages or the
//! last [`MAX_BYTES`] of message content, whichever is fewer. The cap
//! holds the first prompt's cost on a long file: the boot reads no
//! session file, and the blocks lay out only for the rows on screen,
//! so the replay costs at the first prompt and only for the window.
//! The messages before the cut are counted, and the interface names
//! the count.
//!
//! A tool call and its result are two messages: the call in the
//! model's, the result in the user's that follows. The replay pairs
//! them into one card by call id. A result whose call fell before
//! the cut shows nothing — a card without a call is noise — and a
//! call without a result shows as interrupted, as the live card
//! would after a cut-off turn.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use demerzel_core::{ContentBlock, Message, Replay, Role};
use demerzel_tui::{ReplayEntry, ReplayResult};
use serde_json::Value;

use crate::preview::run_preview;

/// Messages the replay shows at most, from the end of the file.
pub(crate) const MAX_MESSAGES: usize = 200;

/// Bytes of message content the replay shows at most, from the end
/// of the file: the text of every block, and the strings of a tool
/// call's input.
pub(crate) const MAX_BYTES: usize = 256 * 1024;

/// The interface's replay of a session file: the entries of the last
/// messages inside the caps, in order, with each call's card paired
/// to its result and each usage line placed after the message its
/// call produced.
pub(crate) fn entries(replay: &Replay) -> demerzel_tui::Replay {
    let first = cut(&replay.messages);
    let clocks: HashMap<&str, Duration> = replay
        .tools
        .iter()
        .map(|tool| (tool.call_id.as_str(), tool.duration))
        .collect();
    let mut out = demerzel_tui::Replay {
        left_out: first,
        entries: Vec::new(),
    };
    // The usage records before the cut report calls the replay
    // leaves out; the one that follows the last left-out message
    // does too.
    let mut usage = replay
        .usage
        .iter()
        .filter(|record| record.before > first)
        .peekable();
    // Open calls by id: the index of the card entry a result fills.
    let mut pending: HashMap<&str, usize> = HashMap::new();
    // When the turn's first call went out: the usage line shows the
    // time since the turn started, and the first request follows the
    // start at once.
    let mut turn_start: Option<SystemTime> = None;
    for (index, message) in replay.messages.iter().enumerate().skip(first) {
        for block in &message.content {
            match (message.role, block) {
                (Role::User, ContentBlock::Text { text }) => {
                    turn_start = None;
                    out.entries.push(ReplayEntry::Prompt(text.clone()));
                }
                (
                    Role::User,
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    },
                ) => {
                    if let Some(at) = pending.remove(tool_use_id.as_str())
                        && let Some(ReplayEntry::Tool { result, .. }) = out.entries.get_mut(at)
                    {
                        *result = Some(ReplayResult {
                            output: content.clone(),
                            is_error: *is_error,
                            duration: clocks.get(tool_use_id.as_str()).copied(),
                        });
                    }
                }
                (Role::Assistant, ContentBlock::Text { text }) => {
                    out.entries.push(ReplayEntry::Text(text.clone()));
                }
                (Role::Assistant, ContentBlock::Reasoning { summary, .. }) => {
                    out.entries.push(ReplayEntry::Reasoning(summary.clone()));
                }
                (Role::Assistant, ContentBlock::ToolUse { id, name, input }) => {
                    pending.insert(id, out.entries.len());
                    out.entries.push(ReplayEntry::Tool {
                        name: name.clone(),
                        preview: run_preview(name, input),
                        result: None,
                    });
                }
                _ => {}
            }
        }
        while let Some(record) = usage.next_if(|record| record.before == index + 1) {
            let elapsed = record.timing.map(|timing| {
                let start = *turn_start.get_or_insert(timing.at);
                (timing.at + timing.duration)
                    .duration_since(start)
                    .unwrap_or(timing.duration)
            });
            out.entries.push(ReplayEntry::Usage {
                usage: record.usage,
                elapsed,
            });
        }
    }
    out
}

/// The index of the first message the replay shows: the last
/// [`MAX_MESSAGES`] messages, or the last ones whose content fits
/// [`MAX_BYTES`], whichever is fewer.
fn cut(messages: &[Message]) -> usize {
    let mut bytes = 0usize;
    let mut first = messages.len();
    for (index, message) in messages.iter().enumerate().rev() {
        bytes += content_bytes(message);
        if messages.len() - index > MAX_MESSAGES || bytes > MAX_BYTES {
            break;
        }
        first = index;
    }
    first
}

/// The bytes of a message's content the replay counts against the
/// cap: its text, and the strings of a tool call's input.
fn content_bytes(message: &Message) -> usize {
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::Reasoning { summary, .. } => summary.len(),
            ContentBlock::ToolResult { content, .. } => content.len(),
            ContentBlock::ToolUse { name, input, .. } => name.len() + string_bytes(input),
        })
        .sum()
}

/// The bytes of the strings inside a JSON value, keys included. No
/// allocation: the count walks the value.
fn string_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        Value::Array(items) => items.iter().map(string_bytes).sum(),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| key.len() + string_bytes(value))
            .sum(),
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demerzel_core::{ReplayTool, ReplayUsage, Timing, Usage};
    use serde_json::json;

    fn text(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    fn usage(before: usize, at_ms: u64, duration_ms: u64) -> ReplayUsage {
        ReplayUsage {
            before,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 2,
                cache_creation_tokens: 0,
            },
            timing: Some(Timing::new(
                SystemTime::UNIX_EPOCH + Duration::from_millis(at_ms),
                Duration::from_millis(duration_ms),
            )),
        }
    }

    /// One turn: the prompt, a reasoning segment with a bash call,
    /// its result, then the reply. Four messages, two usage
    /// records, one tool record.
    fn tool_turn() -> Replay {
        Replay {
            meta: None,
            messages: vec![
                text(Role::User, "run it"),
                Message {
                    role: Role::Assistant,
                    content: vec![
                        ContentBlock::Reasoning {
                            summary: "check the file first".into(),
                            sealed: None,
                        },
                        ContentBlock::ToolUse {
                            id: "call_1".into(),
                            name: "bash".into(),
                            input: json!({"command": "echo hi"}),
                        },
                    ],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "call_1".into(),
                        content: "hi\n".into(),
                        is_error: false,
                    }],
                },
                text(Role::Assistant, "done"),
            ],
            usage: vec![usage(2, 1_000, 300), usage(4, 1_500, 200)],
            tools: vec![ReplayTool {
                call_id: "call_1".into(),
                duration: Duration::from_millis(40),
            }],
        }
    }

    #[test]
    fn a_turn_replays_as_its_blocks_in_order_with_the_card_paired_to_its_result() {
        let out = entries(&tool_turn());
        assert_eq!(out.left_out, 0);
        assert_eq!(
            out.entries,
            vec![
                ReplayEntry::Prompt("run it".into()),
                ReplayEntry::Reasoning("check the file first".into()),
                ReplayEntry::Tool {
                    name: "bash".into(),
                    preview: run_preview("bash", &json!({"command": "echo hi"})),
                    result: Some(ReplayResult {
                        output: "hi\n".into(),
                        is_error: false,
                        duration: Some(Duration::from_millis(40)),
                    }),
                },
                // The first call's line shows its own time; the
                // second shows the time since the turn started, the
                // way the live line does.
                ReplayEntry::Usage {
                    usage: usage(2, 0, 0).usage,
                    elapsed: Some(Duration::from_millis(300)),
                },
                ReplayEntry::Text("done".into()),
                ReplayEntry::Usage {
                    usage: usage(4, 0, 0).usage,
                    elapsed: Some(Duration::from_millis(700)),
                },
            ]
        );
    }

    #[test]
    fn a_call_without_a_result_is_interrupted_and_a_file_without_clocks_shows_no_time() {
        let mut replay = tool_turn();
        replay.messages.truncate(2);
        replay.usage.truncate(1);
        replay.usage[0].timing = None;
        replay.tools.clear();
        let out = entries(&replay);
        assert!(matches!(
            &out.entries[2],
            ReplayEntry::Tool { result: None, .. }
        ));
        assert!(matches!(
            &out.entries[3],
            ReplayEntry::Usage { elapsed: None, .. }
        ));
        assert_eq!(out.entries.len(), 4);
    }

    #[test]
    fn the_message_cap_keeps_the_last_messages_and_counts_the_rest() {
        let messages: Vec<Message> = (0..MAX_MESSAGES + 3)
            .map(|index| {
                let role = if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                };
                text(role, &format!("m{index}"))
            })
            .collect();
        // Usage records: one after each model message, the last one
        // before the cut included, so its line must not show.
        let usage = (0..MAX_MESSAGES + 3)
            .filter(|index| index % 2 == 1)
            .map(|index| ReplayUsage {
                before: index + 1,
                usage: Usage::default(),
                timing: None,
            })
            .collect();
        let replay = Replay {
            meta: None,
            messages,
            usage,
            tools: Vec::new(),
        };
        let out = entries(&replay);
        assert_eq!(out.left_out, 3);
        assert_eq!(out.entries.len(), MAX_MESSAGES + (MAX_MESSAGES + 3) / 2 - 1);
        assert_eq!(out.entries[0], ReplayEntry::Text("m3".into()));
        assert_eq!(
            out.entries[1],
            ReplayEntry::Usage {
                usage: Usage::default(),
                elapsed: None
            }
        );
        let usage_lines = out
            .entries
            .iter()
            .filter(|entry| matches!(entry, ReplayEntry::Usage { .. }))
            .count();
        // Model messages m3, m5, ... m201: one line each; m1 falls
        // before the cut, and its line with it.
        assert_eq!(usage_lines, (MAX_MESSAGES + 3) / 2 - 1);
    }

    #[test]
    fn the_byte_cap_counts_every_block_and_the_strings_of_a_call_input() {
        let big = "x".repeat(MAX_BYTES / 2);
        let replay = Replay {
            meta: None,
            messages: vec![
                text(Role::User, &big),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "c".into(),
                        name: "create".into(),
                        input: json!({"path": "a.txt", "content": big}),
                    }],
                },
                text(Role::User, "small"),
                text(Role::Assistant, "reply"),
            ],
            usage: Vec::new(),
            tools: Vec::new(),
        };
        // The call's input alone fills half the cap; with the first
        // message the sum crosses it, so the first message falls.
        let out = entries(&replay);
        assert_eq!(out.left_out, 1);
        assert!(matches!(&out.entries[0], ReplayEntry::Tool { .. }));
        assert_eq!(string_bytes(&json!({"k": ["ab", 1, null, {"n": "c"}]})), 5);
    }

    #[test]
    fn a_result_whose_call_fell_before_the_cut_shows_nothing() {
        let mut replay = tool_turn();
        // The result fills the byte cap up to a few bytes, so the
        // cut lands between the call and its result.
        replay.messages[2] = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "x".repeat(MAX_BYTES - 20),
                is_error: false,
            }],
        };
        replay.usage.clear();
        replay.tools.clear();
        let out = entries(&replay);
        assert_eq!(out.left_out, 2);
        assert_eq!(out.entries, vec![ReplayEntry::Text("done".into())]);
    }
}
