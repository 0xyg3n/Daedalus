//! Telegram launch requests retain exact parent evidence across confirmation.
//! Actual spawning is delegated to the shared reviewed hook-launch operation.
use crate::{adapters::terminals, core::identity::TerminalId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub harness: String,
}
/// Parses the arguments after /new. The command dispatcher owns bot suffix and
/// human/Manage authorization; no labels are interpreted as terminal identities.
pub fn parse(arguments: &str) -> Result<Selection> {
    let fields: Vec<_> = arguments.split_whitespace().collect();
    if fields.is_empty() || fields.len() > 2 {
        return Err("Use /new claude|pi|grok [split] on the selected route".into());
    }
    let selection = Selection {
        harness: fields[0].into(),
    };
    if fields.get(1).is_some_and(|value| *value != "split") {
        return Err("Reviewed Telegram launches currently use split placement".into());
    }
    selection.validate()?;
    Ok(selection)
}
impl Selection {
    pub fn validate(&self) -> Result<()> {
        if !["claude", "pi", "grok"].contains(&self.harness.as_str()) {
            return Err(
                "Reviewed Telegram hook launch currently supports Claude, Pi and Grok".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parent {
    pub terminal: TerminalId,
    pub directory: PathBuf,
    pub conversation: Option<String>,
}
impl Parent {
    /// Supply a fresh verified pane; this pure function does not discover or
    /// trust a terminal by name. The caller still verifies it before effects.
    pub fn observe(pane: &Value) -> Result<Self> {
        let terminal = terminals::identity(pane)?;
        let directory = PathBuf::from(pane["cwd"].as_str().ok_or("Parent workspace unavailable")?);
        crate::core::layouts::check_path(&directory)?;
        let conversation = if pane["agent_session"]["kind"] == "id" {
            Some(
                pane["agent_session"]["value"]
                    .as_str()
                    .filter(|value| {
                        !value.is_empty()
                            && value.len() <= 4096
                            && !value.chars().any(char::is_control)
                    })
                    .ok_or("Invalid parent conversation")?
                    .into(),
            )
        } else {
            None
        };
        Ok(Self {
            terminal,
            directory,
            conversation,
        })
    }
    pub fn verify(&self, fresh: &Value) -> Result<()> {
        if &Self::observe(fresh)? != self {
            return Err(
                "Parent terminal, workspace or conversation changed; review launch again".into(),
            );
        }
        Ok(())
    }
}

/// Private callback payload. Never render its serialization into a chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reviewed {
    parent: Parent,
    selection: Selection,
    plan: Value,
    digest: String,
}
impl Reviewed {
    fn decode(&self) -> Result<super::pane_launch::Plan> {
        self.selection.validate()?;
        let plan: super::pane_launch::Plan =
            serde_json::from_value(self.plan.clone()).map_err(|_| "Invalid saved launch plan")?;
        if plan.fingerprint()? != self.digest
            || plan.request.harness != self.selection.harness
            || plan.request.cwd != self.parent.directory
        {
            return Err("Saved launch review changed".into());
        }
        self.parent.verify(&plan.request.parent)?;
        Ok(plan)
    }
    pub fn description(&self) -> Result<String> {
        let plan = self.decode()?;
        // Whitelist review fields. In particular never serialize callback data,
        // configuration, registration or native environment into a Telegram reply.
        Ok(format!(
            "Start {} in a new split pane\nWorkspace: {}\nProfile: {}\nHooks register before the native provider starts. No Telegram recipient or memory bank is added automatically. Submitted does not mean ready.{}{}",
            self.selection.harness,
            self.parent.directory.display(),
            plan.profile.display(),
            if self.selection.harness == "grok" {
                " Grok supports native lifecycle hooks and verified turn-bound goal continuation; feeder readiness remains unavailable."
            } else {
                ""
            },
            plan.hook
                .native_hook_file()
                .map(|path| format!("\nCreate-only native hook file: {}", path.display()))
                .unwrap_or_default()
        ))
    }
    pub fn verify_parent(&self, pane: &Value) -> Result<()> {
        self.decode()?;
        self.parent.verify(pane)
    }
}
pub fn prepare(
    selection: &Selection,
    pane: &Value,
    config: &super::config::ControlConfig,
    directory: &std::path::Path,
    terminals: &terminals::Terminals,
    environment: &std::collections::BTreeMap<String, String>,
    now: u64,
) -> Result<Reviewed> {
    selection.validate()?;
    let parent = Parent::observe(pane)?;
    let plan = super::pane_launch::prepare(
        config,
        directory,
        terminals,
        environment,
        super::pane_launch::Request {
            harness: selection.harness.clone(),
            parent: pane.clone(),
            cwd: parent.directory.clone(),
            model: None,
        },
        now,
    )?;
    parent.verify(&plan.request.parent)?;
    let digest = plan.fingerprint()?;
    Ok(Reviewed {
        parent,
        selection: selection.clone(),
        plan: serde_json::to_value(plan).map_err(|_| "Cannot save launch review")?,
        digest,
    })
}
pub fn apply(
    reviewed: &Reviewed,
    fresh: &Value,
    config: &super::config::ControlConfig,
    terminals: &terminals::Terminals,
    now: u64,
) -> Result<crate::adapters::terminal_actions::Receipt> {
    reviewed.verify_parent(fresh)?;
    super::pane_launch::apply(
        &reviewed.decode()?,
        config,
        terminals,
        &reviewed.digest,
        now,
    )
}

#[cfg(test)]
pub(crate) fn fixture(pane: Value) -> Reviewed {
    use serde_json::json;
    let parent = Parent::observe(&pane).unwrap();
    let executable = json!({"device":1,"inode":2,"size":3,"modified":4,"modified_ns":0,"changed":4,"changed_ns":0});
    let value = json!({
        "request":{"harness":"claude","parent":pane,"cwd":parent.directory,"model":null},
        "configuration":super::config::ControlConfig::fresh(&parent.directory).unwrap(),
        "profile":"/tmp/profile",
        "environment":{"PRIVATE_TEST_VALUE":"DO_NOT_RENDER_PRIVATE_PLAN"},
        "hook":{
            "spec":{
                "registration":{"version":1,"id":"fixture","directory":"/tmp/instance",
                    "scope":{"instance":"example","terminal":parent.terminal,"conversation":"fresh-conversation","harness":"claude"},
                    "goal_id":null,"transcript":null,"events":["SessionStart"],
                    "options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},
                    "activity_lease_ms":60000,"pending_timeout_ms":60000,"hook_timeout_ms":1000},
                "native_binary":"/fixture/claude","daedalus_binary":"/fixture/daedalus","working_directory":parent.directory,
                "observed_version":"2.1.0","expires_ms":100000,"model":null},
            "native":executable,"daemon":executable,"hooks":[]}
    });
    let plan: super::pane_launch::Plan = serde_json::from_value(value).unwrap();
    Reviewed {
        parent,
        selection: Selection {
            harness: "claude".into(),
        },
        digest: plan.fingerprint().unwrap(),
        plan: serde_json::to_value(plan).unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn pane() -> Value {
        json!({"backend":"tmux","socket":crate::test_support::path("/tmp/fixture.sock"),"pane_id":"%7","terminal_id":"generation","cwd":crate::test_support::path("/tmp/project"),"agent_session":{"kind":"id","value":"session"}})
    }
    #[test]
    fn private_plan_is_never_rendered_and_tampering_is_rejected() {
        let mut reviewed = fixture(pane());
        assert!(
            serde_json::to_string(&reviewed)
                .unwrap()
                .contains("DO_NOT_RENDER_PRIVATE_PLAN")
        );
        assert!(
            !reviewed
                .description()
                .unwrap()
                .contains("DO_NOT_RENDER_PRIVATE_PLAN")
        );
        reviewed.plan["environment"]["PRIVATE_TEST_VALUE"] = "changed".into();
        assert!(reviewed.description().is_err());
    }
    #[test]
    fn grok_review_explains_verified_goals_and_unavailable_feeder_readiness() {
        let mut reviewed = fixture(pane());
        reviewed.selection.harness = "grok".into();
        reviewed.plan["request"]["harness"] = "grok".into();
        reviewed.plan["hook"]["spec"]["registration"]["scope"]["harness"] = "grok".into();
        reviewed.plan["hook"]["spec"]["hook_profile"] =
            crate::test_support::path("/tmp/grok-profile").into();
        let plan: super::super::pane_launch::Plan =
            serde_json::from_value(reviewed.plan.clone()).unwrap();
        reviewed.digest = plan.fingerprint().unwrap();
        let description = reviewed.description().unwrap();
        #[cfg(unix)]
        {
            assert!(description.contains("verified turn-bound goal continuation"));
            assert!(description.contains(&format!(
                "Create-only native hook file: {}",
                std::path::Path::new(crate::test_support::path("/tmp/grok-profile"))
                    .join("hooks").join("daedalus-fixture.json")
                    .display()
            )));
            assert!(description.contains("feeder readiness remains unavailable"));
        }
        #[cfg(windows)]
        {
            let failure = super::super::hook_launch::prepare(plan.hook.spec.clone(), 0)
                .expect_err("Windows guided Grok launch must fail closed");
            assert!(failure.contains("Windows"));
        }
        assert!(!description.contains("DO_NOT_RENDER_PRIVATE_PLAN"));
    }
    #[test]
    fn launch_selection_has_no_shell_or_profile_argument_channel() {
        assert_eq!(
            parse("claude").unwrap(),
            Selection {
                harness: "claude".into(),
            }
        );
        assert_eq!(parse("pi split").unwrap().harness, "pi");
        assert_eq!(parse("grok").unwrap().harness, "grok");
        assert!(parse("pi window").is_err());
        for args in [
            "",
            "claude --dangerously-skip-permissions",
            "claude split extra",
            "$(touch file)",
            "shell",
            "codex",
        ] {
            assert!(parse(args).is_err(), "{args}");
        }
        assert!(
            Selection {
                harness: "shell".into(),
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn parent_evidence_rejects_recycled_panes_workspace_and_conversation_changes() {
        let original = pane();
        let parent = Parent::observe(&original).unwrap();
        parent.verify(&original).unwrap();
        for (key, value) in [
            ("terminal_id", "new-generation"),
            ("cwd", crate::test_support::path("/tmp/other")),
            ("socket", crate::test_support::path("/tmp/other.sock")),
        ] {
            let mut changed = original.clone();
            changed[key] = value.into();
            assert!(parent.verify(&changed).is_err(), "{key}");
        }
        let mut changed = original.clone();
        changed["agent_session"]["value"] = "new-session".into();
        assert!(parent.verify(&changed).is_err());
        changed = original;
        changed["cwd"] = "../relative".into();
        assert!(Parent::observe(&changed).is_err());
    }
}
