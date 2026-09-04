//! The transcript replay on resume: the session file's past turns as
//! the entries the full-screen interface shows before the
//! `continuing session` notice. The plain interface prints the notice
//! alone.
//!
//! The replay is capped: the last [`MAX_MESSAGES`] messages or the
//! last [`MAX_BYTES`] of shown content, whichever is smaller. A tool
//! result counts and carries the tail the card reads, so one large
//! read cannot spend the budget on bytes nothing shows. The last
//! message always shows, whatever its size. The cap holds the first
//! prompt's cost on a long file: the boot reads no session file, and
//! the blocks lay out only for the rows on screen, so the replay
//! costs at the first prompt and only for the window. The messages
//! before the cut are counted, and the interface names the count.
//!
//! A tool call and its result are two messages: the call in the
//! model's, the result in the user's that follows. The replay pairs
//! them into one card by call id. A result whose call fell before
//! the cut shows nothing — a card without a call is noise — and a
//! call without a result shows as interrupted, as the live card
//! would after a cut-off turn.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use demerzel_core::{ContentBlock, Message, Replay as SessionReplay, Role};
use demerzel_tui::{OUTPUT_TAIL_BYTES, Replay, ReplayEntry, ReplayResult};
use serde_json::Value;

use crate::preview::run_preview;

/// Messages the replay shows at most, from the end of the file.
pub(crate) const MAX_MESSAGES: usize = 200;

/// Bytes of shown content the replay takes at most, from the end of
/// the file: the text of every block, the strings of a tool call's
/// input, and the tail of a tool result the card reads.
pub(crate) const MAX_BYTES: usize = 256 * 1024;

/// The interface's replay of a session file: the entries of the last
/// messages inside the caps, in order, with each call's card paired
/// to its result and each usage line placed after the message its
/// call produced.
pub(crate) fn entries(replay: &SessionReplay) -> Replay {
    let first = cut(&replay.messages);
    let clocks: HashMap<&str, Duration> = replay
        .tools
        .iter()
        .map(|tool| (tool.call_id.as_str(), tool.duration))
        .collect();
    let mut out = Replay::default();
    out.left_out = first;
    out.entries = Vec::with_capacity(replay.messages.len() - first);
    // The usage records before the cut report calls the replay
    // leaves out; the one that follows the last left-out message
    // does too.
    let mut usage = replay
        .usage
        .iter()
        .filter(|record| record.messages_before > first)
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
                            output: output_tail(content).to_string(),
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
        while let Some(record) = usage.next_if(|record| record.messages_before == index + 1) {
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
/// [`MAX_MESSAGES`] messages, or the last ones whose shown content
/// fits [`MAX_BYTES`], whichever is smaller. The last message always
/// shows: a replay of nothing but a count helps no one.
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
    first.min(messages.len().saturating_sub(1))
}

/// The bytes of a message's content the replay counts against the
/// cap: its text, the strings of a tool call's input, and the tail
/// of a tool result the card reads.
fn content_bytes(message: &Message) -> usize {
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.len(),
            ContentBlock::Reasoning { summary, .. } => summary.len(),
            ContentBlock::ToolResult { content, .. } => output_tail(content).len(),
            ContentBlock::ToolUse { name, input, .. } => name.len() + string_bytes(input),
        })
        .sum()
}

