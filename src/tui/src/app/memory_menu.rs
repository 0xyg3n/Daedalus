//! Offline Hindsight configuration through the existing dialog protocol.
use super::{
    backend::Source,
    hook_runtime::Registration,
    memory::{Config, Plan},
    session_menu::Outcome,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
type Result<T> = std::result::Result<T, String>;

struct Draft {
    expected: Option<Config>,
    config: Config,
    guards: BTreeMap<String, Registration>,
}

enum Stage {
    Root(Draft),
    Field(Draft, String),
    Bindings(Draft),
    Binding(Draft, usize),
    Add(Draft, Vec<Registration>),
    Bank(Draft, Box<Registration>),
    Review(Box<Plan>),
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
        let current = source.memory_configuration()?;
        self.open(current, seq)
    }
    pub fn open(&mut self, current: Option<Config>, seq: &mut u64) -> Result<Outcome> {
        if self.pending() {
            return Err("Finish the current memory dialog first".into());
        }
        let config = current.clone().unwrap_or_else(Config::fresh);
        self.root(
            Draft {
                expected: current,
                config,
                guards: BTreeMap::new(),
            },
            seq,
            None,
        )
    }
    fn root(&mut self, draft: Draft, seq: &mut u64, error: Option<&str>) -> Result<Outcome> {
        let mut options: Vec<_> = super::memory::FIELDS
            .iter()
            .map(|(key, label, help)| {
                let value = draft.config.value(key).expect("listed memory field");
                let display = if *key == "recall_query" {
                    "Edit recall text".into()
                } else {
                    value.to_string()
                };
                json!([key, format!("{label} · {display}"), help])
            })
            .collect();
        options.push(json!([
            "bindings",
            format!("Registration banks · {}", draft.config.bindings.len()),
            "Add, change or remove exact registration bindings"
        ]));
        options.push(json!([
            "review",
            "Review and save",
            "Review the complete draft before confirming a private save"
        ]));
        options.push(json!(["cancel", "Cancel", "Discard this draft"]));
        self.frame(seq,"choose","Hindsight settings",json!({"options":options,
            "description":error.unwrap_or("Changes remain in this draft until you review and save. New configurations start disabled. Saving makes no network request.")}),Stage::Root(draft))
    }
    fn field(
        &mut self,
        draft: Draft,
        key: String,
        seq: &mut u64,
        error: Option<&str>,
    ) -> Result<Outcome> {
        let (_, label, help) = super::memory::FIELDS
            .iter()
            .find(|field| field.0 == key)
            .ok_or("Unknown memory field")?;
        if key == "enabled" {
            return self.frame(seq,"choose",label,json!({"description":help,"options":[
                ["false","Disabled","Keep memory effects off for all registrations"],
                ["true","Enabled","Allow effects only for exact configured bindings and memory-enabled hooks"]]}),Stage::Field(draft,key));
        }
        let value = draft.config.value(&key)?;
        let default = match key.as_str() {
            "credential_file" => value.as_str().unwrap_or("none").into(),
            "recall_query" => value.to_string(),
            _ => value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string()),
        };
        let description = if let Some(error) = error {
            error.to_owned()
        } else if key == "recall_query" {
            "Enter text, or edit the quoted text: use \\n for a line break and \\\" for a quote. Up to 8192 bytes of decoded text.".into()
        } else {
            help.to_string()
        };
        self.frame(
            seq,
            "input",
            label,
            json!({"default":default,"input":default,"secret":false,"description":format!("{description} Ctrl+U clears the field.")}),
            Stage::Field(draft, key),
        )
    }
    fn bindings(&mut self, draft: Draft, seq: &mut u64, error: Option<&str>) -> Result<Outcome> {
        let mut options: Vec<_> = draft
            .config
            .bindings
            .iter()
            .enumerate()
            .map(|(i, b)| {
                json!([
                    format!("binding:{i}"),
                    format!("{} · {}", b.registration_id, b.bank),
                    serde_json::to_string(&b.scope).expect("serializable scope")
                ])
            })
            .collect();
        options.push(json!([
            "add",
            "Add or rebind an installed hook",
            "Choose an exact memory-enabled registration; no bank is inferred"
        ]));
        options.push(json!(["back", "Back", "Keep the draft"]));
        self.frame(seq,"choose","Hindsight registration banks",json!({"options":options,
            "description":error.unwrap_or("Changing a bank preserves every other registration. Inactive bindings can be removed without enabling their hooks.")}),Stage::Bindings(draft))
    }
    fn bank(
        &mut self,
        draft: Draft,
        registration: Box<Registration>,
        seq: &mut u64,
        error: Option<&str>,
    ) -> Result<Outcome> {
        let default = draft
            .config
            .bindings
            .iter()
            .find(|b| b.registration_id == registration.id && b.scope == registration.scope)
            .map_or("", |b| b.bank.as_str());
        self.frame(seq,"input","Bank for the selected registration",json!({"default":default,"input":default,"secret":false,
            "description":error.map(str::to_owned).unwrap_or_else(||format!("{} · {}. Choose 1 to 256 bytes without control characters. Memory enable state is unchanged.",registration.id,serde_json::to_string(&registration.scope).unwrap()))}),Stage::Bank(draft,registration))
    }
    pub fn reply<S: Source>(
        &mut self,
        request: &Value,
        source: &mut S,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let pending = self.pending.as_ref().ok_or("No memory dialog is pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected memory reply".into());
        }
        let value = request.get("value").ok_or("Missing memory reply")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid memory reply")?;
            let valid = match pending.mode {
                "choose" => pending.options.iter().any(|option| option == text),
                "output" => text == "close",
                "input" => text.len() <= 65536 && !text.chars().any(char::is_control),
                _ => false,
            };
            if !valid {
                return Err("Invalid memory reply".into());
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() || (pending.mode == "choose" && value == "cancel") {
            return Ok(Outcome {
                message: "Memory configuration unchanged".into(),
                ..Default::default()
            });
        }
        let text = value.as_str().unwrap();
        match pending.stage {
            Stage::Root(draft)=>match text {
                "bindings"=>self.bindings(draft,seq,None),
                "review"=>{
                    let plan=(||{
                        let mut plan=source.prepare_memory(draft.expected.clone(),draft.config.clone())?;
                        for registration in draft.guards.values() { plan=plan.guard_registration(registration)?; }
                        Ok::<_,String>(plan)
                    })();
                    let plan=match plan {Ok(plan)=>plan,Err(error)=>return self.root(draft,seq,Some(&error))};
                    let text=super::dialog_text::format(&plan.review()).map_err(|_|"Cannot format memory review")?;
                    self.frame(seq,"output","Review Hindsight configuration",json!({"text":text}),Stage::Review(Box::new(plan)))
                }
                key=>self.field(draft,key.into(),seq,None),
            },
            Stage::Field(mut draft,key)=>{
                let text=if text.is_empty() { match key.as_str() {
                    "credential_file"=>draft.config.credential_file.as_ref().map(|p|p.to_string_lossy().into_owned()).unwrap_or_else(||"none".into()),
                    "recall_query"=>serde_json::to_string(&draft.config.recall_query).unwrap(),
                    _=>draft.config.value(&key)?.as_str().map(str::to_owned).unwrap_or_else(||draft.config.value(&key).unwrap().to_string()),
                }} else {text.into()};
                let decoded=if key=="recall_query" && text.starts_with('"') {
                    match serde_json::from_str::<String>(&text) {
                        Ok(value)=>value,Err(_)=>return self.field(draft,key,seq,Some("Invalid quoted query. Use \\n for line breaks and \\\" for quotes.")),
                    }
                } else {text};
                match draft.config.set(&key,&decoded) {
                    Ok(config)=>{draft.config=config;self.root(draft,seq,None)},
                    Err(error)=>self.field(draft,key,seq,Some(&error)),
                }
            }
            Stage::Bindings(draft)=>match text {
                "back"=>self.root(draft,seq,None),
                "add"=>{
                    let registrations=match source.memory_registrations() {
                        Ok(registrations)=>registrations,
                        Err(error)=>return self.bindings(draft,seq,Some(&error)),
                    };
                    if registrations.is_empty() {return self.bindings(draft,seq,Some("Enable Hindsight on an installed hook before adding a binding."));}
                    let options:Vec<_>=registrations.iter().enumerate().map(|(i,r)|json!([i.to_string(),r.id,serde_json::to_string(&r.scope).unwrap()])).collect();
                    self.frame(seq,"choose","Select an exact Hindsight hook",json!({"options":options}),Stage::Add(draft,registrations))
                }
                value=>{
                    let index=value.strip_prefix("binding:").and_then(|s|s.parse::<usize>().ok()).ok_or("Invalid memory binding")?;
                    self.frame(seq,"choose","Edit registration bank",json!({"options":[["bank","Change bank","Requires the same installed registration identity"],["remove","Remove binding","Keep all other registration bindings and settings"]]}),Stage::Binding(draft,index))
                }
            },
            Stage::Binding(mut draft,index)=>{
                let binding=draft.config.bindings.get(index).ok_or("Memory binding changed")?;
                if text=="remove" {
                    draft.guards.remove(&binding.registration_id);
                    draft.config.bindings.remove(index);
                    return self.bindings(draft,seq,None);
                }
                let registrations=match source.memory_registrations() {
                    Ok(registrations)=>registrations,
                    Err(error)=>return self.bindings(draft,seq,Some(&error)),
                };
                let registration=registrations.into_iter().find(|r|r.id==binding.registration_id && r.scope==binding.scope);
                match registration {
                    Some(registration)=>self.bank(draft,Box::new(registration),seq,None),
                    None=>self.bindings(draft,seq,Some("That exact memory-enabled registration is unavailable. Remove the stale binding, or select Add to review a current hook identity.")),
                }
            }
            Stage::Add(draft,registrations)=>{
                let index=text.parse::<usize>().map_err(|_|"Invalid memory registration selection")?;
                let registration=registrations.into_iter().nth(index).ok_or("Memory registration selection changed")?;
                self.bank(draft,Box::new(registration),seq,None)
            }
            Stage::Bank(mut draft,registration)=>{
                let bank=if text.is_empty() {draft.config.bindings.iter().find(|b|b.registration_id==registration.id && b.scope==registration.scope).map_or("",|b|b.bank.as_str())}else{text};
                match draft.config.bind(&registration,bank) {
                    Ok(config)=>{draft.config=config;draft.guards.insert(registration.id.clone(),*registration);self.bindings(draft,seq,None)},
                    Err(error)=>self.bank(draft,registration,seq,Some(&error)),
                }
            }
            Stage::Review(plan)=>self.frame(seq,"choose","Confirm Hindsight configuration",json!({"options":[["cancel","Cancel","Keep the saved configuration unchanged"],["yes","Save","Save exactly the reviewed configuration with a private backup"]]}),Stage::Confirm(plan)),
            Stage::Confirm(plan)=>{
                source.apply_memory(*plan)?;
                Ok(Outcome {refresh:true,message:"Hindsight configuration saved. Network connectivity has not been tested.".into(),..Default::default()})
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        memory::{self, configuration_fixture::Fixture},
        session_menu::{SessionMenu, View},
    };
    use super::*;
    use crate::core::layouts::{self, Layouts};
    struct Driver {
        menu: SessionMenu,
        source: super::super::backend::NativeSource,
        control: super::super::config::ControlConfig,
        seq: u64,
        frame: Value,
    }
    impl Driver {
        fn new(f: &Fixture) -> Self {
            let mut driver = Self {
                menu: SessionMenu::default(),
                source: f.source(),
                control: f.control.clone(),
                seq: 0,
                frame: Value::Null,
            };
            let view = View {
                snapshot: json!({}),
                health: json!({}),
                layouts: Layouts::default(),
                selected: None,
                checkpoint: None,
                node: String::new(),
            };
            let out = driver
                .menu
                .start(
                    "settings.memory",
                    view,
                    &mut driver.source,
                    &driver.control,
                    &mut driver.seq,
                )
                .unwrap();
            driver.frame = out.frames[0].clone();
            driver
        }
        fn answer(&mut self, value: Value) -> Result<Outcome> {
            let result = self.menu.reply(
                &json!({"op":"reply","id":self.frame["id"],"value":value}),
                &mut self.source,
                &self.control,
                &mut self.seq,
            );
            if let Ok(out) = &result
                && let Some(frame) = out.frames.first()
            {
                self.frame = frame.clone();
            }
            result
        }
        fn reply(&mut self, text: &str) -> Outcome {
            self.answer(json!(text)).unwrap()
        }
        fn edit(&mut self, key: &str, value: &str) {
            self.reply(key);
            self.reply(value);
        }
        fn review(&mut self) {
            self.reply("review");
            assert_eq!(self.frame["mode"], "output");
            self.reply("close");
        }
    }
    #[test]
    fn direct_settings_edit_every_scalar_preserve_bindings_and_commit_once() {
        let f = Fixture::new(true);
        let mut d = Driver::new(&f);
        let original = std::fs::read(f.root.join("hindsight.json")).unwrap();
        assert_eq!(
            d.frame["options"].as_array().unwrap().len(),
            memory::FIELDS.len() + 3
        );
        let key = f.root.join("private-key");
        layouts::write_private(&key, b"fixture-raw-credential").unwrap();
        d.edit("endpoint", "http://localhost:8999");
        d.edit("credential_file", key.to_str().unwrap());
        let query = "  Decisions\nwith \\\"quotes\\\" and 界  ";
        d.edit("recall_query", &serde_json::to_string(query).unwrap());
        d.edit("timeout_ms", "2500");
        d.edit("enabled", "true");
        assert_eq!(
            std::fs::read(f.root.join("hindsight.json")).unwrap(),
            original
        );
        let preview = d.reply("review");
        let text = preview.frames[0]["text"].as_str().unwrap();
        assert!(
            text.contains("2500")
                && text.contains("8999")
                && !text.contains("fixture-raw-credential")
        );
        d.reply("close");
        let saved = d.reply("yes");
        assert!(saved.refresh);
        let config = memory::load(&f.root).unwrap().unwrap();
        assert!(config.enabled);
        assert_eq!(config.endpoint, "http://localhost:8999");
        assert_eq!(config.credential_file, Some(key));
        assert_eq!(config.recall_query, query);
        assert_eq!(config.timeout_ms, 2500);
        assert_eq!(config.bindings, f.config.bindings);
        assert!(!d.menu.pending());
        assert!(d.answer(json!("yes")).is_err());
        assert!(std::fs::read_dir(&f.root).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("hindsight.backup-")
        }));
        assert!(!f.root.join("delivery.sqlite3").exists());
    }
    #[test]
    fn new_settings_invalid_fields_and_cancel_keep_the_file_absent() {
        let f = Fixture::new(false);
        let mut d = Driver::new(&f);
        d.reply("review");
        assert_eq!(d.frame["mode"], "choose");
        d.reply("endpoint");
        d.reply("http://external.invalid");
        assert_eq!(d.frame["mode"], "input");
        d.reply("http://127.0.0.1:8999");
        d.reply("timeout_ms");
        d.reply("99");
        assert_eq!(d.frame["mode"], "input");
        d.reply("8000");
        d.reply("credential_file");
        d.reply("paste-a-token-here");
        assert_eq!(d.frame["mode"], "input");
        d.reply("none");
        d.reply("recall_query");
        d.reply(&serde_json::to_string(&"x".repeat(8193)).unwrap());
        assert_eq!(d.frame["mode"], "input");
        d.reply("\"broken quote");
        assert_eq!(d.frame["mode"], "input");
        d.reply("cancel");
        d.review();
        d.reply("cancel");
        assert!(!d.menu.pending());
        assert!(!f.root.join("hindsight.json").exists());
    }
    #[test]
    fn bank_change_and_stale_binding_removal_preserve_disabled_state_and_other_settings() {
        let f = Fixture::new(true);
        let mut d = Driver::new(&f);
        d.reply("bindings");
        d.reply("binding:0");
        d.reply("bank");
        d.reply("cancel");
        d.reply("binding:1");
        d.reply("remove");
        d.reply("back");
        let out = d.reply("review");
        assert!(
            out.frames[0]["text"]
                .as_str()
                .unwrap()
                .contains("keep-this-bank")
        );
        d.reply("close");
        d.reply("yes");
        let config = memory::load(&f.root).unwrap().unwrap();
        assert!(!config.enabled);
        assert_eq!(config.bindings.len(), 1);
        assert_eq!(config.bindings[0].bank, "cancel");
        assert_eq!(config.bindings[0].scope, f.registration.scope);
        assert_eq!(config.endpoint, f.config.endpoint);
        assert_eq!(config.recall_query, f.config.recall_query);
        assert_eq!(config.timeout_ms, f.config.timeout_ms);
    }
    #[test]
    fn add_uses_only_the_selected_registration_and_refuses_a_changed_review() {
        let f = Fixture::new(true);
        let mut d = Driver::new(&f);
        d.reply("bindings");
        d.reply("add");
        assert_eq!(d.frame["options"].as_array().unwrap().len(), 1);
        d.reply("0");
        d.reply("replacement-bank");
        d.reply("back");
        d.review();
        let mut replacement = f.registration.clone();
        replacement.scope.conversation = "replacement-session".into();
        layouts::write_private(
            &replacement.path(),
            &serde_json::to_vec(&replacement).unwrap(),
        )
        .unwrap();
        let error = d.answer(json!("yes")).unwrap_err();
        assert!(error.contains("registration changed"), "{error}");
        assert_eq!(memory::load(&f.root).unwrap().unwrap(), f.config);
    }
    #[test]
    fn external_configuration_change_invalidates_confirmation_without_overwrite() {
        let f = Fixture::new(true);
        let mut d = Driver::new(&f);
        d.edit("timeout_ms", "2000");
        d.review();
        let changed = f.config.set("timeout_ms", "3000").unwrap();
        layouts::write_private(&f.root.join("hindsight.json"), &changed.encode().unwrap()).unwrap();
        assert!(
            d.answer(json!("yes"))
                .unwrap_err()
                .contains("configuration changed")
        );
        assert_eq!(memory::load(&f.root).unwrap().unwrap(), changed);
    }
    #[test]
    fn unavailable_hook_discovery_keeps_scalar_edits_in_the_draft() {
        let f = Fixture::new(true);
        let mut d = Driver::new(&f);
        d.edit("timeout_ms", "2000");
        layouts::write_private(&f.root.join("hooks/invalid.json"), b"{}").unwrap();
        d.reply("bindings");
        d.reply("add");
        assert!(
            d.frame["description"]
                .as_str()
                .unwrap()
                .contains("Invalid memory hook registration")
        );
        d.reply("back");
        d.review();
        d.reply("yes");
        let config = memory::load(&f.root).unwrap().unwrap();
        assert_eq!(config.timeout_ms, 2000);
        assert_eq!(config.bindings, f.config.bindings);
    }
}
