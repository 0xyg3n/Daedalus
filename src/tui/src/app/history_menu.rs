//! History settings and explicit archive review in the existing dialog UI.
use super::{
    backend::Source,
    history::{self, Plan, State},
    session_menu::Outcome,
};
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, String>;
enum Stage {
    Root(bool, State),
    Field(bool, State, String),
    Confirm(Box<Plan>),
    List(bool, usize, bool, Value, Option<String>),
    ReturnList(bool, usize, bool, Option<String>),
    Archives(bool, usize, Value),
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::{
        backend::Observations, config::ControlConfig, dashboard::fixtures::Fixture,
    };
    use super::*;
    struct Mock {
        root: std::path::PathBuf,
        writes: usize,
    }
    impl Source for Mock {
        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            unreachable!()
        }
        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            unreachable!()
        }
        fn save_layouts(&mut self, _: &crate::core::layouts::Layouts) -> Result<()> {
            unreachable!()
        }
        fn history_access(&mut self, _: bool) -> Result<State> {
            State::inspect(&self.root)
        }
        fn history_page(&mut self, _: bool, offset: usize, unresolved: bool) -> Result<Value> {
            super::super::control_audit::list(&self.root, offset, unresolved)
        }
        fn apply_history(&mut self, plan: Plan) -> Result<()> {
            let approval = plan.approval();
            plan.apply(&approval)?;
            self.writes += 1;
            Ok(())
        }
    }
    #[test]
    fn readonly_review_preserves_unsaved_recovery_values_and_cancel_discards_them() {
        let f = Fixture::new();
        crate::core::layouts::write_private(&f.0.join("control-history.json"), b"bad policy")
            .unwrap();
        let mut source = Mock {
            root: f.0.clone(),
            writes: 0,
        };
        let (mut menu, mut seq) = (Menu::default(), 0);
        menu.start(&mut source, &mut seq).unwrap();
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"entry_limit"}),
            &mut source,
            &mut seq,
        )
        .unwrap();
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"2048"}),
            &mut source,
            &mut seq,
        )
        .unwrap();
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"unresolved"}),
            &mut source,
            &mut seq,
        )
        .unwrap();
        let root = menu
            .reply(
                &json!({"op":"reply","id":seq,"value":"back"}),
                &mut source,
                &mut seq,
            )
            .unwrap();
        assert!(
            root.frames[0]["options"][0][1]
                .as_str()
                .unwrap()
                .contains("2048")
        );
        assert_eq!(source.writes, 0);
        assert_eq!(
            std::fs::read(f.0.join("control-history.json")).unwrap(),
            b"bad policy"
        );
        menu.cancel();
        let root = menu.start(&mut source, &mut seq).unwrap();
        assert!(
            root.frames[0]["options"][0][1]
                .as_str()
                .unwrap()
                .contains("1000")
        );
    }
    #[test]
    fn settings_review_rejects_wrong_replies_stale_bytes_and_apply_replay() {
        let f = Fixture::new();
        let mut source = Mock {
            root: f.0.clone(),
            writes: 0,
        };
        let (mut menu, mut seq) = (Menu::default(), 0);
        menu.start(&mut source, &mut seq).unwrap();
        let id = seq;
        assert!(
            menu.reply(
                &json!({"op":"reply","id":id+1,"value":"entry_limit"}),
                &mut source,
                &mut seq
            )
            .is_err()
        );
        assert!(
            menu.reply(
                &json!({"op":"reply","id":id,"value":"unrecognized"}),
                &mut source,
                &mut seq
            )
            .is_err()
        );
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"entry_limit"}),
            &mut source,
            &mut seq,
        )
        .unwrap();
        let review = menu
            .reply(
                &json!({"op":"reply","id":seq,"value":"8"}),
                &mut source,
                &mut seq,
            )
            .unwrap();
        assert_eq!(review.frames[0]["options"][0][0], "cancel");
        assert_eq!(source.writes, 0);
        let state = State::load(&source.root).unwrap();
        let updated = state.settings.set("display_limit", "2").unwrap();
        let plan = Plan::edit(state, updated).unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        let request = json!({"op":"reply","id":seq,"value":"yes"});
        assert!(menu.reply(&request, &mut source, &mut seq).is_err());
        assert!(menu.reply(&request, &mut source, &mut seq).is_err());
        assert_eq!(source.writes, 0);
        menu.start(&mut source, &mut seq).unwrap();
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"entry_limit"}),
            &mut source,
            &mut seq,
        )
        .unwrap();
        menu.reply(
            &json!({"op":"reply","id":seq,"value":"8"}),
            &mut source,
            &mut seq,
        )
        .unwrap();
        let request = json!({"op":"reply","id":seq,"value":"yes"});
        menu.reply(&request, &mut source, &mut seq).unwrap();
        assert_eq!(source.writes, 1);
        assert!(menu.reply(&request, &mut source, &mut seq).is_err());
    }
}
struct Pending {
    id: u64,
    mode: &'static str,
    choices: Vec<String>,
    stage: Stage,
}
#[derive(Default)]
pub struct Menu {
    pending: Option<Pending>,
    draft: Option<(bool, State)>,
}
impl Menu {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn cancel(&mut self) {
        self.pending = None;
        self.draft = None;
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
        let choices = frame["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v[0].as_str().map(str::to_owned))
            .collect();
        self.pending = Some(Pending {
            id: *seq,
            mode,
            choices,
            stage,
        });
        Ok(Outcome {
            frames: vec![frame],
            ..Default::default()
        })
    }
    pub fn start<S: Source>(&mut self, source: &mut S, seq: &mut u64) -> Result<Outcome> {
        self.cancel();
        self.root(source, false, seq, None)
    }
    fn root<S: Source>(
        &mut self,
        source: &mut S,
        global: bool,
        seq: &mut u64,
        error: Option<&str>,
    ) -> Result<Outcome> {
        let mut state = source.history_access(global)?;
        let mut notice = error;
        if let Some((scope, draft)) = &self.draft
            && *scope == global
        {
            if state.same_policy(draft) {
                state.settings = draft.settings.clone();
                state.recovery_required = true;
            } else {
                notice = Some(
                    "Policy changed while reviewing; the unsaved recovery draft was discarded.",
                );
            }
        }
        self.root_state(global, state, seq, notice)
    }
    fn root_state(
        &mut self,
        global: bool,
        state: State,
        seq: &mut u64,
        error: Option<&str>,
    ) -> Result<Outcome> {
        self.draft = state.recovery_required.then(|| (global, state.clone()));
        let mut options = history::FIELDS
            .iter()
            .map(|f| {
                Ok(json!([
                    f.key,
                    format!("{} · {}", f.label, state.settings.display(f.key)?),
                    f.description
                ]))
            })
            .collect::<Result<Vec<_>>>()?;
        if state.recovery_required {
            options.push(json!([
                "recover",
                "Review policy recovery",
                "Back up the current policy and save the recovery draft explicitly"
            ]));
        }
        options.extend([
            json!([
                "recent",
                "Recent receipts",
                "Browse pages without changing saved history"
            ]),
            json!([
                "unresolved",
                "Unresolved outcomes",
                "Attempted, accepted, submitted and unconfirmed remain protected"
            ]),
            json!([
                "archive",
                "Archive active receipts",
                "Review an exact private archive before starting a new history"
            ]),
            json!([
                "archives",
                "Saved receipt archives",
                "Review uncertain outcomes retained in private archives"
            ]),
            json!([
                "scope",
                if global {
                    "Current instance history"
                } else {
                    "Global dashboard history"
                },
                "Select which private directory to manage"
            ]),
        ]);
        let description = format!(
            "{}: {}. Settings and archives require a separate review.{}{}{}",
            if global {
                "Global dashboard history"
            } else {
                "Current instance history"
            },
            state.directory().display(),
            if !state.recovery_required {
                ""
            } else {
                "\nPolicy needs recovery. These values are an unsaved recovery draft. Normal effect writes still require a valid policy and sufficient protected capacity."
            },
            if state.history_valid {
                ""
            } else {
                "\nActive receipts are damaged. Archive their exact bytes before changing policy; their outcomes remain unknown."
            },
            error.map(|e| format!("\n{e}")).unwrap_or_default()
        );
        self.frame(
            seq,
            "choose",
            "Control action history",
            json!({"description":description,"options":options}),
            Stage::Root(global, state),
        )
    }
    fn field(
        &mut self,
        global: bool,
        state: State,
        key: String,
        seq: &mut u64,
        error: Option<&str>,
    ) -> Result<Outcome> {
        let field = history::FIELDS
            .iter()
            .find(|f| f.key == key)
            .ok_or("Unknown history setting")?;
        let input = state.settings.display(&key)?;
        let description = format!(
            "{}\n{}: {input}. Ctrl+U clears the field.{}",
            field.description,
            if !state.recovery_required {
                "Saved value"
            } else {
                "Recovery draft value"
            },
            error.map(|e| format!("\n{e}")).unwrap_or_default()
        );
        self.frame(
            seq,
            "input",
            field.label,
            json!({"description":description,"input":input,"secret":false}),
            Stage::Field(global, state, key),
        )
    }
    fn review(&mut self, plan: Plan, seq: &mut u64) -> Result<Outcome> {
        let description = super::dialog_text::format(&plan.review())
            .map_err(|_| "Cannot format history review")?;
        self.frame(seq,"choose",if plan.is_archive(){"Archive control receipts?"}else if plan.is_recovery(){"Recover history policy?"}else{"Save history settings?"},json!({"description":description,"options":[["cancel","Cancel","Retain the current history and settings"],["yes","Apply reviewed change","Save exactly this review with a private retained original"]]}),Stage::Confirm(Box::new(plan)))
    }
    fn list<S: Source>(
        &mut self,
        source: &mut S,
        global: bool,
        offset: usize,
        unresolved: bool,
        seq: &mut u64,
        archive: Option<String>,
    ) -> Result<Outcome> {
        let page = if let Some(name) = &archive {
            source.history_archive_page(global, name, offset, unresolved)?
        } else {
            source.history_page(global, offset, unresolved)?
        };
        let mut options = page["events"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, e)| {
                json!([
                    i.to_string(),
                    format!(
                        "{} · {}",
                        e["state"].as_str().unwrap_or("unknown"),
                        e["action"].as_str().unwrap_or("unknown")
                    ),
                    format!(
                        "{} · {}",
                        e["operation"].as_str().unwrap_or("unknown"),
                        e["time"]
                    )
                ])
            })
            .collect::<Vec<_>>();
        if page["next_offset"].as_u64().is_some() {
            options.push(json!([
                "next",
                "Next page",
                "Continue through retained receipts"
            ]));
        }
        if offset > 0 {
            options.push(json!([
                "previous",
                "First page",
                "Return to the newest receipts"
            ]));
        }
        options.push(json!([
            "back",
            "Back to history settings",
            "Reading does not prune or resolve receipts"
        ]));
        let description = format!(
            "{} protected operations; {} retained entries. Page offset {offset}. Select a receipt to inspect its operation and outcome.",
            page["protected_operations"], page["entries"]
        );
        self.frame(
            seq,
            "choose",
            if archive.is_some() {
                "Archived control receipts"
            } else if unresolved {
                "Unresolved control outcomes"
            } else {
                "Recent control receipts"
            },
            json!({"description":description,"options":options}),
            Stage::List(global, offset, unresolved, page, archive),
        )
    }
    fn archives<S: Source>(
        &mut self,
        source: &mut S,
        global: bool,
        offset: usize,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let page = source.history_archives(global, offset)?;
        let mut options = page["archives"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, name)| json!([i.to_string(), name, "Verify and review this saved archive"]))
            .collect::<Vec<_>>();
        if page["next_offset"].as_u64().is_some() {
            options.push(json!([
                "next",
                "Next archive page",
                "Continue the bounded catalog"
            ]));
        }
        if offset > 0 {
            options.push(json!([
                "first",
                "First archive page",
                "Return to the first listed archive"
            ]));
        }
        options.push(json!([
            "back",
            "Back to history settings",
            "No archive is changed or deleted"
        ]));
        let description = format!(
            "{} directory entries scanned; limit {}. {}{}",
            page["scanned_entries"],
            page["scan_limit"],
            if page["partial"] == true {
                "Partial archive listing; raise the scan limit to inspect more entries. "
            } else {
                ""
            },
            if page["policy_valid"] == false {
                "Policy unavailable; safe viewing defaults apply."
            } else {
                "Select an archive to inspect its receipts without changing them."
            }
        );
        self.frame(
            seq,
            "choose",
            "Saved receipt archives",
            json!({"options":options,"description":description}),
            Stage::Archives(global, offset, page),
        )
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
            .ok_or("No history dialog is pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected history reply".into());
        }
        let value = request.get("value").ok_or("Missing history reply")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid history reply")?;
            let valid = match pending.mode {
                "choose" => pending.choices.iter().any(|v| v == text),
                "input" => text.len() <= 64 && !text.chars().any(char::is_control),
                "output" => text == "close",
                _ => false,
            };
            if !valid {
                return Err("Invalid history reply".into());
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() || value == "cancel" {
            self.draft = None;
            return Ok(Outcome {
                message: "Control history unchanged".into(),
                ..Default::default()
            });
        }
        let text = value.as_str().unwrap();
        match pending.stage {
            Stage::Root(global, state) => match text {
                "scope" => self.root(source, !global, seq, None),
                "recent" | "unresolved" => {
                    self.list(source, global, 0, text == "unresolved", seq, None)
                }
                "archives" => self.archives(source, global, 0, seq),
                "recover" => match Plan::recover_policy(state.clone(), state.settings.clone()) {
                    Ok(plan) => self.review(plan, seq),
                    Err(error) => self.root_state(global, state, seq, Some(&error)),
                },
                "archive" => match Plan::archive(state) {
                    Ok(plan) => self.review(plan, seq),
                    Err(error) => self.root(source, global, seq, Some(&error)),
                },
                key => self.field(global, state, key.into(), seq, None),
            },
            Stage::Field(global, mut state, key) => match state.settings.set(&key, text) {
                Ok(updated) if state.recovery_required => {
                    state.settings = updated;
                    self.root_state(global, state, seq, None)
                }
                Ok(updated) => match Plan::edit(state.clone(), updated) {
                    Ok(plan) => self.review(plan, seq),
                    Err(error) => self.field(global, state, key, seq, Some(&error)),
                },
                Err(error) => self.field(global, state, key, seq, Some(&error)),
            },
            Stage::Confirm(plan) => {
                let archived = plan.is_archive();
                let recovered = plan.is_recovery();
                source.apply_history(*plan)?;
                self.draft = None;
                Ok(Outcome {
                    message: if archived {
                        "Control history archived; unresolved outcomes retained"
                    } else if recovered {
                        "History policy recovered"
                    } else {
                        "History settings saved"
                    }
                    .into(),
                    refresh: true,
                    ..Default::default()
                })
            }
            Stage::List(global, offset, unresolved, page, archive) => {
                match text {
                    "back" => self.root(source, global, seq, None),
                    "previous" => self.list(source, global, 0, unresolved, seq, archive),
                    "next" => self.list(
                        source,
                        global,
                        page["next_offset"].as_u64().ok_or("History page changed")? as usize,
                        unresolved,
                        seq,
                        archive,
                    ),
                    index => {
                        let entry = page["events"]
                            .get(
                                index
                                    .parse::<usize>()
                                    .map_err(|_| "Invalid receipt selection")?,
                            )
                            .ok_or("Receipt disappeared")?;
                        let text = super::dialog_text::format(&json!({"source":page["source"],"archive":page["archive"],"receipt":entry})).map_err(|_|"Cannot format receipt")?;
                        self.frame(
                            seq,
                            "output",
                            "Control receipt",
                            json!({"text":text}),
                            Stage::ReturnList(global, offset, unresolved, archive),
                        )
                    }
                }
            }
            Stage::ReturnList(global, offset, unresolved, archive) => {
                self.list(source, global, offset, unresolved, seq, archive)
            }
            Stage::Archives(global, _offset, page) => match text {
                "back" => self.root(source, global, seq, None),
                "first" => self.archives(source, global, 0, seq),
                "next" => self.archives(
                    source,
                    global,
                    page["next_offset"]
                        .as_u64()
                        .ok_or("Archive catalog changed")? as usize,
                    seq,
                ),
                index => {
                    let name = page["archives"]
                        .get(
                            index
                                .parse::<usize>()
                                .map_err(|_| "Invalid archive selection")?,
                        )
                        .and_then(Value::as_str)
                        .ok_or("Archive disappeared")?
                        .to_string();
                    self.list(source, global, 0, false, seq, Some(name))
                }
            },
        }
    }
}
