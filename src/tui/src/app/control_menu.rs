//! Structured instance settings in the existing dialog protocol.
use super::{
    backend::Source,
    config::ControlConfig,
    control_settings::{self, Kind},
    session_menu::Outcome,
    setup::SetupPlan,
};
use serde_json::{Value, json};
use std::path::PathBuf;
type Result<T> = std::result::Result<T, String>;
const PAGE: usize = 40;
#[derive(Clone)]
struct Context {
    expected: Box<ControlConfig>,
    destination: PathBuf,
}
#[derive(Clone)]
struct Draft {
    context: Context,
    key: &'static str,
    value: Value,
    offset: usize,
}
#[derive(Clone)]
enum Entry {
    Index(usize),
    Binding(String),
}
enum Stage {
    Root(Context),
    Text(Draft),
    Toggle(Draft),
    List(Draft),
    Entry(Draft, Entry),
    Item(Draft, Option<Entry>),
    BindingKey(Draft),
    BindingPath(Draft, String),
    Confirm(Box<SetupPlan>),
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
}
fn caption(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    };
    let mut out = text.chars().take(100).collect::<String>();
    if out.len() < text.len() {
        out.push('…');
    }
    out
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
        let choices = frame["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row[0].as_str().map(str::to_owned))
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
    fn unchanged(&mut self) -> Outcome {
        self.cancel();
        Outcome {
            message: "Instance settings unchanged".into(),
            ..Default::default()
        }
    }
    pub fn start<S: Source>(
        &mut self,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
        key: Option<&str>,
    ) -> Result<Outcome> {
        let context = Context {
            expected: Box::new(config.clone()),
            destination: source.configuration_file(config)?,
        };
        if let Some(key) = key {
            let key = control_settings::field(key)?.key;
            let value = control_settings::value(config, key)?;
            self.field(
                Draft {
                    context,
                    key,
                    value,
                    offset: 0,
                },
                seq,
                None,
            )
        } else {
            self.root(context, seq)
        }
    }
    fn root(&mut self, context: Context, seq: &mut u64) -> Result<Outcome> {
        let options = control_settings::FIELDS
            .iter()
            .map(|f| {
                Ok(json!([
                    f.key,
                    format!(
                        "{} · {}",
                        f.label,
                        caption(&control_settings::value(&context.expected, f.key)?)
                    ),
                    f.description
                ]))
            })
            .collect::<Result<Vec<_>>>()?;
        self.frame(seq,"choose","Instance settings",json!({"options":options,"description":format!("{}\nChanges require an explicit review. Lists and connections are edited as exact entries; credentials and service grants have separate controls.",context.destination.display())}),Stage::Root(context))
    }
    fn input(
        &mut self,
        seq: &mut u64,
        title: &str,
        description: &str,
        input: &str,
        error: Option<&str>,
        stage: Stage,
    ) -> Result<Outcome> {
        if input.len() > 8192 {
            return Err("Saved value exceeds the interactive input limit; use configuration import for this value".into());
        }
        self.frame(seq,"input",title,json!({"secret":false,"input":input,"description":format!("{description}\nCtrl+U clears the field.{}",error.map(|e|format!("\n{e}")).unwrap_or_default())}),stage)
    }
    fn field(&mut self, draft: Draft, seq: &mut u64, error: Option<&str>) -> Result<Outcome> {
        let field = control_settings::field(draft.key)?;
        match field.kind {
            Kind::Text | Kind::Directory | Kind::Optional => {
                let input = if draft.key == "discovery.native" {
                    draft
                        .value
                        .as_bool()
                        .map_or_else(|| "inherit".into(), |value| value.to_string())
                } else {
                    draft.value.as_str().unwrap_or("").to_owned()
                };
                self.input(
                    seq,
                    field.label,
                    field.description,
                    &input,
                    error,
                    Stage::Text(draft),
                )
            }
            Kind::Toggle => {
                let saved = draft.value.as_bool().ok_or("Invalid saved toggle")?;
                self.frame(seq,"choose",field.label,json!({"description":format!("{}\nSaved value: {saved}",field.description),"options":[[saved.to_string(),format!("{} · saved",if saved{"On"}else{"Off"}),"Keep the saved value"],[(!saved).to_string(),if saved{"Off"}else{"On"},field.description]]}),Stage::Toggle(draft))
            }
            _ => self.list(draft, seq, error),
        }
    }
    fn list(&mut self, mut draft: Draft, seq: &mut u64, error: Option<&str>) -> Result<Outcome> {
        let field = control_settings::field(draft.key)?;
        let mut options = Vec::new();
        let count = if field.kind == Kind::Tools {
            for tool in super::config::HARNESSES.into_iter().chain(["gateway"]) {
                let selected = draft
                    .value
                    .as_array()
                    .ok_or("Invalid tool selection")?
                    .iter()
                    .any(|v| v == tool);
                options.push(json!([
                    format!("tool:{tool}"),
                    format!("{} {tool}", if selected { "[x]" } else { "[ ]" }),
                    "Toggle this saved preference; no installation or sign-in"
                ]));
            }
            draft.value.as_array().unwrap().len()
        } else {
            let entries = if field.kind == Kind::Bindings {
                draft
                    .value
                    .as_object()
                    .ok_or("Invalid metadata connections")?
                    .iter()
                    .map(|(k, v)| (k.clone(), caption(v)))
                    .collect::<Vec<_>>()
            } else {
                draft
                    .value
                    .as_array()
                    .ok_or("Invalid saved list")?
                    .iter()
                    .map(|v| (v.as_str().unwrap_or("").to_owned(), String::new()))
                    .collect()
            };
            let count = entries.len();
            draft.offset = draft.offset.min(count.saturating_sub(1) / PAGE * PAGE);
            for (i, (key, value)) in entries.iter().enumerate().skip(draft.offset).take(PAGE) {
                options.push(json!([
                    format!("item:{i}"),
                    caption(&json!(key)),
                    if value.is_empty() {
                        "Edit or remove this exact entry".to_owned()
                    } else {
                        value.clone()
                    }
                ]));
            }
            options.push(json!([
                "add",
                "Add an entry",
                "Keep spaces and commas exactly as entered"
            ]));
            if draft.offset + PAGE < count {
                options.push(json!([
                    "next",
                    "Next page",
                    "Continue through saved entries"
                ]));
            }
            if draft.offset > 0 {
                options.push(json!([
                    "previous",
                    "Previous page",
                    "Return to earlier entries"
                ]));
            }
            count
        };
        options.extend([
            json!([
                "save",
                "Review saved changes",
                "Show the exact old and new value before saving"
            ]),
            json!([
                "back",
                "Back to instance settings",
                "Discard this unsaved list draft"
            ]),
        ]);
        self.frame(seq,"choose",field.label,json!({"options":options,"description":format!("{}\n{count} {}; edits are an unsaved draft.{}",field.description,if count==1{"entry"}else{"entries"},error.map(|e|format!("\n{e}")).unwrap_or_default())}),Stage::List(draft))
    }
    fn entry_value(draft: &Draft, entry: &Entry) -> Result<String> {
        match entry {
            Entry::Index(i) => draft.value.get(*i),
            Entry::Binding(key) => draft.value.get(key),
        }
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Saved entry disappeared".into())
    }
    fn item(
        &mut self,
        draft: Draft,
        entry: Option<Entry>,
        seq: &mut u64,
        input: Option<&str>,
        error: Option<&str>,
    ) -> Result<Outcome> {
        let field = control_settings::field(draft.key)?;
        let saved = match &entry {
            Some(entry) => Self::entry_value(&draft, entry)?,
            None => String::new(),
        };
        self.input(
            seq,
            field.label,
            "Enter one exact value.",
            input.unwrap_or(&saved),
            error,
            Stage::Item(draft, entry),
        )
    }
    fn review<S: Source>(
        &mut self,
        source: &mut S,
        draft: Draft,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let text = match &draft.value {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            _ => draft.value.to_string(),
        };
        let updated = match control_settings::set(&draft.context.expected, draft.key, &text) {
            Ok(updated) => updated,
            Err(error) => return self.field(draft, seq, Some(&error)),
        };
        if updated == *draft.context.expected {
            return Ok(self.unchanged());
        }
        let plan = source.prepare_configuration(&draft.context.expected, updated)?;
        if plan.preview.destination != draft.context.destination {
            return Err("Instance directory changed; reopen settings".into());
        }
        let before = control_settings::value(&draft.context.expected, draft.key)?;
        let description=super::dialog_text::format(&json!({"file":plan.preview.destination,"setting":draft.key,"before":before,"after":draft.value,"effect":control_settings::field(draft.key)?.description})).map_err(|_|"Cannot format instance settings review")?;
        if description.len() > 32000 {
            return Err("Setting exceeds the interactive review limit; use configuration import to review this value".into());
        }
        self.frame(seq,"choose","Save instance settings?",json!({"description":description,"options":[["cancel","Cancel","Keep the saved configuration"],["yes","Apply reviewed change","Save the frozen configuration with a private backup"]]}),Stage::Confirm(Box::new(plan)))
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
            .ok_or("No instance settings dialog is pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected instance settings reply".into());
        }
        let value = request
            .get("value")
            .ok_or("Missing instance settings reply")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid instance settings reply")?;
            let valid = if pending.mode == "choose" {
                pending.choices.iter().any(|v| v == text)
            } else {
                text.len() <= 8192 && !text.chars().any(char::is_control)
            };
            if !valid {
                return Err("Invalid instance settings reply".into());
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() || (pending.mode == "choose" && value == "cancel") {
            return Ok(self.unchanged());
        }
        let text = value.as_str().unwrap();
        match pending.stage {
            Stage::Root(context) => {
                let key = control_settings::field(text)?.key;
                let value = control_settings::value(&context.expected, key)?;
                self.field(
                    Draft {
                        context,
                        key,
                        value,
                        offset: 0,
                    },
                    seq,
                    None,
                )
            }
            Stage::Text(mut draft) => {
                match control_settings::set(&draft.context.expected, draft.key, text) {
                    Ok(updated) => {
                        draft.value = control_settings::value(&updated, draft.key)?;
                        self.review(source, draft, seq)
                    }
                    Err(error) => self.input(
                        seq,
                        control_settings::field(draft.key)?.label,
                        control_settings::field(draft.key)?.description,
                        text,
                        Some(&error),
                        Stage::Text(draft),
                    ),
                }
            }
            Stage::Toggle(mut draft) => {
                draft.value = json!(text == "true");
                self.review(source, draft, seq)
            }
            Stage::List(mut draft) => match text {
                "back" => self.root(draft.context, seq),
                "next" => {
                    draft.offset = draft.offset.saturating_add(PAGE);
                    self.list(draft, seq, None)
                }
                "previous" => {
                    draft.offset = draft.offset.saturating_sub(PAGE);
                    self.list(draft, seq, None)
                }
                "save" => self.review(source, draft, seq),
                "add" => {
                    if control_settings::field(draft.key)?.kind == Kind::Bindings {
                        self.input(
                            seq,
                            "Herdr session name",
                            "Enter the exact session to bind.",
                            "",
                            None,
                            Stage::BindingKey(draft),
                        )
                    } else {
                        self.item(draft, None, seq, None, None)
                    }
                }
                _ => {
                    if let Some(tool) = text.strip_prefix("tool:") {
                        let values = draft.value.as_array_mut().ok_or("Invalid tool selection")?;
                        if values.iter().any(|v| v == tool) {
                            values.retain(|v| v != tool);
                        } else {
                            values.push(json!(tool));
                        }
                        return self.list(draft, seq, None);
                    }
                    let i = text
                        .strip_prefix("item:")
                        .and_then(|v| v.parse::<usize>().ok())
                        .ok_or("Invalid saved entry selection")?;
                    let entry = if control_settings::field(draft.key)?.kind == Kind::Bindings {
                        Entry::Binding(
                            draft
                                .value
                                .as_object()
                                .ok_or("Invalid connections")?
                                .keys()
                                .nth(i)
                                .ok_or("Connection disappeared")?
                                .clone(),
                        )
                    } else {
                        Entry::Index(i)
                    };
                    let saved = Self::entry_value(&draft, &entry)?;
                    let name = match &entry {
                        Entry::Binding(key) => key.clone(),
                        _ => saved.clone(),
                    };
                    self.frame(seq,"choose","Saved connection entry",json!({"description":format!("{}\n{}",caption(&json!(name)),caption(&json!(saved))),"options":[["edit","Edit value","Change only this exact entry"],["remove","Remove from draft","Keep the change unsaved until review"],["back","Back to draft","Keep this entry"]]}),Stage::Entry(draft,entry))
                }
            },
            Stage::Entry(mut draft, entry) => match text {
                "back" => self.list(draft, seq, None),
                "remove" => {
                    match entry {
                        Entry::Index(i) => {
                            let values = draft.value.as_array_mut().ok_or("Invalid list")?;
                            if i >= values.len() {
                                return Err("Entry disappeared".into());
                            }
                            values.remove(i);
                        }
                        Entry::Binding(key) => {
                            draft
                                .value
                                .as_object_mut()
                                .ok_or("Invalid connections")?
                                .remove(&key)
                                .ok_or("Connection disappeared")?;
                        }
                    }
                    self.list(draft, seq, None)
                }
                _ => match entry {
                    Entry::Binding(key) => {
                        let saved = Self::entry_value(&draft, &Entry::Binding(key.clone()))?;
                        self.input(
                            seq,
                            "Metadata socket path",
                            "Enter the absolute metadata socket path.",
                            &saved,
                            None,
                            Stage::BindingPath(draft, key),
                        )
                    }
                    other => self.item(draft, Some(other), seq, None, None),
                },
            },
            Stage::Item(mut draft, entry) => {
                let one = json!([text]).to_string();
                if let Err(error) = control_settings::set(&draft.context.expected, draft.key, &one)
                {
                    return self.item(draft, entry, seq, Some(text), Some(&error));
                }
                let values = draft.value.as_array_mut().ok_or("Invalid list")?;
                let index = match entry {
                    Some(Entry::Index(i)) => Some(i),
                    _ => None,
                };
                if values
                    .iter()
                    .enumerate()
                    .any(|(i, v)| v == text && Some(i) != index)
                {
                    return self.item(
                        draft,
                        index.map(Entry::Index),
                        seq,
                        Some(text),
                        Some("This exact entry is already configured"),
                    );
                }
                if let Some(i) = index {
                    *values.get_mut(i).ok_or("Entry disappeared")? = json!(text);
                } else {
                    values.push(json!(text));
                    draft.offset = values.len().saturating_sub(1) / PAGE * PAGE;
                }
                self.list(draft, seq, None)
            }
            Stage::BindingKey(draft) => {
                let error = if draft.value.get(text).is_some() {
                    Some("That session already has a connection; edit its saved entry")
                } else if control_settings::set(
                    &draft.context.expected,
                    "discovery.herdr_sessions",
                    &json!([text]).to_string(),
                )
                .is_err()
                {
                    Some("Use a valid exact Herdr session name")
                } else {
                    None
                };
                if let Some(error) = error {
                    return self.input(
                        seq,
                        "Herdr session name",
                        "Enter the exact session to bind.",
                        text,
                        Some(error),
                        Stage::BindingKey(draft),
                    );
                }
                self.input(
                    seq,
                    "Metadata socket path",
                    "Enter the absolute metadata socket path.",
                    "",
                    None,
                    Stage::BindingPath(draft, text.into()),
                )
            }
            Stage::BindingPath(mut draft, key) => {
                let one = serde_json::to_string(&std::collections::BTreeMap::from([(
                    key.clone(),
                    text.to_owned(),
                )]))
                .map_err(|_| "Cannot encode connection")?;
                if let Err(error) = control_settings::set(&draft.context.expected, draft.key, &one)
                {
                    return self.input(
                        seq,
                        "Metadata socket path",
                        "Enter the absolute metadata socket path.",
                        text,
                        Some(&error),
                        Stage::BindingPath(draft, key),
                    );
                }
                draft
                    .value
                    .as_object_mut()
                    .ok_or("Invalid connections")?
                    .insert(key, json!(text));
                self.list(draft, seq, None)
            }
            Stage::Confirm(plan) => {
                let configuration = source.apply_configuration(*plan)?;
                Ok(Outcome {
                    configuration: Some(configuration),
                    message: "Configuration saved privately".into(),
                    refresh: true,
                    ..Default::default()
                })
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        app::{backend::Observations, dashboard::fixtures::Fixture},
        core::layouts::{self, Layouts},
    };
    struct Mock {
        directory: PathBuf,
        writes: usize,
    }
    impl Source for Mock {
        fn configuration_file(&mut self, expected: &ControlConfig) -> Result<PathBuf> {
            let path = self.directory.join("control.json");
            let actual =
                ControlConfig::parse(&std::fs::read(&path).map_err(|_| "Cannot read fixture")?)?;
            if &actual != expected {
                return Err("Configuration changed".into());
            }
            Ok(path)
        }
        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            unreachable!()
        }
        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            unreachable!()
        }
        fn save_layouts(&mut self, _: &Layouts) -> Result<()> {
            unreachable!()
        }
        fn prepare_configuration(
            &mut self,
            expected: &ControlConfig,
            updated: ControlConfig,
        ) -> Result<SetupPlan> {
            SetupPlan::edit_if_current(&self.directory, expected, updated)
        }
        fn apply_configuration(&mut self, plan: SetupPlan) -> Result<ControlConfig> {
            if plan.preview.destination != self.directory.join("control.json") {
                return Err("Configuration directory changed".into());
            }
            let updated = plan.config().clone();
            plan.apply()?;
            self.writes += 1;
            Ok(updated)
        }
    }
    fn reply(menu: &mut Menu, source: &mut Mock, seq: &mut u64, value: Value) -> Result<Outcome> {
        menu.reply(&json!({"op":"reply","id":*seq,"value":value}), source, seq)
    }
    #[test]
    fn exact_paths_paginate_preserve_commas_and_bind_review_to_the_original_directory() {
        let f = Fixture::new();
        let directory = f.instance("first");
        let mut config =
            ControlConfig::parse(&std::fs::read(directory.join("control.json")).unwrap()).unwrap();
        config.tmux_sockets = (0..41)
            .map(|i| f.0.join(format!("socket,{i}")).to_str().unwrap().to_owned())
            .collect();
        layouts::write_private(&directory.join("control.json"), &config.encode().unwrap()).unwrap();
        let original = std::fs::read(directory.join("control.json")).unwrap();
        let (mut menu, mut source, mut seq) = (
            Menu::default(),
            Mock {
                directory: directory.clone(),
                writes: 0,
            },
            0,
        );
        let frame = menu
            .start(&mut source, &config, &mut seq, Some("tmux_sockets"))
            .unwrap();
        assert!(
            frame.frames[0]["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row[0] == "next")
        );
        assert!(
            menu.reply(
                &json!({"op":"reply","id":seq+1,"value":"next"}),
                &mut source,
                &mut seq
            )
            .is_err()
        );
        reply(&mut menu, &mut source, &mut seq, json!("next")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("previous")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("item:0")).unwrap();
        let form = reply(&mut menu, &mut source, &mut seq, json!("edit")).unwrap();
        assert_eq!(form.frames[0]["input"], config.tmux_sockets[0]);
        let updated = f.0.join("new, socket").to_str().unwrap().to_owned();
        reply(&mut menu, &mut source, &mut seq, json!(updated)).unwrap();
        let review = reply(&mut menu, &mut source, &mut seq, json!("save")).unwrap();
        assert_eq!(review.frames[0]["options"][0][0], "cancel");
        assert_eq!(
            std::fs::read(directory.join("control.json")).unwrap(),
            original
        );
        let other = f.instance("other");
        layouts::write_private(&other.join("control.json"), &original).unwrap();
        source.directory = other.clone();
        assert!(reply(&mut menu, &mut source, &mut seq, json!("yes")).is_err());
        assert_eq!(source.writes, 0);
        assert_eq!(std::fs::read(other.join("control.json")).unwrap(), original);
        source.directory = directory.clone();
        menu.start(&mut source, &config, &mut seq, Some("tmux_sockets"))
            .unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("item:0")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("edit")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!(updated)).unwrap();
        source.directory = other;
        assert!(reply(&mut menu, &mut source, &mut seq, json!("save")).is_err());
        assert_eq!(source.writes, 0);
        assert_eq!(
            std::fs::read(directory.join("control.json")).unwrap(),
            original
        );
    }
    #[test]
    fn toggle_tool_and_remove_drafts_apply_only_the_reviewed_field() {
        let f = Fixture::new();
        let directory = f.instance("state");
        let mut config =
            ControlConfig::parse(&std::fs::read(directory.join("control.json")).unwrap()).unwrap();
        let (mut menu, mut source, mut seq) = (
            Menu::default(),
            Mock {
                directory: directory.clone(),
                writes: 0,
            },
            0,
        );
        menu.start(&mut source, &config, &mut seq, Some("subscriptions"))
            .unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("tool:gateway")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("tool:codex")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("save")).unwrap();
        assert_eq!(source.writes, 0);
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        config =
            ControlConfig::parse(&std::fs::read(directory.join("control.json")).unwrap()).unwrap();
        assert_eq!(config.subscriptions, vec!["gateway", "codex"]);
        menu.start(&mut source, &config, &mut seq, Some("discovery.automatic"))
            .unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("false")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        config =
            ControlConfig::parse(&std::fs::read(directory.join("control.json")).unwrap()).unwrap();
        assert!(!config.discovery.automatic);
        assert_eq!(config.subscriptions, vec!["gateway", "codex"]);
        config.tmux_sockets = vec![f.0.join("a, socket").to_str().unwrap().into()];
        config
            .discovery
            .herdr_sockets
            .insert("example".into(), f.0.join("m").to_str().unwrap().into());
        layouts::write_private(&directory.join("control.json"), &config.encode().unwrap()).unwrap();
        for key in ["tmux_sockets", "discovery.herdr_sockets"] {
            let before = std::fs::read(directory.join("control.json")).unwrap();
            menu.start(&mut source, &config, &mut seq, Some(key))
                .unwrap();
            reply(&mut menu, &mut source, &mut seq, json!("item:0")).unwrap();
            reply(&mut menu, &mut source, &mut seq, json!("remove")).unwrap();
            assert_eq!(
                std::fs::read(directory.join("control.json")).unwrap(),
                before
            );
            reply(&mut menu, &mut source, &mut seq, json!("save")).unwrap();
            reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
            config = ControlConfig::parse(&std::fs::read(directory.join("control.json")).unwrap())
                .unwrap();
        }
        assert!(config.tmux_sockets.is_empty());
        assert!(config.discovery.herdr_sockets.is_empty());
        assert_eq!(source.writes, 4);
        menu.start(&mut source, &config, &mut seq, Some("subscriptions"))
            .unwrap();
        let result = reply(&mut menu, &mut source, &mut seq, json!("save")).unwrap();
        assert_eq!(result.message, "Instance settings unchanged");
        assert_eq!(source.writes, 4);
    }
    #[test]
    fn bindings_correct_invalid_input_and_cancel_is_valid_identity_data() {
        let f = Fixture::new();
        let directory = f.instance("first");
        let config =
            ControlConfig::parse(&std::fs::read(directory.join("control.json")).unwrap()).unwrap();
        let (mut menu, mut source, mut seq) = (
            Menu::default(),
            Mock {
                directory: directory.clone(),
                writes: 0,
            },
            0,
        );
        menu.start(
            &mut source,
            &config,
            &mut seq,
            Some("discovery.herdr_sockets"),
        )
        .unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("add")).unwrap();
        let error = reply(&mut menu, &mut source, &mut seq, json!("../invalid")).unwrap();
        assert_eq!(error.frames[0]["input"], "../invalid");
        reply(&mut menu, &mut source, &mut seq, json!("work")).unwrap();
        let error = reply(&mut menu, &mut source, &mut seq, json!("relative")).unwrap();
        assert_eq!(error.frames[0]["input"], "relative");
        let socket = f.0.join("m, s").to_str().unwrap().to_owned();
        reply(&mut menu, &mut source, &mut seq, json!(socket)).unwrap();
        assert_eq!(source.writes, 0);
        reply(&mut menu, &mut source, &mut seq, json!("save")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert_eq!(source.writes, 1);
        let config =
            ControlConfig::parse(&std::fs::read(directory.join("control.json")).unwrap()).unwrap();
        assert_eq!(config.discovery.herdr_sockets["work"], socket);
        menu.start(&mut source, &config, &mut seq, Some("agent_name"))
            .unwrap();
        let review = reply(&mut menu, &mut source, &mut seq, json!("cancel")).unwrap();
        assert_eq!(review.frames[0]["title"], "Save instance settings?");
        reply(&mut menu, &mut source, &mut seq, json!("cancel")).unwrap();
        assert_eq!(source.writes, 1);
        menu.start(&mut source, &config, &mut seq, Some("agent_name"))
            .unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("cancel")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert_eq!(source.writes, 2);
        assert!(reply(&mut menu, &mut source, &mut seq, json!("yes")).is_err());
        assert_eq!(
            ControlConfig::parse(&std::fs::read(directory.join("control.json")).unwrap())
                .unwrap()
                .agent_name,
            "cancel"
        );
    }
}
