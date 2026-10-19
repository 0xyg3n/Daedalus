//! Existing TUI protocol dialogs around the reviewed native setup transaction.
use super::{
    backend::Source,
    config::{ControlConfig, HARNESSES},
    dependencies::Selection,
    session_menu::Outcome,
    wizard::Wizard,
};
use serde_json::{Value, json};
use std::path::PathBuf;
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug)]
pub enum Mode {
    Current,
    Fresh { workspace: PathBuf },
    Settings { source: PathBuf },
    Instance { source: PathBuf, credentials: bool },
}
#[derive(Clone, Debug)]
pub struct Spec {
    pub destination: Option<PathBuf>,
    pub mode: Mode,
    pub selection: Selection,
}
impl Spec {
    fn current(config: &ControlConfig) -> Self {
        Self {
            destination: None,
            mode: Mode::Current,
            selection: Selection {
                tmux: false,
                herdr: false,
                harnesses: config
                    .subscriptions
                    .iter()
                    .filter(|h| HARNESSES.contains(&h.as_str()))
                    .cloned()
                    .collect(),
            },
        }
    }
}
enum Stage {
    Root,
    Local,
    FreshDirectory,
    Workspace(PathBuf),
    ImportSource(bool),
    ImportDestination { source: PathBuf, whole: bool },
    Credentials(Spec),
    Tools(Spec),
    Backends(Spec),
    Download(Box<Wizard>),
    Review(Box<Wizard>),
    Apply(Box<Wizard>),
    Output,
    CutoverOffer(PathBuf),
    CutoverDirectory,
    CutoverReview(Value),
    CutoverApply(Value),
    CutoverResult(Value),
    CutoverRollback(Value),
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
    harness: super::harness_menu::Menu,
    telegram: super::telegram_menu::Menu,
}
pub fn available(id: &str) -> bool {
    matches!(id, "setup" | "tools.choose")
}
fn path(text: &str) -> Result<PathBuf> {
    let p = PathBuf::from(text);
    crate::core::layouts::check_path(&p)
        .map_err(|_| "Use an absolute path without symlinks or parent traversal")?;
    Ok(p)
}
impl Menu {
    pub fn pending(&self) -> bool {
        self.pending.is_some() || self.harness.pending() || self.telegram.pending()
    }
    pub fn cancel(&mut self) {
        self.telegram.cancel();
        self.harness.cancel();
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
            .filter_map(|r| r[0].as_str().map(str::to_owned))
            .collect();
        self.pending = Some(Pending {
            id: *seq,
            mode,
            choices,
            stage,
        });
        Ok(Outcome {
            frames: vec![frame],
            ..Outcome::default()
        })
    }
    fn choose(
        &mut self,
        seq: &mut u64,
        title: &str,
        options: Value,
        description: &str,
        stage: Stage,
    ) -> Result<Outcome> {
        self.frame(
            seq,
            "choose",
            title,
            json!({"options":options,"description":description}),
            stage,
        )
    }
    fn input(&mut self, seq: &mut u64, title: &str, stage: Stage) -> Result<Outcome> {
        self.frame(seq, "input", title, json!({"secret":false}), stage)
    }
    fn tools(&mut self, seq: &mut u64, spec: Spec) -> Result<Outcome> {
        let mut options = HARNESSES
            .into_iter()
            .map(|h| {
                json!([
                    h,
                    h,
                    if spec.selection.harnesses.iter().any(|s| s == h) {
                        "Selected · choose to remove"
                    } else {
                        "Choose to select"
                    }
                ])
            })
            .collect::<Vec<_>>();
        options.push(json!([
            "done",
            "Continue",
            "Review backends and missing dependencies next."
        ]));
        self.choose(
            seq,
            "Choose your AI tools",
            json!(options),
            "Select the tools or subscriptions you already use. Sign-in stays with each harness.",
            Stage::Tools(spec),
        )
    }
    fn backends(&mut self, seq: &mut u64, spec: Spec) -> Result<Outcome> {
        let windows = cfg!(windows);
        let description = format!(
            "Detected: {} / {}. Native terminals are built in. Existing external backends are discovered automatically. Choose only what you need; no backend installation is required to save your configuration. {}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            if windows {
                "Missing Windows backends need their official native installer; Linux installers will not run."
            } else {
                "Missing tools are installed only after you review and approve the plan."
            }
        );
        self.choose(
            seq,
            "Terminal backends",
            json!([
                [
                    "existing",
                    "Daedalus native terminals",
                    "Built in; no tmux or Herdr installation. Existing external backends are also discovered."
                ],
                [
                    "herdr",
                    "Herdr only",
                    if windows {
                        "Use installed Herdr; show native installation guidance if missing."
                    } else {
                        "Install Herdr only if missing."
                    }
                ],
                [
                    "tmux",
                    if windows {
                        "PSMux / tmux-compatible backend"
                    } else {
                        "tmux only"
                    },
                    if windows {
                        "Use the configured native Windows backend; show guidance if missing."
                    } else {
                        "Install tmux only if missing."
                    }
                ],
                [
                    "both",
                    "Both backends",
                    "Explicitly include both in the reviewed plan."
                ]
            ]),
            &description,
            Stage::Backends(spec),
        )
    }
    fn review(&mut self, seq: &mut u64, wizard: Box<Wizard>) -> Result<Outcome> {
        let text = super::dialog_text::format(wizard.review())
            .map_err(|_| "Cannot encode setup review")?;
        self.frame(
            seq,
            "output",
            "Setup review",
            json!({"text":text}),
            Stage::Review(wizard),
        )
    }
    pub fn start(&mut self, id: &str, config: &ControlConfig, seq: &mut u64) -> Result<Outcome> {
        if self.pending() {
            return Err("Finish the current setup dialog first".into());
        }
        match id {
            "setup" => self.choose(
                seq,
                "Set up Daedalus",
                json!([
                    [
                        "local",
                        "Use my existing setup",
                        "Import private configuration or create a fresh instance."
                    ],
                    [
                        "tools.choose",
                        "1. Choose your AI tools",
                        "Select the tools or subscriptions you already use."
                    ],
                    [
                        "tools.signin",
                        "2. Sign in to an AI tool",
                        "Open the selected tool’s native sign-in in its account directory."
                    ],
                    [
                        "telegram",
                        "3. Connect Telegram (optional)",
                        "Follow the guided settings, bot token, service and destination steps."
                    ],
                    [
                        "done",
                        "Finish and explore terminals",
                        "Save local configuration; make this directory private (0700). No installs or sign-in."
                    ]
                ]),
                "Each step is optional. Existing agents keep running.",
                Stage::Root,
            ),
            "tools.choose" => self.tools(seq, Spec::current(config)),
            _ => Err("Unknown setup action".into()),
        }
    }
    pub fn reply<S: Source>(
        &mut self,
        request: &Value,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        if self.telegram.pending() {
            return self.telegram.answer(source, config, request, seq);
        }
        if self.harness.pending() {
            return self.harness.reply(request, source, config, seq);
        }
        let pending = self.pending.as_ref().ok_or("No setup dialog is pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected setup reply".into());
        }
        let value = request.get("value").ok_or("Missing setup response")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid setup response")?;
            let valid = match pending.mode {
                "choose" => pending.choices.iter().any(|c| c == text),
                "input" => {
                    !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control)
                }
                "output" => text == "close",
                _ => false,
            };
            if !valid {
                return Err("Invalid setup response".into());
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() {
            return Ok(Outcome {
                refresh: matches!(pending.stage, Stage::Output),
                message: "Setup cancelled; no further changes applied".into(),
                ..Outcome::default()
            });
        }
        let text = value.as_str().unwrap();
        match pending.stage {
            Stage::Root => match text {
                "local" => self.choose(
                    seq,
                    "Use my existing setup",
                    json!([
                        [
                            "current",
                            "Use this configuration",
                            "Review this instance's selected tools."
                        ],
                        [
                            "settings",
                            "Import settings",
                            "Portable configuration; no runtime state."
                        ],
                        [
                            "instance",
                            "Import an instance",
                            "Preview private routes, checkpoints and compatible state."
                        ],
                        ["cutover","Finish staged import","Review final state transfer, source retirement and rollback."],
                        [
                            "fresh",
                            "Set up from scratch",
                            "Create a new private configuration directory."
                        ]
                    ]),
                    "Import source files remain unchanged.",
                    Stage::Local,
                ),
                "done" => {
                    let configuration = source.finish_local_setup(config)?;
                    Ok(Outcome {
                        configuration: Some(configuration),
                        refresh: true,
                        message: "Local setup saved. Terminal control is ready.".into(),
                        ..Outcome::default()
                    })
                },
                "telegram" => self.telegram.start("telegram.details", source, config, seq),
                "tools.signin" => self.harness.start(text, seq),
                "tools.choose" => self.start(text, config, seq),
                _ => Err("This step is unsupported by the setup menu".into()),
            },
            Stage::Local => match text {
                "current" => self.tools(seq, Spec::current(config)),
                "cutover" => self.input(seq,"Staged destination configuration directory",Stage::CutoverDirectory),
                "fresh" => self.input(
                    seq,
                    "New private configuration directory",
                    Stage::FreshDirectory,
                ),
                _ => self.input(
                    seq,
                    if text == "instance" {
                        "Existing instance directory"
                    } else {
                        "Existing control.json path"
                    },
                    Stage::ImportSource(text == "instance"),
                ),
            },
            Stage::FreshDirectory => self.input(
                seq,
                "Project workspace directory",
                Stage::Workspace(path(text)?),
            ),
            Stage::Workspace(destination) => {
                let mut spec = Spec::current(config);
                spec.destination = Some(destination);
                spec.mode = Mode::Fresh {
                    workspace: path(text)?,
                };
                spec.selection.harnesses.clear();
                self.tools(seq, spec)
            }
            Stage::ImportSource(whole) => self.input(
                seq,
                "Destination private configuration directory",
                Stage::ImportDestination {
                    source: path(text)?,
                    whole,
                },
            ),
            Stage::ImportDestination {
                source: import,
                whole,
            } => {
                let mut spec = Spec::current(config);
                spec.destination = Some(path(text)?);
                spec.mode = if whole {
                    Mode::Instance {
                        source: import,
                        credentials: false,
                    }
                } else {
                    Mode::Settings { source: import }
                };
                if whole {
                    self.choose(
                        seq,
                        "Import credentials?",
                        json!([
                            [
                                "no",
                                "Leave credentials out",
                                "Import portable settings and runtime references."
                            ],
                            [
                                "yes",
                                "Include private token files",
                                "Copies existing credentials to the reviewed destination."
                            ]
                        ]),
                        "Credentials remain outside the repository.",
                        Stage::Credentials(spec),
                    )
                } else {
                    self.backends(seq, spec)
                }
            }
            Stage::Credentials(mut spec) => {
                if let Mode::Instance { credentials, .. } = &mut spec.mode {
                    *credentials = text == "yes";
                }
                self.backends(seq, spec)
            }
            Stage::Tools(mut spec) => {
                if text == "done" {
                    self.backends(seq, spec)
                } else {
                    if spec.selection.harnesses.iter().any(|h| h == text) {
                        spec.selection.harnesses.retain(|h| h != text);
                    } else {
                        spec.selection.harnesses.push(text.into());
                    }
                    self.tools(seq, spec)
                }
            }
            Stage::Backends(mut spec) => {
                spec.selection.tmux = matches!(text, "tmux" | "both");
                spec.selection.herdr = matches!(text, "herdr" | "both");
                let wizard = Box::new(source.prepare_setup(config, spec)?);
                let urls = wizard
                    .review()
                    .dependencies
                    .operations
                    .iter()
                    .filter_map(|o| o.installer_url.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                if urls.is_empty() {
                    self.review(seq, wizard)
                } else {
                    self.choose(seq,"Download installers for review?",json!([["cancel","Cancel","Download and installation are skipped."],["yes","Download","Fetch installer bytes and calculate their hashes; do not execute them."]]),&urls,Stage::Download(wizard))
                }
            }
            Stage::Download(mut wizard) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                source.download_setup(&mut wizard)?;
                self.review(seq, wizard)
            }
            Stage::Review(wizard) => {
                if !wizard.review().dependencies.blockers.is_empty() {
                    return Err("Setup has unresolved prerequisites; no installers executed".into());
                }
                self.choose(
                    seq,
                    "Confirm action",
                    json!([
                        [
                            "cancel",
                            "Cancel",
                            "No installers or configuration changes."
                        ],
                        [
                            "yes",
                            "Continue",
                            "Apply the reviewed configuration and dependency installation plan."
                        ]
                    ]),
                    "Existing agents keep running. No account sign-in or feeder is started.",
                    Stage::Apply(wizard),
                )
            }
            Stage::Apply(wizard) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                let destination = wizard.review().configuration.destination.parent()
                    .ok_or("Setup destination has no directory")?.to_path_buf();
                let token = wizard.approval_token().to_owned();
                let (result, updated) = source.apply_setup(*wizard, &token)?;
                let summary = if result["instance_import"]["requires_cutover"] == true {
                    "Import staged. Review the final transfer before switching instances."
                } else if result["ready"] == true {
                    "Setup complete. Your configuration is saved."
                } else {
                    "Setup incomplete. Review the details below before retrying."
                };
                let text = format!("{summary}\n\n{}", super::dialog_text::format(&result)
                    .map_err(|_| "Cannot encode setup result")?);
                let mut outcome = self.frame(
                    seq,
                    "output",
                    "Setup result",
                    json!({"text":text}),
                    if result["instance_import"]["requires_cutover"]==true {
                        Stage::CutoverOffer(path(result["instance_import"]["destination"].as_str().ok_or("Import destination missing")?)?)
                    } else { Stage::Output },
                )?;
                outcome.configuration = updated;
                // Full instance imports remain staged until their explicit cutover.
                if result["ready"] == true && result["instance_import"].is_null() {
                    outcome.setup_directory = Some(destination);
                }
                Ok(outcome)
            }
            Stage::CutoverDirectory => {
                let destination=path(text)?;
                let review=source.cutover(&destination,None)?;
                self.frame(seq,"output","Review native cutover",json!({"text":super::dialog_text::format(&review).map_err(|_|"Cannot format cutover")?}),Stage::CutoverReview(review))
            }
            Stage::CutoverOffer(destination)=>{
                let review=source.cutover(&destination,None)?;
                self.frame(seq,"output","Review native cutover",json!({"text":super::dialog_text::format(&review).map_err(|_|"Cannot format cutover")?}),Stage::CutoverReview(review))
            }
            Stage::CutoverReview(review)=>self.choose(seq,"Confirm native cutover",json!([["cancel","Keep staged import","Leave the source instance active"],["yes","Commit cutover","Recheck source and destination; retire source without starting services"]]),"This does not start the Telegram service. Rollback remains available until activation.",Stage::CutoverApply(review)),
            Stage::CutoverApply(review)=>{
                if text=="cancel" {return Ok(Outcome::default());}
                let destination=path(review["destination"].as_str().ok_or("Missing cutover destination")?)?;
                let approval=review["approval"].as_str().ok_or("Missing cutover approval")?;
                let result=source.cutover(&destination,Some(approval))?;
                self.choose(seq,"Cutover committed",json!([["done","Done","Start the destination service when ready"],["rollback","Rollback cutover","Restore source ownership only if destination has not activated"]]),"The destination is ready for explicit activation; no service was started.",Stage::CutoverResult(result))
            }
            Stage::CutoverResult(result)=>{
                if text=="done" {return Ok(Outcome{refresh:true,..Default::default()});}
                self.choose(seq,"Confirm cutover rollback",json!([["cancel","Cancel","Keep the committed cutover"],["yes","Restore source","Refuse if destination has activated"]]),"Restore the previous source ownership using the committed approval.",Stage::CutoverRollback(result))
            }
            Stage::CutoverRollback(result)=>{
                if text=="cancel" {return Ok(Outcome::default());}
                let source_path=path(result["source"].as_str().ok_or("Missing source path")?)?;
                let approval=result["approval"].as_str().ok_or("Missing rollback approval")?;
                source.rollback_cutover(&source_path,approval)?;
                Ok(Outcome{message:"Cutover rolled back; source ownership restored".into(),refresh:true,..Default::default()})
            }
            Stage::Output => Ok(Outcome {
                refresh: true,
                ..Outcome::default()
            }),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::{
        backend::Observations,
        dependencies::{Environment, Fetcher},
    };
    use super::*;
    use crate::core::layouts::Layouts;
    use std::{
        collections::BTreeMap,
        os::unix::fs::PermissionsExt,
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Mock {
        home: PathBuf,
        specs: Vec<Spec>,
        downloads: usize,
        applies: usize,
        missing_herdr: bool,
        cutovers: Vec<String>,
        applied_result: Value,
    }
    impl Mock {
        fn new() -> Self {
            let home = std::env::temp_dir().join(format!(
                "daedalus-wizard-menu-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&home).unwrap();
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                home,
                specs: vec![],
                downloads: 0,
                applies: 0,
                missing_herdr: false,
                cutovers: vec![],
                applied_result: json!({"ready":true}),
            }
        }
    }
    impl Drop for Mock {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
    impl Source for Mock {
        fn cutover(&mut self, destination: &Path, approval: Option<&str>) -> Result<Value> {
            if let Some(approval) = approval {
                assert_eq!(approval, "reviewed");
                self.cutovers.push("apply".into());
            } else {
                self.cutovers.push("preview".into());
            }
            Ok(
                json!({"source":self.home.join("source"),"destination":destination,"approval":"reviewed"}),
            )
        }
        fn rollback_cutover(&mut self, source: &Path, approval: &str) -> Result<Value> {
            assert_eq!(source, self.home.join("source"));
            assert_eq!(approval, "reviewed");
            self.cutovers.push("rollback".into());
            Ok(json!({"state":"rolled_back"}))
        }
        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            Ok(Observations::default())
        }
        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            Err("No terminal probes".into())
        }
        fn save_layouts(&mut self, _: &Layouts) -> Result<()> {
            Err("No layout mutation".into())
        }
        fn prepare_setup(&mut self, _: &ControlConfig, spec: Spec) -> Result<Wizard> {
            self.specs.push(spec.clone());
            let mut binaries: BTreeMap<String, PathBuf> = [
                "tmux", "herdr", "claude", "codex", "grok", "hermes", "cursor", "pi", "opencode",
                "node", "npm", "sh", "bash", "curl", "git", "tar", "gzip", "unzip",
            ]
            .into_iter()
            .map(|n| (n.into(), crate::test_support::path("/usr/bin/true").into()))
            .collect();
            if self.missing_herdr {
                binaries.remove("herdr");
            }
            let env = Environment {
                os: "linux".into(),
                architecture: "x86_64".into(),
                home: self.home.clone(),
                search: vec![crate::test_support::path("/usr/bin").into()],
                binaries,
                root: false,
            };
            Wizard::fresh(
                &self.home.join("new-config"),
                Path::new(crate::test_support::path("/tmp/workspace")),
                env,
                spec.selection,
            )
        }
        fn download_setup(&mut self, wizard: &mut Wizard) -> Result<()> {
            struct Fake;
            impl Fetcher for Fake {
                fn fetch(&mut self, _: &str) -> Result<Vec<u8>> {
                    Ok(b"#!/bin/sh\nexit 0\n".to_vec())
                }
            }
            self.downloads += 1;
            wizard.prepare_installers(&mut Fake)
        }
        fn apply_setup(
            &mut self,
            wizard: Wizard,
            token: &str,
        ) -> Result<(Value, Option<ControlConfig>)> {
            assert_eq!(wizard.approval_token(), token);
            self.applies += 1;
            Ok((self.applied_result.clone(), None))
        }
    }
    fn config() -> ControlConfig {
        ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/workspace"))).unwrap()
    }
    fn reply(menu: &mut Menu, source: &mut Mock, seq: &mut u64, value: Value) -> Result<Outcome> {
        menu.reply(
            &json!({"op":"reply","id":*seq,"value":value}),
            source,
            &config(),
            seq,
        )
    }
    #[test]
    fn choose_tools_reviews_exact_plan_and_confirm_is_single_use() {
        let (mut menu, mut source, mut seq) = (Menu::default(), Mock::new(), 0);
        menu.start("tools.choose", &config(), &mut seq).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("codex")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("done")).unwrap();
        let review = reply(&mut menu, &mut source, &mut seq, json!("tmux")).unwrap();
        assert_eq!(review.frames[0]["title"], "Setup review");
        assert_eq!(source.specs[0].selection.harnesses, vec!["codex"]);
        assert!(source.specs[0].selection.tmux);
        assert!(!source.specs[0].selection.herdr);
        assert_eq!(source.applies, 0);
        reply(&mut menu, &mut source, &mut seq, json!("close")).unwrap();
        let confirmation = seq;
        assert!(
            menu.reply(
                &json!({"op":"reply","id":seq+1,"value":"yes"}),
                &mut source,
                &config(),
                &mut seq
            )
            .is_err()
        );
        let applied = reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert_eq!(
            applied.setup_directory,
            Some(source.home.join("new-config"))
        );
        assert_eq!(source.applies, 1);
        assert!(
            menu.reply(
                &json!({"op":"reply","id":confirmation,"value":"yes"}),
                &mut source,
                &config(),
                &mut seq
            )
            .is_err()
        );
        assert_eq!(source.applies, 1);
        assert_eq!(std::fs::read_dir(&source.home).unwrap().count(), 0);
    }
    #[test]
    fn failed_setup_and_staged_import_do_not_select_an_instance() {
        for result in [
            json!({"ready":false}),
            json!({"ready":true,"instance_import":{"requires_cutover":true,"destination":crate::test_support::path("/tmp/staged")}}),
        ] {
            let (mut menu, mut source, mut seq) = (Menu::default(), Mock::new(), 0);
            source.applied_result = result;
            menu.start("tools.choose", &config(), &mut seq).unwrap();
            reply(&mut menu, &mut source, &mut seq, json!("done")).unwrap();
            reply(&mut menu, &mut source, &mut seq, json!("tmux")).unwrap();
            reply(&mut menu, &mut source, &mut seq, json!("close")).unwrap();
            let applied = reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
            assert!(applied.setup_directory.is_none());
        }
    }

    #[test]
    fn default_backend_choice_saves_configuration_without_installing_backends() {
        let (mut menu, mut source, mut seq) = (Menu::default(), Mock::new(), 0);
        source.missing_herdr = true;
        menu.start("tools.choose", &config(), &mut seq).unwrap();
        let backends = reply(&mut menu, &mut source, &mut seq, json!("done")).unwrap();
        assert_eq!(backends.frames[0]["options"][0][0], "existing");
        assert!(
            backends.frames[0]["description"]
                .as_str()
                .unwrap()
                .contains(std::env::consts::OS)
        );
        let review = reply(&mut menu, &mut source, &mut seq, json!("existing")).unwrap();
        assert_eq!(review.frames[0]["title"], "Setup review");
        assert_eq!(source.downloads, 0);
        assert!(!source.specs[0].selection.tmux && !source.specs[0].selection.herdr);
        reply(&mut menu, &mut source, &mut seq, json!("close")).unwrap();
        let result = reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert!(
            result.frames[0]["text"]
                .as_str()
                .unwrap()
                .starts_with("Setup complete.")
        );
        assert_eq!(source.applies, 1);
    }
    #[test]
    fn downloads_and_installations_have_separate_cancellable_reviews() {
        let (mut menu, mut source, mut seq) = (Menu::default(), Mock::new(), 0);
        source.missing_herdr = true;
        menu.start("tools.choose", &config(), &mut seq).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("done")).unwrap();
        let download = reply(&mut menu, &mut source, &mut seq, json!("both")).unwrap();
        assert_eq!(
            download.frames[0]["title"],
            "Download installers for review?"
        );
        assert_eq!(source.downloads, 0);
        reply(&mut menu, &mut source, &mut seq, json!("cancel")).unwrap();
        assert_eq!(source.downloads, 0);
        menu.start("tools.choose", &config(), &mut seq).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("done")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("both")).unwrap();
        let reviewed = reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert_eq!(source.downloads, 1);
        assert!(
            reviewed.frames[0]["text"]
                .as_str()
                .unwrap()
                .contains("Script SHA256")
        );
        assert!(
            reviewed.frames[0]["text"]
                .as_str()
                .unwrap()
                .contains("306c6ca7407560340797866e077e053627ad409277d1b9da58106fce4cf717cb")
        );
        reply(&mut menu, &mut source, &mut seq, json!("close")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("cancel")).unwrap();
        assert_eq!(source.applies, 0);
        assert_eq!(std::fs::read_dir(&source.home).unwrap().count(), 0);
    }
    #[test]
    fn fresh_and_instance_import_are_distinct_and_credentials_are_explicit() {
        for whole in [false, true] {
            let (mut menu, mut source, mut seq) = (Menu::default(), Mock::new(), 0);
            menu.start("setup", &config(), &mut seq).unwrap();
            reply(&mut menu, &mut source, &mut seq, json!("local")).unwrap();
            if whole {
                for value in [
                    "instance",
                    crate::test_support::path("/tmp/source-private"),
                    crate::test_support::path("/tmp/new-private"),
                    "no",
                    "tmux",
                ] {
                    reply(&mut menu, &mut source, &mut seq, json!(value)).unwrap();
                }
                assert!(
                    matches!(&source.specs[0].mode,Mode::Instance{source,credentials:false} if source==Path::new(crate::test_support::path("/tmp/source-private")))
                );
            } else {
                for value in [
                    "fresh",
                    crate::test_support::path("/tmp/new-private"),
                    crate::test_support::path("/tmp/workspace"),
                    "done",
                    "tmux",
                ] {
                    reply(&mut menu, &mut source, &mut seq, json!(value)).unwrap();
                }
                assert!(
                    matches!(&source.specs[0].mode,Mode::Fresh{workspace} if workspace==Path::new(crate::test_support::path("/tmp/workspace")))
                );
            }
            assert_eq!(
                source.specs[0].destination.as_deref(),
                Some(Path::new(crate::test_support::path("/tmp/new-private")))
            );
            reply(&mut menu, &mut source, &mut seq, Value::Null).unwrap();
            assert_eq!(source.applies, 0);
        }
    }
    #[test]
    fn current_configuration_review_preserves_profiles_and_rejects_stale_apply() {
        use super::super::dependencies::{Executor, Operation};
        struct NoInstall;
        impl Executor for NoInstall {
            fn installed(&mut self, _: &str, _: &Environment) -> bool {
                true
            }
            fn execute(&mut self, _: &Operation, _: &Environment) -> Result<()> {
                panic!("No installation expected")
            }
        }
        let source = Mock::new();
        let directory = source.home.join("config");
        let mut original = config();
        original.profiles.insert(
            "claude".into(),
            crate::test_support::path("/tmp/private-profile").into(),
        );
        original.telegram.owner_id = Some("123".into());
        crate::core::layouts::write_private(
            &directory.join("control.json"),
            &original.encode().unwrap(),
        )
        .unwrap();
        let environment = Environment {
            os: "linux".into(),
            architecture: "x86_64".into(),
            home: source.home.clone(),
            search: vec![crate::test_support::path("/usr/bin").into()],
            binaries: [
                (
                    "tmux".into(),
                    crate::test_support::path("/usr/bin/true").into(),
                ),
                (
                    "codex".into(),
                    crate::test_support::path("/usr/bin/true").into(),
                ),
            ]
            .into_iter()
            .collect(),
            root: false,
        };
        let selection = Selection {
            tmux: true,
            herdr: false,
            harnesses: vec!["codex".into()],
        };
        let cancelled = Wizard::configure(
            &directory,
            original.clone(),
            environment.clone(),
            selection.clone(),
        )
        .unwrap();
        drop(cancelled);
        assert_eq!(
            std::fs::read(directory.join("control.json")).unwrap(),
            original.encode().unwrap()
        );
        let reviewed = Wizard::configure(
            &directory,
            original.clone(),
            environment.clone(),
            selection.clone(),
        )
        .unwrap();
        let token = reviewed.approval_token().to_owned();
        let result = reviewed.apply(&token, &mut NoInstall).unwrap();
        assert!(result.ready);
        let installed =
            ControlConfig::parse(&std::fs::read(directory.join("control.json")).unwrap()).unwrap();
        assert_eq!(installed.profiles, original.profiles);
        assert_eq!(installed.telegram, original.telegram);
        assert_eq!(installed.subscriptions, vec!["codex"]);
        assert!(result.configuration.unwrap().backup.is_some());
        let reviewed =
            Wizard::configure(&directory, installed.clone(), environment, selection).unwrap();
        let token = reviewed.approval_token().to_owned();
        let mut changed = installed;
        changed.agent_name = "changed-after-review".into();
        crate::core::layouts::write_private(
            &directory.join("control.json"),
            &changed.encode().unwrap(),
        )
        .unwrap();
        let result = reviewed.apply(&token, &mut NoInstall).unwrap();
        assert!(!result.ready);
        assert!(result.configuration_error.is_some());
        assert_eq!(
            std::fs::read(directory.join("control.json")).unwrap(),
            changed.encode().unwrap()
        );
    }
    #[test]
    fn staged_import_cutover_requires_review_and_can_rollback_without_activation() {
        let (mut menu, mut source, mut seq) = (Menu::default(), Mock::new(), 0);
        menu.start("setup", &config(), &mut seq).unwrap();
        fn answer(menu: &mut Menu, source: &mut Mock, seq: &mut u64, value: Value) {
            menu.reply(
                &json!({"op":"reply","id":*seq,"value":value}),
                source,
                &config(),
                seq,
            )
            .unwrap();
        }
        answer(&mut menu, &mut source, &mut seq, json!("local"));
        answer(&mut menu, &mut source, &mut seq, json!("cutover"));
        let destination = source.home.join("destination");
        answer(&mut menu, &mut source, &mut seq, json!(destination));
        assert_eq!(source.cutovers, vec!["preview"]);
        answer(&mut menu, &mut source, &mut seq, json!("close"));
        answer(&mut menu, &mut source, &mut seq, json!("yes"));
        assert_eq!(source.cutovers, vec!["preview", "apply"]);
        answer(&mut menu, &mut source, &mut seq, json!("rollback"));
        answer(&mut menu, &mut source, &mut seq, json!("yes"));
        assert_eq!(source.cutovers, vec!["preview", "apply", "rollback"]);
        assert!(!menu.pending());
        menu.start("setup", &config(), &mut seq).unwrap();
        answer(&mut menu, &mut source, &mut seq, json!("local"));
        answer(&mut menu, &mut source, &mut seq, json!("cutover"));
        answer(&mut menu, &mut source, &mut seq, json!(destination));
        answer(&mut menu, &mut source, &mut seq, json!("close"));
        answer(&mut menu, &mut source, &mut seq, json!("cancel"));
        assert_eq!(
            source.cutovers,
            vec!["preview", "apply", "rollback", "preview"]
        );
        assert!(!menu.pending());
    }
}
