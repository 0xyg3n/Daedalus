//! Literal native command planning; never runs a process or infers changed settings.
//!
//! Contracts audited against installed Claude Code 2.1.296 and 2.1.317
//! registrations and https://code.claude.com/docs/en/commands (2026-11-04).
//! Availability may still depend on account policy and interactive UI state.
//! The caller must authorize the operation and verify foreground process and
//! terminal identities immediately before submission; version text alone is not
//! process identity. Never mark a setting changed from a successful terminal write.

pub const AUDITED_CLAUDE_VERSION: &str = "2.1.296";
pub const AUDITED_CLAUDE_VERSIONS: &[&str] = &["2.1.296", "2.1.317"];
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub argument_bytes: u64,
    pub model_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            argument_bytes: 2048,
            model_bytes: 128,
        }
    }
}
impl Limits {
    pub fn validate(self) -> Result<(), Error> {
        if !(1..=65536).contains(&self.argument_bytes) || !(1..=4096).contains(&self.model_bytes) {
            return Err(Error::InvalidLimits);
        }
        Ok(())
    }
    pub const fn maximum() -> Self {
        Self {
            argument_bytes: 65536,
            model_bytes: 4096,
        }
    }
}
pub const SUBMISSION_NOTICE: &str = "Submitted to the harness. Completion and resulting settings are not verified; inspect its terminal for results or prompts.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    literal: String,
    pub command: String,
    /// The native command can initiate billed model work.
    pub may_invoke_model: bool,
    /// Do not apply to a protected controller conversation without explicit review.
    pub changes_context: bool,
    /// Submission may open a picker, permission prompt or editor.
    pub may_require_interaction: bool,
}
impl Plan {
    pub fn literal(&self) -> &str {
        &self.literal
    }
    pub fn submission_notice(&self) -> &'static str {
        SUBMISSION_NOTICE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    UnsupportedHarness,
    UnverifiedVersion,
    UnsupportedCommand,
    InvalidArguments,
    InvalidLimits,
    ArgumentLimit,
    ModelLimit,
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedHarness => {
                "No verified native slash-command contract for this harness"
            }
            Self::UnverifiedVersion => "Harness version has no audited slash-command contract; supported Claude Code versions: 2.1.296, 2.1.317",
            Self::UnsupportedCommand => "Command is not in the verified native allowlist",
            Self::InvalidArguments => "Arguments do not match the supported native command form",
            Self::InvalidLimits => "Invalid native command input limits",
            Self::ArgumentLimit => "Native arguments exceed native_command_argument_limit_bytes",
            Self::ModelLimit => "Native model ID exceeds native_command_model_limit_bytes",
        })
    }
}
impl std::error::Error for Error {}

