//! Receipt history policy and explicit archive plans. These operations never
//! replay an effect or change an unresolved outcome into a completed one.
use super::{
    control_audit,
    interface_settings::{Kind, Setting},
    telegram_config,
};
use crate::core::layouts;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
const POLICY_LIMIT: u64 = 8192;
const RECOVERY_LIMIT: u64 = super::config::MAX_CONFIG_BYTES as u64;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub version: u8,
    pub entry_limit: u64,
    pub byte_limit: u64,
    pub display_limit: u64,
    pub archive_scan_limit: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            entry_limit: 1000,
            byte_limit: 1048576,
            display_limit: 50,
            archive_scan_limit: 1024,
        }
    }
}
pub const FIELDS: &[Setting] = &[
    Setting {
        key: "entry_limit",
        label: "Retained receipt entries",
        group: "Control history",
        description: "Total active entries plus reserved outcomes. Only completed or rejected operations rotate automatically; uncertain receipts stay protected.",
        kind: Kind::Number { min: 2, max: 8192 },
    },
    Setting {
        key: "byte_limit",
        label: "Active history size (bytes)",
        group: "Control history",
        description: "Serialized active history plus reserved outcome bytes. Archive protected history explicitly when full.",
        kind: Kind::Number {
            min: 4096,
            max: 8388608,
        },
    },
    Setting {
        key: "display_limit",
        label: "Receipts per page",
        group: "Control history",
        description: "Maximum receipts shown per recent-history or unresolved-history page. Reading never prunes history.",
        kind: Kind::Number { min: 1, max: 1024 },
    },
    Setting {
        key: "archive_scan_limit",
        label: "Archive directory scan entries",
        group: "Control history",
        description: "Maximum directory entries examined when listing saved archives. A partial listing is reported; no archive is deleted.",
        kind: Kind::Number { min: 1, max: 65536 },
    },
];
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err("Unsupported history policy version".into());
        }
        let value = serde_json::to_value(self).map_err(|_| "Cannot encode history policy")?;
        for field in FIELDS {
            let Kind::Number { min, max } = field.kind else {
                unreachable!()
            };
            if !value[field.key]
                .as_u64()
                .is_some_and(|n| (min..=max).contains(&n))
            {
                return Err(format!("{} must be between {min} and {max}", field.label));
            }
        }
        Ok(())
    }
    pub(crate) fn decode(bytes: Option<&[u8]>) -> Result<Self> {
        if bytes.is_some_and(|b| b.len() as u64 > POLICY_LIMIT) {
            return Err("History policy exceeds 8 KiB".into());
        }
        let settings = match bytes {
            Some(bytes) => serde_json::from_slice(bytes).map_err(|_| "Invalid history policy")?,
            None => Self::default(),
        };
        settings.validate()?;
        Ok(settings)
    }
    pub(crate) fn load(directory: &Path) -> Result<Self> {
        Self::decode(
            telegram_config::read_private(&directory.join("control-history.json"), POLICY_LIMIT)?
                .as_deref(),
        )
    }
    pub fn display(&self, key: &str) -> Result<String> {
        if !FIELDS.iter().any(|f| f.key == key) {
            return Err("Unknown history setting".into());
        }
        Ok(
            serde_json::to_value(self).map_err(|_| "Cannot encode history policy")?[key]
                .to_string(),
        )
    }
    pub fn set(&self, key: &str, text: &str) -> Result<Self> {
        self.display(key)?;
        let n = text
            .trim()
            .parse::<u64>()
            .map_err(|_| "Enter a whole number")?;
        let mut value = serde_json::to_value(self).map_err(|_| "Cannot encode history policy")?;
        value[key] = Value::from(n);
        let updated: Self = serde_json::from_value(value).map_err(|_| "Invalid history setting")?;
        updated.validate()?;
        Ok(updated)
    }
}
#[derive(Clone)]
pub struct State {
    pub settings: Settings,
    directory: PathBuf,
    before: Option<Vec<u8>>,
    history: Option<Vec<u8>>,
    entries: Vec<control_audit::Entry>,
    pub policy_valid: bool,
    pub history_valid: bool,
    pub recovery_required: bool,
}
fn validate_directory(directory: &Path) -> Result<()> {
    layouts::check_path(directory)?;
    if directory
        .to_str()
        .is_none_or(|text| text.chars().any(char::is_control))
    {
        return Err("History directory must use printable UTF-8".into());
    }
    if directory.exists() {
        control_audit::directory(directory)?;
    }
    Ok(())
}
/// Browsing can use safe defaults; effect writers always load policy strictly.
pub(crate) fn view_settings(directory: &Path) -> (Settings, bool) {
    Settings::load(directory).map_or_else(
        |_| (Settings::default(), false),
        |settings| (settings, true),
    )
}
fn archive_hash(name: &str) -> Result<&str> {
    let hash = name
        .strip_prefix("control-audit.archive-")
        .and_then(|s| s.strip_suffix(".json"))
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("Select a saved archive filename from this history directory")?;
    Ok(hash)
}
pub fn archives(directory: &Path, offset: usize) -> Result<Value> {
    validate_directory(directory)?;
    if offset > 65536 {
        return Err("Invalid archive page offset".into());
    }
    let (settings, policy_valid) = view_settings(directory);
    let mut names = Vec::new();
    let mut scanned = 0;
    let mut partial = false;
    if directory.exists() {
        for (index, entry) in std::fs::read_dir(directory)
            .map_err(|_| "Cannot list history archives")?
            .enumerate()
        {
            if index >= settings.archive_scan_limit as usize {
                partial = true;
                break;
            }
            scanned += 1;
            let entry = entry.map_err(|_| "Cannot inspect archive directory entry")?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if archive_hash(&name).is_ok() && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                names.push(name);
            }
        }
    }
    names.sort();
    let next = offset
        .saturating_add(settings.display_limit as usize)
        .min(names.len());
    Ok(
        json!({"directory":directory,"archives":names.iter().skip(offset).take(settings.display_limit as usize).collect::<Vec<_>>(),"next_offset":if next<names.len(){Some(next)}else{None},"offset":offset,"partial":partial,"scanned_entries":scanned,"scan_limit":settings.archive_scan_limit,"policy_valid":policy_valid,"notice":"A partial listing can omit archives. Raise archive_scan_limit after policy recovery, or open an exact archive filename. Selecting an archive verifies its private bytes against its name."}),
    )
}
pub fn archive_page(
    directory: &Path,
    name: &str,
    offset: usize,
    unresolved: bool,
) -> Result<Value> {
    validate_directory(directory)?;
    let hash = archive_hash(name)?;
    let bytes = control_audit::read_bytes(&directory.join(name))?
        .ok_or("Saved history archive is missing")?;
    if !format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(hash) {
        return Err("Saved history archive changed or has the wrong digest".into());
    }
    let entries = control_audit::decode(&bytes)?;
    let (settings, policy_valid) = view_settings(directory);
    let mut page = control_audit::page(&entries, &settings, offset, unresolved)?;
    page["source"] = json!("archive");
    page["archive"] = json!(name);
    page["policy_valid"] = json!(policy_valid);
    page["status"] = json!("ok");
    Ok(page)
}
impl State {
    pub fn load(directory: &Path) -> Result<Self> {
        Self::capture(directory, false)
    }
    pub fn inspect(directory: &Path) -> Result<Self> {
        Self::capture(directory, true)
    }
    fn capture(directory: &Path, inspection: bool) -> Result<Self> {
        validate_directory(directory)?;
        let before = telegram_config::read_private(
            &directory.join("control-history.json"),
            if inspection {
                RECOVERY_LIMIT
            } else {
                POLICY_LIMIT
            },
        )?;
        let decoded = Settings::decode(before.as_deref());
        let (settings, policy_valid) = match decoded {
            Ok(settings) => (settings, true),
            Err(_) if inspection => (Settings::default(), false),
            Err(error) => return Err(error),
        };
        let history = control_audit::read_bytes(&directory.join("control-audit.json"))?;
        let decoded = history
            .as_ref()
            .map_or_else(|| Ok(vec![]), |bytes| control_audit::decode(bytes));
        let (entries, history_valid) = match decoded {
            Ok(entries) => (entries, true),
            Err(_) if inspection => (vec![], false),
            Err(error) => return Err(error),
        };
        let recovery_required = !policy_valid
            || (history_valid && control_audit::fit(&entries, &settings, None).is_err());
        Ok(Self {
            settings,
            directory: directory.into(),
            before,
            history,
            entries,
            policy_valid,
            history_valid,
            recovery_required,
        })
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub(crate) fn same_policy(&self, other: &Self) -> bool {
        self.directory == other.directory && self.before == other.before
    }
    pub fn show(&self) -> Value {
        let (protected, pending) =
            control_audit::protected_counts(&self.entries).expect("validated history groups");
        json!({"directory":self.directory,"policy_valid":self.policy_valid,"history_valid":self.history_valid,"recovery_required":self.recovery_required,"configured":self.before.is_some(),"configuration":self.settings,"configuration_source":if self.recovery_required{"unsaved_recovery_draft"}else{"saved_or_default"},"editable_settings":FIELDS,"entries":if self.history_valid{Some(self.entries.len())}else{None},"protected_operations":if self.history_valid{Some(protected)}else{None},"pending_operations":if self.history_valid{Some(pending)}else{None},"protected_receipts_fit_policy":self.history_valid&&control_audit::fit(&self.entries,&self.settings,None).is_ok(),"notice":"Uncertain outcomes stay protected. An explicit archive preserves original receipts without retrying any action."})
    }
}
pub struct Plan {
    state: State,
    updated: Option<Settings>,
    after: Vec<u8>,
    recovery: bool,
}
impl Plan {
    pub fn edit(state: State, updated: Settings) -> Result<Self> {
        if !state.policy_valid || !state.history_valid {
            return Err(
                "Recover the policy or archive damaged receipts before saving normal settings"
                    .into(),
            );
        }
        Self::policy(state, updated, false)
    }
    pub fn recover_policy(state: State, updated: Settings) -> Result<Self> {
        if !state.recovery_required {
            return Err("History policy is valid or missing; use normal settings".into());
        }
        if !state.history_valid {
            return Err("Archive damaged receipts before recovering policy".into());
        }
        Self::policy(state, updated, true)
    }
    fn policy(state: State, updated: Settings, recovery: bool) -> Result<Self> {
        updated.validate()?;
        control_audit::fit(&state.entries, &updated, None)?;
        let after =
            serde_json::to_vec_pretty(&updated).map_err(|_| "Cannot encode history policy")?;
        Ok(Self {
            state,
            updated: Some(updated),
            after,
            recovery,
        })
    }
    pub fn archive(state: State) -> Result<Self> {
        if state.history.is_none() || (state.history_valid && state.entries.is_empty()) {
            return Err("History is empty; nothing to archive".into());
        }
        Ok(Self {
            state,
            updated: None,
            after: b"[]".to_vec(),
            recovery: false,
        })
    }
    pub fn directory(&self) -> &Path {
        self.state.directory()
    }
    pub fn is_recovery(&self) -> bool {
        self.recovery
    }
    pub fn is_archive(&self) -> bool {
        self.updated.is_none()
    }
    pub fn approval(&self) -> String {
        let archive = if self.is_archive() {
            self.state.history.as_deref()
        } else {
            None
        };
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    &self.state.directory,
                    &self.state.before,
                    &self.after,
                    archive,
                    self.recovery
                ))
                .expect("serializable history plan")
            )
        )
    }
    fn retained_file(&self) -> Option<PathBuf> {
        let (bytes, prefix) = if self.is_archive() {
            (self.state.history.as_ref(), "control-audit.archive")
        } else {
            (self.state.before.as_ref(), "control-history.backup")
        };
        bytes.map(|b| {
            self.state
                .directory
                .join(format!("{prefix}-{:x}.json", Sha256::digest(b)))
        })
    }
    pub fn review(&self) -> Value {
        if let Some(updated) = &self.updated {
            json!({"directory":self.state.directory,"recovery":self.recovery,"previous_configuration":Settings::decode(self.state.before.as_deref()).ok(),"configuration":updated,"backup_file":self.retained_file(),"changes":self.state.before.as_deref()!=Some(self.after.as_slice()),"approval":self.approval(),"notice":"Review all proposed limits before recovery. An existing original policy stays in a private backup; receipt data is retained. History is read using fixed format bounds. New active limits apply to future writes; reading does not prune. Protected receipts and reserved outcomes must still fit."})
        } else {
            json!({"directory":self.state.directory,"archive_file":self.retained_file(),"history_valid":self.state.history_valid,"original_bytes":self.state.history.as_ref().map(Vec::len),"history":self.state.show(),"approval":self.approval(),"notice":"Keep an exact private archive and start a new empty active history. Uncertain outcomes remain uncertain in the archive. Actions are never retried. In-flight actions may still finish; their late receipt can be unconfirmed if the new active history fills."})
        }
    }
    pub fn apply(self, approval: &str) -> Result<()> {
        if approval != self.approval() {
            return Err("History approval does not match review".into());
        }
        let directory = &self.state.directory;
        let _configuration = telegram_config::configuration_lock(directory)?;
        let _history = control_audit::lock(directory)?;
        if telegram_config::read_private(&directory.join("control-history.json"), RECOVERY_LIMIT)?
            != self.state.before
        {
            return Err("History policy changed; reopen the review".into());
        }
        if let Some(updated) = &self.updated {
            control_audit::fit(
                &control_audit::read(&directory.join("control-audit.json"))?,
                updated,
                None,
            )?;
            if self.state.before.as_deref() == Some(self.after.as_slice()) {
                return Ok(());
            }
        } else if control_audit::read_bytes(&directory.join("control-audit.json"))?
            != self.state.history
        {
            return Err("History changed; prepare the archive again".into());
        }
        if let Some(retained) = self.retained_file() {
            let original = if self.is_archive() {
                self.state.history.as_ref()
            } else {
                self.state.before.as_ref()
            }
            .expect("retained original exists");
            match telegram_config::read_private(&retained, control_audit::MAX)? {
                Some(bytes) if bytes != *original => {
                    return Err("History backup or archive conflicts".into());
                }
                Some(_) => (),
                None => layouts::write_private_limit(&retained, original, control_audit::MAX)?,
            }
        }
        let filename = if self.is_archive() {
            "control-audit.json"
        } else {
            "control-history.json"
        };
        layouts::write_private_limit(&directory.join(filename), &self.after, control_audit::MAX)
    }
}
pub const HELP: &str = "daedalus history config show --config-dir ABS\ndaedalus history config set --config-dir ABS --key SETTING --value NUMBER [--apply]\ndaedalus history config recover --config-dir ABS [--entry-limit N] [--byte-limit N] [--display-limit N] [--archive-scan-limit N] [--apply --expect-sha SHA]\ndaedalus history list --config-dir ABS [--archive FILENAME] [--unresolved] [--offset N]\ndaedalus history archives --config-dir ABS [--offset N]\ndaedalus history archive --config-dir ABS [--apply --expect-sha SHA]\nRecovery and archive previews require explicit matching approval to apply. Originals stay private.\nBrowsing never changes receipts or treats an uncertain outcome as complete.\n";
pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    if args.is_empty() || args == ["--help"] || args == ["config"] || args == ["config", "--help"] {
        return write!(out, "{HELP}").map_err(|_| "Cannot write history help".into());
    }
    let (verb, start) = if args[0] == "config" {
        let verb = args.get(1).ok_or("Use history --help")?;
        if !["show", "set", "recover"].contains(&verb.as_str()) {
            return Err("Unknown history configuration command".into());
        }
        (verb.as_str(), 2)
    } else if ["list", "archive", "archives"].contains(&args[0].as_str()) {
        (args[0].as_str(), 1)
    } else {
        return Err("Use history config, list, archives or archive".into());
    };
    let recovery_fields = [
        ("--entry-limit", "entry_limit"),
        ("--byte-limit", "byte_limit"),
        ("--display-limit", "display_limit"),
        ("--archive-scan-limit", "archive_scan_limit"),
    ];
    let mut flags = std::collections::BTreeMap::new();
    let mut apply = false;
    let mut unresolved = false;
    let mut rest = args[start..].iter();
    while let Some(key) = rest.next() {
        if key == "--apply" && ["set", "archive", "recover"].contains(&verb) {
            if apply {
                return Err("Repeated history apply option".into());
            }
            apply = true;
            continue;
        }
        if key == "--unresolved" && verb == "list" {
            if unresolved {
                return Err("Repeated unresolved option".into());
            }
            unresolved = true;
            continue;
        }
        let allowed = key == "--config-dir"
            || (verb == "set" && ["--key", "--value"].contains(&key.as_str()))
            || (["archive", "recover"].contains(&verb) && key == "--expect-sha")
            || (["list", "archives"].contains(&verb) && key == "--offset")
            || (verb == "list" && key == "--archive")
            || (verb == "recover" && recovery_fields.iter().any(|(flag, _)| key == flag));
        if !allowed {
            return Err("Unknown history option".into());
        }
        let value = rest.next().ok_or("Missing history option value")?;
        if flags.insert(key.as_str(), value.as_str()).is_some() {
            return Err("Repeated history option".into());
        }
    }
    let flag = |key: &str| -> Result<&str> {
        flags
            .get(key)
            .copied()
            .ok_or_else(|| format!("Missing {key}"))
    };
    let directory = Path::new(flag("--config-dir")?);
    let offset = flags
        .get("--offset")
        .copied()
        .unwrap_or("0")
        .parse()
        .map_err(|_| "Invalid history offset")?;
    let result = match verb {
        "archives" => archives(directory, offset)?,
        "list" => match flags.get("--archive") {
            Some(name) => archive_page(directory, name, offset, unresolved)?,
            None => control_audit::list(directory, offset, unresolved)?,
        },
        "show" => State::inspect(directory)?.show(),
        _ => {
            let plan = if verb == "set" {
                let state = State::load(directory)?;
                let updated = state.settings.set(flag("--key")?, flag("--value")?)?;
                Plan::edit(state, updated)?
            } else if verb == "recover" {
                let state = State::inspect(directory)?;
                let mut updated = state.settings.clone();
                for (flag, key) in recovery_fields {
                    if let Some(value) = flags.get(flag) {
                        updated = updated.set(key, value)?;
                    }
                }
                Plan::recover_policy(state, updated)?
            } else {
                Plan::archive(State::inspect(directory)?)?
            };
            let review = plan.review();
            if apply {
                let approval = if ["archive", "recover"].contains(&verb) {
                    flag("--expect-sha")?.to_string()
                } else {
                    plan.approval()
                };
                plan.apply(&approval)?;
            } else if flags.contains_key("--expect-sha") {
                return Err("Use --expect-sha only with --apply".into());
            }
            json!({"review":review,"saved":apply})
        }
    };
    serde_json::to_writer(&mut *out, &result).map_err(|_| "Cannot write history result")?;
    writeln!(out).map_err(|_| "Cannot write history result".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::dashboard::fixtures::Fixture;
    use super::*;
    use std::{fs, os::unix::fs::MetadataExt};
    fn protected_history(directory: &Path, count: usize) -> Vec<u8> {
        let entries=(0..count).map(|i|json!({"version":1,"operation":format!("{i:x}"),"action":"account","state":"unconfirmed","time":1.0})).collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&entries).unwrap();
        layouts::write_private(&directory.join("control-audit.json"), &bytes).unwrap();
        bytes
    }
    #[test]
    fn capacity_recovery_can_raise_multiple_limits_without_archiving_uncertain_receipts() {
        let f = Fixture::new();
        let entries=(0..24).map(|i|json!({"version":1,"operation":format!("{i:096x}"),"action":"account","state":"unconfirmed","time":1.0})).collect::<Vec<_>>();
        let original = serde_json::to_vec(&entries).unwrap();
        layouts::write_private(&f.0.join("control-audit.json"), &original).unwrap();
        let settings = Settings {
            entry_limit: 2,
            byte_limit: 4096,
            ..Default::default()
        };
        layouts::write_private(
            &f.0.join("control-history.json"),
            &serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        let state = State::inspect(&f.0).unwrap();
        assert!(state.policy_valid);
        assert!(state.recovery_required);
        let updated = state
            .settings
            .set("entry_limit", "64")
            .unwrap()
            .set("byte_limit", "8192")
            .unwrap();
        let plan = Plan::recover_policy(state, updated).unwrap();
        assert_eq!(plan.review()["previous_configuration"]["entry_limit"], 2);
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        assert_eq!(fs::read(f.0.join("control-audit.json")).unwrap(), original);
        assert!(!State::load(&f.0).unwrap().recovery_required);
    }
    #[test]
    fn recovery_capture_bound_and_unsafe_policy_files_refuse_without_writes() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let path = f.0.join("control-history.json");
        let receipt = protected_history(&f.0, 1);
        let oversized = vec![b'x'; POLICY_LIMIT as usize + 1];
        layouts::write_private(&path, &oversized).unwrap();
        assert!(State::load(&f.0).is_err());
        let state = State::inspect(&f.0).unwrap();
        let plan = Plan::recover_policy(state.clone(), state.settings).unwrap();
        let backup = plan.retained_file().unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        assert_eq!(fs::read(backup).unwrap(), oversized);
        layouts::write_private_limit(
            &path,
            &vec![b'x'; RECOVERY_LIMIT as usize + 1],
            control_audit::MAX,
        )
        .unwrap();
        assert!(State::inspect(&f.0).is_err());
        assert_eq!(fs::metadata(&path).unwrap().len(), RECOVERY_LIMIT + 1);
        layouts::write_private(&path, b"bad policy").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(State::inspect(&f.0).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"bad policy");
        assert_eq!(fs::read(f.0.join("control-audit.json")).unwrap(), receipt);
    }
    #[test]
    fn policy_recovery_is_explicit_private_and_cannot_hide_protected_capacity() {
        use std::cell::Cell;
        let f = Fixture::new();
        let before = protected_history(&f.0, 1002);
        let path = f.0.join("control-history.json");
        let missing = State::inspect(&f.0).unwrap();
        assert!(missing.recovery_required);
        let updated = missing.settings.set("entry_limit", "2048").unwrap();
        let draft = Plan::recover_policy(missing, updated).unwrap();
        assert!(draft.review()["backup_file"].is_null());
        assert!(!path.exists());
        let corrupt = b"{broken-HISTORY_PRIVATE_SENTINEL";
        layouts::write_private(&path, corrupt).unwrap();
        let called = Cell::new(false);
        assert!(
            control_audit::perform(
                &f.0,
                control_audit::Action::Account,
                || {
                    called.set(true);
                    Ok(())
                },
                |_| control_audit::State::Completed
            )
            .is_err()
        );
        assert!(!called.get());
        let state = State::inspect(&f.0).unwrap();
        assert!(!state.policy_valid);
        assert!(state.history_valid);
        assert!(
            !state
                .show()
                .to_string()
                .contains("HISTORY_PRIVATE_SENTINEL")
        );
        assert!(Plan::recover_policy(state.clone(), state.settings.clone()).is_err());
        let updated = state.settings.set("entry_limit", "2048").unwrap();
        let plan = Plan::recover_policy(state, updated).unwrap();
        let backup = plan.retained_file().unwrap();
        let approval = plan.approval();
        assert!(!backup.exists());
        assert_eq!(fs::read(&path).unwrap(), corrupt);
        plan.apply(&approval).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), corrupt);
        assert_eq!(fs::metadata(&backup).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::read(f.0.join("control-audit.json")).unwrap(), before);
        assert_eq!(State::load(&f.0).unwrap().settings.entry_limit, 2048);
    }
    #[test]
    fn recovery_refuses_stale_policy_and_unknown_receipts_then_archives_exact_raw_bytes() {
        let f = Fixture::new();
        let policy = f.0.join("control-history.json");
        layouts::write_private(&policy, b"bad policy").unwrap();
        protected_history(&f.0, 1);
        let state = State::inspect(&f.0).unwrap();
        let plan = Plan::recover_policy(state.clone(), state.settings).unwrap();
        let approval = plan.approval();
        layouts::write_private(&policy, b"changed policy").unwrap();
        assert!(plan.apply(&approval).is_err());
        assert_eq!(fs::read(&policy).unwrap(), b"changed policy");
        let raw = b"{broken-PRIVATE-RECEIPTS";
        layouts::write_private(&f.0.join("control-audit.json"), raw).unwrap();
        let state = State::inspect(&f.0).unwrap();
        assert!(!state.history_valid);
        assert!(state.show()["entries"].is_null());
        assert!(Plan::recover_policy(state.clone(), state.settings.clone()).is_err());
        let plan = Plan::archive(state).unwrap();
        assert!(!plan.review().to_string().contains("PRIVATE-RECEIPTS"));
        let archive = plan.retained_file().unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        assert_eq!(fs::read(archive).unwrap(), raw);
        assert_eq!(fs::read(f.0.join("control-audit.json")).unwrap(), b"[]");
        assert_eq!(fs::read(&policy).unwrap(), b"changed policy");
        let state = State::inspect(&f.0).unwrap();
        let plan = Plan::recover_policy(state.clone(), state.settings).unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        assert!(State::load(&f.0).is_ok());
    }
    #[test]
    fn bounded_archive_catalog_and_digest_verified_pages_work_with_bad_active_state() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        protected_history(&f.0, 3);
        let plan = Plan::archive(State::load(&f.0).unwrap()).unwrap();
        let archive = plan.retained_file().unwrap();
        let name = archive.file_name().unwrap().to_str().unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        layouts::write_private(&f.0.join("control-history.json"), b"bad policy").unwrap();
        layouts::write_private(&f.0.join("control-audit.json"), b"bad receipts").unwrap();
        let catalog = archives(&f.0, 0).unwrap();
        assert_eq!(catalog["policy_valid"], false);
        assert!(
            catalog["archives"]
                .as_array()
                .unwrap()
                .contains(&json!(name))
        );
        let page = archive_page(&f.0, name, 0, true).unwrap();
        assert_eq!(page["source"], "archive");
        assert_eq!(page["protected_operations"], 3);
        assert_eq!(page["policy_valid"], false);
        assert_eq!(page["events"].as_array().unwrap().len(), 3);
        assert!(archive_page(&f.0, "../control-audit.json", 0, false).is_err());
        assert!(archive_page(&f.0, "control-audit.json", 0, false).is_err());
        let original = fs::read(&archive).unwrap();
        layouts::write_private(&archive, b"[]").unwrap();
        assert!(archive_page(&f.0, name, 0, false).is_err());
        layouts::write_private(&archive, &original).unwrap();
        fs::remove_file(&archive).unwrap();
        let target = f.0.join("retained-target");
        layouts::write_private(&target, &original).unwrap();
        symlink(&target, &archive).unwrap();
        assert!(archive_page(&f.0, name, 0, false).is_err());
        assert_eq!(fs::read(target).unwrap(), original);
        let settings = Settings {
            archive_scan_limit: 1,
            ..Default::default()
        };
        layouts::write_private(
            &f.0.join("control-history.json"),
            &serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        let catalog = archives(&f.0, 0).unwrap();
        assert_eq!(catalog["partial"], true);
        assert_eq!(catalog["scanned_entries"], 1);
    }
    fn edit(directory: &Path, key: &str, value: &str) {
        let state = State::load(directory).unwrap();
        let updated = state.settings.set(key, value).unwrap();
        let plan = Plan::edit(state, updated).unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
    }
    #[test]
    fn metadata_examples_defaults_and_invalid_policy_are_consistent() {
        let defaults = Settings::default();
        defaults.validate().unwrap();
        let example: Settings = serde_json::from_str(include_str!(
            "../../../../config/control-history.example.json"
        ))
        .unwrap();
        assert_eq!(example, defaults);
        assert_eq!(Settings::decode(Some(b"{}")).unwrap(), defaults);
        let mut saved = serde_json::to_value(&defaults)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();
        saved.remove("version");
        assert_eq!(
            saved,
            FIELDS.iter().map(|field| field.key.to_string()).collect()
        );
        for field in FIELDS {
            let Kind::Number { min, max } = field.kind else {
                unreachable!()
            };
            assert!(defaults.set(field.key, &(min - 1).to_string()).is_err());
            assert!(defaults.set(field.key, &(max + 1).to_string()).is_err());
            assert!(
                !defaults
                    .set(field.key, "PRIVATE-FIXTURE")
                    .unwrap_err()
                    .contains("PRIVATE-FIXTURE")
            );
        }
        assert!(Settings::decode(Some(br#"{"version":2}"#)).is_err());
        assert!(Settings::decode(Some(br#"{"unrecognized":1}"#)).is_err());
        assert!(defaults.set("unrecognized", "4").is_err());
    }
    #[test]
    fn settings_snapshots_backup_stale_bytes_and_noop_inode_protection() {
        let f = Fixture::new();
        let directory = f.0.join("history");
        let state = State::load(&directory).unwrap();
        assert!(!directory.exists());
        let plan = Plan::edit(
            state.clone(),
            state.settings.set("entry_limit", "8").unwrap(),
        )
        .unwrap();
        assert!(!directory.exists());
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        let before = fs::read(directory.join("control-history.json")).unwrap();
        let state = State::load(&directory).unwrap();
        let plan = Plan::edit(
            state.clone(),
            state.settings.set("display_limit", "2").unwrap(),
        )
        .unwrap();
        let backup = PathBuf::from(plan.review()["backup_file"].as_str().unwrap());
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), before);
        assert_eq!(fs::metadata(backup).unwrap().mode() & 0o777, 0o600);
        let state = State::load(&directory).unwrap();
        let stale = Plan::edit(
            state.clone(),
            state.settings.set("entry_limit", "10").unwrap(),
        )
        .unwrap();
        let approval = stale.approval();
        edit(&directory, "display_limit", "3");
        let current = fs::read(directory.join("control-history.json")).unwrap();
        assert!(stale.apply(&approval).is_err());
        assert_eq!(
            fs::read(directory.join("control-history.json")).unwrap(),
            current
        );
        let inode = fs::metadata(directory.join("control-history.json"))
            .unwrap()
            .ino();
        let state = State::load(&directory).unwrap();
        let plan = Plan::edit(state.clone(), state.settings).unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        assert_eq!(
            fs::metadata(directory.join("control-history.json"))
                .unwrap()
                .ino(),
            inode
        );
    }
    #[test]
    fn archive_is_exact_private_and_stale_approval_or_conflicting_backup_refuses() {
        let f = Fixture::new();
        super::super::control_audit::perform(
            &f.0,
            control_audit::Action::Account,
            || Ok(()),
            |_| control_audit::State::Unconfirmed,
        )
        .unwrap();
        let before = fs::read(f.0.join("control-audit.json")).unwrap();
        let plan = Plan::archive(State::load(&f.0).unwrap()).unwrap();
        let approval = plan.approval();
        let archive = plan.retained_file().unwrap();
        assert!(!archive.exists());
        layouts::write_private(&archive, b"conflicting private archive").unwrap();
        assert!(plan.apply(&approval).is_err());
        assert_eq!(fs::read(f.0.join("control-audit.json")).unwrap(), before);
        fs::remove_file(&archive).unwrap();
        let plan = Plan::archive(State::load(&f.0).unwrap()).unwrap();
        let approval = plan.approval();
        control_audit::perform(
            &f.0,
            control_audit::Action::Configure,
            || Ok(()),
            |_| control_audit::State::Completed,
        )
        .unwrap();
        assert!(plan.apply(&approval).is_err());
        let before = fs::read(f.0.join("control-audit.json")).unwrap();
        let plan = Plan::archive(State::load(&f.0).unwrap()).unwrap();
        let archive = plan.retained_file().unwrap();
        let approval = plan.approval();
        plan.apply(&approval).unwrap();
        assert_eq!(fs::read(&archive).unwrap(), before);
        assert_eq!(fs::metadata(&archive).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::read(f.0.join("control-audit.json")).unwrap(), b"[]");
        assert!(String::from_utf8(before).unwrap().contains("unconfirmed"));
        assert!(Plan::archive(State::load(&f.0).unwrap()).is_err());
    }
}
