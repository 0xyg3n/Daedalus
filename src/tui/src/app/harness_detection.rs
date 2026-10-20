//! Bounded local discovery. Never return account identities, tokens or raw status output.
use super::config::ControlConfig;
use crate::adapters::{harnesses, process};
use std::{collections::BTreeMap, process::Command, time::Duration};
#[derive(Clone, Debug)]
pub struct Detected {
    pub id: String,
    pub installed: bool,
    pub status: String,
}
fn status(id: &str, success: bool, stdout: &[u8], stderr: &[u8]) -> Option<&'static str> {
    match id {
        "claude" => {
            let value: serde_json::Value = serde_json::from_slice(stdout).ok()?;
            match value["loggedIn"].as_bool()? {
                true if success => Some("Signed in · native status"),
                false => Some("Not signed in"),
                _ => None,
            }
        }
        "codex" => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(stdout),
                String::from_utf8_lossy(stderr)
            )
            .to_lowercase();
            if success
                && text
                    .lines()
                    .any(|l| l.trim().starts_with("logged in using"))
            {
                Some("Signed in · native status")
            } else if text.lines().any(|l| l.trim() == "not logged in") {
                Some("Not signed in")
            } else {
                None
            }
        }
        _ => None,
    }
}
pub fn detect(config: &ControlConfig) -> Vec<Detected> {
    detect_with(config, &crate::platform::environment())
}
fn detect_with(config: &ControlConfig, environment: &BTreeMap<String, String>) -> Vec<Detected> {
    std::thread::scope(|scope| {
        let workers = harnesses::tools()
            .map(|tool| {
                scope.spawn(move || {
                    let binary = harnesses::executable(&tool, environment);
                    let profile =
                        harnesses::profile_directory(&tool, environment, &config.profiles).ok();
                    let mut state = if binary.is_none() {
                        "Not installed"
                    } else if profile.as_ref().is_some_and(|p| p.is_dir()) {
                        "Profile found · sign-in unverified"
                    } else {
                        "Installed · sign-in unverified"
                    };
                    if let (Some(binary), Some(profile)) = (&binary, &profile) {
                        let args: &[&str] = match tool.id {
                            "codex" => &["login", "status"],
                            "claude" => &["auth", "status", "--json"],
                            _ => &[],
                        };
                        if !args.is_empty() {
                            let mut command = Command::new(binary);
                            command.args(args).env(tool.profile_variable, profile);
                            if let Ok(result) =
                                process::run(&mut command, Duration::from_secs(2), 16384)
                            {
                                state = status(
                                    tool.id,
                                    result.status.success(),
                                    &result.stdout,
                                    &result.stderr,
                                )
                                .unwrap_or(state);
                            }
                        }
                    }
                    Detected {
                        id: tool.id.into(),
                        installed: binary.is_some(),
                        status: state.into(),
                    }
                })
            })
            .collect::<Vec<_>>();
        workers.into_iter().filter_map(|w| w.join().ok()).collect()
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_status_never_exposes_native_identity_or_accepts_arbitrary_success() {
        assert_eq!(
            status(
                "codex",
                true,
                b"Logged in using ChatGPT (private@example.invalid)",
                b""
            ),
            Some("Signed in · native status")
        );
        assert_eq!(
            status("codex", false, b"", b"Not logged in"),
            Some("Not signed in")
        );
        assert_eq!(status("codex", true, b"unexpected output", b""), None);
        assert_eq!(
            status(
                "claude",
                true,
                br#"{"loggedIn":true,"email":"private@example.invalid"}"#,
                b""
            ),
            Some("Signed in · native status")
        );
        assert_eq!(status("claude", false, br#"{"loggedIn":true}"#, b""), None);
        assert_eq!(status("claude", true, b"invalid", b""), None);
    }
}