/// The tail of a tool result the card reads: the last
/// [`OUTPUT_TAIL_BYTES`] at most, on a char boundary. The card shows
/// the same for the whole text, so the rest is neither counted nor
/// copied.
fn output_tail(content: &str) -> &str {
    let mut start = content.len().saturating_sub(OUTPUT_TAIL_BYTES);
    while !content.is_char_boundary(start) {
        start += 1;
    }
    &content[start..]
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
    use demerzel_core::{ReplayToolClock, ReplayUsage, Timing, Usage};
    use serde_json::json;

    fn text(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    fn usage(messages_before: usize, at_ms: u64, duration_ms: u64) -> ReplayUsage {
        ReplayUsage {
            messages_before,
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

    /// A session replay of `messages` with no records.
    fn session(messages: Vec<Message>) -> SessionReplay {
        let mut replay = SessionReplay::default();
        replay.messages = messages;
        replay
    }

    /// One turn: the prompt, a reasoning segment with a bash call,
    /// its result, then the reply. Four messages, two usage
    /// records, one tool record.
    fn tool_turn() -> SessionReplay {
        let mut replay = session(vec![
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
        ]);
        replay.usage = vec![usage(2, 1_000, 300), usage(4, 1_500, 200)];
        replay.tools = vec![ReplayToolClock {
            call_id: "call_1".into(),
            duration: Duration::from_millis(40),
        }];
        replay
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
        let mut replay = session(messages);
        replay.usage = (0..MAX_MESSAGES + 3)
            .filter(|index| index % 2 == 1)
            .map(|index| ReplayUsage {
                messages_before: index + 1,
                usage: Usage::default(),
                timing: None,
            })
            .collect();
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
        let replay = session(vec![
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
        ]);
        // The call's input alone fills half the cap; with the first
        // message the sum crosses it, so the first message falls.
        let out = entries(&replay);
        assert_eq!(out.left_out, 1);
        assert!(matches!(&out.entries[0], ReplayEntry::Tool { .. }));
        assert_eq!(string_bytes(&json!({"k": ["ab", 1, null, {"n": "c"}]})), 5);
    }

    #[test]
    fn a_tool_result_counts_and_carries_the_tail_the_card_reads() {
        let mut replay = tool_turn();
        // A result far over the tail bound, with a multi-byte char
        // at the boundary.
        let mut content = "é".repeat(MAX_BYTES / 2);
        content.push_str("END");
        replay.messages[2] = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content,
                is_error: false,
            }],
        };
        let out = entries(&replay);
        // Nothing fell: the result counts its tail, not its length.
        assert_eq!(out.left_out, 0);
        let ReplayEntry::Tool {
            result: Some(result),
            ..
        } = &out.entries[2]
        else {
            panic!("the card, with its result: {:?}", out.entries[2]);
        };
        assert!(result.output.len() <= OUTPUT_TAIL_BYTES);
        assert!(result.output.len() > OUTPUT_TAIL_BYTES - 4);
        assert!(result.output.ends_with("END"));
        assert!(result.output.starts_with('é'));
    }

    #[test]
    fn a_result_whose_call_fell_before_the_cut_shows_nothing() {
        let mut replay = tool_turn();
        // The reply and the result's tail fill the byte cap up to a
        // few bytes, so the cut lands between the call and its
        // result.
        replay.messages[2] = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "x".repeat(2 * OUTPUT_TAIL_BYTES),
                is_error: false,
            }],
        };
        let reply = "y".repeat(MAX_BYTES - OUTPUT_TAIL_BYTES - 20);
        replay.messages[3] = text(Role::Assistant, &reply);
        replay.usage.clear();
        replay.tools.clear();
        let out = entries(&replay);
        assert_eq!(out.left_out, 2);
        assert_eq!(out.entries, vec![ReplayEntry::Text(reply)]);
    }

    #[test]
    fn the_last_message_always_shows() {
        let big = "z".repeat(MAX_BYTES + 1);
        let out = entries(&session(vec![
            text(Role::User, "first"),
            text(Role::User, &big),
        ]));
        assert_eq!(out.left_out, 1);
        assert_eq!(out.entries, vec![ReplayEntry::Prompt(big.clone())]);
        // Under the cap it counts as any message: with a small last
        // message, the big one before it falls.
        let out = entries(&session(vec![
            text(Role::User, &big),
            text(Role::User, "last"),
        ]));
        assert_eq!(out.left_out, 1);
        assert_eq!(out.entries, vec![ReplayEntry::Prompt("last".into())]);
        assert_eq!(entries(&session(Vec::new())).entries, Vec::new());
    }
}
