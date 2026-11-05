//! Global dashboard discovery preferences; unrelated saved keys stay private.
use super::interface_settings::{Kind, Setting};
use crate::core::layouts;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
pub(crate) const LIMIT: u64 = 64 * 1024;
// Recovery may retain an oversized legacy file, but never reads unbounded data.
const RECOVERY_LIMIT: u64 = super::config::MAX_CONFIG_BYTES as u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub scan_default_roots: bool,
    pub scan_processes: bool,
    pub instance_limit: u64,
    pub root_limit: u64,
    pub entries_per_root: u64,
    pub process_limit: u64,
    pub argv_limit_bytes: u64,
    pub search_roots: Vec<PathBuf>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            scan_default_roots: true,
            scan_processes: true,
            instance_limit: 128,
            root_limit: 16,
            entries_per_root: 1024,
            process_limit: 4096,
            argv_limit_bytes: 8192,
            search_roots: vec![],
        }
    }
}
pub const FIELDS: &[Setting] = &[
    Setting {
        key: "scan_default_roots",
        label: "Scan default configuration roots",
        group: "Sources",
        description: "Include the platform's default roots on the next refresh. Explicit search roots remain selected.",
        kind: Kind::Toggle,
    },
    Setting {
        key: "scan_processes",
        label: "Discover owned service processes",
        group: "Sources",
        description: "Inspect owned Linux process candidates. Windows arbitrary process discovery remains unavailable. A candidate is never proof of a running service.",
        kind: Kind::Toggle,
    },
    Setting {
        key: "instance_limit",
        label: "Instance candidate limit",
        group: "Limits",
        description: "Maximum distinct candidates. Saved-directory checks include the preference root; discovery always retains the current instance.",
        kind: Kind::Number { min: 1, max: 4096 },
    },
    Setting {
        key: "root_limit",
        label: "Configuration search roots",
        group: "Limits",
        description: "Maximum distinct roots examined, with explicit roots first. Each scan is only one directory level deep.",
        kind: Kind::Number { min: 1, max: 256 },
    },
    Setting {
        key: "entries_per_root",
        label: "Directory entries per root",
        group: "Limits",
        description: "Maximum filesystem entries examined in each configuration search root.",
        kind: Kind::Number { min: 1, max: 65536 },
    },
    Setting {
        key: "process_limit",
        label: "Process entries examined",
        group: "Limits",
        description: "Maximum entries examined in the owned Linux process candidate scan.",
        kind: Kind::Number { min: 1, max: 65536 },
    },
    Setting {
        key: "argv_limit_bytes",
        label: "Process command size (bytes)",
        group: "Limits",
        description: "Maximum command-line bytes accepted for an owned service candidate; exact executable and process incarnation are still verified independently.",
        kind: Kind::Number {
            min: 1024,
            max: 131072,
        },
    },
];
impl Settings {
    pub fn validate(&self) -> Result<()> {
        let value = serde_json::to_value(self).map_err(|_| "Cannot inspect dashboard settings")?;
        for field in FIELDS {
            if let Kind::Number { min, max } = field.kind
                && !value[field.key]
                    .as_u64()
                    .is_some_and(|v| (min..=max).contains(&v))
            {
                return Err(format!("Invalid dashboard setting: {}", field.key));
            }
        }
        if self.search_roots.len() as u64 > self.root_limit {
            return Err("Remove search roots before lowering root_limit".into());
        }
        let mut roots = BTreeSet::new();
        for path in &self.search_roots {
            layouts::check_path(path)
                .map_err(|_| "Search roots must be absolute and symlink-free")?;
            if !roots.insert(path) {
                return Err("Duplicate dashboard search root".into());
            }
        }
        Ok(())
    }
    pub fn display(&self, key: &str) -> Result<String> {
        if !FIELDS.iter().any(|f| f.key == key) {
            return Err("Unknown dashboard setting".into());
        }
        Ok(
            serde_json::to_value(self).map_err(|_| "Cannot inspect dashboard setting")?[key]
                .to_string(),
        )
    }
    pub fn set(&self, key: &str, text: &str) -> Result<Self> {
        let field = FIELDS
            .iter()
            .find(|f| f.key == key)
            .ok_or("Unknown dashboard setting")?;
        let mut value =
            serde_json::to_value(self).map_err(|_| "Cannot inspect dashboard settings")?;
        value[key] = match field.kind {
            Kind::Toggle => Value::Bool(match text.trim() {
                "true" => true,
                "false" => false,
                _ => return Err("Enter true or false".into()),
            }),
            Kind::Number { min, max } => {
                let n = text
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| "Enter a whole number")?;
                if !(min..=max).contains(&n) {
                    return Err(format!("Enter a number from {min} to {max}"));
                }
                Value::from(n)
            }
            Kind::Choice(_) => unreachable!(),
        };
        let updated: Self =
            serde_json::from_value(value).map_err(|_| "Invalid dashboard settings")?;
        updated.validate()?;
        Ok(updated)
    }
    pub fn add_root(&self, text: &str) -> Result<Self> {
        let path = super::hooks::windows_native_path(Path::new(text));
        let mut updated = self.clone();
        if !updated.search_roots.contains(&path) {
            updated.search_roots.push(path);
        }
        updated.validate()?;
        Ok(updated)
    }
    pub fn remove_root(&self, text: &str) -> Result<Self> {
        let path = super::hooks::windows_native_path(Path::new(text));
        let mut updated = self.clone();
        if !updated.search_roots.contains(&path) {
            return Err("Search root is not configured".into());
        }
        updated.search_roots.retain(|p| p != &path);
        updated.validate()?;
        Ok(updated)
    }
}
pub(crate) fn settings(value: &Value) -> Result<Settings> {
    let settings: Settings =
        serde_json::from_value(value.get("discovery").cloned().unwrap_or_else(|| json!({})))
            .map_err(|_| "Invalid dashboard discovery preferences")?;
    settings.validate()?;
    Ok(settings)
}
pub(crate) fn saved_paths(directory: &Path, value: &Value) -> Result<BTreeSet<PathBuf>> {
    let mut paths = BTreeSet::from([directory.to_path_buf()]);
    let entries = match value.get("instances") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(values)) => values.as_slice(),
        _ => return Err("Invalid saved instance directories".into()),
    };
    for entry in entries {
        let path = PathBuf::from(entry.as_str().ok_or("Invalid saved instance directory")?);
        layouts::check_path(&path).map_err(|_| "Unsafe saved instance directory")?;
        paths.insert(path);
    }
    match value.get("last_instance") {
        None | Some(Value::Null) => (),
        Some(Value::String(text)) => {
            let path = PathBuf::from(text);
            layouts::check_path(&path).map_err(|_| "Unsafe saved startup selection")?;
            paths.insert(path);
        }
        _ => return Err("Invalid saved startup selection".into()),
    }
    Ok(paths)
}