/// Parse a request without assuming the running version or saved limits. The
/// runtime preparation must subsequently check the observed version and policy.
pub fn validate_request(command: &str, arguments: &str) -> Result<(), Error> {
    validate_request_with_limits(command, arguments, Limits::maximum())
}
pub fn validate_request_with_limits(
    command: &str,
    arguments: &str,
    limits: Limits,
) -> Result<(), Error> {
    let mut error = Error::UnsupportedCommand;
    for version in AUDITED_CLAUDE_VERSIONS {
        match plan_with_limits("claude", version, command, arguments, limits) {
            Ok(_) => return Ok(()),
            Err(current) if current != Error::UnsupportedCommand => error = current,
            Err(_) => (),
        }
    }
    Err(error)
}
/// Default policy for the pure planner; runtime consumers pass saved limits.
#[cfg(test)]
fn plan(harness: &str, version: &str, command: &str, arguments: &str) -> Result<Plan, Error> {
    plan_with_limits(harness, version, command, arguments, Limits::default())
}
/// Only explicitly audited versions are accepted. Native account/model policy
/// may still reject a recognized form; a terminal write never proves completion.
pub fn plan_with_limits(
    harness: &str,
    observed_version: &str,
    command: &str,
    arguments: &str,
    limits: Limits,
) -> Result<Plan, Error> {
    if harness != "claude" {
        return Err(Error::UnsupportedHarness);
    }
    let version = observed_version
        .strip_suffix(" (Claude Code)")
        .unwrap_or(observed_version);
    if !AUDITED_CLAUDE_VERSIONS.contains(&version) {
        return Err(Error::UnverifiedVersion);
    }
    limits.validate()?;
    if arguments.len() as u64 > limits.argument_bytes {
        return Err(Error::ArgumentLimit);
    }
    if arguments
        .chars()
        .any(|c| c.is_control() || (c.is_whitespace() && c != ' '))
    {
        return Err(Error::InvalidArguments);
    }
    if command == "cc" {
        let (head, tail) = arguments
            .trim()
            .split_once(' ')
            .unwrap_or((arguments.trim(), ""));
        let head = head.strip_prefix('/').ok_or(Error::InvalidArguments)?;
        if head == "cc" {
            return Err(Error::UnsupportedCommand);
        }
        return plan_with_limits(harness, observed_version, head, tail.trim(), limits);
    }
    let arguments = arguments.trim();
    let modern = version == "2.1.317";
    let (native, may_invoke_model, changes_context, may_require_interaction) = match command {
        "clear" => ("clear", false, true, false),
        "compact" => ("compact", true, true, false),
        "context" => ("context", false, false, true),
        "cost" if modern => ("usage", false, false, true),
        "cost" => ("cost", false, false, true),
        "usage" | "stats" if modern => ("usage", false, false, true),
        // /agents is a local configuration reminder in these audited versions.
        "agents" => ("agents", false, false, false),
        "list-agents" | "peers" if modern => ("list-agents", false, false, false),
        "mcp" => ("mcp", false, false, true),
        "review" | "code-review" if modern => ("code-review", true, false, true),
        "review" => ("review", true, false, true),
        "secreview" | "security-review" => ("security-review", true, false, true),
        "export" => ("export", false, false, true),
        "config" => ("config", false, false, true),
        "memory" => ("memory", false, false, true),
        "model" => ("model", true, false, true),
        "effort" => ("effort", false, false, true),
        "plan" => ("plan", false, false, true),
        _ => return Err(Error::UnsupportedCommand),
    };
    let valid = match native {
        "compact" => !arguments.chars().any(|c| "$`;|&<>\\\"'()".contains(c)),
        "model" => {
            if arguments.len() as u64 > limits.model_bytes {
                return Err(Error::ModelLimit);
            }
            arguments.is_empty()
                || (arguments.as_bytes()[0].is_ascii_alphanumeric()
                    && arguments
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._:-/@".contains(&c)))
        }
        "effort" => {
            matches!(arguments, "" | "low" | "medium" | "high" | "auto")
                || (modern
                    && matches!(
                        arguments,
                        "xhigh" | "max" | "status" | "ultracode" | "ultracode on" | "ultracode off"
                    ))
        }
        // Review arguments can post externally or apply changes. This adapter
        // continues to expose only the unparameterized native review.
        _ => arguments.is_empty(),
    };
    if !valid {
        return Err(Error::InvalidArguments);
    }
    Ok(Plan {
        literal: if arguments.is_empty() {
            format!("/{native}")
        } else {
            format!("/{native} {arguments}")
        },
        command: native.into(),
        may_invoke_model,
        changes_context,
        may_require_interaction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_contract_preserves_legacy_and_refuses_unaudited_versions_and_effectful_arguments() {
        for version in ["2.1.317", "2.1.317 (Claude Code)"] {
            for (input, literal) in [
                ("cost", "/usage"),
                ("usage", "/usage"),
                ("stats", "/usage"),
                ("review", "/code-review"),
                ("code-review", "/code-review"),
                ("list-agents", "/list-agents"),
                ("peers", "/list-agents"),
                ("agents", "/agents"),
            ] {
                let native = plan("claude", version, input, "").unwrap();
                assert_eq!(native.literal(), literal);
                assert_eq!(
                    native.may_invoke_model,
                    matches!(input, "review" | "code-review")
                );
            }
            for effort in [
                "low",
                "medium",
                "high",
                "auto",
                "xhigh",
                "max",
                "status",
                "ultracode",
                "ultracode on",
                "ultracode off",
            ] {
                assert!(plan("claude", version, "effort", effort).is_ok());
                assert!(validate_request("effort", effort).is_ok());
            }
            for (command, arguments) in [
                ("review", "--comment"),
                ("code-review", "--fix"),
                ("plan", "do work"),
                ("mcp", "reconnect all"),
                ("effort", "max; command"),
                ("effort", "ultracode on off"),
            ] {
                assert!(plan("claude", version, command, arguments).is_err());
            }
        }
        assert_eq!(
            plan("claude", "2.1.296", "cost", "").unwrap().literal(),
            "/cost"
        );
        assert_eq!(
            plan("claude", "2.1.296", "review", "").unwrap().literal(),
            "/review"
        );
        assert!(plan("claude", "2.1.296", "effort", "max").is_err());
        assert!(plan("claude", "2.1.296", "usage", "").is_err());
        for version in [
            "2.1.316",
            "2.1.318",
            "2.1.317-custom",
            "2.1.317 (unverified)",
        ] {
            assert_eq!(
                plan("claude", version, "model", "sonnet"),
                Err(Error::UnverifiedVersion)
            );
        }
    }
    #[test]
    fn saved_argument_and_model_limits_apply_through_aliases_without_relaxing_native_syntax() {
        for id in [
            "vendor/model-v1",
            "model@20261001",
            "arn:aws:bedrock:region:123456789012:inference-profile/vendor.model-v1:0",
        ] {
            let model = plan("claude", "2.1.317", "model", id).unwrap();
            assert!(
                model.may_invoke_model,
                "Model validation may make an inference request"
            );
        }
        let id = "m".repeat(256);
        assert!(validate_request("model", &id).is_ok());
        assert_eq!(
            plan("claude", "2.1.317", "model", &id),
            Err(Error::ModelLimit)
        );
        let limits = Limits {
            argument_bytes: 1024,
            model_bytes: 256,
        };
        assert!(
            plan_with_limits("claude", "2.1.317", "cc", &format!("/model {id}"), limits).is_ok()
        );
        assert!(
            plan_with_limits(
                "claude",
                "2.1.317",
                "model",
                &format!("{id};cmd"),
                Limits::maximum()
            )
            .is_err()
        );
        assert_eq!(
            plan_with_limits(
                "claude",
                "2.1.317",
                "compact",
                "long instructions",
                Limits {
                    argument_bytes: 4,
                    model_bytes: 128
                }
            ),
            Err(Error::ArgumentLimit)
        );
        assert_eq!(
            plan_with_limits(
                "claude",
                "2.1.317",
                "cc",
                "/model short",
                Limits {
                    argument_bytes: 4,
                    model_bytes: 128
                }
            ),
            Err(Error::ArgumentLimit)
        );
        assert!(validate_request("compact", &"x".repeat(65537)).is_err());
    }
    #[test]
    fn audited_command_fixture_has_exact_literals_and_honest_submission_semantics() {
        let rows: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/harness-slash-contract.json"
        ))
        .unwrap();
        for row in rows["commands"].as_array().unwrap() {
            let result = plan(
                "claude",
                AUDITED_CLAUDE_VERSION,
                row["input"].as_str().unwrap(),
                "",
            )
            .unwrap();
            assert_eq!(result.literal(), row["literal"].as_str().unwrap());
            assert_eq!(
                result.may_invoke_model,
                row["model_work"].as_bool().unwrap()
            );
            assert!(result.submission_notice().contains("not verified"));
        }
    }
    #[test]
    fn aliases_cannot_bypass_capability_version_or_argument_checks() {
        assert_eq!(
            plan("codex", AUDITED_CLAUDE_VERSION, "clear", ""),
            Err(Error::UnsupportedHarness)
        );
        assert_eq!(
            plan("claude", "2.1.999", "clear", ""),
            Err(Error::UnverifiedVersion)
        );
        for literal in [
            "/cc /model sonnet",
            "/mode bypassPermissions",
            "/unknown",
            "!touch file",
        ] {
            assert!(plan("claude", AUDITED_CLAUDE_VERSION, "cc", literal).is_err());
        }
        assert_eq!(
            plan("claude", AUDITED_CLAUDE_VERSION, "cc", "/secreview")
                .unwrap()
                .literal(),
            "/security-review"
        );
    }
    #[test]
    fn single_literal_preserves_text_without_terminal_or_setting_injection() {
        for args in [
            "sonnet\n/clear",
            "sonnet\r",
            "sonnet\u{1b}[0m",
            "sonnet\u{2028}/clear",
            "--flag",
            "$(command)",
            "sonnet /clear",
        ] {
            assert!(plan("claude", AUDITED_CLAUDE_VERSION, "model", args).is_err());
        }
        assert!(plan("claude", AUDITED_CLAUDE_VERSION, "review", "--comment").is_err());
        assert!(
            plan(
                "claude",
                AUDITED_CLAUDE_VERSION,
                "config",
                "permissions=bypass"
            )
            .is_err()
        );
        assert!(plan("claude", AUDITED_CLAUDE_VERSION, "effort", "extreme").is_err());
        for argument in [
            "$(command)",
            "`command`",
            "summary; command",
            "summary && command",
            "summary > file",
        ] {
            assert!(plan("claude", AUDITED_CLAUDE_VERSION, "compact", argument).is_err());
        }
        assert_eq!(
            plan(
                "claude",
                AUDITED_CLAUDE_VERSION,
                "compact",
                "Preserve implementation decisions"
            )
            .unwrap()
            .literal(),
            "/compact Preserve implementation decisions"
        );
        assert_eq!(
            plan("claude", AUDITED_CLAUDE_VERSION, "model", "sonnet")
                .unwrap()
                .literal(),
            "/model sonnet"
        );
    }
}
