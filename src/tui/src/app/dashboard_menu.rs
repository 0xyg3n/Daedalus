//! Global preference editing through the existing dialog protocol.
use super::{
    backend::Source,
    dashboard::{self, Plan, State},
    session_menu::Outcome,
};
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Copy)]
enum Edit {
    AddRoot,
    RemoveRoot,
    AddInstance,
    RemoveInstance,
}
enum Stage {
    Root(State),
    Field(State, String),
    Path(State, Edit),
    Remove(State, Edit),
    Confirm(Box<Plan>),
}
struct Pending {
    id: u64,
    mode: &'static str,
    options: Vec<String>,
    stage: Stage,
}
#[derive(Default)]
pub struct Menu {
    pending: Option<Pending>,
}
impl Menu {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn cancel(&mut self) {
        self.pending = None;
    }
    fn frame(
        &mut self,
        seq: &mut u64,
        mode: &'static str,
        title: &str,
        extra: Value,
        stage: Stage,
    ) -> Result<Outcome> {
        *seq = seq.checked_add(1).ok_or("Dialog sequence exhausted")?;
        let mut frame = json!({"version":2,"kind":"dialog","id":*seq,"mode":mode,"title":title});
        frame
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let options = frame["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v[0].as_str().map(str::to_owned))
            .collect();
        self.pending = Some(Pending {
            id: *seq,
            mode,
            options,
            stage,
        });
        Ok(Outcome {
            frames: vec![frame],
            ..Default::default()
        })
    }
    pub fn start<S: Source>(&mut self, source: &mut S, seq: &mut u64) -> Result<Outcome> {
        self.cancel();
        match source.dashboard_state() {
            Ok(state) => self.root(state, seq, None),
            Err(_) => {
                let plan = source.prepare_dashboard_recovery()?;
                let description = super::dialog_text::format(&plan.review())
                    .map_err(|_| "Cannot format dashboard recovery review")?;
                self.frame(seq,"choose","Recover dashboard preferences?",json!({"description":description,"options":[["cancel","Cancel","Retain the original preferences"],["yes","Back up and recover","Replace preferences with automatic instance scanning disabled"]]}),Stage::Confirm(Box::new(plan)))
            }
        }
    }
    fn root(&mut self, state: State, seq: &mut u64, error: Option<&str>) -> Result<Outcome> {
        let mut options = dashboard::FIELDS
            .iter()
            .map(|f| {
                Ok(json!([
                    f.key,
                    format!("{} · {}", f.label, state.settings.display(f.key)?),
                    format!("{} · {}", f.group, f.description)
                ]))
            })
            .collect::<Result<Vec<_>>>()?;
        options.extend([
            json!([
                "add-root",
                "Add a search root",
                "Scan an explicit absolute directory"
            ]),
            json!([
                "remove-root",
                "Remove a search root",
                format!("{} configured", state.settings.search_roots.len())
            ]),
            json!([
                "add-instance",
                "Add a saved instance",
                "Add a valid native configuration to the instance selector"
            ]),
            json!([
                "remove-instance",
                "Forget a saved instance",
                format!("{} saved references", state.saved_instances().len())
            ]),
        ]);
        let description = format!(
            "Global preferences: {}. Review each change before saving; it applies on the next discovery refresh.{}",
            state.directory().display(),
            error.map(|v| format!("\n{v}")).unwrap_or_default()
        );
        self.frame(
            seq,
            "choose",
            "Dashboard and instance discovery",
            json!({"options":options,"description":description}),
            Stage::Root(state),
        )
    }
    fn field(
        &mut self,
        state: State,
        key: String,
        seq: &mut u64,
        error: Option<&str>,
    ) -> Result<Outcome> {
        let field = dashboard::FIELDS
            .iter()
            .find(|f| f.key == key)
            .ok_or("Unknown dashboard setting")?;
        let bounds = match field.kind {
            super::interface_settings::Kind::Toggle => "Enter true or false.".into(),
            super::interface_settings::Kind::Number { min, max } => {
                format!("Enter a whole number from {min} to {max}.")
            }
            _ => unreachable!(),
        };
        let current = state.settings.display(&key)?;
        let description = format!(
            "{}\nSaved value: {current}. {bounds}{}",
            field.description,
            error.map(|v| format!("\n{v}")).unwrap_or_default()
        );
        self.frame(
            seq,
            "input",
            field.label,
            json!({"secret":false,"input":current,"description":description}),
            Stage::Field(state, key),
        )
    }
    fn path(
        &mut self,
        state: State,
        edit: Edit,
        seq: &mut u64,
        error: Option<&str>,
    ) -> Result<Outcome> {
        let title = match edit {
            Edit::AddRoot => "New dashboard search root",
            Edit::AddInstance => "Native instance directory",
            _ => unreachable!(),
        };
        self.frame(seq,"input",title,json!({"secret":false,"description":error.unwrap_or("Enter an absolute local directory.")}),Stage::Path(state,edit))
    }
    fn remove(&mut self, state: State, edit: Edit, seq: &mut u64) -> Result<Outcome> {
        let paths = match edit {
            Edit::RemoveRoot => state.settings.search_roots.clone(),
            Edit::RemoveInstance => state.saved_instances(),
            _ => unreachable!(),
        };
        if paths.is_empty() {
            return self.root(
                state,
                seq,
                Some("No directories are saved for this operation"),
            );
        }
        let options = paths
            .iter()
            .map(|p| json!([p, p, "Remove this saved reference"]))
            .collect::<Vec<_>>();
        self.frame(
            seq,
            "choose",
            "Select a saved directory",
            json!({"options":options}),
            Stage::Remove(state, edit),
        )
    }
    fn review(&mut self, state: State, result: Result<Plan>, seq: &mut u64) -> Result<Outcome> {
        let plan = match result {
            Ok(plan) => plan,
            Err(error) => return self.root(state, seq, Some(&error)),
        };
        let description = super::dialog_text::format(&plan.review())
            .map_err(|_| "Cannot format dashboard review")?;
        self.frame(seq,"choose","Save dashboard preferences?",json!({"description":description,"options":[["cancel","Cancel","Keep saved preferences"],["yes","Save","Apply the reviewed change with a private backup"]]}),Stage::Confirm(Box::new(plan)))
    }
    pub fn reply<S: Source>(
        &mut self,
        request: &Value,
        source: &mut S,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let pending = self
            .pending
            .as_ref()
            .ok_or("No dashboard dialog is pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected dashboard reply".into());
        }
        let value = request.get("value").ok_or("Missing dashboard reply")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid dashboard reply")?;
            let valid = match pending.mode {
                "choose" => pending.options.iter().any(|v| v == text),
                "input" => {
                    text.len() <= dashboard::LIMIT as usize && !text.chars().any(char::is_control)
                }
                _ => false,
            };
            if !valid {
                return Err("Invalid dashboard reply".into());
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() || (pending.mode == "choose" && value == "cancel") {
            return Ok(Outcome {
                message: "Dashboard preferences unchanged".into(),
                ..Default::default()
            });
        }
        let text = value.as_str().unwrap();
        match pending.stage {
            Stage::Root(state) => match text {
                "add-root" => self.path(state, Edit::AddRoot, seq, None),
                "add-instance" => self.path(state, Edit::AddInstance, seq, None),
                "remove-root" => self.remove(state, Edit::RemoveRoot, seq),
                "remove-instance" => self.remove(state, Edit::RemoveInstance, seq),
                key => self.field(state, key.into(), seq, None),
            },
            Stage::Field(state, key) => match state.settings.set(&key, text) {
                Ok(updated) => {
                    let plan = Plan::prepare(state.clone(), updated);
                    self.review(state, plan, seq)
                }
                Err(error) => self.field(state, key, seq, Some(&error)),
            },
            Stage::Path(state, edit) => {
                let result = match edit {
                    Edit::AddRoot => state
                        .settings
                        .add_root(text)
                        .and_then(|updated| Plan::prepare(state.clone(), updated)),
                    Edit::AddInstance => Plan::add_instance(state.clone(), text),
                    _ => unreachable!(),
                };
                match result {
                    Ok(plan) => self.review(state, Ok(plan), seq),
                    Err(error) => self.path(state, edit, seq, Some(&error)),
                }
            }
            Stage::Remove(state, edit) => {
                let plan = match edit {
                    Edit::RemoveRoot => state
                        .settings
                        .remove_root(text)
                        .and_then(|updated| Plan::prepare(state.clone(), updated)),
                    Edit::RemoveInstance => Plan::remove_instance(state.clone(), text),
                    _ => unreachable!(),
                };
                self.review(state, plan, seq)
            }
            Stage::Confirm(plan) => {
                let recovery = plan.is_recovery();
                source.apply_dashboard(*plan)?;
                Ok(Outcome {
                    message: if recovery {
                        "Dashboard preferences recovered; automatic instance scanning is disabled"
                    } else {
                        "Dashboard preferences saved"
                    }
                    .into(),
                    refresh: true,
                    ..Default::default()
                })
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::{
        backend::Observations, config::ControlConfig, dashboard::fixtures::Fixture,
    };
    use super::*;
    use crate::core::layouts::Layouts;
    struct Mock {
        directory: std::path::PathBuf,
        saves: usize,
    }
    impl Source for Mock {
        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            unreachable!()
        }
        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            unreachable!()
        }
        fn save_layouts(&mut self, _: &Layouts) -> Result<()> {
            unreachable!()
        }
        fn dashboard_state(&mut self) -> Result<State> {
            State::load(&self.directory)
        }
        fn prepare_dashboard_recovery(&mut self) -> Result<Plan> {
            Plan::recover(&self.directory)
        }
        fn apply_dashboard(&mut self, plan: Plan) -> Result<()> {
            let approval = plan.approval();
            plan.apply(&approval)?;
            self.saves += 1;
            Ok(())
        }
    }
    #[test]
    fn recovery_review_cancels_refuses_stale_bytes_and_cannot_apply_twice() {
        let f = Fixture::new();
        let directory = f.0.join("preferences");
        let path = directory.join("dashboard.json");
        crate::core::layouts::write_private(&path, b"broken-DASHBOARD_PRIVATE_SENTINEL").unwrap();
        let mut source = Mock {
            directory: directory.clone(),
            saves: 0,
        };
        let mut menu = Menu::default();
        let mut seq = 0;
        let review = menu.start(&mut source, &mut seq).unwrap();
        assert_eq!(review.frames[0]["title"], "Recover dashboard preferences?");
        assert_eq!(review.frames[0]["options"][0][0], "cancel");
        assert!(
            !review.frames[0]
                .to_string()
                .contains("DASHBOARD_PRIVATE_SENTINEL")
        );
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"cancel"}),
            &mut source,
            &mut seq,
        )
        .unwrap();
        assert_eq!(source.saves, 0);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"broken-DASHBOARD_PRIVATE_SENTINEL"
        );
        menu.start(&mut source, &mut seq).unwrap();
        crate::core::layouts::write_private(&path, b"changed malformed file").unwrap();
        let request = json!({"op":"reply","id":seq,"value":"yes"});
        assert!(menu.reply(&request, &mut source, &mut seq).is_err());
        assert!(menu.reply(&request, &mut source, &mut seq).is_err());
        assert_eq!(source.saves, 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"changed malformed file");
        menu.start(&mut source, &mut seq).unwrap();
        let request = json!({"op":"reply","id":seq,"value":"yes"});
        let saved = menu.reply(&request, &mut source, &mut seq).unwrap();
        assert!(saved.refresh);
        assert!(saved.message.starts_with("Dashboard preferences recovered"));
        assert_eq!(source.saves, 1);
        assert!(menu.reply(&request, &mut source, &mut seq).is_err());
        assert!(State::load(&directory).is_ok());
    }
    #[test]
    fn editor_rejects_wrong_replies_corrects_input_cancels_and_applies_once() {
        let f = Fixture::new();
        let mut source = Mock {
            directory: f.0.join("preferences"),
            saves: 0,
        };
        let mut menu = Menu::default();
        let mut seq = 0;
        let mut out = menu.start(&mut source, &mut seq).unwrap();
        let id = out.frames[0]["id"].as_u64().unwrap();
        assert!(
            menu.reply(
                &json!({"op":"reply","id":id+1,"value":"instance_limit"}),
                &mut source,
                &mut seq
            )
            .is_err()
        );
        assert!(
            menu.reply(
                &json!({"op":"reply","id":id,"value":"yes"}),
                &mut source,
                &mut seq
            )
            .is_err()
        );
        out = menu
            .reply(
                &json!({"op":"reply","id":id,"value":"instance_limit"}),
                &mut source,
                &mut seq,
            )
            .unwrap();
        assert_eq!(out.frames[0]["input"], "128");
        out = menu
            .reply(
                &json!({"op":"reply","id":seq,"value":"0"}),
                &mut source,
                &mut seq,
            )
            .unwrap();
        assert!(
            out.frames[0]["description"]
                .as_str()
                .unwrap()
                .contains("Enter a number")
        );
        out = menu
            .reply(
                &json!({"op":"reply","id":seq,"value":"4"}),
                &mut source,
                &mut seq,
            )
            .unwrap();
        assert_eq!(out.frames[0]["options"][0][0], "cancel");
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"cancel"}),
            &mut source,
            &mut seq,
        )
        .unwrap();
        assert!(!source.directory.join("dashboard.json").exists());
        assert_eq!(source.saves, 0);
        menu.start(&mut source, &mut seq).unwrap();
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"scan_processes"}),
            &mut source,
            &mut seq,
        )
        .unwrap();
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"false"}),
            &mut source,
            &mut seq,
        )
        .unwrap();
        let request = json!({"op":"reply","id":seq,"value":"yes"});
        let saved = menu.reply(&request, &mut source, &mut seq).unwrap();
        assert!(saved.refresh);
        assert_eq!(source.saves, 1);
        assert!(!menu.pending());
        assert!(menu.reply(&request, &mut source, &mut seq).is_err());
        assert!(
            !State::load(&source.directory)
                .unwrap()
                .settings
                .scan_processes
        );
    }
}
