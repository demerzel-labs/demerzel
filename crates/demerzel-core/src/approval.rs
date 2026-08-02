//! Approval policy: which tool calls run, which wait for the user, and
//! which are refused. Policy lives in the harness, never in the prompt,
//! so the prompt prefix stays stable.

use serde::{Deserialize, Serialize};

/// What a tool can do to the world. Every tool declares one class; the
/// policy keys on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAccess {
    /// Reads files or metadata. Never gated.
    ReadOnly,
    /// Writes or deletes files.
    Write,
    /// Runs arbitrary commands.
    Execute,
}

/// The three approval modes. Non-interactive runs default to
/// `ReadOnly`; `--auto` enables writes without prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    ReadOnly,
    Ask,
    Auto,
}

/// What the harness does with one tool call before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Run the call.
    Allow,
    /// Ask the user, then run or refuse.
    AskUser,
    /// Refuse the call and tell the model why.
    Deny,
}

impl ApprovalMode {
    /// Decide one tool call from its declared access class.
    pub fn decide(self, access: ToolAccess) -> ApprovalDecision {
        match (self, access) {
            (_, ToolAccess::ReadOnly) => ApprovalDecision::Allow,
            (ApprovalMode::ReadOnly, _) => ApprovalDecision::Deny,
            (ApprovalMode::Ask, _) => ApprovalDecision::AskUser,
            (ApprovalMode::Auto, _) => ApprovalDecision::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ApprovalDecision::{Allow, AskUser, Deny};

    #[test]
    fn the_full_decision_matrix_holds() {
        let cases = [
            (ApprovalMode::ReadOnly, ToolAccess::ReadOnly, Allow),
            (ApprovalMode::ReadOnly, ToolAccess::Write, Deny),
            (ApprovalMode::ReadOnly, ToolAccess::Execute, Deny),
            (ApprovalMode::Ask, ToolAccess::ReadOnly, Allow),
            (ApprovalMode::Ask, ToolAccess::Write, AskUser),
            (ApprovalMode::Ask, ToolAccess::Execute, AskUser),
            (ApprovalMode::Auto, ToolAccess::ReadOnly, Allow),
            (ApprovalMode::Auto, ToolAccess::Write, Allow),
            (ApprovalMode::Auto, ToolAccess::Execute, Allow),
        ];
        for (mode, access, expected) in cases {
            assert_eq!(mode.decide(access), expected, "{mode:?} x {access:?}");
        }
    }

    #[test]
    fn approval_types_use_snake_case_for_the_session_log() {
        // Approval decisions go into the session record as non-prompt
        // events; the serialized shape is part of the session format.
        assert_eq!(
            serde_json::to_value(ApprovalMode::ReadOnly).unwrap(),
            serde_json::json!("read_only")
        );
        assert_eq!(
            serde_json::to_value(ToolAccess::Execute).unwrap(),
            serde_json::json!("execute")
        );
        assert_eq!(
            serde_json::to_value(ApprovalDecision::AskUser).unwrap(),
            serde_json::json!("ask_user")
        );
    }
}
