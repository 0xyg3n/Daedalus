//! Private layout state. The daemon must serialize mutations through one writer.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::{fs::OpenOptions, path::Component};

pub type Result<T> = std::result::Result<T, String>;
const MAX_FILE: u64 = 8 * 1024 * 1024;
static SERIAL: AtomicU64 = AtomicU64::new(0);

pub use super::identity::Backend;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalRef {
    pub backend: Backend,
    pub server: String,
    pub pane_id: String,
    pub terminal_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub generation: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub place: String,
}
impl TerminalRef {
    pub fn validate(&self) -> Result<()> {
        for field in [&self.server, &self.pane_id, &self.terminal_id] {
            if field.is_empty() || field.len() > 4096 || field.chars().any(char::is_control) {
                return Err("Terminal identity is incomplete or invalid".into());
            }
        }
        Ok(())
    }
    pub fn same_identity(&self, other: &Self) -> bool {
        self.backend == other.backend
            && self.server == other.server
            && self.pane_id == other.pane_id
            && self.terminal_id == other.terminal_id
            && self.generation == other.generation
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    Live(usize),
    Stale,
    Missing,
    Ambiguous,
}
pub fn resolve(reference: &TerminalRef, live: &[TerminalRef]) -> Resolution {
    if reference.validate().is_err() {
        return Resolution::Missing;
    }
    let exact: Vec<_> = live
        .iter()
        .enumerate()
        .filter(|(_, p)| reference.same_identity(p))
        .collect();
    match exact.len() {
        1 => Resolution::Live(exact[0].0),
        n if n > 1 => Resolution::Ambiguous,
        _ if live.iter().any(|p| {
            p.backend == reference.backend
                && p.server == reference.server
                && p.pane_id == reference.pane_id
        }) =>
        {
            Resolution::Stale
        }
        _ => Resolution::Missing,
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    #[default]
    Tree,
    Tiles,
    Wall,
    Cube,
    Focus,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sizes {
    pub sidebar: i32,
    pub details: i32,
    pub screen: i32,
    pub tiles: i32,
    #[serde(default = "cube_width_default")]
    pub cube_width: i32,
    #[serde(default = "cube_height_default")]
    pub cube_height: i32,
}
fn cube_width_default() -> i32 {
    60
}
fn cube_height_default() -> i32 {
    50
}
impl Sizes {
    pub fn normalized(mut self) -> Self {
        self.sidebar = match self.sidebar {
            1..=11 => 0,
            12..=15 => 16,
            n => n.clamp(0, 40),
        };
        self.details = self.details.clamp(20, 80);
        self.screen = self.screen.clamp(20, 90);
        self.tiles = self.tiles.clamp(100, 300);
        self.cube_width = self.cube_width.clamp(40, 84);
        self.cube_height = self.cube_height.clamp(30, 80);
        self
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct View {
    #[serde(default)]
    pub mode: ViewMode,
    #[serde(default)]
    pub pins: Vec<TerminalRef>,
    #[serde(default)]
    pub selected: Option<TerminalRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Sizes>,
    #[serde(default, flatten)]
    pub preferences: BTreeMap<String, serde_json::Value>,
}
impl View {
    pub fn validate(&self) -> Result<()> {
        if self.pins.len() > 64 {
            return Err("At most 64 terminals may be pinned".into());
        }
        for (i, pin) in self.pins.iter().enumerate() {
            pin.validate()?;
            if self.pins[..i].iter().any(|p| p.same_identity(pin)) {
                return Err("Duplicate pinned terminal".into());
            }
        }
        if let Some(selected) = &self.selected {
            selected.validate()?;
        }
        for (key, value) in &self.preferences {
            let allowed: &[&str] = match key.as_str() {
                "backdrop" => &["glow", "art", "art-bright", "off"],
                "order" => &["activity", "layout"],
                "saver" | "details" | "screen" | "pinned" | "recent" | "badges" | "vitals"
                | "motion" | "keys" => &["on", "off"],
                "saved_at" => continue,
                _ => return Err(format!("Unknown view preference: {key}")),
            };
            if !value.as_str().is_some_and(|s| allowed.contains(&s)) {
                return Err(format!("Invalid preference: {key}"));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachMode {
    #[default]
    Observe,
    Control,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    OpenLayout {
        layout: String,
    },
    Select {
        terminal: TerminalRef,
    },
    Pin {
        terminal: TerminalRef,
    },
    RouteTelegram {
        terminal: TerminalRef,
        chat: String,
    },
    Attach {
        terminal: TerminalRef,
        #[serde(default)]
        mode: AttachMode,
    },
}
impl Step {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::OpenLayout { layout } => {
                validate_name(layout)?;
            }
            Self::Select { terminal } | Self::Pin { terminal } | Self::Attach { terminal, .. } => {
                terminal.validate()?
            }
            Self::RouteTelegram { terminal, chat } => {
                terminal.validate()?;
                if chat.parse::<i64>().is_err() || chat == "0" {
                    return Err("Telegram chat ID must be a nonzero signed integer".into());
                }
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub steps: Vec<Step>,
    #[serde(default)]
    pub saved_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layouts {
    pub version: u32,
    pub current: View,
    pub layouts: BTreeMap<String, View>,
    pub workflows: BTreeMap<String, Workflow>,
}
impl Default for Layouts {
    fn default() -> Self {
        Self {
            version: 1,
            current: View::default(),
            layouts: BTreeMap::new(),
            workflows: BTreeMap::new(),
        }
    }
}
impl Layouts {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err("Unsupported layout version".into());
        }
        self.current.validate()?;
        for (name, view) in &self.layouts {
            validate_name(name)?;
            view.validate()?;
        }
        for (name, workflow) in &self.workflows {
            validate_name(name)?;
            if workflow.steps.len() > 20 {
                return Err("At most 20 workflow steps allowed".into());
            }
            for step in &workflow.steps {
                step.validate()?;
            }
        }
        Ok(())
    }
    pub fn load(directory: &Path) -> Result<Self> {
        let path = directory.join("layouts.json");
        let state = match read_private(&path)? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
            None => Self::default(),
        };
        state.validate()?;
        Ok(state)
    }
    pub fn save(&self, directory: &Path) -> Result<()> {
        self.validate()?;
        write_private(
            &directory.join("layouts.json"),
            &serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?,
        )
    }
    pub fn toggle_pin(&mut self, reference: TerminalRef) -> Result<bool> {
        reference.validate()?;
        if let Some(index) = self
            .current
            .pins
            .iter()
            .position(|p| p.same_identity(&reference))
        {
            self.current.pins.remove(index);
            return Ok(false);
        }
        if self.current.pins.len() >= 64 {
            return Err("Unpin a terminal first".into());
        }
        self.current.pins.push(reference);
        Ok(true)
    }
    pub fn save_layout(&mut self, name: &str, mut view: View) -> Result<()> {
        let name = validate_name(name)?;
        view.validate()?;
        view.preferences.clear();
        view.sizes = view.sizes.map(Sizes::normalized);
        self.layouts.insert(name, view);
        Ok(())
    }
    pub fn open_layout(&mut self, name: &str) -> Result<&View> {
        let mut view = self
            .layouts
            .get(name)
            .ok_or("Layout no longer exists")?
            .clone();
        view.preferences = self.current.preferences.clone();
        view.preferences.remove("saved_at");
        self.current = view;
        Ok(&self.current)
    }
}
pub fn validate_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
        return Err("Use 1 to 60 printable characters".into());
    }
    Ok(name.into())
}

// Reject symlinks in the complete path. The instance directory must be private;
// this is not a substitute for descriptor-relative operations in a hostile directory.
#[cfg(unix)]
pub(crate) fn check_path(path: &Path) -> Result<()> {
    #[cfg(windows)]
    crate::platform::check_path(path).map_err(|e| e.to_string())?;
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("State path must be absolute without parent traversal".into());
    }
    let mut current = std::path::PathBuf::new();
    for part in path.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("Symlink state paths are forbidden".into());
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
pub(crate) fn read_private(path: &Path) -> Result<Option<Vec<u8>>> {
    read_private_limit(path, MAX_FILE)
}
#[cfg(unix)]
pub(crate) fn read_private_limit(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    use std::os::unix::fs::OpenOptionsExt;
    check_path(path)?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Expected a regular state file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("State file exceeds size limit".into());
    }
    Ok(Some(bytes))
}
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    write_private_limit(path, bytes, MAX_FILE)
}
#[cfg(unix)]
pub(crate) fn write_private_limit(path: &Path, bytes: &[u8], limit: u64) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    check_path(path)?;
    if bytes.len() as u64 > limit {
        return Err("State file exceeds size limit".into());
    }
    let parent = path.parent().ok_or("Missing state directory")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|e| e.to_string())?;
    if fs::metadata(parent).map_err(|e| e.to_string())?.uid() != unsafe { libc::geteuid() } {
        return Err("State directory must be owned by the current user".into());
    }
    if fs::metadata(parent)
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err("State directory must be private (0700)".into());
    }
    let temporary = parent.join(format!(
        ".daedalus-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
pub(crate) fn check_path(path: &Path) -> Result<()> {
    crate::platform::check_path(path).map_err(|e| e.to_string())
}
#[cfg(windows)]
pub(crate) fn read_private_limit(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let file = match crate::platform::open_private(path, false, false) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("State file exceeds size limit".into());
    }
    Ok(Some(bytes))
}
#[cfg(windows)]
pub(crate) fn write_private_limit(path: &Path, bytes: &[u8], limit: u64) -> Result<()> {
    check_path(path)?;
    if bytes.len() as u64 > limit {
        return Err("State file exceeds size limit".into());
    }
    let parent = path.parent().ok_or("Missing state directory")?;
    crate::platform::ensure_private_dir(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(
        ".daedalus-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = crate::platform::create_private_new(&temp).map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        crate::platform::replace_private(&temp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn reference() -> TerminalRef {
        TerminalRef {
            backend: Backend::Tmux,
            server: crate::test_support::path("/tmp/socket").into(),
            pane_id: "%1".into(),
            terminal_id: "pid:42:start:100".into(),
            generation: String::new(),
            label: "Agent".into(),
            place: String::new(),
        }
    }
    #[test]
    fn stale_is_not_rebound() {
        let saved = reference();
        let mut live = saved.clone();
        live.terminal_id = "other".into();
        assert_eq!(resolve(&saved, &[live]), Resolution::Stale);
        assert_eq!(
            resolve(&saved, &[saved.clone(), saved.clone()]),
            Resolution::Ambiguous
        );
    }
    #[test]
    fn opening_layout_keeps_preferences() {
        let mut s = Layouts::default();
        s.current.preferences.insert("motion".into(), "off".into());
        s.save_layout(
            "Work",
            View {
                mode: ViewMode::Tiles,
                ..View::default()
            },
        )
        .unwrap();
        s.open_layout("Work").unwrap();
        assert_eq!(s.current.preferences["motion"], "off");
    }
    #[test]
    fn rejects_workflow_code() {
        assert!(
            serde_json::from_str::<Step>(
                r#"{"type":"open_layout","layout":"Work","command":"sh"}"#
            )
            .is_err()
        );
    }
    #[test]
    fn cube_sizes_load_legacy_defaults_and_roundtrip() {
        let old: Sizes =
            serde_json::from_str(r#"{"sidebar":24,"details":44,"screen":60,"tiles":100}"#).unwrap();
        assert_eq!((old.cube_width, old.cube_height), (60, 50));
        let custom = Sizes {
            cube_width: 74,
            cube_height: 68,
            ..old
        };
        let saved = serde_json::to_string(&custom).unwrap();
        assert_eq!(
            serde_json::from_str::<Sizes>(&saved).unwrap().normalized(),
            custom
        );
    }
    #[test]
    fn sizes_match_frontend() {
        assert_eq!(
            Sizes {
                sidebar: 13,
                details: 100,
                screen: 1,
                tiles: 600,
                cube_width: 100,
                cube_height: 0
            }
            .normalized(),
            Sizes {
                sidebar: 16,
                details: 80,
                screen: 20,
                tiles: 300,
                cube_width: 84,
                cube_height: 30
            }
        );
    }
    #[cfg(unix)]
    #[test]
    fn roundtrip_and_symlink_rejection() {
        let dir = std::env::temp_dir().join(format!(
            "daedalus-layout-test-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let mut state = Layouts::default();
        state.toggle_pin(reference()).unwrap();
        state.save(&dir).unwrap();
        assert_eq!(Layouts::load(&dir).unwrap().current.pins.len(), 1);
        let before = fs::read(dir.join("layouts.json")).unwrap();
        state.version = 2;
        assert!(state.save(&dir).is_err());
        assert_eq!(fs::read(dir.join("layouts.json")).unwrap(), before);
        std::os::unix::fs::symlink(dir.join("layouts.json"), dir.join("linked.json")).unwrap();
        assert!(read_private(&dir.join("linked.json")).is_err());
        assert!(write_private(&dir.join("linked.json"), b"bad").is_err());
        assert!(read_private(&dir.join("../escape")).is_err());
        let fifo = dir.join("fifo");
        let encoded = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
        assert!(read_private(&fifo).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
