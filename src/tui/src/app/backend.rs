//! Native version-two stdio backend. The existing Rust renderer is unchanged.
//! All OS access is behind Source; fixture tests never inspect the user's machine.
use super::{
    config::ControlConfig,
    inventory,
    presentation::{self, Context},
    registry,
    session_menu::{self, SessionMenu},
    tree::{ProjectionContext, Tree},
};
use crate::{
    adapters::terminals::{self, Terminals},
    core::layouts::{self, Layouts, Resolution, TerminalRef},
    protocol,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, String>;
const PREVIEW_LIMIT: usize = 48;
const PREVIEW_ROWS: usize = 200;
const PREVIEW_TEXT_BUDGET: usize = 1024 * 1024;
const DISPLAY_LIMIT_MESSAGE: &str = "Terminal display is incomplete; increase Terminal display size limit in settings or narrow discovery scope.";

#[derive(Clone, Debug)]
pub struct Observations {
    pub snapshot: Value,
    pub health: Value,
    pub local: Value,
    pub layouts: Layouts,
    pub now: f64,
    pub home: Option<PathBuf>,
    pub hostname: String,
    pub telegram_token_saved: Option<bool>,
}
impl Default for Observations {
    fn default() -> Self {
        Self {
            snapshot: json!({"panes":[],"clients":[],"errors":[]}),
            health: json!({}),
            local: json!({}),
            layouts: Layouts::default(),
            now: 0.0,
            home: None,
            hostname: String::new(),
            telegram_token_saved: None,
        }
    }
}

pub trait Source {
    fn resize_terminal(
        &mut self,
        _config: &ControlConfig,
        _node: &str,
        _request: super::terminal_resize::Request,
    ) -> Result<String> {
        Err("Pane resizing is unavailable for this source".into())
    }
    fn prepare_pane_launch(
        &mut self,
        _config: &ControlConfig,
        _request: super::pane_launch::Request,
    ) -> Result<super::pane_launch::Plan> {
        Err("Guided hooked launch unavailable".into())
    }
    fn apply_pane_launch(
        &mut self,
        _config: &ControlConfig,
        _plan: super::pane_launch::Plan,
        _approved: &str,
    ) -> Result<crate::adapters::terminal_actions::Receipt> {
        Err("Guided hooked launch unavailable".into())
    }
    fn switch_instance(&mut self, _directory: &Path) -> Result<ControlConfig> {
        Err("Instance switching is unavailable".into())
    }
    fn is_demo(&self) -> bool {
        false
    }
    fn telegram(
        &mut self,
        _request: super::telegram_admin::Request,
    ) -> Result<super::telegram_admin::Reply> {
        Err("Telegram administration is unavailable for this source".into())
    }
    fn memory_configuration(&mut self) -> Result<Option<super::memory::Config>> {
        Err("Memory configuration is unavailable".into())
    }
    fn history_access(&mut self, _global: bool) -> Result<super::history::State> {
        Err("Control history inspection is unavailable".into())
    }
    fn history_archives(&mut self, _global: bool, _offset: usize) -> Result<Value> {
        Err("History archives unavailable".into())
    }
    fn history_archive_page(
        &mut self,
        _global: bool,
        _name: &str,
        _offset: usize,
        _unresolved: bool,
    ) -> Result<Value> {
        Err("History archive review unavailable".into())
    }
    fn history_page(&mut self, _global: bool, _offset: usize, _unresolved: bool) -> Result<Value> {
        Err("Control history is unavailable".into())
    }
    fn apply_history(&mut self, _plan: super::history::Plan) -> Result<()> {
        Err("Control history editing is unavailable".into())
    }
    fn dashboard_state(&mut self) -> Result<super::dashboard::State> {
        Err("Dashboard settings are unavailable for this source".into())
    }
    fn prepare_dashboard_recovery(&mut self) -> Result<super::dashboard::Plan> {
        Err("Dashboard recovery is unavailable for this source".into())
    }
    fn configuration_file(&mut self, _expected: &ControlConfig) -> Result<PathBuf> {
        Err("Configuration editing is unavailable for this source".into())
    }
    fn prepare_configuration(
        &mut self,
        _expected: &ControlConfig,
        _updated: ControlConfig,
    ) -> Result<super::setup::SetupPlan> {
        Err("Configuration editing is unavailable for this source".into())
    }
    fn apply_configuration(&mut self, _plan: super::setup::SetupPlan) -> Result<ControlConfig> {
        Err("Configuration editing is unavailable for this source".into())
    }
    fn apply_dashboard(&mut self, _plan: super::dashboard::Plan) -> Result<()> {
        Err("Dashboard settings are unavailable for this source".into())
    }
    fn prepare_gateway(
        &mut self,
        _expected: Option<super::gateway::Config>,
        _updated: super::gateway::Config,
    ) -> Result<super::gateway::Plan> {
        Err("Gateway configuration is unavailable".into())
    }
    fn apply_gateway(&mut self, _plan: super::gateway::Plan) -> Result<String> {
        Err("Gateway configuration is unavailable".into())
    }
    fn memory_registrations(&mut self) -> Result<Vec<super::hook_runtime::Registration>> {
        Err("Memory registrations are unavailable".into())
    }
    fn memory_settings(
        &mut self,
        _registration: &Path,
    ) -> Result<(
        super::hook_runtime::Registration,
        Option<super::memory::Config>,
    )> {
        Err("Memory configuration is unavailable".into())
    }
    fn prepare_memory(
        &mut self,
        _expected: Option<super::memory::Config>,
        _config: super::memory::Config,
    ) -> Result<super::memory::Plan> {
        Err("Memory configuration is unavailable".into())
    }
    fn apply_memory(&mut self, _plan: super::memory::Plan) -> Result<Value> {
        Err("Memory configuration is unavailable".into())
    }
    fn prepare_herdr_hooks(
        &mut self,
        _config: &ControlConfig,
        _harness: &str,
    ) -> Result<super::herdr_hooks::Plan> {
        Err("Herdr integration installation is unavailable".into())
    }
    fn apply_herdr_hooks(
        &mut self,
        _plan: super::herdr_hooks::Plan,
        _approval: &str,
    ) -> Result<super::herdr_hooks::Receipt> {
        Err("Herdr integration installation is unavailable".into())
    }
    fn prepare_hook_reload(&mut self) -> Result<super::hook_reload::Plan> {
        Err("Hook reload is unavailable".into())
    }
    fn apply_hook_reload(&mut self, _plan: super::hook_reload::Plan) -> Result<Value> {
        Err("Hook reload is unavailable".into())
    }
    fn managed_hooks(&mut self, _harness: &str) -> Result<Vec<super::hook_uninstall::Entry>> {
        Err("Managed hook removal is unavailable".into())
    }
    fn prepare_hook_removal(&mut self, _path: &Path) -> Result<super::hook_uninstall::Plan> {
        Err("Managed hook removal is unavailable".into())
    }
    fn apply_hook_removal(
        &mut self,
        _plan: super::hook_uninstall::Plan,
        _approval: &str,
    ) -> Result<PathBuf> {
        Err("Managed hook removal is unavailable".into())
    }
    fn rollback_hook_removal(&mut self, _path: &Path) -> Result<()> {
        Err("Managed hook rollback is unavailable".into())
    }
    fn default_hook_spec(
        &mut self,
        _config: &ControlConfig,
        _pane: &Value,
    ) -> Result<Option<super::hook_menu::Spec>> {
        Ok(None)
    }
    fn prepare_hooks(
        &mut self,
        _config: &ControlConfig,
        _spec: super::hook_menu::Spec,
    ) -> Result<super::hook_install::Preview> {
        Err("Hook installation is unavailable for this source".into())
    }
    fn apply_hooks(
        &mut self,
        _plan: super::hook_install::Preview,
    ) -> Result<super::hook_install::Receipt> {
        Err("Hook installation is unavailable for this source".into())
    }
    fn rollback_hooks(&mut self, _receipt: super::hook_install::Receipt) -> Result<()> {
        Err("Hook rollback is unavailable for this source".into())
    }
    fn cutover(&mut self, _destination: &Path, _approval: Option<&str>) -> Result<Value> {
        Err("Cutover is unavailable".into())
    }
    fn rollback_cutover(&mut self, _source: &Path, _approval: &str) -> Result<Value> {
        Err("Cutover rollback is unavailable".into())
    }
    fn prepare_setup(
        &mut self,
        _config: &ControlConfig,
        _spec: super::wizard_menu::Spec,
    ) -> Result<super::wizard::Wizard> {
        Err("Setup is unavailable for this source".into())
    }
    fn detect_harnesses(
        &mut self,
        _config: &ControlConfig,
    ) -> Vec<super::harness_detection::Detected> {
        Vec::new()
    }
    fn finish_local_setup(&mut self, _config: &ControlConfig) -> Result<ControlConfig> {
        Err("Local setup is unavailable for this source".into())
    }
    fn download_setup(&mut self, _wizard: &mut super::wizard::Wizard) -> Result<()> {
        Err("Installer download is unavailable for this source".into())
    }
    fn apply_setup(
        &mut self,
        _wizard: super::wizard::Wizard,
        _approved: &str,
    ) -> Result<(Value, Option<ControlConfig>)> {
        Err("Setup application is unavailable for this source".into())
    }
    fn refresh(&mut self, config: &ControlConfig) -> Result<Observations>;
    fn preview(&mut self, terminal: &Value, rows: usize) -> Result<String>;
    fn previews(&mut self, targets: &[Value], rows: usize) -> Vec<Result<String>> {
        targets
            .iter()
            .map(|pane| self.preview(pane, rows))
            .collect()
    }
    fn save_layouts(&mut self, layouts: &Layouts) -> Result<()>;
    fn session(
        &mut self,
        _config: &ControlConfig,
        _request: session_menu::Request,
    ) -> Result<session_menu::Answer> {
        Err("Session operations are unavailable for this source".into())
    }
}

/// Pure state frame assembly: config, observations and explicit UI state only.
pub fn assemble(
    context: &Context<'_>,
    tree_context: &ProjectionContext,
    page: usize,
    message: &str,
    include_catalog: bool,
    restore_view: bool,
) -> Result<Value> {
    if page >= registry::PAGES {
        return Err("Unknown page".into());
    }
    let mut projected = presentation::project(context);
    let unavailable = ("Unavailable".to_owned(), "warn".to_owned());
    for section in &mut projected.sections {
        for item in &mut section.items {
            if registry::builtin().action(&item.id).is_some()
                && item.id != "instances.saved"
                && !session_menu::available(&item.id)
            {
                item.badges.push(unavailable.clone());
                item.note = format!("{} · Not available for this operation", item.note);
            }
        }
    }
    let mut terminal_actions = registry::builtin().terminal_actions();
    for action in &mut terminal_actions {
        if session_menu::available(&action.id) {
            continue;
        }
        action.badges.push(unavailable.clone());
        action.note = format!("{} · Not available for this operation", action.note);
    }
    let topology = Tree::build(
        context.snapshot,
        context.health,
        &ProjectionContext {
            runtime: context.config.runtime.clone(),
            checkpoint_matches: context.config.ui.checkpoint_matches,
            ..tree_context.clone()
        },
    );
    let mut identities = BTreeSet::new();
    let mut views = 0;
    for node in &topology.nodes {
        if node.kind == "pane"
            && let Some(pane) = &node.pane
            && let Ok(id) = terminals::identity(pane)
        {
            identities.insert((
                format!("{:?}", id.backend),
                id.endpoint,
                id.pane,
                id.generation,
            ));
            views += 1;
        }
    }
    let routes = context
        .health
        .get("routes")
        .and_then(Value::as_object)
        .map_or(0, |routes| routes.len());
    let checkpoints = array(context.health, "checkpoints").len();
    let warnings = array(context.snapshot, "errors").len()
        + array(context.health, "errors").len()
        + usize::from(topology.relationships_limited)
        + topology.layout_warnings.len()
        + topology
            .observation_warnings
            .iter()
            .filter(|warning| {
                !array(context.snapshot, "errors")
                    .iter()
                    .any(|entry| entry["error"].as_str() == Some(**warning))
            })
            .count();
    let message = if message.is_empty() && context.local["dashboard_preferences_available"] == false
    {
        "Dashboard preferences unavailable; open Settings > Dashboard and instance discovery to recover"
    } else {
        message
    };
    let mut frame = json!({"version":protocol::VERSION,"kind":"state","tree":[],"sections":projected.sections,"overview":projected.overview,"instance":context.config_dir.file_name().and_then(|s|s.to_str()).unwrap_or("instance"),"release":env!("CARGO_PKG_VERSION"),"demo":context.demo,"freshness":projected.freshness,"observed_at":context.last_success.unwrap_or(0.0),"terminals":identities.len(),"views":views,"routes":routes,"checkpoints":checkpoints,"warnings":warnings,"message":message,"page":page,"focus":"","terminal_actions":terminal_actions,"workspace":workspace_state(&topology,context.layouts,restore_view,context.refresh_error)});
    if !topology.observation_warnings.is_empty() {
        frame["observation_warnings"] = json!(topology.observation_warnings);
        let original = frame["message"].as_str().unwrap_or("");
        let warning = topology.observation_warnings.join("\n");
        frame["message"] = json!(if original.is_empty() {
            warning
        } else {
            format!("{original}\n{warning}")
        });
        if !context.demo && !context.refresh_error {
            let freshness = frame["freshness"].as_str().unwrap_or("");
            frame["freshness"] = json!(format!(
                "PARTIAL · {}",
                freshness
                    .split_once(" · ")
                    .map_or("observations conflict", |(_, age)| age)
            ));
        }
    }
    if !topology.layout_warnings.is_empty() {
        frame["layout_warnings"] = json!(topology.layout_warnings);
        let original = frame["message"].as_str().unwrap_or("");
        let reasons = topology.layout_warnings.join("\n");
        frame["message"] = json!(if original.is_empty() {
            reasons
        } else {
            format!("{original}\n{reasons}")
        });
        if !context.demo && !context.refresh_error {
            let freshness = frame["freshness"].as_str().unwrap_or("");
            frame["freshness"] = json!(format!(
                "PARTIAL · {}",
                freshness
                    .split_once(" · ")
                    .map_or("layout unavailable", |(_, age)| age)
            ));
        }
    }
    if topology.relationships_limited {
        frame["relationship_limited"] = json!(true);
        let original = frame["message"].as_str().unwrap_or("");
        frame["message"] = json!(if original.is_empty() {
            super::tree_relations::WARNING.into()
        } else {
            format!("{original}\n{}", super::tree_relations::WARNING)
        });
        if !context.demo && !context.refresh_error {
            let freshness = frame["freshness"].as_str().unwrap_or("");
            frame["freshness"] = json!(format!(
                "PARTIAL · {}",
                freshness
                    .split_once(" · ")
                    .map_or("relationship details incomplete", |(_, age)| age)
            ));
        }
    }
    frame["ui_settings"] =
        serde_json::to_value(&context.config.ui).map_err(|_| "Cannot encode interface settings")?;
    frame["agent_activity_max_age_ms"] = json!(context.config.runtime.agent_activity_max_age_ms);
    frame["resize_settings"] = serde_json::to_value(context.config.runtime.resize_policy())
        .map_err(|_| "Cannot encode resize settings")?;
    if include_catalog {
        let mut catalog = registry::builtin().catalog();
        for entry in &mut catalog {
            if registry::builtin()
                .catalog_action(&entry.id)
                .is_some_and(|action| !session_menu::available(&action.id))
            {
                entry.badges.push(unavailable.clone());
                entry.detail =
                    "Not available for this operation; this entry documents the planned action."
                        .into();
            }
        }
        frame["catalog"] =
            serde_json::to_value(catalog).map_err(|_| "Cannot encode command catalog")?;
    }
    let limit = (context.config.runtime.discovery_frame_limit_bytes as usize)
        .min(protocol::MAX_FRAME_BYTES);
    let (remaining, metadata_limited) = fit_state_metadata(&mut frame, limit.saturating_sub(512))?;
    let (rows, tree_limited) = topology.rows_bounded(tree_context, remaining);
    frame["tree"] = rows.into();
    if metadata_limited || tree_limited {
        frame["display_limited"] = json!(true);
        frame["warnings"] = json!(warnings.saturating_add(1));
        if !context.demo && !context.refresh_error {
            let freshness = frame["freshness"].as_str().unwrap_or("");
            frame["freshness"] = json!(format!(
                "PARTIAL · {}",
                freshness
                    .split_once(" · ")
                    .map_or("display limited", |(_, age)| age)
            ));
        }
        let message = frame["message"].as_str().unwrap_or("");
        frame["message"] = json!(if message.is_empty() {
            DISPLAY_LIMIT_MESSAGE.into()
        } else {
            format!("{message}\n{DISPLAY_LIMIT_MESSAGE}")
        });
    }
    super::discovery_budget::json_size(&frame, limit)?;
    Ok(frame)
}
/// Keep compiled controls available even when saved presentation metadata is large.
fn fit_state_metadata(frame: &mut Value, limit: usize) -> Result<(usize, bool)> {
    use super::discovery_budget::json_size;
    if let Ok(bytes) = json_size(frame, limit) {
        return Ok((limit - bytes, false));
    }
    let mut records = Vec::new();
    for (index, section) in frame["sections"]
        .as_array_mut()
        .ok_or("Invalid section metadata")?
        .iter_mut()
        .enumerate()
    {
        let items = std::mem::take(
            section["items"]
                .as_array_mut()
                .ok_or("Invalid item metadata")?,
        );
        for item in items {
            if item["id"]
                .as_str()
                .is_some_and(|id| registry::builtin().action(id).is_some())
            {
                section["items"].as_array_mut().unwrap().push(item);
            } else {
                records.push((index, item));
            }
        }
    }
    let pins = frame["workspace"]["pins"].take();
    frame["workspace"]["pins"] = json!([]);
    let apply = frame["workspace"]["apply"].take();
    frame["workspace"]["apply"] = json!({});
    if json_size(frame, limit).is_err() {
        let message = frame["message"].as_str().unwrap_or("");
        let mut end = message.len().min(8192);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        frame["message"] = json!(&message[..end]);
        frame["overview"] = json!({"connections":[],"setup":[]});
    }
    let base = json_size(frame, limit)
        .map_err(|_| "Compiled control metadata exceeds supported display size")?;
    let mut remaining = limit - base;
    if let Ok(bytes) = json_size(&apply, remaining.saturating_add(2)) {
        let added = bytes.saturating_sub(2);
        if added <= remaining {
            frame["workspace"]["apply"] = apply;
            remaining -= added;
        }
    }
    for pin in pins.as_array().into_iter().flatten() {
        let separator = usize::from(!frame["workspace"]["pins"].as_array().unwrap().is_empty());
        if let Ok(bytes) = json_size(pin, remaining.saturating_sub(separator)) {
            remaining -= bytes + separator;
            frame["workspace"]["pins"]
                .as_array_mut()
                .unwrap()
                .push(pin.clone());
        }
    }
    for (index, item) in records {
        let items = frame["sections"][index]["items"].as_array_mut().unwrap();
        let separator = usize::from(!items.is_empty());
        if let Ok(bytes) = json_size(&item, remaining.saturating_sub(separator)) {
            remaining -= bytes + separator;
            items.push(item);
        }
    }
    Ok((remaining, true))
}
fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn reference(pane: &Value) -> Result<TerminalRef> {
    Ok(TerminalRef {
        label: super::tree::title(pane, ""),
        place: pane
            .get("session")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
        ..terminals::reference(pane)?
    })
}
fn workspace_state(tree: &Tree, layouts: Option<&Layouts>, restore: bool, stale: bool) -> Value {
    let Some(layouts) = layouts else {
        return json!({"pins":[],"apply":{}});
    };
    let mut terminals: Vec<(TerminalRef, String)> = Vec::new();
    let mut unique_terminals = std::collections::HashSet::new();
    let mut conflicting = std::collections::HashSet::new();
    for node in &tree.nodes {
        if let Some(pane) = &node.pane
            && crate::adapters::pane_observations::conflicted(pane)
            && let Ok(id) = terminals::identity(pane)
        {
            conflicting.insert(id);
        }

        if node.kind == "pane"
            && let Some(pane) = &node.pane
            && let Ok(reference) = reference(pane)
            && unique_terminals.insert((
                reference.backend.clone(),
                reference.server.clone(),
                reference.pane_id.clone(),
                reference.terminal_id.clone(),
                reference.generation.clone(),
            ))
        {
            terminals.push((reference, node.id()));
        }
    }
    let live: Vec<_> = terminals
        .iter()
        .map(|(reference, _)| reference.clone())
        .collect();
    let resolve = |reference: &TerminalRef| {
        if stale {
            ("unverified", String::new(), "Inventory is stale; refresh")
        } else if reference
            .identity()
            .is_ok_and(|identity| conflicting.contains(&identity))
        {
            (
                "ambiguous",
                String::new(),
                crate::adapters::pane_observations::WARNING,
            )
        } else {
            match layouts::resolve(reference, &live) {
                Resolution::Live(index) => ("live", terminals[index].1.clone(), ""),
                Resolution::Missing => ("missing", String::new(), "Terminal is not present"),
                Resolution::Stale => ("stale", String::new(), "Terminal identity changed"),
                Resolution::Ambiguous => {
                    ("ambiguous", String::new(), "More than one terminal matches")
                }
            }
        }
    };
    let pins:Vec<_>=layouts.current.pins.iter().map(|reference| { let (status,id,reason)=resolve(reference);json!({"id":id,"label":reference.label,"place":reference.place,"status":status,"reason":reason,"key":reference.key()}) }).collect();
    let mut apply = json!({});
    if restore {
        apply = serde_json::to_value(&layouts.current).unwrap_or(json!({}));
        apply.as_object_mut().unwrap().remove("pins");
        apply["selected"] = json!(
            layouts
                .current
                .selected
                .as_ref()
                .map(|reference| resolve(reference).1)
                .unwrap_or_default()
        );
    }
    json!({"pins":pins,"apply":apply})
}

pub struct Backend<S: Source> {
    pub config: ControlConfig,
    pub directory: PathBuf,
    pub observations: Observations,
    pub source: S,
    pub page: usize,
    pub show_saved: bool,
    pub message: String,
    pub refresh_error: bool,
    last_success: Option<f64>,
    catalog_sent: bool,
    restore_pending: bool,
    sequence: u64,
    pending_dialog: Option<u64>,
    menu: SessionMenu,
}
impl<S: Source> Backend<S> {
    pub fn new(directory: PathBuf, config: ControlConfig, source: S) -> Result<Self> {
        layouts::check_path(&directory)?;
        config.validate()?;
        Ok(Self {
            config,
            directory,
            observations: Observations::default(),
            source,
            page: 0,
            show_saved: false,
            message: String::new(),
            refresh_error: false,
            last_success: None,
            catalog_sent: false,
            restore_pending: true,
            sequence: 0,
            pending_dialog: None,
            menu: SessionMenu::default(),
        })
    }
    fn tree_context(&self) -> ProjectionContext {
        ProjectionContext {
            runtime: self.config.runtime.clone(),
            checkpoint_matches: self.config.ui.checkpoint_matches,
            home: self
                .observations
                .home
                .as_ref()
                .map(|home| home.to_string_lossy().into_owned())
                .unwrap_or_default(),
            hostname: self.observations.hostname.clone(),
            now: self.observations.now,
            pins: self
                .observations
                .layouts
                .current
                .pins
                .iter()
                .filter_map(|pin| serde_json::to_value(pin).ok())
                .collect(),
        }
    }
    pub fn topology(&self) -> Tree {
        Tree::build(
            &self.observations.snapshot,
            &self.observations.health,
            &self.tree_context(),
        )
    }
    pub fn state(&mut self) -> Result<Value> {
        let context = Context {
            config: &self.config,
            config_dir: &self.directory,
            home: self.observations.home.as_deref(),
            snapshot: &self.observations.snapshot,
            health: &self.observations.health,
            local: &self.observations.local,
            layouts: (self.observations.health["layouts_available"] != false)
                .then_some(&self.observations.layouts),
            show_saved: self.show_saved,
            demo: self.source.is_demo(),
            refresh_error: self.refresh_error,
            pending: false,
            last_success: self.last_success,
            now: self.observations.now,
            telegram_token_saved: self.observations.telegram_token_saved,
        };
        let frame = assemble(
            &context,
            &self.tree_context(),
            self.page,
            &self.message,
            !self.catalog_sent,
            self.restore_pending,
        )?;
        if let Some(health) = self.observations.health.as_object_mut() {
            if frame["observation_warnings"].is_array() {
                health.insert(
                    "observation_warnings".into(),
                    frame["observation_warnings"].clone(),
                );
            } else {
                health.remove("observation_warnings");
            }
            if frame["layout_warnings"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
            {
                health.insert("layout_warnings".into(), frame["layout_warnings"].clone());
            } else {
                health.remove("layout_warnings");
            }
            if frame["relationship_limited"] == true {
                health.insert(
                    "projection_warning".into(),
                    json!(super::tree_relations::WARNING),
                );
            } else {
                health.remove("projection_warning");
            }
            if frame["display_limited"] == true {
                health.insert("display_warning".into(), json!(DISPLAY_LIMIT_MESSAGE));
            } else {
                health.remove("display_warning");
            }
        }
        self.catalog_sent = true;
        self.restore_pending = false;
        Ok(frame)
    }
    pub fn refresh(&mut self) -> Result<Value> {
        self.refresh_with_message("")
    }
    fn refresh_with_message(&mut self, message: &str) -> Result<Value> {
        match self.source.refresh(&self.config) {
            Ok(mut observations) if observations.now.is_finite() && observations.now >= 0.0 => {
                if self.last_success.is_some() {
                    observations.layouts.current = self.observations.layouts.current.clone();
                }
                self.last_success = Some(observations.now);
                self.observations = observations;
                self.refresh_error = false;
                self.message = message.into();
            }
            _ => {
                self.refresh_error = true;
                self.message = "Refresh failed; previous observations are stale".into();
            }
        }
        self.state()
    }
    fn observe_view(&mut self, value: &Value, force_save: bool) -> Result<()> {
        if value.is_null() {
            return Ok(());
        }
        let object = value.as_object().ok_or("Invalid view request")?;
        let mut current = self.observations.layouts.current.clone();
        current.mode =
            serde_json::from_value(object.get("mode").cloned().ok_or("Missing view mode")?)
                .map_err(|_| "Invalid view mode")?;
        let selected = object
            .get("selected")
            .and_then(Value::as_str)
            .ok_or("Missing selected terminal")?;
        current.selected = if selected.is_empty() {
            None
        } else {
            let topology = self.topology();
            // Browsing preferences and quit must remain usable when the last
            // selection disappears or becomes ambiguous. Never persist a new
            // reference inferred from that selection.
            topology
                .find(selected)
                .and_then(|node| node.pane.as_ref())
                .and_then(|pane| reference(pane).ok())
        };
        if let Some(sizes) = object.get("sizes") {
            current.sizes = Some(
                serde_json::from_value::<layouts::Sizes>(sizes.clone())
                    .map_err(|_| "Invalid panel sizes")?
                    .normalized(),
            );
        }
        for (key, value) in object {
            if !matches!(key.as_str(), "mode" | "selected" | "sizes") {
                current.preferences.insert(key.clone(), value.clone());
            }
        }
        current.validate().map_err(|_| "Invalid view settings")?;
        let mut old = serde_json::to_value(&self.observations.layouts.current)
            .map_err(|_| "Cannot encode previous view")?;
        let mut new = serde_json::to_value(&current).map_err(|_| "Cannot encode view")?;
        old.as_object_mut().unwrap().remove("selected");
        new.as_object_mut().unwrap().remove("selected");
        let mut changed = self.observations.layouts.clone();
        changed.current = current;
        if force_save || old != new {
            self.source
                .save_layouts(&changed)
                .map_err(|_| "View could not be saved")?;
        }
        self.observations.layouts = changed;
        Ok(())
    }
    fn preview(&mut self, request: &Value) -> Result<Value> {
        let requested = request
            .get("nodes")
            .and_then(Value::as_array)
            .ok_or("Invalid preview targets")?;
        if requested.len() > PREVIEW_LIMIT || requested.iter().any(|id| !id.is_string()) {
            return Err("Preview target limit exceeded or invalid target".into());
        }
        let rows = request
            .get("rows")
            .and_then(Value::as_u64)
            .ok_or("Invalid preview rows")?
            .clamp(1, PREVIEW_ROWS as u64) as usize;
        let tree = self.topology();
        let mut seen = BTreeSet::new();
        let mut items = Vec::new();
        let mut targets = Vec::new();
        let mut slots = Vec::new();
        for id in requested.iter().filter_map(Value::as_str) {
            if !seen.insert(id) {
                continue;
            }
            let pane = tree
                .find(id)
                .filter(|node| node.kind == "pane")
                .and_then(|node| node.pane.as_ref());
            if let Some(pane) = pane
                && !self.refresh_error
                && !crate::adapters::pane_observations::conflicted(pane)
            {
                slots.push(items.len());
                targets.push(pane.clone());
                items.push(json!({"id":id}));
                continue;
            }
            let error = if pane.is_none() {
                "Terminal disappeared; refresh"
            } else if pane.is_some_and(crate::adapters::pane_observations::conflicted) {
                crate::adapters::pane_observations::WARNING
            } else {
                "Inventory is stale; refresh before reading"
            };
            items.push(json!({"id":id,"error":error}));
        }
        let mut remaining = PREVIEW_TEXT_BUDGET;
        let mut results = self.source.previews(&targets, rows).into_iter();
        for slot in slots {
            match results.next() {
                Some(Ok(text)) if remaining > 0 => {
                    let text = tail(&text, rows, remaining.min(192_000));
                    remaining -= text.len();
                    items[slot]["text"] = json!(text);
                }
                Some(Ok(_)) => items[slot]["error"] = json!("Preview response limit reached"),
                _ => {
                    items[slot]["error"] =
                        json!("Terminal preview unavailable; refresh and inspect diagnostics")
                }
            }
        }
        Ok(json!({"version":protocol::VERSION,"kind":"previews","items":items}))
    }
    fn action(&mut self, request: &Value) -> Result<Vec<Value>> {
        let page = request
            .get("page")
            .and_then(Value::as_u64)
            .filter(|page| *page < registry::PAGES as u64)
            .ok_or("Unknown page")? as usize;
        let requested_action = request
            .get("action")
            .and_then(Value::as_str)
            .ok_or("Unknown action")?;
        let action = registry::builtin()
            .catalog_action(requested_action)
            .map(|action| action.id.as_str())
            .unwrap_or(requested_action);
        if let Some(view) = request.get("view") {
            self.observe_view(view, false)?;
        }
        if page == registry::COMMANDS && action == requested_action {
            if registry::builtin()
                .catalog_action(requested_action)
                .is_some()
            {
                return Err("This guided action is unsupported by this operation".into());
            }
            let entry = registry::builtin()
                .catalog()
                .into_iter()
                .find(|entry| entry.id == action)
                .ok_or("Unknown catalog entry")?;
            self.page = page;
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or("Dialog sequence exhausted")?;
            self.pending_dialog = Some(self.sequence);
            return Ok(vec![
                json!({"version":protocol::VERSION,"kind":"dialog","id":self.sequence,"mode":"output","title":entry.title,"text":format!("{}\n\n{}\n\n{}",entry.interface,entry.description,entry.detail)}),
            ]);
        }
        if let Some(directory) = action.strip_prefix("instance:") {
            if self.source.is_demo() {
                return Err("Demo does not open private configurations".into());
            }
            let directory = PathBuf::from(directory);
            layouts::check_path(&directory)?;
            // Selection comes from the last observed instance list, never an
            // arbitrary protocol-supplied path outside that inventory.
            if !self.observations.local["instances"]
                .as_array()
                .is_some_and(|rows| {
                    rows.iter()
                        .any(|r| r["directory"].as_str() == directory.to_str())
                })
            {
                return Err("Instance disappeared; refresh its configuration sources".into());
            }
            let config = self.source.switch_instance(&directory)?;
            self.directory = directory;
            self.config = config;
            self.menu.cancel();
            self.pending_dialog = None;
            self.observations = Observations::default();
            self.page = registry::OVERVIEW;
            self.restore_pending = true;
            return Ok(vec![
                self.refresh_with_message("Managing the selected private instance")?,
            ]);
        }
        if action == "instances.saved" && page == registry::INSTANCES {
            self.show_saved = !self.show_saved;
            self.page = page;
            return Ok(vec![self.state()?]);
        }
        if let Some(node) = request.get("node").and_then(Value::as_str)
            && self.topology().find(node).is_none()
        {
            return Err("Terminal disappeared; select it again".into());
        }
        if self.refresh_error
            && !super::telegram_menu::available(action)
            && !super::wizard_menu::available(action)
            && !super::harness_menu::available(action)
            && action != "settings.interface"
            && action != "settings.runtime"
            && action != "settings.dashboard"
            && action != "settings.history"
            && ![
                "help.guide",
                "system.update",
                "help.reference",
                "activity.events",
                "activity.discovery",
                "activity.health",
                "instances.sources",
            ]
            .contains(&action)
        {
            return Err("Inventory is stale; refresh before acting".into());
        }
        let topology = self.topology();
        let selected_id = request
            .get("node")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let selected = topology.find(selected_id);
        if action == "terminal.resize" {
            if selected
                .and_then(|node| node.pane.as_ref())
                .is_some_and(crate::adapters::pane_observations::conflicted)
            {
                return Err(crate::adapters::pane_observations::WARNING.into());
            }
            let resize = serde_json::from_value(
                request
                    .get("resize")
                    .cloned()
                    .ok_or("Missing resize request")?,
            )
            .map_err(|_| "Invalid resize request")?;
            let message = self
                .source
                .resize_terminal(&self.config, selected_id, resize)?;
            return Ok(vec![self.refresh_with_message(&message)?]);
        }
        let view = session_menu::View {
            snapshot: self.observations.snapshot.clone(),
            health: self.observations.health.clone(),
            layouts: self.observations.layouts.clone(),
            selected: selected
                .and_then(|node| node.pane.clone())
                .filter(|pane| !crate::adapters::pane_observations::conflicted(pane)),
            checkpoint: selected
                .and_then(|node| node.attributes.get("checkpoint"))
                .and_then(|row| row.get("key"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            node: selected_id.into(),
        };
        if session_menu::available(action)
            || action.starts_with("hooks:")
            || action.starts_with("switch:")
            || action == "goal:"
            || action.starts_with("armonia:")
            || action.starts_with("instance:")
            || action.starts_with("checkpoint:")
            || action.starts_with("layout:")
            || action.starts_with("workflow:")
            || action.starts_with("unpin:")
            || (!selected_id.is_empty() && action == selected_id)
        {
            self.page = if page == registry::COMMANDS {
                registry::builtin()
                    .action(action)
                    .map(|a| a.page)
                    .unwrap_or(page)
            } else {
                page
            };
            let outcome = self.menu.start(
                action,
                view,
                &mut self.source,
                &self.config,
                &mut self.sequence,
            )?;
            return self.menu_outcome(outcome);
        }
        Err("This action is unsupported by this operation; no terminal action was performed".into())
    }
    fn menu_outcome(&mut self, outcome: session_menu::Outcome) -> Result<Vec<Value>> {
        if let Some(directory) = outcome.setup_directory {
            let config = self.source.switch_instance(&directory)?;
            self.directory = directory;
            self.config = config;
            self.observations = Observations::default();
            self.restore_pending = true;
        } else if let Some(configuration) = outcome.configuration {
            self.config = configuration;
        }
        if let Some(layouts) = outcome.layouts {
            self.observations.layouts = layouts;
        }
        if outcome.restore {
            self.restore_pending = true;
        }
        if !outcome.message.is_empty() {
            self.message = outcome.message.clone();
        }
        if !outcome.frames.is_empty() {
            let mut frames = outcome.frames;
            if outcome.refresh {
                frames.push(self.refresh_with_message(&outcome.message)?);
            }
            return Ok(frames);
        }
        Ok(vec![if outcome.refresh {
            self.refresh_with_message(&outcome.message)?
        } else {
            self.state()?
        }])
    }
    /// Returns frames and whether the transport should close. Dialog replies are
    /// accepted only for the one outstanding request and valid output values.
    pub fn handle(&mut self, request: &Value) -> Result<(Vec<Value>, bool)> {
        if request.get("version").and_then(Value::as_u64) != Some(protocol::VERSION) {
            return Err("Unsupported protocol version".into());
        }
        let op = request
            .get("op")
            .and_then(Value::as_str)
            .ok_or("Missing operation")?;
        // Layout persistence is independent of the outstanding dialog reply.
        if op == "view" {
            self.observe_view(request.get("view").ok_or("Missing view")?, false)?;
            return Ok((vec![], false));
        }
        // Read requests can already be in flight when a dialog is opened.
        // They never consume or replace its pending reply.
        if op == "refresh" {
            return Ok((vec![self.refresh()?], false));
        }
        if op == "previews" {
            return Ok((vec![self.preview(request)?], false));
        }
        if self.menu.pending() {
            if op == "quit" {
                self.menu.cancel();
                return Ok((vec![], true));
            }
            let outcome =
                self.menu
                    .reply(request, &mut self.source, &self.config, &mut self.sequence)?;
            return Ok((self.menu_outcome(outcome)?, false));
        }
        if let Some(expected) = self.pending_dialog {
            if op == "quit" {
                self.pending_dialog = None;
                return Ok((vec![], true));
            }
            if op != "reply"
                || request.get("id").and_then(Value::as_u64) != Some(expected)
                || !(request.get("value") == Some(&Value::Null)
                    || request.get("value").and_then(Value::as_str) == Some("close"))
            {
                return Err("Unexpected dialog response".into());
            }
            self.pending_dialog = None;
            return Ok((vec![self.state()?], false));
        }
        match op {
            "quit" => {
                if let Some(view) = request.get("view") {
                    self.observe_view(view, true)?;
                }
                Ok((vec![], true))
            }
            "action" => Ok((self.action(request)?, false)),
            _ => Err("Unknown operation".into()),
        }
    }
}
fn tail(text: &str, rows: usize, budget: usize) -> String {
    let mut selected = Vec::new();
    let mut used = 0;
    for line in text.lines().rev().take(rows) {
        let mut line = line;
        if line.len() > budget.saturating_sub(used + usize::from(!selected.is_empty())) {
            let keep = budget.saturating_sub(used + usize::from(!selected.is_empty()));
            let mut start = line.len().saturating_sub(keep);
            while start < line.len() && !line.is_char_boundary(start) {
                start += 1;
            }
            line = &line[start..];
        }
        if line.is_empty() && used >= budget {
            break;
        }
        used += line.len() + usize::from(!selected.is_empty());
        selected.push(line);
        if used >= budget {
            break;
        }
    }
    selected.reverse();
    selected.join("\n")
}
fn emit(output: &mut impl Write, frame: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(frame).map_err(|_| "Cannot encode backend frame")?;
    if bytes.len() > protocol::MAX_FRAME_BYTES {
        return Err("Backend frame exceeds protocol limit".into());
    }
    output
        .write_all(&bytes)
        .and_then(|()| output.write_all(b"\n"))
        .and_then(|()| output.flush())
        .map_err(|_| "Cannot write backend frame".into())
}
pub fn run_with<S: Source>(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    backend: &mut Backend<S>,
) -> Result<()> {
    emit(writer, &backend.refresh()?)?;
    run_requests(reader, writer, backend)
}
fn run_requests<S: Source>(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    backend: &mut Backend<S>,
) -> Result<()> {
    while let Some(request) = protocol::read_frame(reader, protocol::MAX_FRAME_BYTES)
        .map_err(|_| "Invalid or oversized backend request")?
    {
        match backend.handle(&request) {
            Ok((frames, quit)) => {
                for frame in frames {
                    emit(writer, &frame)?;
                }
                if quit {
                    break;
                }
            }
            Err(message) => emit(
                writer,
                &json!({"version":protocol::VERSION,"kind":"error","message":message}),
            )?,
        }
    }
    Ok(())
}

/// Fixed worker pool; result order follows requests regardless of completion order.
fn bounded_previews(
    targets: &[Value],
    capture: impl Fn(&Value) -> Result<String> + Sync,
) -> Vec<Result<String>> {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let next = AtomicUsize::new(0);
    let results = Mutex::new(
        (0..targets.len())
            .map(|_| Err("Preview unavailable".into()))
            .collect::<Vec<Result<String>>>(),
    );
    std::thread::scope(|scope| {
        for _ in 0..targets.len().min(6) {
            let capture = &capture;
            let next = &next;
            let results = &results;
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(target) = targets.get(index) else {
                        break;
                    };
                    let result = capture(target);
                    results.lock().expect("preview result lock")[index] = result;
                }
            });
        }
    });
    results.into_inner().expect("preview result lock")
}

/// Observe only routes belonging to this instance's explicitly configured chats.
/// Reading never creates route schemas, changes grants or recovers service state.
fn native_route_observation(directory: &Path, telegram: &Value) -> Result<Value> {
    if telegram["status"] == "unknown" {
        return Err("Telegram configuration is unavailable".into());
    }
    if telegram["configured"] != true {
        return Ok(json!({"status":"unconfigured","routes":{},"native_routes":[]}));
    }
    let file = directory.join("telegram-service.json");
    let before = super::telegram_config::read_private(&file, 1024 * 1024)?
        .ok_or("Telegram configuration disappeared")?;
    let config: super::telegram_config::ServiceConfig =
        serde_json::from_slice(&before).map_err(|_| "Invalid Telegram configuration")?;
    config.validate()?;
    if config.preview() != telegram["configuration"] {
        return Err("Telegram configuration changed during refresh".into());
    }
    let chats = config
        .grants
        .iter()
        .map(|grant| grant.chat)
        .collect::<BTreeSet<_>>();
    let observed = super::diagnostics::with_database(&directory.join("routes.sqlite3"), |con| {
        let mut routes = Vec::new();
        for chat in chats {
            let scope = crate::core::routes::Scope {
                instance: config.instance.clone(),
                bot: config.bot_id.to_string(),
                chat: chat.to_string(),
            };
            if let Some(route) = crate::core::routes::read_route(con, &scope)
                .map_err(|_| "Saved route schema is unavailable")?
            {
                route
                    .terminal
                    .validate()
                    .map_err(|_| "Saved route identity is invalid")?;
                if route.revision <= 0 {
                    return Err("Saved route revision is invalid".into());
                }
                routes.push(route);
            }
        }
        Ok(routes)
    })?;
    if super::telegram_config::read_private(&file, 1024 * 1024)?.as_deref()
        != Some(before.as_slice())
    {
        return Err("Telegram configuration changed during route observation".into());
    }
    let status = if observed.is_some() { "ok" } else { "missing" };
    let routes = observed.unwrap_or_default();
    let by_chat = routes
        .iter()
        .map(|route| {
            (
                route.scope.chat.clone(),
                serde_json::to_value(&route.terminal).expect("Serializable terminal identity"),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Ok(json!({"status":status,"routes":by_chat,"native_routes":routes}))
}
fn telegram_observation(directory: &Path, config: &ControlConfig) -> (Value, Value, Option<bool>) {
    let telegram = super::telegram_admin::inspect(directory).unwrap_or_else(
        |_| json!({"configured":false,"status":"unknown","service":{"state":"unknown"}}),
    );
    let telegram_token_saved = telegram["token_locally_valid"].as_bool();
    let service = &telegram["service"];
    let services = match service["state"].as_str() {
        Some("running") => {
            json!([{"kind":"Telegram feeder","state":"Running","pid":service["process"]["pid"],"start":service["process"]["start_ticks"].as_u64().map(|v|v.to_string()).unwrap_or_default()}])
        }
        Some("stopped") => json!([]),
        _ => Value::Null,
    };
    let local = json!({"instances":[{"directory":directory,"name":config.agent_name,"files":["control.json"],"services":services}]});
    (telegram, local, telegram_token_saved)
}
pub struct NativeSource {
    pub directory: PathBuf,
    pub terminals: Terminals,
    pub last_snapshot: Value,
    pub usage_recorder: super::workspace_usage::Recorder,
}
fn hook_profile_path(harness: &str, observed: Option<&str>, configured: &str) -> Option<PathBuf> {
    let directory = observed
        .filter(|p| !p.is_empty())
        .or_else(|| (!configured.is_empty()).then_some(configured))?;
    let filename = match harness {
        "codex" => "hooks.json",
        "claude" => "settings.json",
        "hermes" => "config.yaml",
        _ => return None,
    };
    Some(PathBuf::from(directory).join(filename))
}

fn opencode_hook_path(environment: &BTreeMap<String, String>, identity: &[u8]) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    // XDG_DATA_HOME holds OpenCode sessions/accounts, not native plugins.
    let root = environment
        .get("OPENCODE_CONFIG_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            environment
                .get("XDG_CONFIG_HOME")
                .filter(|s| !s.is_empty())
                .map(|p| PathBuf::from(p).join("opencode"))
        })
        .or_else(|| {
            environment
                .get("HOME")
                .map(|p| PathBuf::from(p).join(".config/opencode"))
        })
        .ok_or("Cannot locate the OpenCode configuration directory")?;
    let path = root
        .join("plugins")
        .join(format!("daedalus-{:x}.js", Sha256::digest(identity)));
    layouts::check_path(&path)?;
    Ok(path)
}
fn hook_instance(directory: &Path, config: &ControlConfig) -> Result<String> {
    let path = directory.join("telegram-service.json");
    if path.exists() {
        Ok(super::telegram_config::ServiceConfig::load(&path)?.instance)
    } else {
        Ok(config.agent_name.clone())
    }
}

impl NativeSource {
    fn history_directory(&self, global: bool) -> Result<PathBuf> {
        if global {
            let cwd = std::env::current_dir().map_err(|_| "Current directory unavailable")?;
            super::startup::preference_directory(&cwd, |key| std::env::var_os(key))
        } else {
            Ok(self.directory.clone())
        }
    }

    fn herdr_hook_request(
        &self,
        config: &ControlConfig,
        harness: &str,
    ) -> Result<super::herdr_hooks::Request> {
        use crate::adapters::harnesses;
        if !["claude", "codex", "grok"].contains(&harness) {
            return Err("No verified Herdr hook installer for this harness".into());
        }
        let environment = crate::platform::environment();
        let home = PathBuf::from(
            environment
                .get("HOME")
                .ok_or("HOME is required for Herdr integration")?,
        );
        let tool = harnesses::tool(harness).ok_or("Unknown harness")?;
        let profile = harnesses::profile_directory(&tool, &environment, &config.profiles)?;
        let candidate = if self.terminals.herdr.is_absolute() {
            self.terminals.herdr.clone()
        } else {
            std::env::split_paths(environment.get("PATH").map(String::as_str).unwrap_or(""))
                .filter(|p| p.is_absolute())
                .map(|p| p.join(&self.terminals.herdr))
                .find(|p| p.is_file())
                .ok_or("Herdr executable is unavailable")?
        };
        let binary =
            std::fs::canonicalize(candidate).map_err(|_| "Cannot resolve Herdr executable")?;
        Ok(super::herdr_hooks::Request {
            binary,
            home,
            profile,
            directory: self.directory.clone(),
            harness: harness.into(),
        })
    }
    fn require_configuration(&self) -> Result<()> {
        ControlConfig::parse(
            &layouts::read_private(&self.directory.join("control.json"))?
                .ok_or("Complete setup before changing this instance")?,
        )?;
        Ok(())
    }

    fn attach_context(
        &self,
        environment: &BTreeMap<String, String>,
        node: String,
    ) -> Result<super::session_actions::AttachContext> {
        let mut guard = self.terminals.metadata_guard()?;
        let mut ancestors = BTreeSet::new();
        let mut pid = std::process::id();
        for depth in 0..=guard.policy.depth {
            guard.check()?;
            if depth == guard.policy.depth {
                return Err(
                    "Native metadata ancestry limit reached; review native_metadata_depth_limit"
                        .into(),
                );
            }
            if !ancestors.insert(pid.to_string()) {
                return Err("Process ancestry has cycles".into());
            }
            // macOS has no procfs; libproc reports the same parent link.
            // Isolated fixtures keep the procfs reader.
            #[cfg(target_os = "macos")]
            let libproc = self.terminals.proc_root == Path::new("/proc");
            #[cfg(all(unix, not(target_os = "macos")))]
            let libproc = false;
            #[cfg(unix)]
            let parent = if libproc {
                guard.processes(1)?;
                let result = crate::platform::parent_pid(&self.terminals.proc_root, pid);
                guard.check()?;
                match result {
                    Some(parent) => parent,
                    None => break,
                }
            } else {
                guard.processes(1)?;
                let result = guard.process_file_optional(
                    &self.terminals.proc_root.join(pid.to_string()).join("stat"),
                );
                guard.check()?;
                let Ok(Some(stat)) = result else { break };
                let Some(end) = stat.iter().rposition(|byte| *byte == b')') else {
                    break;
                };
                let Some(parent) = std::str::from_utf8(&stat[end + 1..])
                    .ok()
                    .and_then(|s| s.split_whitespace().nth(1))
                    .and_then(|s| s.parse::<u32>().ok())
                else {
                    break;
                };
                parent
            };
            #[cfg(windows)]
            let parent = {
                let result = crate::platform::process_identity_budgeted(pid, &mut guard);
                guard.check()?;
                let Ok(process) = result else { break };
                if process.parent_pid == 0 {
                    break;
                }
                guard.processes(1)?;
                let result = crate::platform::process_metadata(process.parent_pid);
                guard.check()?;
                let Ok(parent) = result else { break };
                if parent.owner != process.owner || parent.creation_time > process.creation_time {
                    break;
                }
                parent.pid
            };
            if parent == 0 {
                break;
            }
            pid = parent;
        }
        guard.check()?;
        Ok(super::session_actions::AttachContext {
            own_terminals: super::session_actions::own_terminals(
                &self.last_snapshot,
                environment,
                &ancestors,
                &[],
            ),
            holders: vec![],
            node,
            hostname: environment.get("HOSTNAME").cloned().unwrap_or_default(),
        })
    }
}
impl Source for NativeSource {
    fn resize_terminal(
        &mut self,
        config: &ControlConfig,
        node: &str,
        request: super::terminal_resize::Request,
    ) -> Result<String> {
        self.require_configuration()?;
        super::terminal_resize::apply(config, &self.terminals, node, request)
    }
    fn prepare_pane_launch(
        &mut self,
        config: &ControlConfig,
        request: super::pane_launch::Request,
    ) -> Result<super::pane_launch::Plan> {
        self.require_configuration()?;
        self.terminals.apply_settings(&config.runtime)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Invalid clock")?
            .as_millis() as u64;
        super::pane_launch::prepare(
            config,
            &self.directory,
            &self.terminals,
            &crate::platform::environment(),
            request,
            now,
        )
    }
    fn apply_pane_launch(
        &mut self,
        config: &ControlConfig,
        plan: super::pane_launch::Plan,
        approved: &str,
    ) -> Result<crate::adapters::terminal_actions::Receipt> {
        self.require_configuration()?;
        self.terminals.apply_settings(&config.runtime)?;
        let current = ControlConfig::parse(
            &layouts::read_private(&self.directory.join("control.json"))?
                .ok_or("Configuration missing")?,
        )?;
        if &current != config || plan.hook.spec.registration.directory != self.directory {
            return Err("Launch instance changed; review again".into());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Invalid clock")?
            .as_millis() as u64;
        super::control_audit::perform(
            &self.directory,
            super::control_audit::Action::Workspace,
            || super::pane_launch::apply(&plan, config, &self.terminals, approved, now),
            |receipt| {
                if receipt.state == crate::adapters::terminal_actions::ReceiptState::Submitted {
                    super::control_audit::State::Submitted
                } else {
                    super::control_audit::State::Unconfirmed
                }
            },
        )
    }
    fn switch_instance(&mut self, directory: &Path) -> Result<ControlConfig> {
        layouts::check_path(directory)?;
        let bytes =
            super::telegram_config::read_private(&directory.join("control.json"), 1024 * 1024)?
                .ok_or("Selected instance has no control configuration")?;
        let config = ControlConfig::parse(&bytes)?;
        let cwd = std::env::current_dir().map_err(|_| "Cannot determine preference location")?;
        let preferences = super::startup::preference_directory(&cwd, |key| std::env::var_os(key))?;
        super::instances::remember(&preferences, directory)?;
        self.directory = directory.into();
        self.last_snapshot = json!({});
        Ok(config)
    }
    fn telegram(
        &mut self,
        request: super::telegram_admin::Request,
    ) -> Result<super::telegram_admin::Reply> {
        if matches!(&request, super::telegram_admin::Request::Inspect) {
            let super::telegram_admin::Reply::Data(mut value) =
                super::telegram_admin::execute(&self.directory, request)?
            else {
                return Err("Invalid Telegram settings response".into());
            };
            value["local_configured"] =
                json!(layouts::read_private(&self.directory.join("control.json"))?.is_some());
            return Ok(super::telegram_admin::Reply::Data(value));
        }
        self.require_configuration()?;
        if matches!(&request, super::telegram_admin::Request::Apply { .. }) {
            self.require_configuration()?;
            super::control_audit::perform(
                &self.directory,
                super::control_audit::Action::Telegram,
                || super::telegram_admin::execute(&self.directory, request),
                |_| super::control_audit::State::Accepted,
            )
        } else {
            super::telegram_admin::execute(&self.directory, request)
        }
    }
    fn prepare_gateway(
        &mut self,
        expected: Option<super::gateway::Config>,
        updated: super::gateway::Config,
    ) -> Result<super::gateway::Plan> {
        self.require_configuration()?;
        super::gateway::Plan::prepare(&self.directory, &expected, updated)
    }
    fn apply_gateway(&mut self, plan: super::gateway::Plan) -> Result<String> {
        self.require_configuration()?;
        let approval = plan.approval();
        super::control_audit::perform(
            &self.directory,
            super::control_audit::Action::Gateway,
            || plan.apply(&approval),
            |_| super::control_audit::State::Completed,
        )?;
        Ok("Gateway configuration saved privately; existing processes are unchanged".into())
    }
    fn configuration_file(&mut self, expected: &ControlConfig) -> Result<PathBuf> {
        self.require_configuration()?;
        let path = self.directory.join("control.json");
        let bytes = layouts::read_private_limit(&path, super::config::MAX_CONFIG_BYTES as u64)?
            .ok_or("Configuration missing")?;
        if &ControlConfig::parse(&bytes)? != expected {
            return Err("Configuration changed; reopen settings".into());
        }
        Ok(path)
    }
    fn prepare_configuration(
        &mut self,
        expected: &ControlConfig,
        updated: ControlConfig,
    ) -> Result<super::setup::SetupPlan> {
        self.require_configuration()?;
        super::setup::SetupPlan::edit_if_current(&self.directory, expected, updated)
    }
    fn apply_configuration(&mut self, plan: super::setup::SetupPlan) -> Result<ControlConfig> {
        self.require_configuration()?;
        if plan.preview.destination != self.directory.join("control.json") {
            return Err("Configuration directory changed; reopen the editor".into());
        }
        let updated = plan.config().clone();
        super::control_audit::perform(
            &self.directory,
            super::control_audit::Action::Configure,
            || plan.apply(),
            |_| super::control_audit::State::Completed,
        )?;
        Ok(updated)
    }
    fn history_access(&mut self, global: bool) -> Result<super::history::State> {
        super::history::State::inspect(&self.history_directory(global)?)
    }
    fn history_archives(&mut self, global: bool, offset: usize) -> Result<Value> {
        super::history::archives(&self.history_directory(global)?, offset)
    }
    fn history_archive_page(
        &mut self,
        global: bool,
        name: &str,
        offset: usize,
        unresolved: bool,
    ) -> Result<Value> {
        super::history::archive_page(&self.history_directory(global)?, name, offset, unresolved)
    }
    fn history_page(&mut self, global: bool, offset: usize, unresolved: bool) -> Result<Value> {
        super::control_audit::list(&self.history_directory(global)?, offset, unresolved)
    }
    fn apply_history(&mut self, plan: super::history::Plan) -> Result<()> {
        if plan.directory() != self.directory {
            let cwd = std::env::current_dir().map_err(|_| "Current directory unavailable")?;
            let preferences =
                super::startup::preference_directory(&cwd, |key| std::env::var_os(key))?;
            if plan.directory() != preferences {
                return Err("History directory changed; reopen the review".into());
            }
        }
        let approval = plan.approval();
        plan.apply(&approval)
    }
    fn dashboard_state(&mut self) -> Result<super::dashboard::State> {
        let cwd = std::env::current_dir().map_err(|_| "Current directory is unavailable")?;
        let preferences = super::startup::preference_directory(&cwd, |key| std::env::var_os(key))?;
        super::dashboard::State::load(&preferences)
    }
    fn prepare_dashboard_recovery(&mut self) -> Result<super::dashboard::Plan> {
        let cwd = std::env::current_dir().map_err(|_| "Current directory is unavailable")?;
        let preferences = super::startup::preference_directory(&cwd, |key| std::env::var_os(key))?;
        super::dashboard::Plan::recover(&preferences)
    }
    fn apply_dashboard(&mut self, plan: super::dashboard::Plan) -> Result<()> {
        let cwd = std::env::current_dir().map_err(|_| "Current directory is unavailable")?;
        let preferences = super::startup::preference_directory(&cwd, |key| std::env::var_os(key))?;
        if plan.directory() != preferences {
            return Err("Dashboard preference directory changed".into());
        }
        let approval = plan.approval();
        super::control_audit::perform(
            &preferences,
            super::control_audit::Action::Configure,
            || plan.apply(&approval),
            |_| super::control_audit::State::Completed,
        )
    }
    fn memory_configuration(&mut self) -> Result<Option<super::memory::Config>> {
        super::memory::load(&self.directory)
    }
    fn memory_registrations(&mut self) -> Result<Vec<super::hook_runtime::Registration>> {
        super::memory::registrations(&self.directory)
    }
    fn memory_settings(
        &mut self,
        path: &Path,
    ) -> Result<(
        super::hook_runtime::Registration,
        Option<super::memory::Config>,
    )> {
        let bytes = super::telegram_config::read_private(path, 1024 * 1024)?
            .ok_or("Installed hook registration is missing")?;
        let registration: super::hook_runtime::Registration =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid hook registration")?;
        registration.validate().map_err(|e| e.to_string())?;
        if registration.directory != self.directory
            || registration.path() != path
            || !registration.options.hindsight
        {
            return Err("Enable Hindsight in this instance's installed hook profile first".into());
        }
        Ok((registration, super::memory::load(&self.directory)?))
    }
    fn prepare_memory(
        &mut self,
        expected: Option<super::memory::Config>,
        config: super::memory::Config,
    ) -> Result<super::memory::Plan> {
        self.require_configuration()?;
        super::memory::Plan::update(&self.directory, expected.as_ref(), config)
    }
    fn apply_memory(&mut self, plan: super::memory::Plan) -> Result<Value> {
        self.require_configuration()?;
        let approval = plan.approval();
        super::control_audit::perform(
            &self.directory,
            super::control_audit::Action::Configure,
            || plan.apply(&approval),
            |_| super::control_audit::State::Completed,
        )
    }
    fn prepare_herdr_hooks(
        &mut self,
        config: &ControlConfig,
        harness: &str,
    ) -> Result<super::herdr_hooks::Plan> {
        self.require_configuration()?;
        let saved = ControlConfig::parse(
            &layouts::read_private(&self.directory.join("control.json"))?
                .ok_or("Configuration disappeared")?,
        )?;
        if saved != *config {
            return Err("Configuration changed; refresh before installing hooks".into());
        }
        super::herdr_hooks::prepare(&self.herdr_hook_request(config, harness)?)
    }
    fn apply_herdr_hooks(
        &mut self,
        plan: super::herdr_hooks::Plan,
        approval: &str,
    ) -> Result<super::herdr_hooks::Receipt> {
        self.require_configuration()?;
        let saved = ControlConfig::parse(
            &layouts::read_private(&self.directory.join("control.json"))?
                .ok_or("Configuration disappeared")?,
        )?;
        let request = self.herdr_hook_request(&saved, &plan.request.harness)?;
        if serde_json::to_value(request).map_err(|_| "Cannot check integration settings")?
            != serde_json::to_value(&plan.request).map_err(|_| "Cannot check integration review")?
        {
            return Err("Herdr integration settings changed; review again".into());
        }
        super::control_audit::perform(
            &self.directory,
            super::control_audit::Action::HookInstall,
            || super::herdr_hooks::apply(&plan, approval),
            |receipt| {
                if receipt.current {
                    super::control_audit::State::Completed
                } else {
                    super::control_audit::State::Unconfirmed
                }
            },
        )
    }
    fn prepare_hook_reload(&mut self) -> Result<super::hook_reload::Plan> {
        super::hook_reload::prepare(
            &self.directory,
            super::hook_install::installed_launcher(
                std::env::current_exe().map_err(|_| "Cannot locate Daedalus")?,
            ),
            &self.terminals,
        )
    }
    fn apply_hook_reload(&mut self, plan: super::hook_reload::Plan) -> Result<Value> {
        plan.apply(&self.directory)
    }
    fn managed_hooks(&mut self, harness: &str) -> Result<Vec<super::hook_uninstall::Entry>> {
        self.require_configuration()?;
        super::hook_uninstall::list(&self.directory, harness)
    }
    fn prepare_hook_removal(&mut self, path: &Path) -> Result<super::hook_uninstall::Plan> {
        self.require_configuration()?;
        super::hook_uninstall::Plan::prepare(&self.directory, path)
    }
    fn apply_hook_removal(
        &mut self,
        plan: super::hook_uninstall::Plan,
        approval: &str,
    ) -> Result<PathBuf> {
        self.require_configuration()?;
        super::control_audit::perform(
            &self.directory,
            super::control_audit::Action::HookRollback,
            || plan.apply(approval),
            |_| super::control_audit::State::Completed,
        )
    }
    fn rollback_hook_removal(&mut self, path: &Path) -> Result<()> {
        self.require_configuration()?;
        if path.parent() != Some(self.directory.join("hook-removals").as_path()) {
            return Err("Removal receipt belongs to another instance".into());
        }
        super::control_audit::perform(
            &self.directory,
            super::control_audit::Action::HookRollback,
            || super::hook_uninstall::rollback(path),
            |_| super::control_audit::State::Completed,
        )
    }
    fn default_hook_spec(
        &mut self,
        config: &ControlConfig,
        pane: &Value,
    ) -> Result<Option<super::hook_menu::Spec>> {
        let store = super::session_actions::SessionStore {
            directory: self.directory.clone(),
        };
        let (checkpoint, frozen) = store.freeze_live_source(
            &self.terminals,
            config,
            &crate::platform::environment(),
            pane,
            "",
        )?;
        // Prefer the exact process profile. Windows falls back to the selected harness's
        // configured/native profile, presented for review before any write.
        let profile = if checkpoint.harness == "opencode" {
            let env = crate::platform::environment();
            let identity = serde_json::to_vec(&json!([checkpoint.source, checkpoint.sid]))
                .map_err(|_| "Cannot encode terminal identity")?;
            Some(opencode_hook_path(&env, &identity)?)
        } else {
            hook_profile_path(
                &checkpoint.harness,
                frozen["_daedalus_handoff_process"]["profile"].as_str(),
                &checkpoint.profile,
            )
        };
        let Some(profile) = profile else {
            return Ok(None);
        };
        crate::core::layouts::check_path(&profile)?;
        Ok(Some(super::hook_menu::Spec {
            pane: frozen,
            profile,
            transcript: None,
            options: super::hook_runtime::Options::guided(),
            goal_id: None,
        }))
    }
    fn prepare_hooks(
        &mut self,
        config: &ControlConfig,
        spec: super::hook_menu::Spec,
    ) -> Result<super::hook_install::Preview> {
        use crate::adapters::harnesses;
        use sha2::{Digest, Sha256};
        let environment: BTreeMap<String, String> = crate::platform::environment();
        let store = super::session_actions::SessionStore {
            directory: self.directory.clone(),
        };
        let (checkpoint, frozen) =
            store.freeze_live_source(&self.terminals, config, &environment, &spec.pane, "")?;
        let tool = harnesses::tool(&checkpoint.harness).ok_or("Unknown native hook harness")?;
        let binary = harnesses::executable(&tool, &environment)
            .ok_or("Harness executable is not installed")?;
        let exact_native = matches!(checkpoint.harness.as_str(), "claude" | "codex");
        let verify_version_binary = || -> Result<()> {
            #[cfg(unix)]
            use std::os::unix::fs::MetadataExt;
            if !exact_native {
                return Ok(());
            }
            let pid = frozen["_daedalus_handoff_process"]["pid"]
                .as_str()
                .ok_or("Native process evidence missing")?;
            #[cfg(unix)]
            {
                // Linux follows the kernel-held image; macOS asks libproc for its path.
                #[cfg(not(target_os = "macos"))]
                let image = self.terminals.proc_root.join(pid).join("exe");
                #[cfg(target_os = "macos")]
                let image = pid
                    .parse::<u32>()
                    .ok()
                    .and_then(|pid| {
                        crate::platform::process_executable(&self.terminals.proc_root, pid)
                    })
                    .unwrap_or_default();
                let running=std::fs::metadata(image).map_err(|_|"Cannot verify running native executable; restart the harness before hook installation")?;
                let installed =
                    std::fs::metadata(&binary).map_err(|_| "Native executable disappeared")?;
                if running.dev() != installed.dev() || running.ino() != installed.ino() {
                    return Err("Running harness differs from the version-probed executable; restart with the selected native build before installing hooks".into());
                }
            }
            #[cfg(windows)]
            {
                let identity = crate::platform::process_metadata(
                    pid.parse().map_err(|_| "Invalid native process identity")?,
                )
                .map_err(|_| "Cannot verify running native process")?;
                let running = crate::platform::open_regular(&identity.executable)
                    .map_err(|e| e.to_string())?;
                let installed =
                    crate::platform::open_regular(&binary).map_err(|e| e.to_string())?;
                if crate::platform::file_identity(&running).map_err(|e| e.to_string())?
                    != crate::platform::file_identity(&installed).map_err(|e| e.to_string())?
                {
                    return Err("Running harness differs from selected executable".into());
                }
            }
            Ok(())
        };
        verify_version_binary()?;
        let native = harnesses::launch_plan(
            &tool,
            &binary,
            Path::new(&config.agent_home),
            &environment,
            &config.profiles,
        )?;
        let result = crate::adapters::process::run(
            native.command().arg("--version"),
            std::time::Duration::from_secs(10),
            16384,
        )
        .map_err(|_| "Cannot observe installed harness version")?;
        verify_version_binary()?;
        if !result.status.success() {
            return Err("Native harness version query failed".into());
        }
        let output =
            String::from_utf8(result.stdout).map_err(|_| "Invalid native version response")?;
        let version = output
            .split_whitespace()
            .map(|v| v.trim_start_matches('v'))
            .find(|v| {
                let parts = v.split('.').collect::<Vec<_>>();
                parts.len() == 3
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
            })
            .ok_or("Native harness did not report a semantic version")?;
        let identity = serde_json::to_vec(&json!([checkpoint.source, checkpoint.sid]))
            .map_err(|_| "Cannot encode registration identity")?;
        let registration_id = format!("native-{:x}", Sha256::digest(identity));
        let request = super::hook_install::LiveRequest {
            instance: hook_instance(&self.directory, config)?,
            directory: self.directory.clone(),
            profile: spec.profile,
            binary: super::hook_install::installed_launcher(
                std::env::current_exe().map_err(|_| "Cannot locate native hook executable")?,
            ),
            transcript: spec.transcript,
            registration_id,
            options: spec.options,
            goal_id: spec.goal_id,
            harness_version: version.into(),
        };
        let plan = super::hook_install::preview_live(
            &request,
            &frozen,
            config,
            &environment,
            &self.terminals,
        )?;
        let before = &frozen["_daedalus_handoff_process"];
        let process = plan
            .registration
            .harness_process
            .as_ref()
            .ok_or("Native process evidence is missing")?;
        if serde_json::to_value(plan.registration.scope.harness).map_err(|_| "Invalid harness")?
            != checkpoint.harness
            || process.pid.to_string() != before["pid"].as_str().unwrap_or("")
            || process.start_ticks.to_string() != before["start"].as_str().unwrap_or("")
            || plan.registration.scope.conversation != checkpoint.sid
        {
            return Err("Native process changed during hook preview; refresh first".into());
        }
        Ok(plan)
    }
    fn apply_hooks(
        &mut self,
        plan: super::hook_install::Preview,
    ) -> Result<super::hook_install::Receipt> {
        self.require_configuration()?;
        super::control_audit::perform(
            &self.directory,
            super::control_audit::Action::HookInstall,
            || super::hook_install::apply(&plan, &self.terminals),
            |_| super::control_audit::State::Completed,
        )
    }
    fn rollback_hooks(&mut self, receipt: super::hook_install::Receipt) -> Result<()> {
        self.require_configuration()?;
        super::control_audit::perform(
            &self.directory,
            super::control_audit::Action::HookRollback,
            || super::hook_install::rollback(&receipt),
            |_| super::control_audit::State::Completed,
        )
    }
    fn cutover(&mut self, destination: &Path, approval: Option<&str>) -> Result<Value> {
        use super::telegram_commands::Operations;
        let config = ControlConfig::parse(
            &crate::core::layouts::read_private(&destination.join("control.json"))?
                .ok_or("Imported configuration is missing")?,
        )?;
        let service = super::telegram_config::ServiceConfig::load(
            &destination.join("telegram-service.json"),
        )?;
        let terminals = self.terminals.configured(&config.runtime)?;
        let mut operations = super::telegram_commands::NativeOperations {
            instance: service.instance,
            directory: destination.into(),
            config,
            terminals,
            environment: crate::platform::environment(),
        };
        let global = super::telegram_service::global_directory()?;
        let plan = super::cutover::Plan::prepare(destination, &global, || operations.inventory())?;
        let mut review =
            serde_json::to_value(&plan.preview).map_err(|_| "Cannot encode cutover review")?;
        if let Some(approval) = approval {
            // Fresh prepare must reproduce the reviewed digest; never hold locks
            // while the operator reads or answers a TUI dialog.
            if plan.preview.approval != approval {
                return Err("Imported or source state changed; preview cutover again".into());
            }
            // Cutover owns durable transaction receipts across directory swaps;
            // the per-directory action audit lock cannot span that rename.
            let backup = plan.apply(approval, || operations.inventory())?;
            review["state"] = json!("committed");
            review["backup"] = json!(backup);
        }
        Ok(review)
    }
    fn rollback_cutover(&mut self, source: &Path, approval: &str) -> Result<Value> {
        let global = super::telegram_service::global_directory()?;
        super::cutover::rollback(source, &global, approval)?;
        Ok(json!({"state":"rolled_back","activated":false}))
    }
    fn prepare_setup(
        &mut self,
        config: &ControlConfig,
        spec: super::wizard_menu::Spec,
    ) -> Result<super::wizard::Wizard> {
        use super::{dependencies::Environment, wizard::Wizard, wizard_menu::Mode};
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or("HOME is not configured")?;
        let search = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        let environment = Environment::detect(&home, search)?;
        let destination = spec.destination.as_deref().unwrap_or(&self.directory);
        match spec.mode {
            Mode::Current => {
                if layouts::read_private(&destination.join("control.json"))?.is_none() {
                    Wizard::fresh(
                        destination,
                        Path::new(&config.agent_home),
                        environment,
                        spec.selection,
                    )
                } else {
                    Wizard::configure(destination, config.clone(), environment, spec.selection)
                }
            }
            Mode::Fresh { workspace } => {
                Wizard::fresh(destination, &workspace, environment, spec.selection)
            }
            Mode::Settings { source } => Wizard::import_settings(
                destination,
                &source,
                environment,
                spec.selection.tmux,
                spec.selection.herdr,
            ),
            Mode::Instance {
                source,
                credentials,
            } => Wizard::import_instance(
                destination,
                &source,
                environment,
                spec.selection.tmux,
                spec.selection.herdr,
                credentials,
            ),
        }
    }
    fn detect_harnesses(
        &mut self,
        config: &ControlConfig,
    ) -> Vec<super::harness_detection::Detected> {
        super::harness_detection::detect(config)
    }
    fn finish_local_setup(&mut self, config: &ControlConfig) -> Result<ControlConfig> {
        if let Some(bytes) = layouts::read_private(&self.directory.join("control.json"))? {
            // Finishing an existing setup must not overwrite settings changed elsewhere.
            return ControlConfig::parse(&bytes);
        }
        super::setup::secure_local_directory(&self.directory)?;
        super::setup::SetupPlan::fresh(&self.directory, config.clone())?.apply()?;
        self.require_configuration()?;
        Ok(config.clone())
    }
    fn download_setup(&mut self, wizard: &mut super::wizard::Wizard) -> Result<()> {
        wizard.prepare_installers(&mut super::dependencies::HttpsFetcher)
    }
    fn apply_setup(
        &mut self,
        wizard: super::wizard::Wizard,
        approved: &str,
    ) -> Result<(Value, Option<ControlConfig>)> {
        let current =
            wizard.review().configuration.destination == self.directory.join("control.json");
        let bootstrap = layouts::read_private(&self.directory.join("control.json"))?.is_none();
        let result = if bootstrap {
            wizard.apply(approved, &mut super::dependencies::NativeExecutor)?
        } else {
            super::control_audit::perform(
                &self.directory,
                super::control_audit::Action::Setup,
                || wizard.apply(approved, &mut super::dependencies::NativeExecutor),
                |result| {
                    if result.ready {
                        super::control_audit::State::Completed
                    } else {
                        super::control_audit::State::Unconfirmed
                    }
                },
            )?
        };
        let configuration = if current && result.ready {
            Some(ControlConfig::parse(
                &layouts::read_private(&self.directory.join("control.json"))?
                    .ok_or("Applied configuration is missing")?,
            )?)
        } else {
            None
        };
        Ok((
            serde_json::to_value(result).map_err(|_| "Cannot encode setup result")?,
            configuration,
        ))
    }
    fn session(
        &mut self,
        config: &ControlConfig,
        request: session_menu::Request,
    ) -> Result<session_menu::Answer> {
        self.terminals.apply_settings(&config.runtime)?;
        use super::session_actions::{self, LaunchRequest, SessionStore};
        use crate::adapters::{harnesses, process};
        use session_menu::{Answer, Request};
        let store = SessionStore {
            directory: self.directory.clone(),
        };
        let environment: BTreeMap<String, String> = crate::platform::environment();
        use super::control_audit::{Action as AuditAction, State as AuditState};
        let audit = match &request {
            Request::HookSwitch { .. } | Request::ReleaseGoal { .. } => {
                Some(AuditAction::Configure)
            }
            Request::Workspace(_) => Some(AuditAction::Workspace),
            Request::Native(_) => Some(AuditAction::NativeAttach),
            Request::Configure { .. } => Some(AuditAction::Configure),
            Request::Save { .. } => Some(AuditAction::CheckpointSave),
            Request::Remove(_) => Some(AuditAction::CheckpointRemove),
            Request::Launch { .. } => Some(AuditAction::CheckpointLaunch),
            Request::Attach { .. } => Some(AuditAction::Attach),
            Request::Duplicate { .. } => Some(AuditAction::DuplicateView),
            Request::Link { .. } => Some(AuditAction::LinkView),
            Request::Unlink(_) => Some(AuditAction::UnlinkView),
            Request::Terminal { action, .. } => Some(match action {
                crate::adapters::terminal_actions::Action::Send(_) => AuditAction::Send,
                crate::adapters::terminal_actions::Action::Keys(_) => AuditAction::Keys,
                crate::adapters::terminal_actions::Action::Interrupt => AuditAction::Interrupt,
                crate::adapters::terminal_actions::Action::Close => AuditAction::Close,
                crate::adapters::terminal_actions::Action::Focus => AuditAction::Focus,
                crate::adapters::terminal_actions::Action::Rename(_) => AuditAction::Rename,
            }),
            Request::Harness(_) => Some(AuditAction::Account),
            _ => None,
        };
        if audit.is_some() {
            self.require_configuration()?;
        }
        let directory = self.directory.clone();
        let operation = || {
            (|| match request {
                Request::HookSwitch { expected, option } => {
                    expected
                        .validate()
                        .map_err(|_| "Invalid hook registration")?;
                    if expected.directory != self.directory {
                        return Err("Hook registration belongs to another instance".into());
                    }
                    let _active = super::hook_runtime::active_guard(&self.directory)
                        .map_err(|e| e.to_string())?;
                    let _lock = super::hook_install::lock(&self.directory)?;
                    let path = expected.path();
                    let before = super::telegram_config::read_private(&path, 256 * 1024)?
                        .ok_or("Hook registration disappeared")?;
                    let current: super::hook_runtime::Registration =
                        serde_json::from_slice(&before)
                            .map_err(|_| "Invalid current registration")?;
                    if serde_json::to_value(&current).ok() != serde_json::to_value(&expected).ok() {
                        return Err("Hook registration changed; refresh before toggling".into());
                    }
                    let mut updated = *expected;
                    match option.as_str() {
                        "telegram_delivery" => {
                            updated.options.telegram_delivery = !updated.options.telegram_delivery
                        }
                        "telegram_progress" => {
                            updated.options.telegram_progress = !updated.options.telegram_progress
                        }
                        "notifications" => {
                            updated.options.notifications = !updated.options.notifications
                        }
                        "hindsight" => updated.options.hindsight = !updated.options.hindsight,
                        _ => return Err("Unknown hook option".into()),
                    }
                    updated.validate().map_err(|e| e.to_string())?;
                    layouts::write_private(
                        &path,
                        &serde_json::to_vec_pretty(&updated)
                            .map_err(|_| "Cannot encode hook options")?,
                    )?;
                    Ok(Answer::Text("Hook setting saved for its next event; recipient and memory bindings are unchanged".into()))
                }
                Request::ReleaseGoal { id, scope, epoch } => {
                    if scope.instance != config.agent_name {
                        return Err("Goal belongs to another instance".into());
                    }
                    let _active = super::hook_runtime::active_guard(&self.directory)
                        .map_err(|e| e.to_string())?;
                    let mut store = crate::core::goal_store::Store::attach_connection(
                        super::telegram_config::private_connection(
                            &self.directory.join("goals.sqlite3"),
                        )?,
                    )
                    .map_err(|e| e.to_string())?;
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|_| "Invalid system time")?
                        .as_millis() as u64;
                    store
                        .control_expected(
                            &id,
                            &scope,
                            epoch,
                            crate::core::goal_store::Control::Complete,
                            now,
                        )
                        .map_err(|e| e.to_string())?;
                    Ok(Answer::Text(
                        "Goal released; this conversation can stop after its current turn".into(),
                    ))
                }
                Request::GatewayLoad => Ok(Answer::Gateway(super::gateway::load(&self.directory)?)),
                Request::Harness(mut run) => {
                    let current = ControlConfig::parse(
                        &layouts::read_private(&self.directory.join("control.json"))?
                            .ok_or("Configuration is missing")?,
                    )?;
                    if &current != config {
                        return Err("Configuration changed; review the account action again".into());
                    }
                    let mut plan = super::harness_menu::plan(&run, config, &environment)?;
                    if run.method.is_none()
                        && let Some(gateway) = super::gateway::load(&self.directory)?
                        && gateway.enabled
                        && gateway.harness == run.tool
                    {
                        super::gateway::apply(&mut plan, &gateway)?;
                    }
                    if plan.secret_stdin {
                        let secret = run
                            .secret
                            .take()
                            .ok_or("Native login requires private input")?;
                        Ok(Answer::Text(super::harness_menu::execute_private_login(
                            &mut plan.command(),
                            secret,
                            &config.runtime,
                        )?))
                    } else {
                        Ok(Answer::Text(super::workspace_actions::run_native(
                            plan.command(),
                        )?))
                    }
                }
                Request::Report(action) => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|_| "Invalid system clock")?
                        .as_secs_f64();
                    let value = match action.as_str() {
                        "system.update" => serde_json::to_value(super::update::check()?)
                            .map_err(|_| "Cannot format update status")?,
                        "armonia.plan" => super::diagnostics::organization(&self.directory, now)?,
                        "activity.health" => super::diagnostics::inspect(&self.directory, now)?,
                        "activity.events" => {
                            json!({"control":super::control_audit::recent(&self.directory)?,"delivery":super::diagnostics::recent(&self.directory)?})
                        }
                        "instances.sources" => {
                            json!({"control":self.directory.join("control.json"),"workspace":config.agent_home,"profiles":config.profiles,"configured_tmux_sockets":config.tmux_sockets,"automatic_discovery":config.discovery.automatic,"configured_herdr_sessions":config.discovery.herdr_sessions,"telegram_service":self.directory.join("telegram-service.json"),"credentials":"Native account credentials and Telegram tokens are not displayed"})
                        }
                        id if id.starts_with("armonia:") => {
                            let registry = super::diagnostics::organization(&self.directory, now)?;
                            if let Some(id) = id.strip_prefix("armonia:agent:") {
                                let selected =
                                    id.parse::<i64>().map_err(|_| "Invalid Armonia identity")?;
                                registry["entries"]
                                    .as_array()
                                    .and_then(|rows| {
                                        rows.iter().find(|r| r["id"].as_i64() == Some(selected))
                                    })
                                    .cloned()
                                    .ok_or("Saved Armonia entry disappeared")?
                            } else {
                                super::armonia_status::inspect(
                                    &self.directory,
                                    &self.terminals.proc_root,
                                    now,
                                )
                            }
                        }
                        _ => return Err("Unknown read-only report".into()),
                    };
                    Ok(Answer::Text(
                        super::dialog_text::format(&value).map_err(|_| "Cannot format report")?,
                    ))
                }
                Request::Workspace(request) => Ok(Answer::Receipt(request.apply(
                    &self.terminals,
                    config,
                    &environment,
                    &self.directory,
                )?)),
                Request::Native(pane) => {
                    let context = self.attach_context(&environment, String::new())?;
                    let command = super::workspace_actions::native_command(
                        &self.terminals,
                        &pane,
                        &self.last_snapshot,
                        &context,
                        &environment,
                    )?;
                    Ok(Answer::Text(super::workspace_actions::run_native(command)?))
                }
                Request::Configure { expected, updated } => {
                    super::setup::SetupPlan::edit_if_current(
                        &self.directory,
                        &expected,
                        (*updated).clone(),
                    )?
                    .apply()?;
                    Ok(Answer::Configured {
                        configuration: updated,
                        message: "Configuration saved privately".into(),
                    })
                }
                Request::Save { pane, request } => Ok(Answer::Saved(store.save(
                    &self.terminals,
                    config,
                    &environment,
                    &pane,
                    &request,
                )?)),
                Request::Load(key) => Ok(Answer::Saved(
                    store.get_with_settings(&key, &config.runtime)?,
                )),
                Request::Remove(key) => {
                    store.remove_with_settings(&key, &config.runtime)?;
                    Ok(Answer::Text(
                    "Saved reference removed; native session history and terminals are unchanged"
                        .into(),
                ))
                }
                Request::Live { pane, sid } => {
                    let (checkpoint, source) = store.freeze_live_source(
                        &self.terminals,
                        config,
                        &environment,
                        &pane,
                        &sid,
                    )?;
                    Ok(Answer::Live { checkpoint, source })
                }
                Request::Licenses(id) => Ok(Answer::Licenses(super::fork_licenses::list(
                    &self.directory,
                    &id,
                    &environment,
                    &config.runtime,
                )?)),
                Request::Capabilities(id) => {
                    let tool = harnesses::tool(&id).ok_or("Unknown harness adapter")?;
                    let binary = harnesses::executable(&tool, &environment)
                        .ok_or("Install this harness before launching it")?;
                    let plan = harnesses::launch_plan(
                        &tool,
                        &binary,
                        Path::new(&config.agent_home),
                        &environment,
                        &config.profiles,
                    )?;
                    let output = process::run(
                        plan.command().arg("--help"),
                        std::time::Duration::from_secs(10),
                        1024 * 1024,
                    )
                    .map_err(|_| "Native capability probe failed")?;
                    if !output.status.success() {
                        return Err("Installed harness help query failed".into());
                    }
                    let help = String::from_utf8(output.stdout)
                        .map_err(|_| "Native help is not valid UTF-8")?;
                    Ok(Answer::Capabilities(harnesses::checkpoint_capabilities(
                        &tool,
                        &binary,
                        Some(&help),
                    )?))
                }
                Request::Launch {
                    mut checkpoint,
                    key,
                    operation,
                    target,
                    parent,
                    source,
                    placement,
                    license,
                } => {
                    if operation == crate::core::checkpoints::Operation::Handoff
                        && checkpoint.context_file.is_empty()
                    {
                        let source=source.as_ref().ok_or("This checkpoint has no context excerpt; save one with terminal output first")?;
                        *checkpoint =
                            store.capture_handoff(&self.terminals, source, *checkpoint)?;
                    }
                    let mut prepared = store.prepare_record(
                        *checkpoint,
                        key,
                        LaunchRequest {
                            operation,
                            target: &target,
                            parent: &parent,
                            placement,
                            license: license.as_ref(),
                        },
                        config,
                        &environment,
                    )?;
                    if license.is_none() {
                        super::gateway::wrap(
                            &self.directory,
                            &target.harness,
                            &mut prepared.launch,
                            &std::env::current_exe()
                                .map_err(|_| "Cannot locate native launcher")?,
                        )?;
                    }
                    Ok(Answer::Receipt(store.apply(&self.terminals, prepared)?))
                }
                Request::Attach { pane, mode, node } => {
                    let context = self.attach_context(&environment, node)?;
                    Ok(Answer::Frame(session_actions::prepare_attach(
                        &self.terminals,
                        &pane,
                        mode,
                        &self.last_snapshot,
                        &context,
                    )?))
                }
                Request::Duplicate {
                    source,
                    destination,
                    read_only,
                } => {
                    let context = self.attach_context(&environment, String::new())?;
                    let prepared = session_actions::prepare_duplicate_view(
                        &self.terminals,
                        &source,
                        &destination,
                        read_only,
                        &environment,
                        &context,
                    )?;
                    let mechanism = prepared.mechanism.clone();
                    let mut receipt =
                        session_actions::apply_duplicate_view(&self.terminals, prepared)?;
                    if receipt.state == crate::adapters::terminal_actions::ReceiptState::Submitted
                        && store
                            .register_view(&source, &receipt, &mechanism, &config.runtime)
                            .is_err()
                    {
                        receipt.message.push_str(". Viewer registration failed; the submitted view must not be launched again automatically.");
                    }
                    Ok(Answer::Receipt(receipt))
                }
                Request::Link {
                    source,
                    destination,
                } => Ok(Answer::Receipt(session_actions::link_window(
                    &self.terminals,
                    &source,
                    &destination,
                )?)),
                Request::Unlink(pane) => Ok(Answer::Receipt(session_actions::unlink_window(
                    &self.terminals,
                    &pane,
                )?)),
                Request::Terminal { pane, action } => {
                    Ok(Answer::Receipt(self.terminals.action(&pane, action)?))
                }
            })()
        };
        if let Some(action) = audit {
            super::control_audit::perform(&directory, action, operation, |answer| match answer {
                Answer::Receipt(receipt) => match receipt.state {
                    crate::adapters::terminal_actions::ReceiptState::Accepted => {
                        AuditState::Accepted
                    }
                    crate::adapters::terminal_actions::ReceiptState::Submitted => {
                        AuditState::Submitted
                    }
                    crate::adapters::terminal_actions::ReceiptState::Unconfirmed => {
                        AuditState::Unconfirmed
                    }
                },
                Answer::Frame(_) => AuditState::Accepted,
                _ => AuditState::Completed,
            })
        } else {
            operation()
        }
    }

    fn refresh(&mut self, config: &ControlConfig) -> Result<Observations> {
        self.terminals.apply_settings(&config.runtime)?;
        let budget = std::sync::Arc::new(super::discovery_budget::Budget::new(&config.runtime)?);
        let mut snapshot = inventory::collect_with_budget(
            config,
            &self.terminals,
            &inventory::socket_roots(),
            &budget,
        );
        let mut errors = Vec::new();
        let mut session_terminals = self.terminals.clone();
        session_terminals.discovery_budget = Some(budget.clone());
        if super::session_actions::enrich_sessions(
            &self.directory,
            &mut snapshot,
            &session_terminals,
        )
        .is_err()
        {
            errors.push("Some harness session metadata could not be verified");
        }
        let mut layouts_available = true;
        let layouts = match Layouts::load(&self.directory) {
            Ok(layouts) => layouts,
            Err(_) => {
                layouts_available = false;
                errors.push("Saved layouts could not be read");
                Layouts::default()
            }
        };
        let checkpoints = match (super::session_actions::SessionStore {
            directory: self.directory.clone(),
        })
        .observe_checkpoints(&config.runtime, &budget)
        {
            Ok(rows) => Some(rows),
            Err(error) => {
                errors.push("Saved checkpoints could not be read");
                snapshot["errors"]
                    .as_array_mut()
                    .expect("discovery errors")
                    .push(json!({"backend":"checkpoints","error":error}));
                None
            }
        };
        match (super::session_actions::SessionStore {
            directory: self.directory.clone(),
        })
        .live_views(&snapshot, &config.runtime, &budget)
        {
            Ok(views) => {
                snapshot["daedalus_views"] = json!(views.rows);
                if views.unresolved > 0 {
                    snapshot["view_records_incomplete"] = json!(true);
                    errors.push(super::view_records::WARNING);
                }
            }
            Err(error) => {
                snapshot["view_records_incomplete"] = json!(true);
                errors.push("Saved view identities could not be read");
                snapshot["errors"]
                    .as_array_mut()
                    .expect("discovery errors")
                    .push(json!({"backend":"views","error":error}));
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "System clock is before epoch")?
            .as_secs_f64();
        if self.directory.join("control.json").is_file() {
            match self.usage_recorder.sample(
                &self.directory,
                &snapshot,
                now,
                config.ui.workspace_usage_recording,
                config.runtime.workspace_usage_policy(),
                Some(budget.deadline),
            ) {
                Ok(Some(usage)) => {
                    if budget.admit_value(&usage).is_ok() {
                        snapshot["workspace_usage"] = usage;
                    }
                }
                Ok(None) => (),
                Err(error) => budget.error(
                    &mut snapshot,
                    json!({"backend":"workspace_usage","error":error}),
                ),
            }
        }
        budget.finish(&mut snapshot);
        self.last_snapshot = snapshot.clone();
        let (telegram, fallback_local, telegram_token_saved) =
            telegram_observation(&self.directory, config);
        let local = (|| -> Result<Value> {
            let cwd = std::env::current_dir().map_err(|_| "Current directory is unavailable")?;
            let preferences =
                super::startup::preference_directory(&cwd, |key| std::env::var_os(key))?;
            let mut roots = Vec::new();
            if let Some(parent) = preferences.parent() {
                roots.push(parent.to_path_buf());
            }
            if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
            {
                let legacy = PathBuf::from(home).join(".config");
                if !roots.contains(&legacy) {
                    roots.push(legacy);
                }
            }
            super::instances::discover(
                &self.directory,
                &preferences,
                &roots,
                &self.terminals.proc_root,
            )
        })()
        .unwrap_or_else(|_| {
            let mut fallback = fallback_local;
            fallback["warnings"] = json!(["Instance discovery unavailable"]);
            fallback
        });
        if local["warnings"]
            .as_array()
            .is_some_and(|warnings| !warnings.is_empty())
        {
            errors.push("Instance discovery is incomplete; inspect Dashboard and instance discovery settings");
        }
        let mut health = json!({"errors":errors,"layouts_available":layouts_available});
        match native_route_observation(&self.directory, &telegram) {
            Ok(routes) => {
                health["route_status"] = routes["status"].clone();
                health["routes"] = routes["routes"].clone();
                health["native_routes"] = routes["native_routes"].clone();
            }
            Err(_) => {
                health["route_status"] = json!("unknown");
                health["errors"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("Saved Telegram routes could not be verified"));
            }
        }
        health["telegram"] = telegram;
        health["hooks"] = super::diagnostics::hook_control_view(
            &self.directory,
            &hook_instance(&self.directory, config).unwrap_or_else(|_| config.agent_name.clone()),
            (now * 1000.0) as u64,
        )
        .unwrap_or_else(|_| json!({"tools":[],"switches":[],"goals":null,"status":"unknown"}));
        health["armonia"] =
            super::armonia_status::inspect(&self.directory, &self.terminals.proc_root, now);
        if let Some(checkpoints) = checkpoints {
            health["checkpoints"] = json!(checkpoints);
        }
        Ok(Observations {
            snapshot,
            health,
            local,
            layouts,
            now,
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
            hostname: std::env::var("HOSTNAME").unwrap_or_default(),
            telegram_token_saved,
        })
    }
    fn preview(&mut self, pane: &Value, rows: usize) -> Result<String> {
        self.previews(std::slice::from_ref(pane), rows).remove(0)
    }
    fn previews(&mut self, targets: &[Value], rows: usize) -> Vec<Result<String>> {
        let environment: BTreeMap<String, String> = [
            "TMUX",
            "TMUX_PANE",
            "HERDR_SESSION",
            "HERDR_PANE_ID",
            "DAEDALUS_TERMINAL_ID",
            "DAEDALUS_TERMINAL_GENERATION",
        ]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.into(), value)))
        .collect();
        let own = match self.attach_context(&environment, String::new()) {
            Ok(context) => context.own_terminals,
            Err(error) => return bounded_previews(targets, |_| Err(error.clone())),
        };
        bounded_previews(targets, |pane| {
            let identity = terminals::identity(pane)?;
            if own.contains(&identity) {
                return Err("This terminal contains the dashboard".into());
            }
            self.terminals
                .preview(pane, rows)
                .map(|text| tail(&text, rows, 192_000))
        })
    }
    fn save_layouts(&mut self, layouts: &Layouts) -> Result<()> {
        // Browsing or quitting first-run setup must not create private state.
        if layouts::read_private(&self.directory.join("control.json"))?.is_none() {
            return Ok(());
        }
        // Never overwrite an unreadable existing layout with a fallback empty state.
        let old = Layouts::load(&self.directory)
            .map_err(|_| "Existing layouts must be repaired before saving")?;
        let changed = serde_json::to_value((&old.layouts, &old.workflows, &old.current.pins))
            .map_err(|_| "Invalid saved layouts")?
            != serde_json::to_value((&layouts.layouts, &layouts.workflows, &layouts.current.pins))
                .map_err(|_| "Invalid new layouts")?;
        if changed {
            super::control_audit::perform(
                &self.directory,
                super::control_audit::Action::Layout,
                || layouts.save(&self.directory),
                |_| super::control_audit::State::Completed,
            )
        } else {
            layouts.save(&self.directory)
        }
    }
}
/// Ephemeral first-run state. Setup is reviewed before any files are created.
pub fn run_first_run(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    config_dir: &Path,
    workspace: &Path,
) -> Result<()> {
    layouts::check_path(config_dir)?;
    if layouts::read_private(&config_dir.join("control.json"))?.is_some() {
        return Err("Configuration already exists; use the configured backend".into());
    }
    let config = ControlConfig::fresh(workspace)?;
    let source = NativeSource {
        directory: config_dir.into(),
        terminals: Terminals::default(),
        last_snapshot: json!({}),
        usage_recorder: Default::default(),
    };
    run_with(
        reader,
        writer,
        &mut Backend::new(config_dir.into(), config, source)?,
    )
}
pub fn run(reader: &mut impl BufRead, writer: &mut impl Write, config_dir: &Path) -> Result<()> {
    layouts::check_path(config_dir)?;
    let config = ControlConfig::parse(
        &layouts::read_private(&config_dir.join("control.json"))?
            .ok_or("Configuration missing; run config init or import")?,
    )?;
    let source = NativeSource {
        directory: config_dir.into(),
        terminals: Terminals::default(),
        last_snapshot: json!({}),
        usage_recorder: Default::default(),
    };
    run_with(
        reader,
        writer,
        &mut Backend::new(config_dir.into(), config, source)?,
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Cursor;
    struct Mock {
        observation: Observations,
        saves: usize,
        previews: usize,
        effects: usize,
        fail: bool,
    }
    impl Source for Mock {
        fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
            if self.fail {
                Err("secret token must not escape".into())
            } else {
                Ok(self.observation.clone())
            }
        }
        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            self.previews += 1;
            Ok("first\nsecond\nthird".into())
        }
        fn save_layouts(&mut self, _: &Layouts) -> Result<()> {
            self.saves += 1;
            Ok(())
        }
        fn session(
            &mut self,
            _: &ControlConfig,
            request: session_menu::Request,
        ) -> Result<session_menu::Answer> {
            match request {
                session_menu::Request::Save { request, .. } => {
                    assert_eq!(request.name, "test checkpoint");
                    self.effects += 1;
                    Ok(session_menu::Answer::Text("Checkpoint saved".into()))
                }
                _ => Err("Unsupported mock operation".into()),
            }
        }
    }
    fn backend() -> Backend<Mock> {
        let observation = Observations {
            now: 10.0,
            snapshot: json!({"panes":[{"backend":"tmux","socket":crate::test_support::path("/tmp/example.sock"),"server":crate::test_support::path("/tmp/example.sock"),"session":"project","session_id":"$1","window":"main","window_id":"@1","pane_id":"%1","terminal_id":"generation-1","agent":"bash","pid":"1"}],"clients":[],"errors":[]}),
            health: json!({"checkpoints":[],"errors":[]}),
            ..Observations::default()
        };
        Backend::new(
            crate::test_support::path("/tmp/isolated-instance").into(),
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/project"))).unwrap(),
            Mock {
                observation,
                saves: 0,
                previews: 0,
                effects: 0,
                fail: false,
            },
        )
        .unwrap()
    }
    #[test]
    fn invalid_layout_is_partial_preserves_stale_and_clears_on_recovery() {
        let mut backend = backend();
        let pane = &mut backend.source.observation.snapshot["panes"][0];
        for (key, value) in [("left", 0), ("top", 0), ("width", 70000), ("height", 24)] {
            pane[key] = json!(value);
        }
        let partial = backend.refresh().unwrap();
        assert_eq!(partial["terminals"], 1);
        assert!(
            partial["freshness"]
                .as_str()
                .unwrap()
                .starts_with("PARTIAL")
        );
        assert!(!partial["layout_warnings"].as_array().unwrap().is_empty());
        let (frames, _) = backend
            .handle(&json!({"version":2,"op":"action","page":6,"action":"activity.discovery"}))
            .unwrap();
        assert!(frames.iter().any(|frame| {
            frame["text"]
                .as_str()
                .is_some_and(|text| text.contains("Layout geometry is incomplete"))
        }));
        backend.menu.cancel();
        backend.refresh_error = true;
        let stale = backend.state().unwrap();
        assert!(stale["freshness"].as_str().unwrap().starts_with("STALE"));
        assert!(stale["layout_warnings"].is_array());
        backend.source.observation.snapshot["panes"][0]["width"] = json!(120);
        let recovered = backend.refresh().unwrap();
        assert!(recovered.get("layout_warnings").is_none());
        assert!(backend.observations.health.get("layout_warnings").is_none());
        assert!(
            !recovered["freshness"]
                .as_str()
                .unwrap()
                .starts_with("PARTIAL")
        );
        assert_eq!(backend.source.effects, 0);
    }
    #[test]
    fn conflicting_base_observations_keep_known_ids_refuse_previews_and_clear_after_refresh() {
        let mut backend = backend();
        let mut original = backend.source.observation.snapshot["panes"][0].clone();
        original["agent"] = json!("codex");
        original["agent_session"] = json!({"kind":"id","agent":"codex","value":"one"});
        backend.source.observation.layouts.current.pins = vec![reference(&original).unwrap()];
        let mut changed = original.clone();
        changed["agent_session"]["value"] = json!("two");
        backend.source.observation.snapshot["panes"] = json!([original.clone(), changed]);
        let partial = backend.refresh().unwrap();
        assert_eq!(partial["terminals"], 1);
        assert_eq!(partial["views"], 1);
        assert!(
            partial["freshness"]
                .as_str()
                .unwrap()
                .starts_with("PARTIAL")
        );
        let pane = partial["tree"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "pane")
            .unwrap();
        let id = pane["id"].as_str().unwrap();
        assert_eq!(pane["actionable"], false);
        assert_eq!(partial["workspace"]["pins"][0]["status"], "ambiguous");
        let _ = backend.preview(&json!({"nodes":[id],"rows":24})).unwrap();
        assert_eq!(backend.source.previews, 0);
        assert!(backend.handle(&json!({"version":2,"op":"action","page":1,"action":"terminal.resize","node":id})).is_err());
        backend.refresh_error = true;
        assert!(
            backend.state().unwrap()["freshness"]
                .as_str()
                .unwrap()
                .starts_with("STALE")
        );
        backend.source.observation.snapshot["panes"] = json!([original.clone(), original]);
        let recovered = backend.refresh().unwrap();
        assert_eq!(recovered["terminals"], 1);
        assert_eq!(recovered["views"], 1);
        assert!(recovered.get("observation_warnings").is_none());
        assert!(
            backend
                .observations
                .health
                .get("observation_warnings")
                .is_none()
        );
        assert_eq!(recovered["workspace"]["pins"][0]["status"], "live");
        assert_eq!(backend.source.effects, 0);
    }
    #[test]
    fn unverified_selection_does_not_block_view_preferences_or_graceful_quit() {
        let mut backend = backend();
        let first = backend.source.observation.snapshot["panes"][0].clone();
        let mut second = first.clone();
        second["title"] = json!("conflicting title");
        backend.source.observation.snapshot["panes"] = json!([first, second]);
        let frame = backend.refresh().unwrap();
        let id = frame["tree"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "pane")
            .unwrap()["id"]
            .clone();
        let view = json!({"mode":"tiles","selected":id,"motion":"off"});
        backend
            .handle(&json!({"version":2,"op":"view","view":view}))
            .unwrap();
        assert!(backend.observations.layouts.current.selected.is_none());
        assert_eq!(
            backend.observations.layouts.current.preferences["motion"],
            "off"
        );
        assert!(
            backend
                .handle(&json!({"version":2,"op":"quit","view":view}))
                .unwrap()
                .1
        );
        assert_eq!(backend.source.effects, 0);
    }
    #[test]
    fn relationship_partial_state_keeps_all_terminals_controls_and_stale_evidence() {
        let mut backend = backend();
        backend.config.runtime.discovery_relationship_limit = 1;
        let original = backend.source.observation.snapshot["panes"][0].clone();
        backend.source.observation.snapshot["panes"] = (0..128)
            .map(|n| {
                let mut pane = original.clone();
                pane["pane_id"] = json!(format!("%{n}"));
                pane
            })
            .collect::<Vec<_>>()
            .into();
        let partial = backend.refresh().unwrap();
        assert_eq!(partial["relationship_limited"], true);
        assert_eq!(partial["terminals"], 128);
        assert!(
            partial["freshness"]
                .as_str()
                .unwrap()
                .starts_with("PARTIAL")
        );
        assert_ne!(partial["display_limited"], true);
        assert_eq!(
            partial["tree"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["kind"] == "pane")
                .count(),
            128
        );
        assert!(
            partial["catalog"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["id"] == "action:settings.runtime")
        );
        let (frames, _) = backend
            .handle(&json!({"version":2,"op":"action","page":6,"action":"activity.discovery"}))
            .unwrap();
        assert!(frames.iter().any(|frame| {
            frame["text"]
                .as_str()
                .is_some_and(|text| text.contains(super::super::tree_relations::WARNING))
        }));
        backend.menu.cancel();
        backend.refresh_error = true;
        let stale = backend.state().unwrap();
        assert_eq!(stale["relationship_limited"], true);
        assert!(stale["freshness"].as_str().unwrap().starts_with("STALE"));
        backend.config.runtime.discovery_relationship_limit = 16384;
        let complete = backend.refresh().unwrap();
        assert_ne!(complete["relationship_limited"], true);
        assert!(
            backend
                .observations
                .health
                .get("projection_warning")
                .is_none()
        );
        assert_eq!(backend.source.effects, 0);
    }
    #[test]
    fn limited_state_preserves_controls_exact_tree_prefix_and_verified_totals() {
        let mut backend = backend();
        backend.config.runtime.discovery_frame_limit_bytes = 1048576;
        let original = backend.source.observation.snapshot["panes"][0].clone();
        backend.source.observation.snapshot["panes"] = (0..2000)
            .map(|index| {
                let mut pane = original.clone();
                pane["pane_id"] = json!(format!("%{index}"));
                pane
            })
            .collect::<Vec<_>>()
            .into();
        let frame = backend.refresh().unwrap();
        assert_eq!(frame["display_limited"], true);
        assert_eq!(frame["terminals"], 2000);
        assert!(frame["freshness"].as_str().unwrap().starts_with("PARTIAL"));
        assert!(super::super::discovery_budget::json_size(&frame, 1048576).is_ok());
        backend.refresh_error = true;
        let stale = backend.state().unwrap();
        assert_eq!(stale["display_limited"], true);
        assert!(stale["freshness"].as_str().unwrap().starts_with("STALE"));
        backend.refresh_error = false;
        let rows = frame["tree"].as_array().unwrap();
        let full = backend.topology().rows(&backend.tree_context());
        assert!(rows.len() < full.len());
        assert_eq!(rows, &full[..rows.len()]);
        let mut seen = BTreeSet::new();
        for row in rows {
            let parent = row["parent"].as_str().unwrap();
            assert!(parent.is_empty() || seen.contains(parent));
            seen.insert(row["id"].as_str().unwrap().to_owned());
        }
        assert!(
            frame["catalog"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["id"] == "action:settings.runtime")
        );
        let (frames, _) = backend
            .handle(&json!({"version":2,"op":"action","page":6,"action":"activity.discovery"}))
            .unwrap();
        assert!(frames.iter().any(|frame| {
            frame["text"]
                .as_str()
                .is_some_and(|text| text.contains(DISPLAY_LIMIT_MESSAGE))
        }));
        assert_eq!(backend.source.effects, 0);
        backend.menu.cancel();
        backend.source.observation.snapshot["panes"]
            .as_array_mut()
            .unwrap()
            .truncate(1);
        let complete = backend.refresh().unwrap();
        assert_ne!(complete["display_limited"], true);
        assert!(backend.observations.health.get("display_warning").is_none());
    }
    #[test]
    fn completed_setup_switches_backend_and_source_before_terminal_actions() {
        struct SwitchingSource {
            directory: PathBuf,
        }
        impl Source for SwitchingSource {
            fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
                Ok(String::new())
            }
            fn save_layouts(&mut self, _: &Layouts) -> Result<()> {
                Ok(())
            }
            fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
                Ok(Observations::default())
            }
            fn switch_instance(&mut self, directory: &Path) -> Result<ControlConfig> {
                self.directory = directory.to_owned();
                ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/new-workspace")))
            }
        }
        let original = PathBuf::from(crate::test_support::path("/tmp/original-first-run"));
        let selected = PathBuf::from(crate::test_support::path("/tmp/reviewed-setup-destination"));
        let mut backend = Backend::new(
            original.clone(),
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/old-workspace")))
                .unwrap(),
            SwitchingSource {
                directory: original,
            },
        )
        .unwrap();
        backend.observations.snapshot = json!({"panes":[{"pane_id":"stale"}]});
        backend.restore_pending = false;
        let frames = backend
            .menu_outcome(session_menu::Outcome {
                setup_directory: Some(selected.clone()),
                frames: vec![json!({"kind":"dialog","title":"Setup result"})],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(frames[0]["title"], "Setup result");
        assert_eq!(backend.directory, selected);
        assert_eq!(backend.source.directory, selected);
        assert_eq!(
            backend.config.agent_home,
            crate::test_support::path("/tmp/new-workspace")
        );
        assert_eq!(
            backend.observations.snapshot,
            Observations::default().snapshot
        );
        assert!(backend.restore_pending);
    }

    #[test]
    fn hook_profile_detection_prefers_process_then_configured_profile() {
        let observed = crate::test_support::path("/tmp/observed-profile");
        let configured = crate::test_support::path("/tmp/configured-profile");
        assert_eq!(
            hook_profile_path("codex", Some(observed), configured),
            Some(PathBuf::from(observed).join("hooks.json"))
        );
        assert_eq!(
            hook_profile_path("claude", None, configured),
            Some(PathBuf::from(configured).join("settings.json"))
        );
        assert_eq!(
            hook_profile_path("codex", Some(""), configured),
            Some(PathBuf::from(configured).join("hooks.json"))
        );
        assert!(hook_profile_path("codex", None, "").is_none());
        assert_eq!(
            hook_profile_path("hermes", None, configured),
            Some(PathBuf::from(configured).join("config.yaml"))
        );
        assert!(hook_profile_path("unknown", Some(observed), configured).is_none());
    }
    #[test]
    fn opencode_hooks_use_config_not_session_data_and_keep_terminals_separate() {
        let home = crate::test_support::path("/tmp/hook-home");
        let custom = crate::test_support::path("/tmp/hook-config");
        let mut env = BTreeMap::from([
            ("HOME".into(), home.into()),
            (
                "XDG_DATA_HOME".into(),
                crate::test_support::path("/tmp/accounts").into(),
            ),
        ]);
        let first = opencode_hook_path(&env, b"first-terminal").unwrap();
        assert_eq!(
            first.parent().unwrap(),
            Path::new(home).join(".config/opencode/plugins")
        );
        assert_ne!(first, opencode_hook_path(&env, b"second-terminal").unwrap());
        env.insert("XDG_CONFIG_HOME".into(), custom.into());
        assert_eq!(
            opencode_hook_path(&env, b"first-terminal")
                .unwrap()
                .parent()
                .unwrap(),
            Path::new(custom).join("opencode/plugins")
        );
        env.insert("OPENCODE_CONFIG_DIR".into(), home.into());
        assert_eq!(
            opencode_hook_path(&env, b"first-terminal")
                .unwrap()
                .parent()
                .unwrap(),
            Path::new(home).join("plugins")
        );
        env.insert("OPENCODE_CONFIG_DIR".into(), "relative/path".into());
        assert!(opencode_hook_path(&env, b"first-terminal").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn hook_namespace_matches_saved_telegram_service_and_rejects_invalid_config() {
        let directory =
            std::env::temp_dir().join(format!("daedalus-hook-namespace-{}", std::process::id()));
        let config = ControlConfig::fresh(&directory).unwrap();
        assert_eq!(
            hook_instance(&directory, &config).unwrap(),
            config.agent_name
        );
        let service = json!({"version":1,"instance":"telegram","bot_id":1,"token_file":directory.join("telegram.env"),"grants":[],"listen_chats":[],"registrations":[],"hook_bindings":[]});
        layouts::write_private(
            &directory.join("telegram-service.json"),
            &serde_json::to_vec(&service).unwrap(),
        )
        .unwrap();
        assert_eq!(hook_instance(&directory, &config).unwrap(), "telegram");
        layouts::write_private(&directory.join("telegram-service.json"), b"{}").unwrap();
        assert!(hook_instance(&directory, &config).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn initial_state_has_existing_ui_contract_and_catalog_only_once() {
        let mut backend = backend();
        let first = backend.refresh().unwrap();
        assert_eq!(first["version"], 2);
        assert_eq!(first["kind"], "state");
        assert_eq!(first["sections"].as_array().unwrap().len(), 11);
        assert_eq!(first["terminal_actions"].as_array().unwrap().len(), 17);
        assert_eq!(first["terminals"], 1);
        assert!(first["catalog"].as_array().unwrap().len() > 42);
        assert_eq!(first["workspace"]["apply"]["mode"], "tree");
        assert!(backend.state().unwrap().get("catalog").is_none());
        assert_eq!(first["overview"]["connections"][0]["state"], "unknown");
    }
    #[test]
    fn layout_persistence_does_not_consume_an_outstanding_setup_dialog() {
        let mut backend = backend();
        backend.refresh().unwrap();
        let (frames, _) = backend
            .handle(&json!({"version":2,"op":"action","page":0,"action":"setup"}))
            .unwrap();
        let id = frames[0]["id"].clone();
        let (frames, _) = backend.handle(&json!({"version":2,"op":"view","view":{"mode":"tiles","selected":"","motion":"off"}})).unwrap();
        assert!(frames.is_empty());
        assert!(backend.menu.pending());
        let (frames, _) = backend
            .handle(&json!({"version":2,"op":"refresh"}))
            .unwrap();
        assert_eq!(frames[0]["kind"], "state");
        assert!(backend.menu.pending());
        let (frames, _) = backend
            .handle(&json!({"version":2,"op":"previews","nodes":[],"rows":8}))
            .unwrap();
        assert_eq!(frames[0]["kind"], "previews");
        assert!(backend.menu.pending());
        let (frames, _) = backend
            .handle(&json!({"version":2,"op":"reply","id":id,"value":null}))
            .unwrap();
        assert_eq!(frames[0]["kind"], "state");
        assert!(!backend.menu.pending());
    }
    #[test]
    fn catalog_dialog_requires_matching_reply_and_never_executes_reference() {
        let mut backend = backend();
        backend.refresh().unwrap();
        let (frames, _) = backend
            .handle(&json!({"version":2,"op":"action","page":10,"action":"telegram:windows"}))
            .unwrap();
        assert_eq!(frames[0]["kind"], "dialog");
        let id = frames[0]["id"].as_u64().unwrap();
        assert!(
            backend
                .handle(&json!({"version":2,"op":"reply","id":id+1,"value":"close"}))
                .is_err()
        );
        assert!(
            backend
                .handle(&json!({"version":2,"op":"reply","id":id,"value":"execute"}))
                .is_err()
        );
        let (frames, _) = backend
            .handle(&json!({"version":2,"op":"reply","id":id,"value":"close"}))
            .unwrap();
        assert_eq!(frames[0]["kind"], "state");
        assert_eq!(backend.source.saves, 0);
        assert_eq!(backend.source.previews, 0);
    }
    #[test]
    fn missing_and_stale_previews_never_capture_another_terminal() {
        let mut backend = backend();
        backend.refresh().unwrap();
        let node = backend
            .topology()
            .nodes
            .iter()
            .find(|node| node.kind == "pane")
            .unwrap()
            .id();
        let (frames, _) = backend
            .handle(&json!({"version":2,"op":"previews","nodes":[node,node,"missing"],"rows":2}))
            .unwrap();
        assert_eq!(frames[0]["items"][0]["text"], "second\nthird");
        assert_eq!(backend.source.previews, 1);
        assert!(frames[0]["items"][1].get("error").is_some());
        backend.source.fail = true;
        let stale = backend.refresh().unwrap();
        assert!(stale["freshness"].as_str().unwrap().starts_with("STALE"));
        assert!(!stale.to_string().contains("secret token"));
        backend
            .handle(&json!({"version":2,"op":"previews","nodes":[node],"rows":2}))
            .unwrap();
        assert_eq!(backend.source.previews, 1);
    }
    #[test]
    fn action_confirmation_survives_its_refresh_but_stale_observations_are_explicit() {
        let mut backend = backend();
        let frame = backend
            .menu_outcome(session_menu::Outcome {
                message: "Configuration saved privately".into(),
                refresh: true,
                ..session_menu::Outcome::default()
            })
            .unwrap();
        assert_eq!(frame[0]["message"], "Configuration saved privately");
        assert_eq!(backend.refresh().unwrap()["message"], "");
        backend.source.fail = true;
        let frame = backend
            .menu_outcome(session_menu::Outcome {
                message: "Configuration saved privately".into(),
                refresh: true,
                ..session_menu::Outcome::default()
            })
            .unwrap();
        assert!(
            frame[0]["message"]
                .as_str()
                .unwrap()
                .contains("Refresh failed")
        );
        assert!(frame[0]["freshness"].as_str().unwrap().starts_with("STALE"));
    }
    #[test]
    fn view_and_quit_persist_valid_settings_and_reject_invalid_changes() {
        let mut backend = backend();
        backend.refresh().unwrap();
        backend.handle(&json!({"version":2,"op":"view","view":{"mode":"tiles","selected":"","motion":"off"}})).unwrap();
        assert_eq!(backend.source.saves, 1);
        assert!(backend.handle(&json!({"version":2,"op":"view","view":{"mode":"tiles","selected":"","shell":"do not execute"}})).is_err());
        assert_eq!(backend.source.saves, 1);
        let (_, quit) = backend
            .handle(&json!({"version":2,"op":"quit","view":{"mode":"tiles","selected":""}}))
            .unwrap();
        assert!(quit);
        assert_eq!(backend.source.saves, 2);
    }
    #[test]
    fn stdio_frames_are_validated_and_unsupported_actions_fail_explicitly() {
        let mut backend = backend();
        let mut input=Cursor::new(b"{\"version\":2,\"op\":\"action\",\"page\":0,\"action\":\"unknown.action\"}\n{\"version\":2,\"op\":\"quit\"}\n");
        let mut output = Vec::new();
        run_with(&mut input, &mut output, &mut backend).unwrap();
        let frames = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1]["kind"], "error");
        assert!(
            frames[1]["message"]
                .as_str()
                .unwrap()
                .contains("unsupported")
        );
        assert_eq!(backend.source.saves, 0);
    }
    #[test]
    fn preview_limits_and_unicode_tail_are_bounded() {
        assert_eq!(tail("αβγ", 1, 3), "γ");
        assert!(tail(&"\u{001b}".repeat(1000), 2, 50).len() <= 50);
        let mut backend = backend();
        backend.refresh().unwrap();
        assert!(
            backend
                .handle(&json!({"version":2,"op":"previews","nodes":vec!["x";49],"rows":20}))
                .is_err()
        );
    }
    #[test]
    fn preview_pool_is_bounded_concurrent_and_ordered() {
        use std::sync::{
            Barrier,
            atomic::{AtomicUsize, Ordering},
        };
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let barrier = Barrier::new(6);
        let targets: Vec<_> = (0..18).map(|i| json!(i)).collect();
        let results = bounded_previews(&targets, |value| {
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(count, Ordering::SeqCst);
            barrier.wait();
            active.fetch_sub(1, Ordering::SeqCst);
            if value == 7 {
                Err("fixture error".into())
            } else {
                Ok(value.to_string())
            }
        });
        assert_eq!(maximum.load(Ordering::SeqCst), 6);
        assert_eq!(results.len(), 18);
        for (index, result) in results.iter().enumerate() {
            if index == 7 {
                assert!(result.is_err());
            } else {
                assert_eq!(result.as_ref().unwrap(), &index.to_string());
            }
        }
        assert!(bounded_previews(&[], |_| panic!("empty batch")).is_empty());
    }
    #[test]
    fn stale_inventory_cannot_advertise_live_pins() {
        let mut backend = backend();
        backend.refresh().unwrap();
        let topology = backend.topology();
        let pane = topology
            .nodes
            .iter()
            .find_map(|node| node.pane.as_ref())
            .unwrap();
        backend
            .observations
            .layouts
            .current
            .pins
            .push(reference(pane).unwrap());
        assert_eq!(
            backend.state().unwrap()["workspace"]["pins"][0]["status"],
            "live"
        );
        backend.source.fail = true;
        let frame = backend.refresh().unwrap();
        assert_eq!(frame["workspace"]["pins"][0]["status"], "unverified");
        assert_eq!(frame["workspace"]["pins"][0]["id"], "");
    }
    #[test]
    fn unavailable_actions_are_marked_without_removing_menu_entries() {
        let frame = backend().refresh().unwrap();
        assert_eq!(frame["terminal_actions"].as_array().unwrap().len(), 17);
        assert!(
            frame["terminal_actions"]
                .as_array()
                .unwrap()
                .iter()
                .all(
                    |entry| entry["note"].as_str().unwrap().contains("Not available")
                        != session_menu::available(entry["id"].as_str().unwrap())
                )
        );
        let sections = frame["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 11);
        let setup = sections
            .iter()
            .flat_map(|section| section["items"].as_array().unwrap())
            .find(|item| item["id"] == "setup")
            .unwrap();
        assert!(!setup["note"].as_str().unwrap().contains("Not available"));
    }
    #[test]
    fn guided_catalog_checkpoint_action_runs_existing_dialog_protocol_end_to_end() {
        let mut backend = backend();
        backend.refresh().unwrap();
        let node = backend
            .topology()
            .nodes
            .into_iter()
            .find(|n| n.kind == "pane")
            .unwrap()
            .id();
        let(mut frames,_)=backend.handle(&json!({"version":2,"op":"action","page":10,"action":"action:checkpoint.save","node":node})).unwrap();
        assert_eq!(frames[0]["mode"], "input");
        for value in ["test checkpoint", "reference", "yes"] {
            let id = frames[0]["id"].clone();
            (frames, _) = backend
                .handle(&json!({"version":2,"op":"reply","id":id,"value":value}))
                .unwrap();
        }
        assert_eq!(backend.source.effects, 1);
        assert_eq!(frames[0]["mode"], "output");
        let id = frames[0]["id"].clone();
        let (state, _) = backend
            .handle(&json!({"version":2,"op":"reply","id":id,"value":null}))
            .unwrap();
        assert_eq!(state[0]["kind"], "state");
        assert!(
            backend
                .handle(&json!({"version":2,"op":"reply","id":id,"value":"yes"}))
                .is_err()
        );
        assert_eq!(backend.source.effects, 1);
    }
    #[test]
    fn pin_dialog_and_tree_tiles_share_legacy_tmux_server_identity() {
        let mut backend = backend();
        backend.source.observation.snapshot["panes"][0]["server"] = json!("tmux@/tmp/example.sock");
        backend.refresh().unwrap();
        let node = backend
            .topology()
            .nodes
            .into_iter()
            .find(|n| n.kind == "pane")
            .unwrap()
            .id();
        let (frames, _) = backend
            .handle(
                &json!({"version":2,"op":"action","page":0,"action":"terminal.pin","node":node}),
            )
            .unwrap();
        assert_eq!(frames[0]["workspace"]["pins"][0]["status"], "live");
        let terminal = frames[0]["tree"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "pane")
            .unwrap();
        assert_eq!(terminal["tile"]["pinned"], true);
        assert_eq!(backend.source.saves, 1);
        assert_eq!(backend.source.effects, 0);
    }
    #[test]
    fn telegram_observation_uses_authoritative_custom_token_file_not_stale_env() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let directory = std::env::temp_dir().join(format!(
            "daedalus-telegram-observation-{}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let token = directory.join("custom-token");
        layouts::write_private(&token, b"123:synthetic_telegram_token").unwrap();
        layouts::write_private(&directory.join("telegram.env"), b"stale_ignored").unwrap();
        let service = json!({"version":1,"instance":"fixture","bot_id":123,"token_file":token,"grants":[],"listen_chats":[],"registrations":[],"hook_bindings":[]});
        layouts::write_private(
            &directory.join("telegram-service.json"),
            &serde_json::to_vec(&service).unwrap(),
        )
        .unwrap();
        let config = ControlConfig::fresh(&directory).unwrap();
        let (health, _, saved) = telegram_observation(&directory, &config);
        assert_eq!(saved, Some(true));
        assert_eq!(health["configuration"]["bot_id"], 123);
        assert!(!health.to_string().contains("synthetic_telegram_token"));
        fs::remove_file(token).unwrap();
        let (_, _, saved) = telegram_observation(&directory, &config);
        assert_eq!(saved, Some(false));
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn native_route_projection_is_scoped_readonly_and_missing_is_explicit() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let directory =
            std::env::temp_dir().join(format!("daedalus-route-observation-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let config:super::super::telegram_config::ServiceConfig=serde_json::from_value(json!({"version":1,"instance":"fixture","bot_id":123,"token_file":directory.join("token"),"grants":[{"chat":42,"actor":7,"permissions":["read","send","manage"]}],"listen_chats":[],"registrations":[],"hook_bindings":[]})).unwrap();
        layouts::write_private(
            &directory.join("telegram-service.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let telegram = json!({"configured":true,"configuration":config.preview()});
        let absent = native_route_observation(&directory, &telegram).unwrap();
        assert_eq!(absent["status"], "missing");
        assert!(!directory.join("routes.sqlite3").exists());
        let db = directory.join("routes.sqlite3");
        let con = rusqlite::Connection::open(&db).unwrap();
        fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
        con.execute_batch("CREATE TABLE chat_routes(instance TEXT,bot TEXT,chat TEXT,identity TEXT,pinned INTEGER,revision INTEGER);").unwrap();
        let terminal = json!({"backend":"tmux","endpoint":crate::test_support::path("/tmp/fixture.sock"),"pane":"%1","generation":"generation"});
        for (instance, bot, chat) in [
            ("fixture", "123", "42"),
            ("other", "123", "42"),
            ("fixture", "999", "42"),
            ("fixture", "123", "43"),
        ] {
            con.execute(
                "INSERT INTO chat_routes VALUES(?,?,?,?,1,7)",
                rusqlite::params![instance, bot, chat, terminal.to_string()],
            )
            .unwrap();
        }
        drop(con);
        let before = fs::read(&db).unwrap();
        let report = native_route_observation(&directory, &telegram).unwrap();
        assert_eq!(report["status"], "ok");
        assert_eq!(report["native_routes"].as_array().unwrap().len(), 1);
        assert_eq!(report["routes"].as_object().unwrap().len(), 1);
        assert_eq!(report["native_routes"][0]["revision"], 7);
        assert_eq!(report["routes"]["42"], terminal);
        assert_eq!(before, fs::read(&db).unwrap());
        let mut changed = telegram.clone();
        changed["configuration"]["bot_id"] = json!(999);
        assert!(native_route_observation(&directory, &changed).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn self_context_and_preview_refuse_exhausted_saved_ancestry_limits() {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let mut nonce = [0; 16];
        crate::platform::random_bytes(&mut nonce).unwrap();
        let fixture = Fixture(
            std::env::temp_dir().join(format!("dd-self-metadata-{:x}", u128::from_le_bytes(nonce))),
        );
        let root = fixture.0.join("proc");
        let pid = std::process::id();
        for (child, parent) in [(pid, pid + 1), (pid + 1, 0)] {
            let path = root.join(child.to_string());
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("stat"),
                format!("{child} (owned synthetic process) S {parent}"),
            )
            .unwrap();
        }
        let pane = json!({"backend":"native","session":"fixture","pane_id":"fixture","terminal_id":"generation","pid":(pid+1).to_string()});
        let mut source = NativeSource {
            directory: fixture.0.clone(),
            terminals: Terminals {
                proc_root: root,
                ..Default::default()
            },
            last_snapshot: json!({"panes":[pane.clone()]}),
            usage_recorder: Default::default(),
        };
        let settings = super::super::runtime_settings::Settings {
            native_metadata_process_limit: 1,
            ..Default::default()
        };
        source.terminals.apply_settings(&settings).unwrap();
        assert!(
            source
                .attach_context(&BTreeMap::new(), String::new())
                .err()
                .unwrap()
                .contains("native_metadata_process_limit")
        );
        let result = source.previews(std::slice::from_ref(&pane), 10);
        assert!(
            result[0]
                .as_ref()
                .unwrap_err()
                .contains("native_metadata_process_limit")
        );
        let settings = super::super::runtime_settings::Settings {
            native_metadata_depth_limit: 1,
            ..Default::default()
        };
        source.terminals.apply_settings(&settings).unwrap();
        assert!(
            source
                .attach_context(&BTreeMap::new(), String::new())
                .err()
                .unwrap()
                .contains("native_metadata_depth_limit")
        );
        source
            .terminals
            .apply_settings(&Default::default())
            .unwrap();
        let context = source
            .attach_context(&BTreeMap::new(), String::new())
            .unwrap();
        assert!(
            context
                .own_terminals
                .contains(&terminals::identity(&pane).unwrap())
        );
    }
    /// A dashboard running inside a native terminal recognizes that terminal
    /// through live macOS ancestry, so it never attaches to itself.
    #[cfg(target_os = "macos")]
    #[test]
    fn live_macos_ancestry_marks_the_dashboards_own_terminal() {
        let parent = crate::platform::parent_pid(Path::new("/proc"), std::process::id()).unwrap();
        let own = json!({"backend":"native","session":"own","pane_id":"own","terminal_id":"generation","pid":parent.to_string()});
        let other = json!({"backend":"native","session":"other","pane_id":"other","terminal_id":"generation","pid":"999999"});
        let source = NativeSource {
            directory: std::env::temp_dir(),
            terminals: Terminals::default(),
            last_snapshot: json!({"panes":[own.clone(), other.clone()]}),
            usage_recorder: Default::default(),
        };
        let context = source
            .attach_context(&BTreeMap::new(), String::new())
            .unwrap();
        assert!(
            context
                .own_terminals
                .contains(&terminals::identity(&own).unwrap())
        );
        assert!(
            !context
                .own_terminals
                .contains(&terminals::identity(&other).unwrap())
        );
    }
    #[test]
    fn finish_local_setup_enables_verified_attach_and_preserves_existing_settings() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!(
            "daedalus-finish-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o775)).unwrap();
        let binary = directory.join("herdr");
        std::fs::write(&binary, "#!/bin/sh\nprintf '%s\n' '{\"pane\":{\"pane_id\":\"pane\",\"terminal_id\":\"generation\"}}'\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut source = NativeSource {
            directory: directory.clone(),
            terminals: Terminals {
                herdr: binary,
                ..Terminals::default()
            },
            last_snapshot: json!({}),
            usage_recorder: Default::default(),
        };
        let config = ControlConfig::fresh(&directory).unwrap();
        assert!(source.require_configuration().is_err());
        let configured = source.finish_local_setup(&config).unwrap();
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let saved = std::fs::read(directory.join("control.json")).unwrap();
        let pane = json!({"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation"});
        let answer = source
            .session(
                &configured,
                session_menu::Request::Attach {
                    pane,
                    mode: super::super::session_actions::AttachMode::Control,
                    node: "fixture".into(),
                },
            )
            .unwrap();
        let session_menu::Answer::Frame(plan) = answer else {
            panic!("Expected attach plan");
        };
        assert_eq!(plan["adapter"], "herdr-session");
        assert_eq!(plan["mode"], "control");
        let mut stale = config.clone();
        stale.agent_name = "stale".into();
        assert_eq!(source.finish_local_setup(&stale).unwrap(), configured);
        assert_eq!(
            std::fs::read(directory.join("control.json")).unwrap(),
            saved
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn missing_configuration_blocks_native_mutations_and_view_persistence() {
        let directory = std::env::temp_dir().join(format!(
            "daedalus-first-run-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        let mut source = NativeSource {
            directory: directory.clone(),
            terminals: Terminals::default(),
            last_snapshot: json!({}),
            usage_recorder: Default::default(),
        };
        source.save_layouts(&Layouts::default()).unwrap();
        let config =
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/project"))).unwrap();
        assert!(
            source
                .session(&config, session_menu::Request::Remove("checkpoint".into()))
                .is_err()
        );
        assert!(
            source
                .telegram(super::super::telegram_admin::Request::Token {
                    value: "123:fixture-token".into()
                })
                .is_err()
        );
        assert!(!directory.exists());
    }
    #[test]
    fn native_hook_switch_rejects_replacement_registration_and_preserves_other_options() {
        let directory = std::env::temp_dir().join(format!(
            "daedalus-switch-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        let config =
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/project"))).unwrap();
        layouts::write_private(
            &directory.join("control.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let registration:super::super::hook_runtime::Registration=serde_json::from_value(json!({"version":1,"id":"fixture","directory":directory,"scope":{"instance":"agent","terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"generation"},"harness":"claude","conversation":"session"},"goal_id":null,"harness_process":null,"transcript":null,"events":["Stop"],"options":{"telegram_delivery":false,"telegram_progress":true,"notifications":false,"hindsight":false},"activity_lease_ms":1000,"pending_timeout_ms":1000,"hook_timeout_ms":1000})).unwrap();
        let path = registration.path();
        layouts::write_private(&path, &serde_json::to_vec(&registration).unwrap()).unwrap();
        let mut source = NativeSource {
            directory: directory.clone(),
            terminals: Terminals::default(),
            last_snapshot: json!({}),
            usage_recorder: Default::default(),
        };
        source
            .session(
                &config,
                session_menu::Request::HookSwitch {
                    expected: Box::new(registration.clone()),
                    option: "telegram_delivery".into(),
                },
            )
            .unwrap();
        let current: super::super::hook_runtime::Registration =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(current.options.telegram_delivery && current.options.telegram_progress);
        let before = std::fs::read(&path).unwrap();
        assert!(
            source
                .session(
                    &config,
                    session_menu::Request::HookSwitch {
                        expected: Box::new(registration),
                        option: "telegram_delivery".into()
                    }
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(all(test, windows))]
mod windows_hook_tests {
    use super::*;
    #[test]
    fn opencode_plugin_defaults_use_config_not_account_storage() {
        let home = std::env::temp_dir().join("daedalus-profile-fixture");
        let mut env = BTreeMap::from([
            ("HOME".into(), home.to_string_lossy().into_owned()),
            (
                "XDG_DATA_HOME".into(),
                home.join("accounts").to_string_lossy().into_owned(),
            ),
        ]);
        let first = opencode_hook_path(&env, b"terminal-one").unwrap();
        assert_eq!(
            first.parent().unwrap(),
            home.join(".config/opencode/plugins")
        );
        assert_ne!(first, opencode_hook_path(&env, b"terminal-two").unwrap());
        env.insert(
            "OPENCODE_CONFIG_DIR".into(),
            home.join("custom").to_string_lossy().into_owned(),
        );
        assert_eq!(
            opencode_hook_path(&env, b"terminal-one")
                .unwrap()
                .parent()
                .unwrap(),
            home.join("custom/plugins")
        );
        env.insert("OPENCODE_CONFIG_DIR".into(), "relative".into());
        assert!(opencode_hook_path(&env, b"terminal-one").is_err());
    }
}
