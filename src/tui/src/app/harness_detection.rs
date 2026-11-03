//! Bounded local discovery. Never return account identities, tokens or raw status output.
use super::config::ControlConfig;
use crate::adapters::{harnesses, process};
use std::{collections::BTreeMap, path::Path, time::Duration};
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
                    if let (Some(binary), Some(_)) = (&binary, &profile) {
                        let args: &[&str] = match tool.id {
                            "codex" => &["login", "status"],
                            "claude" => &["auth", "status", "--json"],
                            _ => &[],
                        };
                        let cwd = environment
                            .get("HOME")
                            .or_else(|| environment.get("USERPROFILE"))
                            .map(String::as_str)
                            .unwrap_or(&config.agent_home);
                        if !args.is_empty()
                            && config.runtime.validate().is_ok()
                            && let Ok(plan) = harnesses::launch_plan(
                                &tool,
                                binary,
                                Path::new(cwd),
                                environment,
                                &config.profiles,
                            )
                        {
                            // Match the native launch environment, including Claude's
                            // distinction between its default and an explicit profile.
                            let mut command = plan.command();
                            command.args(args);
                            if let Ok(result) = process::run(
                                &mut command,
                                Duration::from_millis(config.runtime.harness_probe_timeout_ms),
                                config.runtime.harness_probe_limit_bytes as usize,
                            ) {
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
    #[cfg(unix)]
    #[test]
    fn local_probes_obey_saved_budgets_and_native_profile_environment() {
        use std::{fs, os::unix::fs::PermissionsExt};
        struct Root(std::path::PathBuf);
        impl Drop for Root {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let mut random = [0; 16];
        crate::platform::random_bytes(&mut random).unwrap();
        let root = Root(std::env::temp_dir().join(format!(
            "daedalus-discovery-{:x}",
            u128::from_le_bytes(random)
        )));
        fs::create_dir(&root.0).unwrap();
        let bin = root.0.join("bin");
        fs::create_dir(&bin).unwrap();
        for name in [".codex", ".claude"] {
            fs::create_dir(root.0.join(name)).unwrap();
        }
        let environment = BTreeMap::from([
            ("HOME".into(), root.0.to_str().unwrap().into()),
            ("PATH".into(), bin.to_str().unwrap().into()),
            (
                "TELEGRAM_BOT_TOKEN".into(),
                "fixture-secret-must-not-enter-probe".into(),
            ),
        ]);
        let mut config = ControlConfig::fresh(&root.0).unwrap();
        let write = |name: &str, script: &str| {
            let path = bin.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        };
        let codex = |config: &ControlConfig| {
            detect_with(config, &environment)
                .into_iter()
                .find(|d| d.id == "codex")
                .unwrap()
        };
        write(
            "codex",
            "#!/bin/sh\n[ -z \"${TELEGRAM_BOT_TOKEN-}\" ] || exit 9\n[ \"$1 $2\" = 'login status' ] || exit 8\n/bin/sleep 0.25\nprintf 'Logged in using ChatGPT (private@example.invalid)\\n'\n",
        );
        config.runtime.harness_probe_timeout_ms = 100;
        assert_eq!(codex(&config).status, "Profile found · sign-in unverified");
        config.runtime.harness_probe_timeout_ms = 2000;
        assert_eq!(codex(&config).status, "Signed in · native status");
        write(
            "codex",
            &format!(
                "#!/bin/sh\nprintf 'Logged in using ChatGPT\\n%s' '{}'\n",
                "x".repeat(1400)
            ),
        );
        config.runtime.harness_probe_limit_bytes = 1024;
        assert_eq!(codex(&config).status, "Profile found · sign-in unverified");
        config.runtime.harness_probe_limit_bytes = 4096;
        assert_eq!(codex(&config).status, "Signed in · native status");
        write(
            "claude",
            "#!/bin/sh\n[ -z \"${TELEGRAM_BOT_TOKEN-}\" ] || exit 9\n[ \"$1 $2 $3\" = 'auth status --json' ] || exit 8\n[ -z \"${CLAUDE_CONFIG_DIR+x}\" ] || exit 7\nprintf '{\"loggedIn\":true}'\n",
        );
        let claude = |config: &ControlConfig| {
            detect_with(config, &environment)
                .into_iter()
                .find(|d| d.id == "claude")
                .unwrap()
        };
        assert_eq!(claude(&config).status, "Signed in · native status");
        let custom = root.0.join("custom");
        fs::create_dir(&custom).unwrap();
        config
            .profiles
            .insert("claude".into(), custom.to_str().unwrap().into());
        write(
            "claude",
            "#!/bin/sh\n[ \"$CLAUDE_CONFIG_DIR\" = \"$HOME/custom\" ] || exit 7\nprintf '{\"loggedIn\":true}'\n",
        );
        assert_eq!(claude(&config).status, "Signed in · native status");
    }
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
