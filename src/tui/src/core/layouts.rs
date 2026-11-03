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
    /// Decode identity without retaining potentially large display decorations.
    pub fn decode_identity(value: &serde_json::Value) -> Option<Self> {
        let backend = match value.get("backend")?.as_str()? {
            "tmux" => Backend::Tmux,
            "herdr" => Backend::Herdr,
            "native" => Backend::Native,
            _ => return None,
        };
        let field = |name: &str| -> Option<String> {
            let text = value.get(name)?.as_str()?;
            (text.len() <= 4096 && !text.chars().any(char::is_control)).then(|| text.to_owned())
        };
        for name in ["label", "place"] {
            if value.get(name).is_some_and(|field| !field.is_string()) {
                return None;
            }
        }
        let reference = Self {
            backend,
            server: field("server")?,
            pane_id: field("pane_id")?,
            terminal_id: field("terminal_id")?,
            generation: if value.get("generation").is_some() {
                field("generation")?
            } else {
                String::new()
            },
            label: String::new(),
            place: String::new(),
        };
        reference.validate().ok()?;
        Some(reference)
    }
    /// Canonical endpoint identity, including the two historical display aliases.
    /// No filesystem canonicalization or identity inference is performed.
    pub fn identity(&self) -> Result<super::identity::TerminalId> {
        let endpoint = match self.backend {
            Backend::Native if self.server == "local" => self.pane_id.clone(),
            Backend::Tmux if self.server.starts_with("tmux@") => {
                let mut decoded = Vec::new();
                let mut bytes = self.server[5..].bytes();
                while let Some(byte) = bytes.next() {
                    if byte == b'%' {
                        let high = bytes.next().and_then(|b| (b as char).to_digit(16));
                        let low = bytes.next().and_then(|b| (b as char).to_digit(16));
                        let (Some(high), Some(low)) = (high, low) else {
                            return Err("Invalid saved tmux endpoint encoding".into());
                        };
                        decoded.push((high * 16 + low) as u8);
                    } else {
                        decoded.push(byte);
                    }
                }
                String::from_utf8(decoded).map_err(|_| "Invalid saved tmux endpoint encoding")?
            }
            _ => self.server.clone(),
        };
        let identity = super::identity::TerminalId {
            backend: self.backend.clone(),
            endpoint,
            pane: self.pane_id.clone(),
            generation: self.terminal_id.clone(),
        };
        identity.validate().map_err(str::to_owned)?;
        Ok(identity)
    }
    pub fn validate(&self) -> Result<()> {
        for field in [&self.server, &self.pane_id, &self.terminal_id] {
            if field.is_empty() || field.len() > 4096 || field.chars().any(char::is_control) {
                return Err("Terminal identity is incomplete or invalid".into());
            }
        }
        if self.generation.len() > 4096 || self.generation.chars().any(char::is_control) {
            return Err("Invalid supplemental terminal generation".into());
        }
        self.identity()?;
        Ok(())
    }
    pub fn same_identity(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self
                .identity()
                .ok()
                .zip(other.identity().ok())
                .is_some_and(|(a, b)| a == b)
    }
    /// Opaque UI selector includes every saved identity field, never labels.
    pub fn key(&self) -> String {
        serde_json::to_string(&(
            &self.backend,
            &self.server,
            &self.pane_id,
            &self.terminal_id,
            &self.generation,
        ))
        .expect("Terminal reference strings serialize")
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
            reference
                .identity()
                .ok()
                .zip(p.identity().ok())
                .is_some_and(|(a, b)| {
                    a.backend == b.backend && a.endpoint == b.endpoint && a.pane == b.pane
                })
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
    fn legacy_display_endpoints_resolve_exactly_and_full_selectors_keep_generations() {
        let live = reference();
        let mut saved = live.clone();
        saved.server = format!(
            "tmux@{}",
            live.server
                .bytes()
                .map(|b| format!("%{b:02X}"))
                .collect::<String>()
        );
        assert!(saved.same_identity(&live));
        assert_eq!(
            resolve(&saved, std::slice::from_ref(&live)),
            Resolution::Live(0)
        );
        saved.generation = "older".into();
        assert_eq!(
            resolve(&saved, std::slice::from_ref(&live)),
            Resolution::Stale
        );
        assert_ne!(saved.key(), live.key());
        saved.server = "tmux@%ZZ".into();
        assert!(saved.validate().is_err());
        let mut native = live;
        native.backend = Backend::Native;
        native.server = "native-1".into();
        native.pane_id = "native-1".into();
        let mut old = native.clone();
        old.server = "local".into();
        assert!(old.same_identity(&native));
        old.generation = "bad\nidentity".into();
        assert!(old.validate().is_err());
    }
    #[test]
    fn identity_decoder_preserves_serde_validation_without_copying_display_labels() {
        let mut value = serde_json::to_value(reference()).unwrap();
        value["label"] = serde_json::json!("label".repeat(100000));
        value["place"] = serde_json::json!("place".repeat(100000));
        let decoded = TerminalRef::decode_identity(&value).unwrap();
        assert!(decoded.same_identity(&reference()));
        assert!(decoded.label.is_empty() && decoded.place.is_empty());
        assert!(serde_json::to_vec(&decoded).unwrap().len() < 1024);
        for key in [
            "server",
            "pane_id",
            "terminal_id",
            "generation",
            "label",
            "place",
        ] {
            let mut invalid = value.clone();
            invalid[key] = serde_json::json!(17);
            assert!(TerminalRef::decode_identity(&invalid).is_none());
            assert!(serde_json::from_value::<TerminalRef>(invalid).is_err());
        }
        value["generation"] = serde_json::json!("x".repeat(4097));
        assert!(TerminalRef::decode_identity(&value).is_none());
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
