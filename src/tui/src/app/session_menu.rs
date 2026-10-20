//! Stateful version-two dialogs for the existing session controls. Replies are
//! validated before consuming a pending step; confirmed effects are single-use.
use super::{
    backend::Source,
    config::ControlConfig,
    registry,
    session_actions::{AttachMode, CheckpointRow, SaveRequest},
    tree,
    workspace_actions::WorkspaceRequest,
};
use crate::{
    adapters::terminal_actions::{Action, Placement, Receipt},
    core::{
        checkpoints::{Capabilities, Checkpoint, Operation},
        layouts::{Layouts, TerminalRef},
    },
};
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, String>;

pub enum Request {
    HookSwitch {
        expected: Box<super::hook_runtime::Registration>,
        option: String,
    },
    ReleaseGoal {
        id: String,
        scope: crate::core::goal_store::Scope,
        epoch: i64,
    },
    GatewayLoad,
    GatewaySave {
        expected: Option<super::gateway::Config>,
        updated: super::gateway::Config,
    },
    Report(String),
    Harness(super::harness_menu::Run),
    Workspace(Box<WorkspaceRequest>),
    Native(Value),
    Configure {
        expected: Box<ControlConfig>,
        updated: Box<ControlConfig>,
    },
    Save {
        pane: Value,
        request: SaveRequest,
    },
    Load(String),
    Remove(String),
    Live {
        pane: Value,
        sid: String,
    },
    Capabilities(String),
    Launch {
        checkpoint: Box<Checkpoint>,
        key: Option<String>,
        operation: Operation,
        target: Capabilities,
        parent: Value,
        source: Option<Value>,
        placement: Placement,
    },
    Attach {
        pane: Value,
        mode: AttachMode,
        node: String,
    },
    Duplicate {
        source: Value,
        destination: Value,
        read_only: bool,
    },
    Link {
        source: Value,
        destination: Value,
    },
    Unlink(Value),
    Terminal {
        pane: Value,
        action: Action,
    },
}
pub enum Answer {
    Gateway(Option<super::gateway::Config>),
    Configured {
        configuration: ControlConfig,
        message: String,
    },
    Saved(CheckpointRow),
    Live {
        checkpoint: Checkpoint,
        source: Value,
    },
    Capabilities(Capabilities),
    Receipt(Receipt),
    Frame(Value),
    Text(String),
}
#[derive(Clone)]
pub struct View {
    pub snapshot: Value,
    pub health: Value,
    pub layouts: Layouts,
    pub selected: Option<Value>,
    pub checkpoint: Option<String>,
    pub node: String,
}
#[derive(Default, Debug)]
pub struct Outcome {
    pub setup_directory: Option<std::path::PathBuf>,
    pub configuration: Option<ControlConfig>,
    pub frames: Vec<Value>,
    pub refresh: bool,
    pub restore: bool,
    pub layouts: Option<Layouts>,
    pub message: String,
}
struct Pending {
    id: u64,
    mode: &'static str,
    choices: Vec<String>,
    stage: Stage,
}
#[derive(Default)]
pub struct SessionMenu {
    wizard: super::wizard_menu::Menu,
    workflows: super::workflow_menu::Menu,
    harness: super::harness_menu::Menu,
    hooks: super::hook_menu::Menu,
    telegram: super::telegram_menu::Menu,
    pending: Option<Pending>,
    view: Option<View>,
}
struct Launch {
    checkpoint: Checkpoint,
    key: Option<String>,
    operation: Operation,
    target: Capabilities,
    parent: Value,
    source: Option<Value>,
    placement: Placement,
}
enum Commit {
    Workspace(Box<WorkspaceRequest>),
    Native(Value),
    Configure {
        expected: Box<ControlConfig>,
        updated: Box<ControlConfig>,
    },
    Save {
        pane: Value,
        request: SaveRequest,
    },
    Remove(String),
    Launch(Box<Launch>),
    Terminal {
        pane: Value,
        action: Action,
    },
    Duplicate {
        source: Value,
        destination: Value,
        read_only: bool,
    },
    Link {
        source: Value,
        destination: Value,
    },
    Unlink(Value),
    DeleteLayout(String),
}
enum Stage {
    HookTool(String),
    HookSwitch(Box<super::hook_runtime::Registration>, String),
    GoalSelect(Vec<Value>),
    GoalRelease(Value),
    ProjectBackend,
    ProjectServer {
        backend: crate::core::identity::Backend,
        panes: Vec<Value>,
    },
    ProjectName {
        backend: crate::core::identity::Backend,
        anchor: Option<Value>,
    },
    ProjectPath {
        backend: crate::core::identity::Backend,
        anchor: Option<Value>,
        name: String,
    },
    Organize(Value),
    OrganizeLabel {
        pane: Value,
        operation: String,
    },
    WorkspaceTool(Box<WorkspaceRequest>),
    Native(Value),
    SocketInput,
    Output(bool),
    Pick {
        action: String,
        panes: Vec<Value>,
    },
    Terminal(Value),
    LinkedViews(Vec<Value>),
    SaveName(Value),
    SaveKind {
        pane: Value,
        name: String,
    },
    SaveSid {
        pane: Value,
        name: String,
    },
    Confirm(Box<Commit>),
    Checkpoint(CheckpointRow),
    Fork {
        pane: Value,
        checkpoint: Checkpoint,
    },
    ForkSid(Value),
    Target {
        checkpoint: Checkpoint,
        key: Option<String>,
        source: Option<Value>,
    },
    Destination {
        checkpoint: Checkpoint,
        key: Option<String>,
        operation: Operation,
        target: Capabilities,
        source: Option<Value>,
        panes: Vec<Value>,
    },
    Text {
        pane: Value,
        rename: bool,
    },
    Duplicate(Value),
    ViewerDestination {
        source: Value,
        panes: Vec<Value>,
    },
    ViewerAccess {
        source: Value,
        destination: Value,
    },
    LinkDestination {
        source: Value,
        panes: Vec<Value>,
    },
    LayoutName,
    Layout(String),
}
pub fn available(id: &str) -> bool {
    super::telegram_menu::available(id)
        || super::harness_menu::available(id)
        || super::wizard_menu::available(id)
        || matches!(
            id,
            "checkpoint.save"
                | "terminal.attach"
                | "terminal.watch"
                | "terminal.native"
                | "terminal.organize"
                | "project.new"
                | "workflow.new"
                | "terminal.fork"
                | "terminal.duplicate"
                | "terminal.pin"
                | "terminal.view"
                | "terminal.views"
                | "workspace.views"
                | "help.guide"
                | "system.update"
                | "help.reference"
                | "hooks.profile"
                | "hooks.adapter"
                | "hooks.all"
                | "hooks.reload"
                | "activity.events"
                | "activity.discovery"
                | "activity.health"
                | "instances.sources"
                | "armonia.plan"
                | "workspace.socket"
                | "terminal.focus"
                | "terminal.send"
                | "terminal.rename"
                | "terminal.interrupt"
                | "terminal.close"
                | "layout.save"
        )
}
fn field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .into()
}
fn description(pane: &Value) -> String {
    format!(
        "{} · {} / {}",
        tree::title(pane, ""),
        field(pane, "session"),
        field(pane, "pane_id")
    )
}
pub(crate) fn reference(pane: &Value) -> Result<TerminalRef> {
    let id = crate::adapters::terminals::identity(pane)?;
    Ok(TerminalRef {
        backend: id.backend,
        server: pane
            .get("server")
            .and_then(Value::as_str)
            .unwrap_or(&id.endpoint)
            .into(),
        pane_id: id.pane,
        terminal_id: id.generation,
        generation: field(pane, "generation"),
        label: tree::title(pane, ""),
        place: field(pane, "session"),
    })
}
impl SessionMenu {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
            || self.wizard.pending()
            || self.workflows.pending()
            || self.harness.pending()
            || self.hooks.pending()
            || self.telegram.pending()
    }
    pub fn cancel(&mut self) {
        self.telegram.cancel();
        self.hooks.cancel();
        self.harness.cancel();
        self.wizard.cancel();
        self.workflows.cancel();
        self.pending = None;
        self.view = None;
    }
    fn dialog(
        &mut self,
        sequence: &mut u64,
        stage: Stage,
        mode: &'static str,
        title: &str,
        extra: Value,
    ) -> Result<Outcome> {
        *sequence = sequence.checked_add(1).ok_or("Dialog sequence exhausted")?;
        let mut frame =
            json!({"version":2,"kind":"dialog","id":sequence,"mode":mode,"title":title});
        if let Some(values) = extra.as_object() {
            frame.as_object_mut().unwrap().extend(values.clone());
        }
        let choices = frame["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row[0].as_str().map(str::to_owned))
            .collect();
        self.pending = Some(Pending {
            id: *sequence,
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
        stage: Stage,
        title: &str,
        options: Vec<Value>,
        description: &str,
    ) -> Result<Outcome> {
        self.dialog(
            seq,
            stage,
            "choose",
            title,
            json!({"options":options,"description":description}),
        )
    }
    fn output(&mut self, seq: &mut u64, title: &str, text: &str, refresh: bool) -> Result<Outcome> {
        self.dialog(
            seq,
            Stage::Output(refresh),
            "output",
            title,
            json!({"text":text}),
        )
    }
    fn input(&mut self, seq: &mut u64, stage: Stage, title: &str) -> Result<Outcome> {
        self.dialog(seq, stage, "input", title, json!({"secret":false}))
    }
    fn confirm(&mut self, seq: &mut u64, commit: Commit, label: &str) -> Result<Outcome> {
        self.choose(
            seq,
            Stage::Confirm(Box::new(commit)),
            "Confirm action",
            vec![
                json!(["cancel", "Cancel", "Leave everything as it is."]),
                json!(["yes", "Continue", label]),
            ],
            label,
        )
    }
    fn panes(&self) -> Vec<Value> {
        let mut ids = std::collections::HashSet::new();
        self.view
            .as_ref()
            .into_iter()
            .flat_map(|v| v.snapshot["panes"].as_array().into_iter().flatten())
            .filter(|p| crate::adapters::terminals::identity(p).is_ok_and(|id| ids.insert(id)))
            .cloned()
            .collect()
    }
    fn link_destinations(&self, pane: &Value) -> Vec<Value> {
        let mut seen = std::collections::BTreeSet::new();
        self.panes()
            .into_iter()
            .filter(|p| {
                p["backend"] == "tmux"
                    && p.get("socket") == pane.get("socket")
                    && p.get("session_id") != pane.get("session_id")
                    && p.get("window_id") != pane.get("window_id")
                    && seen.insert(field(p, "session_id"))
            })
            .collect()
    }
    fn picker_options(panes: &[Value]) -> Vec<Value> {
        panes
            .iter()
            .enumerate()
            .map(|(i, p)| json!([i.to_string(), tree::title(p, ""), description(p)]))
            .collect()
    }
    pub fn start<S: Source>(
        &mut self,
        action: &str,
        view: View,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        if self.pending() {
            return Err("Finish the current dialog first".into());
        }
        if super::telegram_menu::available(action) {
            if action == "telegram.route"
                && let Some(pane) = view.selected.as_ref()
            {
                return self.telegram.for_pane(pane, source, config, seq);
            }
            return self.telegram.start(action, source, config, seq);
        }
        if action == "hooks.adapter" {
            return self.hooks.start(view, seq);
        }
        if action == "hooks.reload" {
            return self.hooks.reload(source, seq);
        }
        if action == "hooks.all" {
            return self.hooks.start_all(view, seq);
        }
        if action == "workflow.new" || action.starts_with("workflow:") {
            return self.workflows.start(action, view, seq);
        }
        if super::harness_menu::available(action) {
            return self.harness.start(action, seq);
        }
        if super::wizard_menu::available(action) {
            return self.wizard.start(action, config, seq);
        }
        self.view = Some(view);
        if let Some(harness) = action.strip_prefix("hooks:") {
            if harness == "status" {
                return self.output(
                    seq,
                    "Hook status",
                    "No verified hook observation is available. Refresh or inspect diagnostics.",
                    false,
                );
            }
            let row = self.view.as_ref().unwrap().health["hooks"]["tools"]
                .as_array()
                .and_then(|rows| rows.iter().find(|r| r["tool"] == harness))
                .ok_or("Unknown hook harness")?;
            let detail =
                super::dialog_text::format(row).map_err(|_| "Cannot format hook details")?;
            return self.choose(
                seq,
                Stage::HookTool(harness.into()),
                &format!("{harness} hooks"),
                {
                    let mut options = vec![
                        json!([
                            "details",
                            "Details",
                            "Inspect registration and capability evidence"
                        ]),
                        json!([
                            "remove",
                            "Remove managed installation",
                            "Review an unchanged installation receipt; preserve manual edits"
                        ]),
                        json!([
                            "install",
                            "Install or update",
                            "Select a verified live conversation and review native profile changes"
                        ]),
                    ];
                    if ["claude", "codex", "grok"].contains(&harness) {
                        options.push(json!([
                            "herdr",
                            "Herdr integration",
                            "Inspect the configured profile and review Herdr's hook installer"
                        ]));
                    }
                    options
                },
                &detail,
            );
        }
        if let Some(key) = action.strip_prefix("switch:") {
            let (id, option) = key.split_once(':').ok_or("Unknown hook switch")?;
            if ![
                "telegram_delivery",
                "telegram_progress",
                "notifications",
                "hindsight",
            ]
            .contains(&option)
            {
                return Err("Unknown hook switch".into());
            }
            let value = self.view.as_ref().unwrap().health["hooks"]["registrations"]
                .as_array()
                .and_then(|rows| rows.iter().find(|r| r["id"] == id))
                .ok_or("Hook registration disappeared")?;
            let registration: super::hook_runtime::Registration =
                serde_json::from_value(value.clone()).map_err(|_| "Invalid hook registration")?;
            let enabled = value["options"][option]
                .as_bool()
                .ok_or("Unknown hook option")?;
            return self.choose(
                seq,
                Stage::HookSwitch(Box::new(registration), option.into()),
                "Confirm hook switch",
                vec![
                    json!(["cancel", "Cancel", "Keep the current setting"]),
                    json!(["yes", "Change", "Apply only to the reviewed registration"]),
                ],
                &format!(
                    "{} {option} for {id}. Recipient and memory configuration remain explicit.",
                    if enabled { "Disable" } else { "Enable" }
                ),
            );
        }
        if action == "goal:" {
            let goals = self.view.as_ref().unwrap().health["hooks"]["goals"]
                .as_array()
                .ok_or("Goal observation unavailable")?
                .clone();
            if goals.is_empty() {
                return self.output(seq,"Goal loop","No saved goals. Set a goal through an authorized Telegram conversation; release it here after reviewing its identity.",false);
            }
            let options = goals
                .iter()
                .enumerate()
                .map(|(i, g)| json!([i.to_string(), g["name"], g["directive"]]))
                .collect();
            return self.choose(
                seq,
                Stage::GoalSelect(goals),
                "Release a goal",
                options,
                "Releasing lets this exact conversation stop after its current turn.",
            );
        }
        if action.starts_with("armonia:") || action.starts_with("instance:") {
            let Answer::Text(text) = source.session(config, Request::Report(action.into()))? else {
                return Err("Invalid detail response".into());
            };
            return self.output(seq, "Instance details", &text, false);
        }
        if [
            "help.guide",
            "system.update",
            "help.reference",
            "hooks.profile",
            "activity.events",
            "activity.discovery",
            "activity.health",
            "instances.sources",
            "armonia.plan",
        ]
        .contains(&action)
        {
            let title = registry::builtin()
                .action(action)
                .map(|a| a.title.clone())
                .unwrap_or_else(|| action.into());
            let text = match action {
                "help.guide" => "Navigate with Tab or number keys; arrows select, Enter opens an action, Esc cancels. Ctrl+P searches commands. Workspaces groups servers, sessions, windows and exact terminals. Left/Right folds the tree. [ and ] switch Tree, Tiles, Wall, Cube and Focus views.\n\nSelect a terminal: a attaches, w watches, c saves a checkpoint, f forks, d duplicates its view, p pins. L saves the current layout. Ctrl+] then D detaches; the terminal keeps running. q checks for updates before exiting without stopping agents. Options → Application → Check for updates (Ctrl+U) checks on demand.\n\nSetup imports private configuration or starts fresh. Review dependency changes before installation. Account sign-in belongs to each native harness. Checkpoint resume/fork depends on the installed harness capabilities; cross-harness handoff supplies context to a new session. A duplicate view shares the original terminal.\n\nAvailable actions depend on the selected terminal backend and harness. Submitted operations are not proof of completed work. Configuration and credentials are stored privately.".into(),
                "hooks.profile" => {
                    let roster=super::hook_install::capabilities().iter().map(|cap|format!("{} · {}\n{}\n{}",cap.harness,if cap.installable{"native profile installation available"}else{"installation unavailable"},cap.integration,cap.reference)).collect::<Vec<_>>().join("\n\n");
                    format!("Native hook profiles\n\nTelegram delivery publishes final responses. Progress reports tool activity. Notifications report attention and permission events. Optional Hindsight queues memory work. Exact process, conversation and terminal identities are verified for each registration. Hook installation preserves unrelated handlers and requires review; goal continuation is enabled only by adapters with a tested native continuation contract.\n\n{roster}")
                }
                "help.reference" => include_str!("../../../../docs/COMMANDS.md").into(),
                "activity.discovery" => {
                    let view=self.view.as_ref().unwrap();
                    let entries=[&view.snapshot["errors"], &view.health["errors"]].into_iter().flat_map(|v|v.as_array().into_iter().flatten()).collect::<Vec<_>>();
                    if entries.is_empty() { "No discovery warnings in the latest snapshot.".into() } else { super::dialog_text::format(&entries).map_err(|_|"Cannot format discovery warnings")? }
                }
                _ => {
                    let Answer::Text(text)=source.session(config,Request::Report(action.into()))? else {return Err("Invalid report response".into());};
                    text
                }
            };
            return self.output(seq, &title, &text, false);
        }
        if action == "project.new" {
            return self.choose(
                seq,
                Stage::ProjectBackend,
                "New project",
                vec![
                    json!([
                        "native",
                        "Daedalus native terminal",
                        "Built in; start a shell or agent without tmux or Herdr."
                    ]),
                    json!(["tmux", "tmux session", "New project on a tmux server"]),
                    json!([
                        "herdr",
                        "Herdr workspace",
                        "New workspace on a running Herdr server"
                    ]),
                ],
                "",
            );
        }
        if action == "workspace.socket" {
            return self.input(
                seq,
                Stage::SocketInput,
                "Additional tmux socket paths, comma-separated",
            );
        }
        if action == "workspace.views" {
            let view = self.view.as_ref().unwrap();
            let topology = tree::Tree::build(
                &view.snapshot,
                &view.health,
                &tree::ProjectionContext::default(),
            );
            let text = topology
                .rows(&tree::ProjectionContext::default())
                .iter()
                .map(|row| format!("{}{}", field(row, "branch"), field(row, "label")))
                .collect::<Vec<_>>()
                .join("\n");
            return self.output(seq, "Workspace hierarchy", &text, false);
        }
        if let Some(key) = action.strip_prefix("checkpoint:") {
            return self.checkpoint_menu(key, source, config, seq);
        }
        if let Some(name) = action.strip_prefix("layout:") {
            return self.choose(
                seq,
                Stage::Layout(name.into()),
                &format!("Layout “{name}”"),
                vec![
                    json!([
                        "open",
                        "Open",
                        "Restore its view, pinned terminals and focus."
                    ]),
                    json!([
                        "delete",
                        "Delete",
                        "Remove the saved layout; terminals are unaffected."
                    ]),
                ],
                "",
            );
        }
        if action == "layout.save" {
            return self.input(seq, Stage::LayoutName, "Name this layout");
        }
        if let Some(key) = action.strip_prefix("unpin:") {
            let mut layouts = self.view.as_ref().unwrap().layouts.clone();
            let before = layouts.current.pins.len();
            layouts.current.pins.retain(|r| {
                tree::key_id(&[
                    serde_json::to_value(&r.backend).unwrap(),
                    json!(r.server),
                    json!(r.pane_id),
                    json!(r.terminal_id),
                ]) != key
            });
            if before == layouts.current.pins.len() {
                return Err("That pin was already removed".into());
            }
            source.save_layouts(&layouts)?;
            return Ok(Outcome {
                layouts: Some(layouts),
                message: "Pin removed; no terminal was touched".into(),
                ..Outcome::default()
            });
        }
        let view = self.view.as_ref().unwrap();
        if action == view.node {
            if let Some(key) = view.checkpoint.clone() {
                return self.checkpoint_menu(&key, source, config, seq);
            }
            let pane = view.selected.clone().ok_or("Select a terminal")?;
            let actions = registry::builtin().terminal_actions();
            let options: Vec<_> = actions
                .iter()
                .map(|a| {
                    json!([
                        a.id,
                        a.title,
                        if available(&a.id) {
                            a.note.clone()
                        } else {
                            format!("{} · Not available for this operation", a.note)
                        }
                    ])
                })
                .collect();
            let icons: Vec<_> = actions.iter().map(|a| a.icon.clone()).collect();
            return self.dialog(
                seq,
                Stage::Terminal(pane.clone()),
                "choose",
                &tree::title(&pane, ""),
                json!({"options":options,"icons":icons,"description":"Choose what to do with this terminal."}),
            );
        }
        if !available(action) {
            return Err("This action is unsupported by this operation".into());
        }
        if let Some(pane) = view.selected.clone() {
            self.for_pane(action, pane, source, config, seq)
        } else {
            let panes = self.panes();
            if panes.is_empty() {
                return Err("No live terminals available".into());
            }
            let options = Self::picker_options(&panes);
            self.choose(
                seq,
                Stage::Pick {
                    action: action.into(),
                    panes,
                },
                "Choose a terminal",
                options,
                "",
            )
        }
    }
    fn checkpoint_menu<S: Source>(
        &mut self,
        key: &str,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let Answer::Saved(row) = source.session(config, Request::Load(key.into()))? else {
            return Err("Invalid checkpoint response".into());
        };
        let title = row.checkpoint.name.clone();
        self.choose(seq,Stage::Checkpoint(row),&title,vec![json!(["details","View saved ID and source","Show the full session ID, project, profile, and context path."]),json!(["fork","Fork in the same harness","Create a separate native session from the saved history."]),json!(["resume","Resume in the same harness","Reopen the original session; shares its native history."]),json!(["handoff","Continue in another harness","Start a new session using the saved terminal excerpt."]),json!(["remove","Remove saved reference","Native session history and terminals are unaffected."])],"Native session IDs belong to their original harness. Cross-harness handoffs use context.")
    }
    fn workspace_tool(&mut self, seq: &mut u64, request: WorkspaceRequest) -> Result<Outcome> {
        let mut options = vec![json!([
            "shell",
            "Shell only",
            "A plain terminal; start any tool in it later."
        ])];
        options.extend(crate::adapters::harnesses::tools().map(|tool| {
            json!([
                tool.id,
                tool.name,
                "Requires an installed native executable; verified before any terminal is created."
            ])
        }));
        self.choose(
            seq,
            Stage::WorkspaceTool(Box::new(request)),
            "Start a shell or AI tool",
            options,
            "Shell opens in your project directory. AI tools use their configured account directory; sign in through the tool if needed.",
        )
    }
    fn workspace_confirm(&mut self, seq: &mut u64, request: WorkspaceRequest) -> Result<Outcome> {
        let description = request.description();
        self.confirm(seq, Commit::Workspace(Box::new(request)), &description)
    }
    fn for_pane<S: Source>(
        &mut self,
        action: &str,
        pane: Value,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        match action {
            "telegram.route"=>self.telegram.for_pane(&pane,source,config,seq),
            "checkpoint.save" => self.input(seq, Stage::SaveName(pane), "Name this checkpoint"),
            "terminal.attach" | "terminal.watch" => {
                let mode = if action == "terminal.attach" {
                    AttachMode::Control
                } else {
                    AttachMode::Observe
                };
                let Answer::Frame(plan) = source.session(
                    config,
                    Request::Attach {
                        pane,
                        mode,
                        node: self.view.as_ref().unwrap().node.clone(),
                    },
                )?
                else {
                    return Err("Invalid attach response".into());
                };
                Ok(Outcome {
                    frames: vec![json!({"version":2,"kind":"attach","plan":plan})],
                    ..Outcome::default()
                })
            }
            "terminal.views" => {
                let identity = crate::adapters::terminals::identity(&pane)?;
                let panes = self.view.as_ref().unwrap().snapshot["panes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|candidate| {
                        crate::adapters::terminals::identity(candidate)
                            .is_ok_and(|actual| actual == identity)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if panes.is_empty() {
                    return Err("No exact linked views remain".into());
                }
                self.choose(
                    seq,
                    Stage::LinkedViews(panes.clone()),
                    "Open a linked view",
                    Self::picker_options(&panes),
                    "These views share one terminal process.",
                )
            }
            "terminal.organize" => {
                let mut options = vec![
                    json!(["rename-project", "Rename project", "Change its workspace or session name."]),
                    json!(["rename-group", "Rename tab", "Change its tab or window label."]),
                    json!(["new-group", "New tab", "Open another tab or window in this project."]),
                    json!(["split", "Split terminal", "Add a terminal beside this one."]),
                    json!(["close-group", "Close tab", "Affects every terminal in the tab."]),
                    json!(["close-project", "Close project", "Affects all terminals and shared views of this project."]),
                ];
                if field(&pane, "backend") == "native" {
                    options = vec![
                        json!(["rename-project", "Move terminal to a project", "Change this terminal's project name; other terminals stay where they are."]),
                        json!(["split", "New terminal in this project", "Start another independent shell or agent in the same project."]),
                    ];
                } else if field(&pane, "backend") == "tmux" {
                    options.push(json!(["unlink-group", "Remove this session's link", "For a tmux window linked into several sessions; it keeps running in the others."]));
                }
                self.choose(seq, Stage::Organize(pane), "Organize project", options, "")
            }
            "terminal.native"=>self.confirm(seq,Commit::Native(pane),"Open this terminal full screen in its native client; detach to return to Daedalus. The original process stays running."),
            "terminal.view" => {
                let text = source.preview(&pane, 120)?;
                self.output(seq, &description(&pane), &text, false)
            }
            "terminal.pin" => {
                let mut layouts = self.view.as_ref().unwrap().layouts.clone();
                let pinned = layouts.toggle_pin(reference(&pane)?)?;
                source.save_layouts(&layouts)?;
                Ok(Outcome {
                    layouts: Some(layouts),
                    message: if pinned {
                        "Terminal pinned to Tiles"
                    } else {
                        "Terminal unpinned"
                    }
                    .into(),
                    ..Outcome::default()
                })
            }
            "terminal.send" | "terminal.rename" => self.input(
                seq,
                Stage::Text {
                    pane,
                    rename: action == "terminal.rename",
                },
                if action == "terminal.rename" {
                    "New terminal name"
                } else {
                    "Text or command to send"
                },
            ),
            "terminal.focus" | "terminal.interrupt" | "terminal.close" => {
                let operation = match action {
                    "terminal.focus" => Action::Focus,
                    "terminal.interrupt" => Action::Interrupt,
                    _ => Action::Close,
                };
                let label = format!(
                    "{} {}",
                    match action {
                        "terminal.focus" => "Switch to",
                        "terminal.interrupt" => "Interrupt",
                        _ => "Close",
                    },
                    description(&pane)
                );
                self.confirm(
                    seq,
                    Commit::Terminal {
                        pane,
                        action: operation,
                    },
                    &label,
                )
            }
            "terminal.fork" => {
                let Answer::Live {
                    checkpoint: row,
                    source: pane,
                } = source.session(
                    config,
                    Request::Live {
                        pane: pane.clone(),
                        sid: String::new(),
                    },
                )?
                else {
                    return Err("Invalid live session response".into());
                };
                let mut options = Vec::new();
                if row.sid.is_empty() {
                    options.push(json!(["manual","Enter the session ID","This terminal did not report one. Daedalus never guesses it from labels or recent files."]));
                } else {
                    options.push(json!([
                        "fork",
                        format!("Native fork in {}", row.harness),
                        format!("Independent session from ID {} ({})", row.sid, row.origin)
                    ]));
                }
                options.push(json!([
                    "handoff",
                    "Context handoff to another tool",
                    "Not a native fork: a new session reads a private excerpt of this terminal."
                ]));
                self.choose(seq,Stage::Fork{pane:pane.clone(),checkpoint:row},&format!("Fork {}",description(&pane)),options,"A fork is a separate agent process. To show this same terminal elsewhere, use Duplicate view.")
            }
            "terminal.duplicate" => {
                let mut options = vec![
                    json!([
                        "viewer",
                        "Open a viewer pane beside another terminal",
                        "Closing it closes only the view."
                    ]),
                    json!([
                        "pin",
                        "Pin it in Daedalus Tiles",
                        "A Daedalus view only; nothing changes in the terminal."
                    ]),
                ];
                // Native Herdr cannot currently create a second attach client
                // on Windows. Keep Watch/Tiles available without a dead-end plan.
                if (cfg!(windows) && field(&pane, "backend") == "herdr")
                    || field(&pane, "backend") == "native"
                {
                    options.retain(|option| option[0] != "viewer");
                }
                if field(&pane, "backend") == "tmux" {
                    if !self.link_destinations(&pane).is_empty() {
                        options.insert(
                            0,
                            json!([
                                "link",
                                "Link its window into another tmux session",
                                "Same tmux server only. The whole window appears in both sessions."
                            ]),
                        );
                    }
                    if pane["window_linked"] == "1" {
                        options.push(json!([
                            "unlink",
                            "Close this window link",
                            "The shared window keeps running."
                        ]));
                    }
                }
                self.choose(
                    seq,
                    Stage::Duplicate(pane.clone()),
                    &format!("Duplicate view of {}", description(&pane)),
                    options,
                    if field(&pane, "backend") == "native" {
                        "Use Watch in another Daedalus window for a second view of this terminal. Pinning also keeps it visible in Tiles."
                    } else {
                        "Same process, not a new agent (use Fork for that)."
                    },
                )
            }
            _ => Err("This terminal action is not available".into()),
        }
    }
    fn destinations<S: Source>(
        &mut self,
        selection: (Checkpoint, Option<String>, Operation, Option<Value>),
        target: &str,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let (checkpoint, key, operation, source_pane) = selection;
        let Answer::Capabilities(capabilities) =
            source.session(config, Request::Capabilities(target.into()))?
        else {
            return Err("Invalid capability response".into());
        };
        let supported = match operation {
            Operation::Resume => capabilities.resume,
            Operation::NativeFork => capabilities.native_fork,
            Operation::Handoff => capabilities.handoff,
        };
        if !supported {
            return Err("Installed harness does not advertise this capability".into());
        }
        let panes = self.panes();
        if panes.is_empty() {
            return Err("No live destination terminals".into());
        }
        let mut options = Self::picker_options(&panes);
        let mut sessions = std::collections::BTreeSet::new();
        for (index, pane) in panes.iter().enumerate() {
            if pane["backend"] == "tmux"
                && sessions.insert((field(pane, "socket"), field(pane, "session_id")))
            {
                options.push(json!([
                    format!("window:{index}"),
                    format!("New tmux window in {}", field(pane, "session")),
                    "Creates a new window, preserving the original terminal."
                ]));
            }
        }
        self.choose(
            seq,
            Stage::Destination {
                checkpoint,
                key,
                operation,
                target: capabilities,
                source: source_pane,
                panes,
            },
            "Open a new pane below which terminal?",
            options,
            "",
        )
    }
    fn targets(
        &mut self,
        seq: &mut u64,
        checkpoint: Checkpoint,
        key: Option<String>,
        source: Option<Value>,
    ) -> Result<Outcome> {
        self.choose(
            seq,
            Stage::Target {
                checkpoint,
                key,
                source,
            },
            "Continue with",
            ["claude", "codex", "grok"]
                .into_iter()
                .map(|h| json!([h, h, "New session using a private terminal excerpt"]))
                .collect(),
            "",
        )
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
        if self.hooks.pending() {
            return self.hooks.reply(request, source, config, seq);
        }
        if self.harness.pending() {
            return self.harness.reply(request, source, config, seq);
        }
        if self.workflows.pending() {
            return self.workflows.reply(request, source, config, seq);
        }
        if self.wizard.pending() {
            return self.wizard.reply(request, source, config, seq);
        }
        let pending = self.pending.as_ref().ok_or("No dialog is pending")?;
        if request["op"] != "reply" || request["id"].as_u64() != Some(pending.id) {
            return Err("Unexpected dialog response".into());
        }
        let value = request.get("value").ok_or("Missing dialog response")?;
        if !value.is_null() {
            let text = value.as_str().ok_or("Invalid dialog response")?;
            let valid = match pending.mode {
                "choose" => pending.choices.iter().any(|choice| choice == text),
                "input" => text.len() <= 64_000 && !text.chars().any(char::is_control),
                "output" => text == "close",
                "external" => text == "ready",
                _ => false,
            };
            if !valid {
                return Err("Invalid dialog response".into());
            }
        }
        let pending = self.pending.take().unwrap();
        if value.is_null() {
            if let Stage::Output(refresh) = pending.stage {
                return Ok(Outcome {
                    refresh,
                    ..Outcome::default()
                });
            }
            return Ok(Outcome {
                message: "Cancelled".into(),
                ..Outcome::default()
            });
        }
        let text = value.as_str().unwrap().trim();
        match pending.stage {
            Stage::HookTool(harness) => {
                if text == "herdr" {
                    return self.hooks.herdr(config, &harness, source, seq);
                }
                if text == "remove" {
                    return self.hooks.remove(&harness, source, seq);
                }
                if text == "details" {
                    let rows = &self.view.as_ref().unwrap().health["hooks"]["tools"];
                    let row = rows
                        .as_array()
                        .and_then(|rows| rows.iter().find(|r| r["tool"] == harness))
                        .ok_or("Hook observation changed")?;
                    let detail = super::dialog_text::format(row)
                        .map_err(|_| "Cannot format hook details")?;
                    return self.output(seq, "Hook details", &detail, false);
                }
                let mut view = self.view.as_ref().unwrap().clone();
                if let Some(panes) = view.snapshot["panes"].as_array_mut() {
                    panes.retain(|p| p["agent"] == harness);
                }
                view.selected = None;
                self.hooks.start(view, seq)
            }
            Stage::HookSwitch(expected, option) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                let Answer::Text(message) =
                    source.session(config, Request::HookSwitch { expected, option })?
                else {
                    return Err("Invalid hook switch outcome".into());
                };
                Ok(Outcome {
                    message,
                    refresh: true,
                    ..Default::default()
                })
            }
            Stage::GoalSelect(goals) => {
                let row = goals
                    .get(
                        text.parse::<usize>()
                            .map_err(|_| "Invalid goal selection")?,
                    )
                    .ok_or("Goal selection changed")?
                    .clone();
                let detail = super::dialog_text::format(&row).map_err(|_| "Cannot format goal")?;
                self.choose(
                    seq,
                    Stage::GoalRelease(row),
                    "Confirm goal release",
                    vec![
                        json!(["cancel", "Cancel", "Keep the goal unchanged"]),
                        json!([
                            "yes",
                            "Release",
                            "Complete only the exact reviewed goal version"
                        ]),
                    ],
                    &detail,
                )
            }
            Stage::GoalRelease(row) => {
                if text == "cancel" {
                    return Ok(Outcome::default());
                }
                let id = row["id"]
                    .as_str()
                    .ok_or("Missing goal identity")?
                    .to_owned();
                let scope = serde_json::from_value(row["scope"].clone())
                    .map_err(|_| "Invalid goal scope")?;
                let epoch = row["epoch"].as_i64().ok_or("Invalid goal revision")?;
                let Answer::Text(message) =
                    source.session(config, Request::ReleaseGoal { id, scope, epoch })?
                else {
                    return Err("Invalid goal outcome".into());
                };
                Ok(Outcome {
                    message,
                    refresh: true,
                    ..Default::default()
                })
            }
            Stage::ProjectBackend => {
                let backend = match text {
                    "native" => crate::core::identity::Backend::Native,
                    "tmux" => crate::core::identity::Backend::Tmux,
                    "herdr" => crate::core::identity::Backend::Herdr,
                    _ => return Err("Unknown terminal backend".into()),
                };
                if backend == crate::core::identity::Backend::Native {
                    return self.input(
                        seq,
                        Stage::ProjectName {
                            backend,
                            anchor: None,
                        },
                        "Project name",
                    );
                }
                let mut seen = std::collections::HashSet::new();
                let panes = self
                    .panes()
                    .into_iter()
                    .filter(|p| {
                        crate::adapters::terminals::identity(p)
                            .is_ok_and(|id| id.backend == backend && seen.insert(id.endpoint))
                    })
                    .collect::<Vec<_>>();
                if panes.is_empty() {
                    if backend == crate::core::identity::Backend::Herdr {
                        return Err("No live Herdr server found".into());
                    }
                    self.input(
                        seq,
                        Stage::ProjectName {
                            backend,
                            anchor: None,
                        },
                        "Project name",
                    )
                } else {
                    let options = Self::picker_options(&panes);
                    self.choose(
                        seq,
                        Stage::ProjectServer { backend, panes },
                        "Choose the project server",
                        options,
                        "",
                    )
                }
            }
            Stage::ProjectServer { backend, panes } => self.input(
                seq,
                Stage::ProjectName {
                    backend,
                    anchor: Some(pick(&panes, text)?),
                },
                "Project name",
            ),
            Stage::ProjectName { backend, anchor } => {
                if text.is_empty()
                    || text.len() > 80
                    || !text
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                {
                    return Err("Use letters, digits, hyphens and underscores".into());
                }
                self.input(
                    seq,
                    Stage::ProjectPath {
                        backend,
                        anchor,
                        name: text.into(),
                    },
                    "Existing project directory",
                )
            }
            Stage::ProjectPath {
                backend,
                anchor,
                name,
            } => {
                let path = std::path::Path::new(text);
                crate::core::layouts::check_path(path)?;
                self.workspace_tool(
                    seq,
                    WorkspaceRequest::Create {
                        backend,
                        anchor,
                        name,
                        cwd: text.into(),
                        tool: String::new(),
                    },
                )
            }
            Stage::Organize(pane) => {
                if text.starts_with("rename") || text == "new-group" {
                    self.input(
                        seq,
                        Stage::OrganizeLabel {
                            pane,
                            operation: text.into(),
                        },
                        "Label",
                    )
                } else {
                    let request = WorkspaceRequest::Organize {
                        pane,
                        operation: text.into(),
                        label: String::new(),
                        tool: "shell".into(),
                    };
                    if text == "split" {
                        self.workspace_tool(seq, request)
                    } else {
                        self.workspace_confirm(seq, request)
                    }
                }
            }
            Stage::OrganizeLabel { pane, operation } => {
                if text.is_empty() || text.chars().count() > 80 {
                    return Err("Use a printable label of 1 to 80 characters".into());
                }
                let needs_tool = operation == "new-group";
                let request = WorkspaceRequest::Organize {
                    pane,
                    operation,
                    label: text.into(),
                    tool: "shell".into(),
                };
                if needs_tool {
                    self.workspace_tool(seq, request)
                } else {
                    self.workspace_confirm(seq, request)
                }
            }
            Stage::WorkspaceTool(mut request) => {
                request.set_tool(text);
                self.workspace_confirm(seq, *request)
            }

            Stage::Native(pane) => {
                let mut frames = vec![json!({"version":2,"kind":"external_done"})];
                let message = match source.session(config, Request::Native(pane)) {
                    Ok(Answer::Text(message)) => message,
                    Ok(_) => "Native client returned an unexpected result".into(),
                    Err(message) => {
                        frames.push(json!({"version":2,"kind":"error","message":message}));
                        message
                    }
                };
                Ok(Outcome {
                    frames,
                    refresh: true,
                    message,
                    ..Outcome::default()
                })
            }

            Stage::SocketInput => {
                let mut updated = config.clone();
                updated.tmux_sockets = text
                    .split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned)
                    .collect();
                for socket in &updated.tmux_sockets {
                    crate::core::layouts::check_path(std::path::Path::new(socket))?;
                }
                updated.tmux_sockets.sort();
                updated.tmux_sockets.dedup();
                updated.validate()?;
                self.confirm(
                    seq,
                    Commit::Configure {
                        expected: Box::new(config.clone()),
                        updated: Box::new(updated),
                    },
                    "Save the reviewed additional tmux socket paths and refresh discovery",
                )
            }

            Stage::Output(refresh) => Ok(Outcome {
                refresh,
                ..Outcome::default()
            }),
            Stage::Pick { action, panes } => {
                let pane = pick(&panes, text)?;
                self.for_pane(&action, pane, source, config, seq)
            }
            Stage::Terminal(pane) => self.for_pane(text, pane, source, config, seq),
            Stage::LinkedViews(panes) => self.confirm(
                seq,
                Commit::Terminal {
                    pane: pick(&panes, text)?,
                    action: Action::Focus,
                },
                "Focus selected linked view",
            ),
            Stage::SaveName(pane) => {
                if text.is_empty() || text.chars().count() > 100 {
                    return Err("Use a checkpoint name between 1 and 100 characters".into());
                }
                self.choose(
                    seq,
                    Stage::SaveKind {
                        pane,
                        name: text.into(),
                    },
                    "What should be saved?",
                    vec![
                        json!([
                            "reference",
                            "Session reference only",
                            "Save the reported native ID and project directory."
                        ]),
                        json!([
                            "context",
                            "Visible output + session reference if available",
                            "Save the last 120 lines privately, even without a native session ID; context-only saves support handoff, not native resume or fork."
                        ]),
                        json!([
                            "manual",
                            "Enter a session ID",
                            "Use an ID you already know; it will be marked as user supplied."
                        ]),
                    ],
                    "",
                )
            }
            Stage::SaveKind { pane, name } => {
                if text == "manual" {
                    self.input(seq, Stage::SaveSid { pane, name }, "Native session ID")
                } else {
                    let label = format!("Save private checkpoint for {}", description(&pane));
                    self.confirm(
                        seq,
                        Commit::Save {
                            pane,
                            request: SaveRequest {
                                name,
                                sid: String::new(),
                                include_output: text == "context",
                            },
                        },
                        &label,
                    )
                }
            }
            Stage::SaveSid { pane, name } => {
                let label = format!("Save private checkpoint for {}", description(&pane));
                self.confirm(
                    seq,
                    Commit::Save {
                        pane,
                        request: SaveRequest {
                            name,
                            sid: text.into(),
                            include_output: false,
                        },
                    },
                    &label,
                )
            }
            Stage::Checkpoint(row) => match text {
                "details" => self.output(
                    seq,
                    "Saved checkpoint",
                    &super::dialog_text::format(&row).map_err(|_| "Cannot format checkpoint")?,
                    false,
                ),
                "remove" => self.confirm(
                    seq,
                    Commit::Remove(row.key),
                    &format!(
                        "Remove saved reference “{}”; native history and terminals are unaffected",
                        row.checkpoint.name
                    ),
                ),
                "handoff" => self.targets(seq, row.checkpoint, Some(row.key), None),
                _ => {
                    let target = row.checkpoint.harness.clone();
                    self.destinations(
                        (
                            row.checkpoint,
                            Some(row.key),
                            if text == "fork" {
                                Operation::NativeFork
                            } else {
                                Operation::Resume
                            },
                            None,
                        ),
                        &target,
                        source,
                        config,
                        seq,
                    )
                }
            },
            Stage::Fork { pane, checkpoint } => match text {
                "manual" => self.input(seq, Stage::ForkSid(pane), "Native session ID"),
                "handoff" => self.targets(seq, checkpoint, None, Some(pane)),
                _ => {
                    let target = checkpoint.harness.clone();
                    self.destinations(
                        (checkpoint, None, Operation::NativeFork, Some(pane)),
                        &target,
                        source,
                        config,
                        seq,
                    )
                }
            },
            Stage::ForkSid(pane) => {
                let Answer::Live {
                    checkpoint: row,
                    source: pane,
                } = source.session(
                    config,
                    Request::Live {
                        pane: pane.clone(),
                        sid: text.into(),
                    },
                )?
                else {
                    return Err("Invalid live session response".into());
                };
                let target = row.harness.clone();
                self.destinations(
                    (row, None, Operation::NativeFork, Some(pane)),
                    &target,
                    source,
                    config,
                    seq,
                )
            }
            Stage::Target {
                checkpoint,
                key,
                source: source_pane,
            } => self.destinations(
                (checkpoint, key, Operation::Handoff, source_pane),
                text,
                source,
                config,
                seq,
            ),
            Stage::Destination {
                checkpoint,
                key,
                operation,
                target,
                source: source_pane,
                panes,
            } => {
                let (placement, index) = match text.strip_prefix("window:") {
                    Some(index) => (Placement::Window, index),
                    None => (Placement::Split, text),
                };
                let parent = pick(&panes, index)?;
                let label = format!(
                    "{} “{}” with {} in a NEW {} at {}. This launches the AI tool.",
                    match operation {
                        Operation::Resume => "Resume",
                        Operation::NativeFork => "Fork",
                        Operation::Handoff => "Hand off",
                    },
                    checkpoint.name,
                    target.harness,
                    if placement == Placement::Window {
                        "tmux window"
                    } else {
                        "split pane"
                    },
                    description(&parent)
                );
                self.confirm(
                    seq,
                    Commit::Launch(Box::new(Launch {
                        checkpoint,
                        key,
                        operation,
                        target,
                        parent,
                        source: source_pane,
                        placement,
                    })),
                    &label,
                )
            }
            Stage::Confirm(commit) => {
                if text == "cancel" {
                    return Ok(Outcome {
                        message: "Cancelled".into(),
                        ..Outcome::default()
                    });
                }
                self.commit(*commit, source, config, seq)
            }
            Stage::Text { pane, rename } => {
                if text.is_empty() {
                    return Err("Supply nonempty text".into());
                }
                let label = format!(
                    "{} {}",
                    if rename { "Rename" } else { "Send input to" },
                    description(&pane)
                );
                self.confirm(
                    seq,
                    Commit::Terminal {
                        pane,
                        action: if rename {
                            Action::Rename(text.into())
                        } else {
                            Action::Send(text.into())
                        },
                    },
                    &label,
                )
            }
            Stage::Duplicate(pane) => match text {
                "pin" => self.for_pane("terminal.pin", pane, source, config, seq),
                "unlink" => self.confirm(
                    seq,
                    Commit::Unlink(pane),
                    "Remove this session's window link; the shared window keeps running",
                ),
                "link" => {
                    let panes = self.link_destinations(&pane);
                    if panes.is_empty() {
                        return Err("No other tmux session is available for this window".into());
                    }
                    let options = Self::picker_options(&panes);
                    self.choose(
                        seq,
                        Stage::LinkDestination {
                            source: pane,
                            panes,
                        },
                        "Link the window into which session?",
                        options,
                        "",
                    )
                }
                _ => {
                    let panes = self.panes();
                    let options = Self::picker_options(&panes);
                    self.choose(
                        seq,
                        Stage::ViewerDestination {
                            source: pane,
                            panes,
                        },
                        "Open the viewer beside which terminal?",
                        options,
                        "",
                    )
                }
            },
            Stage::ViewerDestination {
                source: original,
                panes,
            } => {
                let destination = pick(&panes, text)?;
                if original["backend"] == "tmux" {
                    self.choose(
                        seq,
                        Stage::ViewerAccess {
                            source: original,
                            destination,
                        },
                        "Viewer access",
                        vec![
                            json!(["watch", "Watch only", "A read-only tmux client."]),
                            json!([
                                "type",
                                "Can type",
                                "Shared tmux input, like any attached client."
                            ]),
                        ],
                        "",
                    )
                } else {
                    self.confirm(seq,Commit::Duplicate{source:original,destination,read_only:false},"Open a Herdr native viewer. This holds its input attach slot; closing the viewer leaves the source running.")
                }
            }
            Stage::ViewerAccess {
                source: original,
                destination,
            } => self.confirm(
                seq,
                Commit::Duplicate {
                    source: original,
                    destination,
                    read_only: text == "watch",
                },
                "Open the viewer in a new pane; closing it closes only the view",
            ),
            Stage::LinkDestination {
                source: original,
                panes,
            } => self.confirm(
                seq,
                Commit::Link {
                    source: original,
                    destination: pick(&panes, text)?,
                },
                "Link this window into the selected tmux session. Nothing is moved or restarted.",
            ),
            Stage::LayoutName => {
                let mut layouts = self.view.as_ref().unwrap().layouts.clone();
                layouts.save_layout(text, layouts.current.clone())?;
                source.save_layouts(&layouts)?;
                Ok(Outcome {
                    layouts: Some(layouts),
                    message: format!("Layout “{text}” saved privately"),
                    ..Outcome::default()
                })
            }
            Stage::Layout(name) => {
                if text == "delete" {
                    self.confirm(
                        seq,
                        Commit::DeleteLayout(name.clone()),
                        &format!("Delete layout “{name}”"),
                    )
                } else {
                    let mut layouts = self.view.as_ref().unwrap().layouts.clone();
                    layouts.open_layout(&name)?;
                    source.save_layouts(&layouts)?;
                    Ok(Outcome {
                        layouts: Some(layouts),
                        restore: true,
                        message: format!("Opened “{name}”; missing terminals are never rebound"),
                        ..Outcome::default()
                    })
                }
            }
        }
    }
    fn commit<S: Source>(
        &mut self,
        commit: Commit,
        source: &mut S,
        config: &ControlConfig,
        seq: &mut u64,
    ) -> Result<Outcome> {
        let request = match commit {
            Commit::Workspace(request) => Request::Workspace(request),
            Commit::Native(pane) => {
                return self.dialog(
                    seq,
                    Stage::Native(pane),
                    "external",
                    "Open native tool",
                    json!({"description":"The dashboard will return when the program exits."}),
                );
            }
            Commit::Configure { expected, updated } => Request::Configure { expected, updated },
            Commit::Save { pane, request } => Request::Save { pane, request },
            Commit::Remove(key) => Request::Remove(key),
            Commit::Launch(launch) => Request::Launch {
                checkpoint: Box::new(launch.checkpoint),
                key: launch.key,
                operation: launch.operation,
                target: launch.target,
                parent: launch.parent,
                source: launch.source,
                placement: launch.placement,
            },
            Commit::Terminal { pane, action } => Request::Terminal { pane, action },
            Commit::Duplicate {
                source,
                destination,
                read_only,
            } => Request::Duplicate {
                source,
                destination,
                read_only,
            },
            Commit::Link {
                source,
                destination,
            } => Request::Link {
                source,
                destination,
            },
            Commit::Unlink(pane) => Request::Unlink(pane),
            Commit::DeleteLayout(name) => {
                let mut layouts = self.view.as_ref().unwrap().layouts.clone();
                layouts
                    .layouts
                    .remove(&name)
                    .ok_or("Layout already removed")?;
                source.save_layouts(&layouts)?;
                return Ok(Outcome {
                    layouts: Some(layouts),
                    message: "Layout deleted".into(),
                    ..Outcome::default()
                });
            }
        };
        match source.session(config, request)? {
            Answer::Configured {
                configuration,
                message,
            } => Ok(Outcome {
                configuration: Some(configuration),
                message,
                refresh: true,
                ..Outcome::default()
            }),
            Answer::Saved(row) => self.output(
                seq,
                "Checkpoint saved",
                &format!(
                    "{}\nSession ID: {}\nStored in {}",
                    row.checkpoint.name,
                    if row.checkpoint.sid.is_empty() {
                        "Not reported; context-only checkpoint"
                    } else {
                        &row.checkpoint.sid
                    },
                    row.registry
                ),
                true,
            ),
            Answer::Receipt(receipt) => self.output(seq, "Result", &receipt.message, true),
            Answer::Text(text) => self.output(seq, "Result", &text, true),
            _ => Err("Unexpected session operation response".into()),
        }
    }
}
fn pick(panes: &[Value], value: &str) -> Result<Value> {
    value
        .parse::<usize>()
        .ok()
        .and_then(|index| panes.get(index))
        .cloned()
        .ok_or_else(|| "Terminal choice is no longer available".into())
}

#[cfg(test)]
mod tests {
    use super::super::backend::Observations;
    use super::*;
    use std::path::Path;

    #[derive(Default)]
    struct Mock {
        requests: Vec<Request>,
        layouts: Vec<Layouts>,
        fail_effect: bool,
        supported: bool,
        missing_sid: bool,
    }
    fn checkpoint() -> Checkpoint {
        serde_json::from_value(
            json!({"name":"saved","harness":"claude","sid":"abc-123","cwd":crate::test_support::path("/tmp/project")}),
        )
        .unwrap()
    }
    fn row() -> CheckpointRow {
        CheckpointRow {
            key: "checkpoint:stable".into(),
            checkpoint: checkpoint(),
            registry: "dashboard-checkpoints.json".into(),
        }
    }
    impl Source for Mock {
        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            Ok(Observations::default())
        }
        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            Ok("isolated preview".into())
        }
        fn save_layouts(&mut self, layouts: &Layouts) -> Result<()> {
            self.layouts.push(layouts.clone());
            Ok(())
        }
        fn session(&mut self, _: &ControlConfig, request: Request) -> Result<Answer> {
            let answer = match &request {
                Request::Configure { updated, .. } => Answer::Configured {
                    configuration: (**updated).clone(),
                    message: "Configured".into(),
                },
                Request::Load(_) => Answer::Saved(row()),
                Request::Live { pane, sid } => {
                    let mut frozen = pane.clone();
                    frozen["_daedalus_handoff_process"] = json!({"pid":123,"start":"456","argv_hash":"fixture","cwd":crate::test_support::path("/tmp/project"),"profile":""});
                    frozen["_fixture_supplied_sid"] = json!(sid);
                    let mut row = checkpoint();
                    if self.missing_sid {
                        row.sid = sid.clone();
                    }
                    Answer::Live {
                        checkpoint: row,
                        source: frozen,
                    }
                }
                Request::Capabilities(harness) => Answer::Capabilities(Capabilities {
                    harness: harness.clone(),
                    binary: "/mock/tool".into(),
                    resume: self.supported,
                    native_fork: self.supported,
                    handoff: self.supported,
                }),
                Request::Attach { mode, node, .. } => Answer::Frame(
                    json!({"mode":if *mode==AttachMode::Observe {"observe"}else{"control"},"node":node,"argv":["mock","attach"]}),
                ),
                _ => Answer::Text("Submitted; completion is not confirmed".into()),
            };
            let effect = matches!(
                &request,
                Request::Save { .. }
                    | Request::Native(_)
                    | Request::Launch { .. }
                    | Request::Remove(_)
                    | Request::Terminal { .. }
                    | Request::Duplicate { .. }
                    | Request::Link { .. }
                    | Request::Unlink(_)
            );
            self.requests.push(request);
            if effect && self.fail_effect {
                Err("Terminal generation changed".into())
            } else {
                Ok(answer)
            }
        }
    }
    fn pane() -> Value {
        json!({"backend":"tmux","server":"tmux@/tmp/mock.sock","socket":crate::test_support::path("/tmp/mock.sock"),"pane_id":"%1","terminal_id":"generation-original","session":"project","session_id":"$1","window_id":"@1"})
    }
    fn view() -> View {
        View {
            snapshot: json!({"panes":[pane()]}),
            health: json!({}),
            layouts: Layouts::default(),
            selected: Some(pane()),
            checkpoint: None,
            node: "synthetic-node".into(),
        }
    }
    fn config() -> ControlConfig {
        ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/project"))).unwrap()
    }
    fn reply(
        menu: &mut SessionMenu,
        source: &mut Mock,
        sequence: &mut u64,
        value: Value,
    ) -> Result<Outcome> {
        menu.reply(
            &json!({"op":"reply","id":*sequence,"value":value}),
            source,
            &config(),
            sequence,
        )
    }
    fn start(
        menu: &mut SessionMenu,
        source: &mut Mock,
        sequence: &mut u64,
        action: &str,
    ) -> Outcome {
        menu.start(action, view(), source, &config(), sequence)
            .unwrap()
    }

    #[test]
    fn herdr_duplicate_view_menu_matches_platform_support() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        let mut terminal = pane();
        terminal["backend"] = json!("herdr");
        let outcome = menu
            .for_pane(
                "terminal.duplicate",
                terminal,
                &mut source,
                &config(),
                &mut seq,
            )
            .unwrap();
        let choices = outcome.frames[0]["options"].as_array().unwrap();
        assert_eq!(choices.iter().any(|row| row[0] == "viewer"), !cfg!(windows));
        assert!(choices.iter().any(|row| row[0] == "pin"));
        assert!(source.requests.is_empty());
    }
    #[test]
    fn checkpoint_save_invalid_reply_cancel_and_confirmation_are_single_use() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        let frame = start(&mut menu, &mut source, &mut seq, "checkpoint.save");
        assert_eq!(frame.frames[0]["mode"], "input");
        reply(&mut menu, &mut source, &mut seq, json!("release")).unwrap();
        assert!(reply(&mut menu, &mut source, &mut seq, json!("invalid-choice")).is_err());
        assert!(menu.pending());
        reply(&mut menu, &mut source, &mut seq, json!("context")).unwrap();
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
        assert!(source.requests.is_empty());
        reply(&mut menu, &mut source, &mut seq, json!("cancel")).unwrap();
        assert!(source.requests.is_empty());
        start(&mut menu, &mut source, &mut seq, "checkpoint.save");
        reply(&mut menu, &mut source, &mut seq, json!("release")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("context")).unwrap();
        let confirmed = seq;
        assert!(confirmed > confirmation);
        let outcome = reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert_eq!(outcome.frames[0]["mode"], "output");
        assert_eq!(source.requests.len(), 1);
        match &source.requests[0] {
            Request::Save {
                pane: saved,
                request,
            } => {
                assert_eq!(saved, &pane());
                assert!(request.include_output);
                assert_eq!(request.name, "release");
            }
            _ => panic!("wrong effect"),
        }
        assert!(
            menu.reply(
                &json!({"op":"reply","id":confirmed,"value":"yes"}),
                &mut source,
                &config(),
                &mut seq
            )
            .is_err()
        );
        assert_eq!(source.requests.len(), 1);
        assert!(
            reply(&mut menu, &mut source, &mut seq, Value::Null)
                .unwrap()
                .refresh
        );
    }

    #[test]
    fn checkpoint_resume_and_fork_require_capability_then_exact_frozen_destination() {
        for choice in ["resume", "fork"] {
            let (mut menu, mut source, mut seq) = (
                SessionMenu::default(),
                Mock {
                    supported: true,
                    ..Mock::default()
                },
                0,
            );
            start(
                &mut menu,
                &mut source,
                &mut seq,
                "checkpoint:checkpoint:stable",
            );
            reply(&mut menu, &mut source, &mut seq, json!(choice)).unwrap();
            reply(&mut menu, &mut source, &mut seq, json!("0")).unwrap();
            assert!(
                !source
                    .requests
                    .iter()
                    .any(|r| matches!(r, Request::Launch { .. }))
            );
            source.fail_effect = true;
            let confirmed = seq;
            assert!(
                reply(&mut menu, &mut source, &mut seq, json!("yes"))
                    .unwrap_err()
                    .contains("generation changed")
            );
            assert!(!menu.pending());
            assert!(
                menu.reply(
                    &json!({"op":"reply","id":confirmed,"value":"yes"}),
                    &mut source,
                    &config(),
                    &mut seq
                )
                .is_err()
            );
            assert_eq!(
                source
                    .requests
                    .iter()
                    .filter(|r| matches!(r, Request::Launch { .. }))
                    .count(),
                1
            );
            match source.requests.last().unwrap() {
                Request::Launch {
                    parent,
                    key,
                    operation,
                    ..
                } => {
                    assert_eq!(parent, &pane());
                    assert_eq!(key.as_deref(), Some("checkpoint:stable"));
                    assert_eq!(
                        *operation,
                        if choice == "fork" {
                            Operation::NativeFork
                        } else {
                            Operation::Resume
                        }
                    );
                }
                _ => panic!("missing launch"),
            }
        }
    }

    #[test]
    fn unsupported_capabilities_never_offer_destination_or_execute() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        start(
            &mut menu,
            &mut source,
            &mut seq,
            "checkpoint:checkpoint:stable",
        );
        assert!(reply(&mut menu, &mut source, &mut seq, json!("fork")).is_err());
        assert!(!menu.pending());
        assert!(
            !source
                .requests
                .iter()
                .any(|r| matches!(r, Request::Launch { .. }))
        );
    }

    #[test]
    fn remove_checkpoint_cancel_preserves_reference_and_confirm_uses_exact_key() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        start(
            &mut menu,
            &mut source,
            &mut seq,
            "checkpoint:checkpoint:stable",
        );
        reply(&mut menu, &mut source, &mut seq, json!("remove")).unwrap();
        reply(&mut menu, &mut source, &mut seq, Value::Null).unwrap();
        assert!(
            !source
                .requests
                .iter()
                .any(|r| matches!(r, Request::Remove(_)))
        );
        start(
            &mut menu,
            &mut source,
            &mut seq,
            "checkpoint:checkpoint:stable",
        );
        reply(&mut menu, &mut source, &mut seq, json!("remove")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert!(
            matches!(source.requests.last(),Some(Request::Remove(key)) if key=="checkpoint:stable")
        );
    }

    #[test]
    fn attach_watch_preserve_existing_frame_and_terminal_menu_keeps_icons() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        for (action, mode) in [
            ("terminal.attach", "control"),
            ("terminal.watch", "observe"),
        ] {
            let outcome = start(&mut menu, &mut source, &mut seq, action);
            assert_eq!(outcome.frames[0]["kind"], "attach");
            assert_eq!(outcome.frames[0]["plan"]["mode"], mode);
            assert_eq!(outcome.frames[0]["plan"]["node"], "synthetic-node");
        }
        let outcome = start(&mut menu, &mut source, &mut seq, "synthetic-node");
        assert_eq!(outcome.frames[0]["options"].as_array().unwrap().len(), 16);
        assert_eq!(outcome.frames[0]["icons"].as_array().unwrap().len(), 16);
        assert!(
            outcome.frames[0]["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row[0] == "telegram.route"
                    && !row[2].as_str().unwrap().contains("Not available"))
        );
    }

    #[test]
    fn save_and_restore_layout_is_private_persistence_without_terminal_operations() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        start(&mut menu, &mut source, &mut seq, "layout.save");
        let saved = reply(&mut menu, &mut source, &mut seq, json!("work")).unwrap();
        let mut saved_view = view();
        saved_view.layouts = saved.layouts.unwrap();
        menu.start(
            "layout:work",
            saved_view.clone(),
            &mut source,
            &config(),
            &mut seq,
        )
        .unwrap();
        let opened = reply(&mut menu, &mut source, &mut seq, json!("open")).unwrap();
        assert!(opened.restore);
        menu.start("layout:work", saved_view, &mut source, &config(), &mut seq)
            .unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("delete")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("cancel")).unwrap();
        assert_eq!(source.layouts.len(), 2);
        assert!(source.requests.is_empty());
    }
    #[test]
    fn live_handoff_and_duplicate_view_remain_distinct_confirmed_operations() {
        let (mut menu, mut source, mut seq) = (
            SessionMenu::default(),
            Mock {
                supported: true,
                ..Mock::default()
            },
            0,
        );
        start(&mut menu, &mut source, &mut seq, "terminal.fork");
        for value in ["handoff", "codex", "0", "yes"] {
            let outcome = reply(&mut menu, &mut source, &mut seq, json!(value)).unwrap();
            assert!(
                !serde_json::to_string(&outcome.frames)
                    .unwrap()
                    .contains("_daedalus_handoff_process")
            );
        }
        match source.requests.last().unwrap() {
            Request::Launch {
                operation,
                target,
                source: Some(original),
                ..
            } => {
                assert_eq!(*operation, Operation::Handoff);
                assert_eq!(target.harness, "codex");
                assert_eq!(original["terminal_id"], pane()["terminal_id"]);
                assert_eq!(
                    original["_daedalus_handoff_process"]["argv_hash"],
                    "fixture"
                );
            }
            _ => panic!("Missing explicit handoff"),
        }
        reply(&mut menu, &mut source, &mut seq, json!("close")).unwrap();
        let count = source.requests.len();
        start(&mut menu, &mut source, &mut seq, "terminal.duplicate");
        for value in ["viewer", "0", "watch"] {
            reply(&mut menu, &mut source, &mut seq, json!(value)).unwrap();
        }
        assert_eq!(source.requests.len(), count);
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        match source.requests.last().unwrap() {
            Request::Duplicate {
                source: original,
                destination,
                read_only,
            } => {
                assert!(read_only);
                assert_eq!(original, &pane());
                assert_eq!(destination, &pane());
            }
            _ => panic!("Viewer must never launch a fork"),
        }
    }

    #[test]
    fn tmux_pin_preserves_legacy_server_wire_identity_and_generation() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        let outcome = start(&mut menu, &mut source, &mut seq, "terminal.pin");
        let layouts = outcome.layouts.unwrap();
        assert_eq!(layouts.current.pins[0].server, "tmux@/tmp/mock.sock");
        assert_eq!(layouts.current.pins[0].terminal_id, "generation-original");
        assert_eq!(source.layouts.len(), 1);
        assert!(source.requests.is_empty());
    }
    #[test]
    fn manual_live_id_uses_newly_frozen_source_from_second_read() {
        let (mut menu, mut source, mut seq) = (
            SessionMenu::default(),
            Mock {
                supported: true,
                missing_sid: true,
                ..Mock::default()
            },
            0,
        );
        start(&mut menu, &mut source, &mut seq, "terminal.fork");
        for value in ["manual", "manual-session", "0", "yes"] {
            reply(&mut menu, &mut source, &mut seq, json!(value)).unwrap();
        }
        match source.requests.last().unwrap() {
            Request::Launch {
                checkpoint,
                source: Some(frozen),
                ..
            } => {
                assert_eq!(checkpoint.sid, "manual-session");
                assert_eq!(frozen["_fixture_supplied_sid"], "manual-session");
                assert_eq!(frozen["_daedalus_handoff_process"]["argv_hash"], "fixture");
            }
            _ => panic!("Expected frozen manual session launch"),
        }
        assert!(source.layouts.is_empty());
    }
    #[test]
    fn new_tmux_window_is_an_explicit_confirmed_destination() {
        let (mut menu, mut source, mut seq) = (
            SessionMenu::default(),
            Mock {
                supported: true,
                ..Mock::default()
            },
            0,
        );
        start(
            &mut menu,
            &mut source,
            &mut seq,
            "checkpoint:checkpoint:stable",
        );
        let destinations = reply(&mut menu, &mut source, &mut seq, json!("fork")).unwrap();
        assert!(
            destinations.frames[0]["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row[0] == "window:0")
        );
        let confirmation = reply(&mut menu, &mut source, &mut seq, json!("window:0")).unwrap();
        assert!(
            confirmation.frames[0]["description"]
                .as_str()
                .unwrap()
                .contains("NEW tmux window")
        );
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert!(matches!(
            source.requests.last(),
            Some(Request::Launch {
                placement: Placement::Window,
                ..
            })
        ));
    }
    #[test]
    fn socket_configuration_is_reviewed_and_preserves_other_fields() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        start(&mut menu, &mut source, &mut seq, "workspace.socket");
        reply(
            &mut menu,
            &mut source,
            &mut seq,
            json!(
                [
                    crate::test_support::path("/tmp/socket-b"),
                    crate::test_support::path("/tmp/socket-a"),
                    crate::test_support::path("/tmp/socket-a")
                ]
                .join(",")
            ),
        )
        .unwrap();
        assert!(source.requests.is_empty());
        let outcome = reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        let updated = outcome.configuration.unwrap();
        assert_eq!(
            updated.tmux_sockets,
            vec![
                crate::test_support::path("/tmp/socket-a"),
                crate::test_support::path("/tmp/socket-b")
            ]
        );
        assert_eq!(updated.agent_home, config().agent_home);
        assert!(outcome.refresh);
        assert!(
            matches!(source.requests.last(),Some(Request::Configure{expected,..}) if expected.tmux_sockets.is_empty())
        );
    }
    #[test]
    fn linked_views_use_shared_identity_not_labels_and_focus_after_confirmation() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        let mut selected = view();
        let mut linked = pane();
        linked["session_id"] = json!("$2");
        linked["session"] = json!("linked-project");
        let mut impostor = linked.clone();
        impostor["terminal_id"] = json!("replacement");
        selected.snapshot["panes"] = json!([pane(), linked, impostor]);
        let choices = menu
            .start("terminal.views", selected, &mut source, &config(), &mut seq)
            .unwrap();
        assert_eq!(choices.frames[0]["options"].as_array().unwrap().len(), 2);
        reply(&mut menu, &mut source, &mut seq, json!("1")).unwrap();
        assert!(source.requests.is_empty());
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert!(
            matches!(source.requests.last(),Some(Request::Terminal{pane,action:Action::Focus}) if pane["session_id"]=="$2" && pane["terminal_id"]=="generation-original")
        );
    }
    #[test]
    fn native_client_waits_for_tty_ready_and_always_restores_renderer_after_error() {
        let (mut menu, mut source, mut seq) = (
            SessionMenu::default(),
            Mock {
                fail_effect: true,
                ..Mock::default()
            },
            0,
        );
        start(&mut menu, &mut source, &mut seq, "terminal.native");
        let external = reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert_eq!(external.frames[0]["mode"], "external");
        assert!(source.requests.is_empty());
        assert!(reply(&mut menu, &mut source, &mut seq, json!("yes")).is_err());
        assert!(source.requests.is_empty());
        let done = reply(&mut menu, &mut source, &mut seq, json!("ready")).unwrap();
        assert_eq!(done.frames[0]["kind"], "external_done");
        assert_eq!(done.frames[1]["kind"], "error");
        assert!(done.refresh);
        assert_eq!(source.requests.len(), 1);
        assert!(!menu.pending());
    }
    #[test]
    fn project_creation_and_organization_keep_exact_targets_and_confirm_before_effect() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        start(&mut menu, &mut source, &mut seq, "project.new");
        for value in [
            "tmux",
            "0",
            "new_project",
            crate::test_support::path("/tmp"),
            "shell",
        ] {
            reply(&mut menu, &mut source, &mut seq, json!(value)).unwrap();
        }
        assert!(source.requests.is_empty());
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        match source.requests.last().unwrap() {
            Request::Workspace(request) => assert!(
                matches!(&**request,WorkspaceRequest::Create{anchor:Some(p),name,cwd,tool,..} if p==&pane()&&name=="new_project"&&cwd==crate::test_support::path("/tmp")&&tool=="shell")
            ),
            _ => panic!("Missing project create"),
        }
        reply(&mut menu, &mut source, &mut seq, json!("close")).unwrap();
        let count = source.requests.len();
        start(&mut menu, &mut source, &mut seq, "terminal.organize");
        reply(&mut menu, &mut source, &mut seq, json!("rename-group")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("renamed tab")).unwrap();
        assert_eq!(source.requests.len(), count);
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        match source.requests.last().unwrap() {
            Request::Workspace(request) => assert!(
                matches!(&**request,WorkspaceRequest::Organize{pane:p,operation,label,..} if p==&pane()&&operation=="rename-group"&&label=="renamed tab")
            ),
            _ => panic!("Missing organize"),
        }
    }
    #[test]
    fn help_and_discovery_reports_need_no_live_source_effects() {
        let mut menu = SessionMenu::default();
        let mut source = Mock::default();
        let mut seq = 0;
        let output = menu
            .start("help.reference", view(), &mut source, &config(), &mut seq)
            .unwrap();
        assert!(
            output.frames[0]["text"]
                .as_str()
                .unwrap()
                .contains("daedalus")
        );
        assert!(source.requests.is_empty());
        menu.cancel();
        let output = menu
            .start(
                "activity.discovery",
                view(),
                &mut source,
                &config(),
                &mut seq,
            )
            .unwrap();
        assert!(
            output.frames[0]["text"]
                .as_str()
                .unwrap()
                .contains("No discovery warnings")
        );
        assert!(source.requests.is_empty());
    }
    #[test]
    fn every_registered_static_action_has_a_native_dispatch_path() {
        let registry = registry::builtin();
        assert_eq!(registry.actions.len(), 45);
        for action in &registry.actions {
            assert!(
                action.id == "instances.saved" || available(&action.id),
                "Missing native action {}",
                action.id
            );
        }
    }
    #[test]
    fn dynamic_goal_release_is_explicit_and_confirmation_cannot_replay() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        let mut view = view();
        view.health["hooks"] = json!({"goals":[{"id":"goal","name":"goal","directive":"Fixture goal","epoch":4,"scope":{"instance":"agent","harness":"claude","terminal":{"backend":"tmux","endpoint":"/fixture.sock","pane":"%1","generation":"generation"},"conversation":"session"}}]});
        menu.start("goal:", view.clone(), &mut source, &config(), &mut seq)
            .unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("0")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("cancel")).unwrap();
        assert!(source.requests.is_empty());
        menu.start("goal:", view, &mut source, &config(), &mut seq)
            .unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("0")).unwrap();
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert!(matches!(
            &source.requests[0],
            Request::ReleaseGoal { epoch: 4, .. }
        ));
        assert!(reply(&mut menu, &mut source, &mut seq, json!("yes")).is_err());
        assert_eq!(source.requests.len(), 1);
    }
    #[test]
    fn native_project_starts_without_existing_terminals_and_requires_confirmation() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        let mut empty = view();
        empty.snapshot["panes"] = json!([]);
        empty.selected = None;
        let options = menu
            .start("project.new", empty, &mut source, &config(), &mut seq)
            .unwrap();
        assert_eq!(options.frames[0]["options"][0][0], "native");
        let name = reply(&mut menu, &mut source, &mut seq, json!("native")).unwrap();
        assert_eq!(name.frames[0]["title"], "Project name");
        for value in ["my_project", crate::test_support::path("/tmp"), "shell"] {
            reply(&mut menu, &mut source, &mut seq, json!(value)).unwrap();
        }
        assert!(source.requests.is_empty());
        reply(&mut menu, &mut source, &mut seq, json!("yes")).unwrap();
        assert!(
            matches!(source.requests.last(), Some(Request::Workspace(request)) if matches!(&**request,
                WorkspaceRequest::Create { backend: crate::core::identity::Backend::Native, anchor: None, name, tool, .. }
                if name == "my_project" && tool == "shell"
            ))
        );
    }
    #[test]
    fn native_menus_offer_supported_organization_and_watch_guidance() {
        let (mut menu, mut source, mut seq) = (SessionMenu::default(), Mock::default(), 0);
        let mut selected = view();
        let mut terminal = pane();
        terminal["backend"] = json!("native");
        selected.selected = Some(terminal.clone());
        selected.snapshot["panes"] = json!([terminal]);
        let organize = menu
            .start(
                "terminal.organize",
                selected.clone(),
                &mut source,
                &config(),
                &mut seq,
            )
            .unwrap();
        let choices = organize.frames[0]["options"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0][0], "rename-project");
        assert_eq!(choices[1][0], "split");
        menu.cancel();
        let duplicate = menu
            .start(
                "terminal.duplicate",
                selected,
                &mut source,
                &config(),
                &mut seq,
            )
            .unwrap();
        assert!(
            duplicate.frames[0]["options"]
                .as_array()
                .unwrap()
                .iter()
                .all(|option| option[0] != "viewer")
        );
        assert!(
            duplicate.frames[0]["description"]
                .as_str()
                .unwrap()
                .contains("Watch in another Daedalus window")
        );
        assert!(source.requests.is_empty());
    }
}