pub(crate) fn encode(directory: &Path, value: &Value) -> Result<Vec<u8>> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|_| "Cannot encode dashboard preferences")?;
    if bytes.len() as u64 > LIMIT {
        return Err("Dashboard preferences exceed 64 KiB".into());
    }
    let configured = settings(value)?;
    if saved_paths(directory, value)?.len() as u64 > configured.instance_limit {
        return Err(
            "Saved instance limit reached; remove saved entries or raise instance_limit".into(),
        );
    }
    Ok(bytes)
}
#[derive(Clone)]
pub struct State {
    pub settings: Settings,
    pub(crate) value: Value,
    directory: PathBuf,
    before: Option<Vec<u8>>,
}
impl State {
    pub fn load(directory: &Path) -> Result<Self> {
        if directory.to_str().is_none() {
            return Err("Dashboard preference directory must be UTF-8".into());
        }
        layouts::check_path(directory)?;
        let before = layouts::read_private_limit(&directory.join("dashboard.json"), LIMIT)?;
        Self::from_bytes(directory, before)
    }
    fn from_bytes(directory: &Path, before: Option<Vec<u8>>) -> Result<Self> {
        if directory.to_str().is_none() {
            return Err("Dashboard preference directory must be UTF-8".into());
        }
        layouts::check_path(directory)?;
        if before
            .as_ref()
            .is_some_and(|bytes| bytes.len() as u64 > LIMIT)
        {
            return Err("Dashboard preferences exceed 64 KiB".into());
        }
        let value: Value = match &before {
            Some(bytes) => {
                serde_json::from_slice(bytes).map_err(|_| "Invalid dashboard preferences")?
            }
            None => json!({}),
        };
        if !value.is_object() {
            return Err("Invalid dashboard preferences".into());
        }
        saved_paths(directory, &value)?;
        Ok(Self {
            settings: settings(&value)?,
            value,
            directory: directory.into(),
            before,
        })
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn saved_instances(&self) -> Vec<PathBuf> {
        self.value["instances"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(std::iter::once(&self.value["last_instance"]))
            .filter_map(|v| v.as_str().map(PathBuf::from))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn show(&self) -> Value {
        json!({"preference_directory":self.directory,"configured":self.before.is_some(),"configuration":self.settings,"saved_instances":self.saved_instances(),"last_instance":self.value["last_instance"],"editable_settings":FIELDS,"root_operations":["add-root","remove-root"],"instance_operations":["add-instance","remove-instance"]})
    }
}
pub struct Plan {
    directory: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
    previous: Settings,
    settings: Settings,
    previous_instances: Vec<PathBuf>,
    instances: Vec<PathBuf>,
    previous_last: Value,
    last: Value,
    recovery: bool,
}
impl Plan {
    pub fn is_recovery(&self) -> bool {
        self.recovery
    }
    /// Explicit replacement only after the caller reviews loss of preferences.
    /// Original malformed bytes stay private and are backed up on apply.
    pub fn recover(directory: &Path) -> Result<Self> {
        if directory.to_str().is_none() {
            return Err("Dashboard preference directory must be UTF-8".into());
        }
        layouts::check_path(directory)?;
        let before = super::telegram_config::read_private(
            &directory.join("dashboard.json"),
            RECOVERY_LIMIT,
        ).map_err(|_| {
            "Recovery needs a private regular file owned by you, without links, no larger than 1 MiB".to_owned()
        })?
        .ok_or("Dashboard preferences are missing; recovery is unnecessary")?;
        if State::from_bytes(directory, Some(before.clone())).is_ok() {
            return Err("Dashboard preferences are valid; use the settings editor".into());
        }
        let settings = Settings {
            scan_default_roots: false,
            scan_processes: false,
            ..Default::default()
        };
        let after = encode(directory, &json!({"discovery":settings}))?;
        Ok(Self {
            directory: directory.into(),
            before: Some(before),
            after,
            previous: Settings::default(),
            settings,
            previous_instances: vec![],
            instances: vec![],
            previous_last: Value::Null,
            last: Value::Null,
            recovery: true,
        })
    }
    pub fn prepare(state: State, updated: Settings) -> Result<Self> {
        updated.validate()?;
        let mut value = state.value.clone();
        value["discovery"] =
            serde_json::to_value(&updated).map_err(|_| "Cannot encode dashboard settings")?;
        Self::prepare_value(state, value)
    }
    pub fn add_instance(state: State, text: &str) -> Result<Self> {
        let path = super::hooks::windows_native_path(Path::new(text));
        super::instances::configuration(&path)?;
        let mut value = state.value.clone();
        let mut paths = state.saved_instances();
        if !paths.contains(&path) {
            paths.push(path);
        }
        value["instances"] = json!(paths);
        Self::prepare_value(state, value)
    }
    pub fn remove_instance(state: State, text: &str) -> Result<Self> {
        let path = super::hooks::windows_native_path(Path::new(text));
        let mut paths = state.saved_instances();
        if !paths.contains(&path) {
            return Err("Instance directory is not saved".into());
        }
        paths.retain(|p| p != &path);
        let mut value = state.value.clone();
        value["instances"] = json!(paths);
        if value["last_instance"]
            .as_str()
            .is_some_and(|p| Path::new(p) == path)
        {
            value.as_object_mut().unwrap().remove("last_instance");
        }
        Self::prepare_value(state, value)
    }
    fn prepare_value(state: State, value: Value) -> Result<Self> {
        if layouts::read_private_limit(&state.directory.join("dashboard.json"), LIMIT)?
            != state.before
        {
            return Err("Dashboard preferences changed; reopen settings".into());
        }
        let updated = settings(&value)?;
        let previous_instances = state.saved_instances();
        let instances = value["instances"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(std::iter::once(&value["last_instance"]))
            .filter_map(|v| v.as_str().map(PathBuf::from))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let previous_last = state.value["last_instance"].clone();
        let last = value["last_instance"].clone();
        let after = encode(&state.directory, &value)?;
        Ok(Self {
            directory: state.directory,
            before: state.before,
            after,
            previous: state.settings,
            settings: updated,
            previous_instances,
            instances,
            previous_last,
            last,
            recovery: false,
        })
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn approval(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(&self.directory, &self.before, &self.after, self.recovery))
                    .expect("serializable dashboard plan")
            )
        )
    }
    pub fn review(&self) -> Value {
        if self.recovery {
            return json!({"recovery":true,"preference_directory":self.directory,"configuration":self.settings,"original_bytes":self.before.as_ref().map(Vec::len),"backup_file":self.backup_path(),"backup_required":true,"changes":true,"approval":self.approval(),"notice":"Replace all dashboard preferences, including saved instance references, startup selection and unrecognized keys. The original file is kept in a private backup. Automatic instance scanning starts disabled; configure it afterwards. Native accounts, instances and running terminals are retained."});
        }
        json!({"preference_directory":self.directory,"previous_configuration":self.previous,"configuration":self.settings,"previous_saved_instances":self.previous_instances,"saved_instances":self.instances,"previous_last_instance":self.previous_last,"last_instance":self.last,"backup_required":self.before.is_some() && self.before.as_deref()!=Some(self.after.as_slice()),"changes":self.before.as_deref()!=Some(self.after.as_slice()),"approval":self.approval()})
    }
    pub fn apply(self, approval: &str) -> Result<()> {
        if approval != self.approval() {
            return Err("Dashboard approval does not match review".into());
        }
        let _lock = super::telegram_config::configuration_lock(&self.directory)?;
        if self.read_before(&self.directory.join("dashboard.json"))? != self.before {
            return Err("Dashboard preferences changed; reopen settings".into());
        }
        if self.before.as_deref() == Some(self.after.as_slice()) {
            return Ok(());
        }
        if let Some(before) = &self.before {
            let backup = self
                .backup_path()
                .expect("existing original has a backup path");
            match self.read_before(&backup)? {
                Some(bytes) if bytes != *before => return Err("Dashboard backup conflicts".into()),
                Some(_) => (),
                None => layouts::write_private_limit(
                    &backup,
                    before,
                    if self.recovery { RECOVERY_LIMIT } else { LIMIT },
                )?,
            }
        }
        layouts::write_private(&self.directory.join("dashboard.json"), &self.after)
    }
    fn read_before(&self, path: &Path) -> Result<Option<Vec<u8>>> {
        if self.recovery {
            super::telegram_config::read_private(path, RECOVERY_LIMIT)
        } else {
            layouts::read_private_limit(path, LIMIT)
        }
    }
    fn backup_path(&self) -> Option<PathBuf> {
        self.before.as_ref().map(|bytes| {
            self.directory
                .join(format!("dashboard.backup-{:x}.json", Sha256::digest(bytes)))
        })
    }
}
pub const HELP: &str = "daedalus dashboard config show --config-dir ABS\ndaedalus dashboard config set --config-dir ABS --key SETTING --value VALUE [--apply]\ndaedalus dashboard config add-root|remove-root|add-instance|remove-instance --config-dir ABS --path ABS [--apply]\ndaedalus dashboard config recover --config-dir ABS [--apply]\nThis directory holds global dashboard.json preferences. Settings affect the next discovery refresh.\nChanges preview by default. --apply saves the reviewed bytes with a private backup; concurrent changes are refused.\nRecovery replaces damaged preferences, clearing saved references and disabling automatic instance scanning.\nNo service, terminal or provider is started.\n";
pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    if args.is_empty() || args == ["--help"] || args == ["config"] || args == ["config", "--help"] {
        return write!(out, "{HELP}").map_err(|_| "Cannot write dashboard help".into());
    }
    if args.first().is_none_or(|v| v != "config")
        || ![
            "show",
            "set",
            "add-root",
            "remove-root",
            "add-instance",
            "remove-instance",
            "recover",
        ]
        .contains(&args[1].as_str())
    {
        return Err("Use dashboard config show, set, add-root, remove-root, add-instance, remove-instance or recover".into());
    }
    let verb = args[1].as_str();
    let mut flags = BTreeMap::new();
    let mut apply = false;
    let mut rest = args[2..].iter();
    while let Some(flag) = rest.next() {
        if flag == "--apply" {
            if apply || verb == "show" {
                return Err("Invalid or repeated dashboard apply option".into());
            }
            apply = true;
            continue;
        }
        let allowed = flag == "--config-dir"
            || match verb {
                "set" => ["--key", "--value"].contains(&flag.as_str()),
                "add-root" | "remove-root" | "add-instance" | "remove-instance" => flag == "--path",
                _ => false,
            };
        if !allowed {
            return Err("Unknown dashboard configuration option".into());
        }
        let value = rest.next().ok_or("Missing dashboard option value")?;
        if flags.insert(flag.as_str(), value.as_str()).is_some() {
            return Err("Repeated dashboard configuration option".into());
        }
    }
    let flag = |key: &str| -> Result<&str> {
        flags
            .get(key)
            .copied()
            .ok_or_else(|| format!("Missing {key}"))
    };
    let directory = Path::new(flag("--config-dir")?);
    let result = if verb == "show" {
        State::load(directory)?.show()
    } else {
        let plan = if verb == "recover" {
            Plan::recover(directory)?
        } else {
            let state = State::load(directory)?;
            match verb {
                "add-instance" => Plan::add_instance(state, flag("--path")?)?,
                "remove-instance" => Plan::remove_instance(state, flag("--path")?)?,
                _ => {
                    let updated = match verb {
                        "set" => state.settings.set(flag("--key")?, flag("--value")?)?,
                        "add-root" => state.settings.add_root(flag("--path")?)?,
                        "remove-root" => state.settings.remove_root(flag("--path")?)?,
                        _ => unreachable!(),
                    };
                    Plan::prepare(state, updated)?
                }
            }
        };
        let review = plan.review();
        if apply {
            let approval = plan.approval();
            plan.apply(&approval)?;
        }
        json!({"review":review,"saved":apply})
    };
    serde_json::to_writer(&mut *out, &result).map_err(|_| "Cannot write dashboard result")?;
    writeln!(out).map_err(|_| "Cannot write dashboard result".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_default_example_and_partial_settings_cover_saved_values() {
        let settings = Settings::default();
        settings.validate().unwrap();
        let saved = serde_json::to_value(&settings).unwrap();
        let mut keys = FIELDS.iter().map(|f| f.key).collect::<BTreeSet<_>>();
        keys.insert("search_roots");
        assert_eq!(
            keys,
            saved
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect()
        );
        let example: Value =
            serde_json::from_str(include_str!("../../../../config/dashboard.example.json"))
                .unwrap();
        assert_eq!(example["discovery"], saved);
        assert_eq!(super::settings(&json!({})).unwrap(), settings);
        let partial = super::settings(&json!({"discovery":{"scan_processes":false}})).unwrap();
        assert!(!partial.scan_processes);
        assert_eq!(partial.instance_limit, 128);
        for field in FIELDS {
            match field.kind {
                Kind::Number { min, max } => {
                    assert!(settings.set(field.key, &(min - 1).to_string()).is_err());
                    assert!(settings.set(field.key, &(max + 1).to_string()).is_err());
                }
                Kind::Toggle => {
                    assert!(settings.set(field.key, "false").is_ok());
                }
                _ => unreachable!(),
            }
            assert!(
                !settings
                    .set(field.key, "private-sentinel")
                    .unwrap_err()
                    .contains("private-sentinel")
            );
        }
        assert!(super::settings(&json!({"discovery":{"unknown":true}})).is_err());
        assert!(settings.add_root("relative").is_err());
    }
}

#[cfg(all(test, unix))]
pub(crate) mod fixtures {
    use super::*;
    pub struct Fixture(pub PathBuf);
    impl Fixture {
        pub fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "daedalus-dashboard-{}-{}",
                std::process::id(),
                crate::test_support::unique_nanos()
            ));
            layouts::write_private(&root.join("marker"), b"fixture").unwrap();
            Self(root)
        }
        pub fn instance(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            let mut config = super::super::config::ControlConfig::fresh(&self.0).unwrap();
            config.agent_name = name.into();
            layouts::write_private(&path.join("control.json"), &config.encode().unwrap()).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
#[cfg(all(test, unix))]
mod private_tests {
    use super::*;
    use fixtures::Fixture;
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    };
    #[test]
    fn recovery_replaces_only_reviewed_bad_preferences_and_keeps_exact_private_backup() {
        let f = Fixture::new();
        let instance = f.instance("retained-instance");
        let control = fs::read(instance.join("control.json")).unwrap();
        let oversized =
            serde_json::to_vec(&json!({"unrelated":"x".repeat(LIMIT as usize)})).unwrap();
        for (index, original) in [
            b"{broken-DASHBOARD_PRIVATE_SENTINEL".to_vec(),
            b"[]".to_vec(),
            br#"{"discovery":{"unknown":true},"unrelated":"DASHBOARD_PRIVATE_SENTINEL"}"#.to_vec(),
            br#"{"instances":[42]}"#.to_vec(),
            oversized,
        ]
        .into_iter()
        .enumerate()
        {
            let directory = f.0.join(format!("recovery-{index}"));
            let path = directory.join("dashboard.json");
            layouts::write_private_limit(&path, &original, RECOVERY_LIMIT).unwrap();
            let inode = fs::metadata(&path).unwrap().ino();
            let plan = Plan::recover(&directory).unwrap();
            let review = plan.review();
            assert!(plan.is_recovery());
            assert!(!review.to_string().contains("DASHBOARD_PRIVATE_SENTINEL"));
            assert!(review["previous_configuration"].is_null());
            assert_eq!(review["configuration"]["scan_processes"], false);
            let backup = PathBuf::from(review["backup_file"].as_str().unwrap());
            assert!(!backup.exists());
            assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
            assert_eq!(fs::read(&path).unwrap(), original);
            let approval = plan.approval();
            plan.apply(&approval).unwrap();
            assert_eq!(fs::read(&backup).unwrap(), original);
            assert_eq!(fs::metadata(&backup).unwrap().mode() & 0o777, 0o600);
            let recovered = State::load(&directory).unwrap();
            assert!(!recovered.settings.scan_processes);
            assert!(!recovered.settings.scan_default_roots);
            assert!(recovered.saved_instances().is_empty());
            assert_eq!(fs::read(instance.join("control.json")).unwrap(), control);
            assert!(Plan::recover(&directory).is_err());
        }
    }
    #[test]
    fn recovery_refuses_bad_approval_changed_bytes_shared_lock_and_backup_conflicts() {
        let f = Fixture::new();
        let directory = f.0.join("preferences");
        let path = directory.join("dashboard.json");
        let original = b"{broken-private-fixture";
        layouts::write_private(&path, original).unwrap();
        assert!(
            Plan::recover(&directory)
                .unwrap()
                .apply("incorrect-approval")
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        let plan = Plan::recover(&directory).unwrap();
        let approval = plan.approval();
        layouts::write_private(&path, b"concurrent modification").unwrap();
        assert!(plan.apply(&approval).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"concurrent modification");
        layouts::write_private(&path, original).unwrap();
        let plan = Plan::recover(&directory).unwrap();
        let approval = plan.approval();
        let lock = super::super::telegram_config::configuration_lock(&directory).unwrap();
        assert!(plan.apply(&approval).is_err());
        drop(lock);
        assert_eq!(fs::read(&path).unwrap(), original);
        let plan = Plan::recover(&directory).unwrap();
        let approval = plan.approval();
        let backup = plan.backup_path().unwrap();
        layouts::write_private(&backup, b"conflicting retained backup").unwrap();
        assert!(plan.apply(&approval).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(&backup).unwrap(), b"conflicting retained backup");
    }
    #[test]
    fn recovery_refuses_missing_valid_oversized_and_unsafe_files() {
        let f = Fixture::new();
        let directory = f.0.join("preferences");
        let path = directory.join("dashboard.json");
        assert!(Plan::recover(&directory).is_err());
        assert!(!directory.exists());
        layouts::write_private(&path, b"{}").unwrap();
        assert!(Plan::recover(&directory).is_err());
        layouts::write_private_limit(&path, &vec![b'x'; RECOVERY_LIMIT as usize], RECOVERY_LIMIT)
            .unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        use std::io::Write;
        file.write_all(b"x").unwrap();
        drop(file);
        assert!(Plan::recover(&directory).is_err());
        assert_eq!(fs::metadata(&path).unwrap().len(), RECOVERY_LIMIT + 1);
        layouts::write_private(&path, b"bad preferences").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Plan::recover(&directory).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = f.0.join("hard-link");
        fs::hard_link(&path, &link).unwrap();
        assert!(Plan::recover(&directory).is_err());
        fs::remove_file(link).unwrap();
        let original = directory.join("original");
        fs::rename(&path, &original).unwrap();
        symlink(&original, &path).unwrap();
        assert!(Plan::recover(&directory).is_err());
        assert_eq!(fs::read(&original).unwrap(), b"bad preferences");
    }
    #[test]
    fn invalid_saved_reference_types_refuse_without_exposing_private_contents() {
        let f = Fixture::new();
        let directory = f.0.join("preferences");
        for value in [
            json!({"last_instance":{"secret":"private-sentinel"}}),
            json!({"instances":[17]}),
            json!({"instances":["private-sentinel"]}),
        ] {
            layouts::write_private(
                &directory.join("dashboard.json"),
                &serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
            let error = State::load(&directory)
                .err()
                .expect("Malformed saved reference was accepted");
            assert!(!error.contains("private-sentinel"));
        }
        use std::os::unix::ffi::OsStringExt;
        let invalid = f.0.join(std::ffi::OsString::from_vec(vec![b'p', 0xff]));
        assert!(State::load(&invalid).is_err());
        assert!(!invalid.exists());
    }
    #[test]
    fn frozen_review_retains_private_other_keys_backups_and_rejects_changed_bytes() {
        let f = Fixture::new();
        let directory = f.0.join("preferences");
        let path = directory.join("dashboard.json");
        let before = br#"{"unrelated":"private-sentinel"}"#;
        layouts::write_private(&path, before).unwrap();
        let state = State::load(&directory).unwrap();
        let updated = state.settings.set("scan_processes", "false").unwrap();
        let plan = Plan::prepare(state.clone(), updated.clone()).unwrap();
        assert!(!plan.review().to_string().contains("private-sentinel"));
        let approval = plan.approval();
        let changed = b"{ \"unrelated\":\"private-sentinel\" }";
        layouts::write_private(&path, changed).unwrap();
        assert!(plan.apply(&approval).unwrap_err().contains("changed"));
        assert_eq!(fs::read(&path).unwrap(), changed);
        assert!(Plan::prepare(state, updated.clone()).is_err());
        let state = State::load(&directory).unwrap();
        let plan = Plan::prepare(state.clone(), updated.clone()).unwrap();
        assert!(plan.apply("wrong").is_err());
        let plan = Plan::prepare(state.clone(), updated.clone()).unwrap();
        let approval = plan.approval();
        let lock = super::super::telegram_config::configuration_lock(&directory).unwrap();
        assert!(plan.apply(&approval).is_err());
        drop(lock);
        let plan = Plan::prepare(state, updated).unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        let backup = directory.join(format!(
            "dashboard.backup-{:x}.json",
            Sha256::digest(changed)
        ));
        assert_eq!(fs::read(&backup).unwrap(), changed);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let saved = State::load(&directory).unwrap();
        assert!(!saved.settings.scan_processes);
        assert_eq!(saved.value["unrelated"], "private-sentinel");
        let inode = fs::metadata(&path).unwrap().ino();
        let count = fs::read_dir(&directory).unwrap().count();
        let plan = Plan::prepare(saved.clone(), saved.settings.clone()).unwrap();
        assert_eq!(plan.review()["changes"], false);
        assert_eq!(plan.review()["backup_required"], false);
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), count);
    }
    #[test]
    fn explicit_root_and_instance_operations_keep_selection_and_enforce_unique_capacity() {
        let f = Fixture::new();
        let directory = f.0.join("preferences");
        let instance = f.instance("instance");
        let state = State::load(&directory).unwrap();
        let mut settings = state.settings.set("instance_limit", "2").unwrap();
        settings = settings.add_root(f.0.to_str().unwrap()).unwrap();
        assert_eq!(settings.add_root(f.0.to_str().unwrap()).unwrap(), settings);
        let plan = Plan::prepare(state, settings).unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        let state = State::load(&directory).unwrap();
        let plan = Plan::add_instance(state, instance.to_str().unwrap()).unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        let state = State::load(&directory).unwrap();
        assert!(state.value["last_instance"].is_null());
        let lowered = state.settings.set("instance_limit", "1").unwrap();
        assert!(Plan::prepare(state, lowered).is_err());
        let other = f.instance("other");
        assert!(
            Plan::add_instance(State::load(&directory).unwrap(), other.to_str().unwrap()).is_err()
        );
        super::super::instances::remember(&directory, &instance).unwrap();
        let state = State::load(&directory).unwrap();
        let plan = Plan::remove_instance(state, instance.to_str().unwrap()).unwrap();
        assert_eq!(plan.review()["last_instance"], Value::Null);
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        assert!(
            super::super::instances::remembered(&directory)
                .unwrap()
                .is_none()
        );
        assert!(instance.join("control.json").is_file());
        let state = State::load(&directory).unwrap();
        let updated = state.settings.remove_root(f.0.to_str().unwrap()).unwrap();
        let plan = Plan::prepare(state, updated).unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        assert!(
            State::load(&directory)
                .unwrap()
                .settings
                .search_roots
                .is_empty()
        );
        let link = f.0.join("link");
        symlink(&instance, &link).unwrap();
        assert!(
            Settings::default()
                .add_root(link.to_str().unwrap())
                .is_err()
        );
    }
}
