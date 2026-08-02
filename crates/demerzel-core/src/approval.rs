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

/// One tool call, as the policy sees it before dispatch.
///
/// The policy receives the full call, not only the access class, so
/// later per-tool rules, input inspection, and path checks extend the
/// policy without a signature break.
#[derive(Debug, Clone, Copy)]
pub struct ApprovalRequest<'a> {
    /// Tool name, e.g. "bash".
    pub tool: &'a str,
    /// The access class the tool declared.
    pub access: ToolAccess,
    /// The call input, verbatim.
    pub input: &'a serde_json::Value,
}

/// The approval policy for one session. In M1 it holds only the mode;
/// per-tool overrides and session-scoped grants land here as fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalPolicy {
    pub mode: ApprovalMode,
}

impl ApprovalPolicy {
    pub fn new(mode: ApprovalMode) -> Self {
        Self { mode }
    }

    /// Decide one tool call before dispatch.
    pub fn decide(&self, request: &ApprovalRequest<'_>) -> ApprovalDecision {
        self.mode.decide(request.access)
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
    fn the_policy_applies_the_mode_to_a_full_request() {
        let policy = ApprovalPolicy::new(ApprovalMode::Ask);
        let input = serde_json::json!({"command": "ls"});
        let request = ApprovalRequest {
            tool: "bash",
            access: ToolAccess::Execute,
            input: &input,
        };
        assert_eq!(policy.decide(&request), AskUser);
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
