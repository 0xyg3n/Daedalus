//! Literal native command planning; never runs a process or infers changed settings.
//!
//! Contract audited against Claude Code 2.1.296's installed command registrations
//! and https://code.claude.com/docs/en/commands (2026-10-13).
//! Availability may still depend on account policy and interactive UI state.
//! The caller must authorize the operation and verify foreground process and
//! terminal identities immediately before submission; version text alone is not
//! process identity. Never mark a setting changed from a successful terminal write.

pub const AUDITED_CLAUDE_VERSION: &str = "2.1.296";
pub const SUBMISSION_NOTICE: &str = "Submitted to the harness. Completion and resulting settings are not verified; inspect its terminal for results or prompts.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    literal: String,
    pub command: &'static str,
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
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedHarness => {
                "No verified native slash-command contract for this harness"
            }
            Self::UnverifiedVersion => "Harness version has no audited slash-command contract",
            Self::UnsupportedCommand => "Command is not in the verified native allowlist",
            Self::InvalidArguments => "Arguments do not match the supported native command form",
        })
    }
}
impl std::error::Error for Error {}

/// Returns the reviewed literal only. Unsupported forms are errors, never plain
/// prompts or keyboard-based fallbacks. `/cc` is an alias into the same allowlist.
pub fn plan(
    harness: &str,
    observed_version: &str,
    command: &str,
    arguments: &str,
) -> Result<Plan, Error> {
    if harness != "claude" {
        return Err(Error::UnsupportedHarness);
    }
    if !matches!(observed_version, "2.1.296" | "2.1.296 (Claude Code)") {
        return Err(Error::UnverifiedVersion);
    }
    if arguments.len() > 2048
        || arguments
            .chars()
            .any(|c| c.is_control() || (c.is_whitespace() && c != ' '))
    {
        return Err(Error::InvalidArguments);
    }
    if command == "cc" {
        let literal = arguments.trim();
        let (head, tail) = literal.split_once(' ').unwrap_or((literal, ""));
        let Some(head) = head.strip_prefix('/') else {
            return Err(Error::InvalidArguments);
        };
        if head == "cc" {
            return Err(Error::UnsupportedCommand);
        }
        return plan(harness, observed_version, head, tail.trim());
    }
    let arguments = arguments.trim();
    let (native, may_invoke_model, changes_context, may_require_interaction) = match command {
        "clear" => ("clear", false, true, false),
        "compact" => ("compact", true, true, false),
        "context" => ("context", false, false, true),
        "cost" => ("cost", false, false, true),
        "agents" => ("agents", false, false, false),
        "mcp" => ("mcp", false, false, true),
        "review" => ("review", true, false, true),
        "secreview" | "security-review" => ("security-review", true, false, true),
        "export" => ("export", false, false, true),
        "config" => ("config", false, false, true),
        "memory" => ("memory", false, false, true),
        "model" => ("model", false, false, true),
        "effort" => ("effort", false, false, true),
        "plan" => ("plan", false, false, true),
        _ => return Err(Error::UnsupportedCommand),
    };
    let valid = match native {
        // Do not allow shell expansion/separators if the foreground provider exits
        // in the unavoidable interval before the terminal receives this literal.
        "compact" => !arguments.chars().any(|c| "$`;|&<>\\\"'()".contains(c)),
        "model" => {
            arguments.is_empty()
                || (arguments.len() <= 128
                    && arguments.as_bytes()[0].is_ascii_alphanumeric()
                    && arguments
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c)))
        }
        "effort" => matches!(arguments, "" | "low" | "medium" | "high" | "auto"),
        // Review argument forms can post externally or apply changes; only the
        // unparameterized native review is exposed by this contract.
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
        command: native,
        may_invoke_model,
        changes_context,
        may_require_interaction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
