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

impl ApprovalMode {
    /// The mode names, for a usage line.
    pub const NAMES: &'static str = "read-only, ask, auto";

    /// The mode's config and command name.
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalMode::ReadOnly => "read-only",
            ApprovalMode::Ask => "ask",
            ApprovalMode::Auto => "auto",
        }
    }
}

impl std::str::FromStr for ApprovalMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "read-only" => Ok(ApprovalMode::ReadOnly),
            "ask" => Ok(ApprovalMode::Ask),
            "auto" => Ok(ApprovalMode::Auto),
            other => Err(format!(
                "unknown approval mode `{other}`; the modes are {}",
                ApprovalMode::NAMES
            )),
        }
    }
}

impl std::fmt::Display for ApprovalMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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

/// The approval policy for one session: the mode, plus the grants the
/// user gave for the rest of the session. Per-tool overrides and
/// path rules land here as later fields.
///
/// Grant scope follows the access class (step-6 decision). A write
/// grant covers the tool by name. An execute grant covers only the
/// exact input it was given for: one "always" on bash must not approve
/// every later bash command in the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPolicy {
    pub mode: ApprovalMode,
    /// Write tools the user answered "allow for this session" for. A
    /// granted tool skips later ask prompts; it never overrides a
    /// `Deny`.
    grants: std::collections::BTreeSet<String>,
    /// Execute grants, keyed by tool name and the serialized input.
    /// `serde_json` orders object keys while `preserve_order` stays
    /// off, so the key is stable for one input value. A key mismatch
    /// fails closed: the call asks again.
    input_grants: std::collections::BTreeSet<(String, String)>,
}

impl ApprovalPolicy {
    pub fn new(mode: ApprovalMode) -> Self {
        Self {
            mode,
            grants: std::collections::BTreeSet::new(),
            input_grants: std::collections::BTreeSet::new(),
        }
    }

    /// Decide one tool call before dispatch.
    pub fn decide(&self, request: &ApprovalRequest<'_>) -> ApprovalDecision {
        let decision = self.mode.decide(request.access);
        if decision == ApprovalDecision::AskUser && self.granted(request) {
            return ApprovalDecision::Allow;
        }
        decision
    }

    pub(crate) fn granted(&self, request: &ApprovalRequest<'_>) -> bool {
        match request.access {
            ToolAccess::Execute => self
                .input_grants
                .contains(&(request.tool.to_string(), input_key(request.input))),
            _ => self.grants.contains(request.tool),
        }
    }

    /// Grant the call for the rest of the session, at the scope its
    /// access class sets: the exact input for execute tools, the tool
    /// name otherwise.
    pub fn grant(&mut self, request: &ApprovalRequest<'_>) {
        match request.access {
            ToolAccess::Execute => {
                self.input_grants
                    .insert((request.tool.to_string(), input_key(request.input)));
            }
            _ => {
                self.grants.insert(request.tool.to_string());
            }
        }
    }
}

/// The grant key for one input value.
fn input_key(input: &serde_json::Value) -> String {
    input.to_string()
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
    fn a_write_grant_covers_the_tool_by_name() {
        let mut policy = ApprovalPolicy::new(ApprovalMode::Ask);
        let first = serde_json::json!({"path": "a.txt"});
        let second = serde_json::json!({"path": "b.txt"});
        let request = |input| ApprovalRequest {
            tool: "edit",
            access: ToolAccess::Write,
            input,
        };
        policy.grant(&request(&first));
        assert_eq!(policy.decide(&request(&first)), Allow);
        assert_eq!(policy.decide(&request(&second)), Allow);
    }

    #[test]
    fn an_execute_grant_covers_only_the_exact_input() {
        let mut policy = ApprovalPolicy::new(ApprovalMode::Ask);
        let granted = serde_json::json!({"command": "cargo test"});
        let same = serde_json::json!({"command": "cargo test"});
        let other = serde_json::json!({"command": "rm -rf /"});
        let request = |input| ApprovalRequest {
            tool: "bash",
            access: ToolAccess::Execute,
            input,
        };
        policy.grant(&request(&granted));
        assert_eq!(policy.decide(&request(&same)), Allow);
        assert_eq!(policy.decide(&request(&other)), AskUser);
    }

    #[test]
    fn grants_never_cross_tools_or_access_classes() {
        let mut policy = ApprovalPolicy::new(ApprovalMode::Ask);
        let input = serde_json::json!({"command": "ls"});
        policy.grant(&ApprovalRequest {
            tool: "bash",
            access: ToolAccess::Execute,
            input: &input,
        });
        // The same input under another tool name stays a question.
        assert_eq!(
            policy.decide(&ApprovalRequest {
                tool: "shell",
                access: ToolAccess::Execute,
                input: &input,
            }),
            AskUser
        );
        // A name grant on a write tool leaves execute tools untouched.
        policy.grant(&ApprovalRequest {
            tool: "edit",
            access: ToolAccess::Write,
            input: &input,
        });
        assert_eq!(
            policy.decide(&ApprovalRequest {
                tool: "edit",
                access: ToolAccess::Execute,
                input: &serde_json::json!({"other": 1}),
            }),
            AskUser
        );
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

    #[test]
    fn a_mode_name_round_trips_and_a_stranger_names_the_modes() {
        for mode in [
            ApprovalMode::ReadOnly,
            ApprovalMode::Ask,
            ApprovalMode::Auto,
        ] {
            assert_eq!(mode.as_str().parse::<ApprovalMode>(), Ok(mode));
            assert_eq!(mode.to_string(), mode.as_str());
        }
        let error = "Auto".parse::<ApprovalMode>().unwrap_err();
        assert!(error.contains(ApprovalMode::NAMES), "{error}");
    }
}
