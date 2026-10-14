//! Reviewed removal of unchanged managed installations. A manual profile edit
//! is a conflict, never permission to overwrite unrelated work.
use super::{
    hook_install::{self, Receipt},
    hook_runtime, telegram_config,
};
use crate::core::layouts;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    telegram_config::read_private(path, 1024 * 1024)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Removal {
    version: u32,
    directory: PathBuf,
    profile: PathBuf,
    registration: PathBuf,
    original_profile: Vec<u8>,
    removed_profile: Option<Vec<u8>>,
    original_registration: Vec<u8>,
    disabled_registration: Vec<u8>,
}
pub struct Plan {
    receipt_path: PathBuf,
    receipt_bytes: Vec<u8>,
    backup_path: PathBuf,
    backup_bytes: Vec<u8>,
    removal: Removal,
}
#[derive(Clone, Serialize)]
pub struct Entry {
    pub receipt: PathBuf,
    pub profile: PathBuf,
    pub registration: String,
}
pub fn list(directory: &Path, harness: &str) -> Result<Vec<Entry>> {
    let root = directory.join("hook-backups");
    layouts::check_path(&root)?;
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(_) => return Err("Cannot list managed installations".into()),
    };
    let mut found = Vec::new();
    for (count, entry) in entries.enumerate() {
        if count >= 1000 {
            return Err("Managed installation limit reached".into());
        }
        let path = entry
            .map_err(|_| "Cannot inspect installation receipt")?
            .path();
        if !path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.ends_with(".receipt.json"))
        {
            continue;
        }
        let Ok(plan) = Plan::prepare(directory, &path) else {
            continue;
        };
        let registration =
            hook_runtime::inspect(&plan.removal.registration).map_err(|e| e.to_string())?;
        if serde_json::to_value(registration.scope.harness).ok() == Some(json!(harness)) {
            found.push(Entry {
                receipt: path,
                profile: plan.removal.profile,
                registration: registration.id,
            });
        }
    }
    found.sort_by(|a, b| a.receipt.cmp(&b.receipt));
    Ok(found)
}
impl Plan {
    pub fn prepare(directory: &Path, receipt_path: &Path) -> Result<Self> {
        let receipt_bytes = read(receipt_path)?.ok_or("Managed installation receipt is missing")?;
        let receipt: Receipt =
            serde_json::from_slice(&receipt_bytes).map_err(|_| "Invalid installation receipt")?;
        let original_registration =
            read(&receipt.registration)?.ok_or("Hook registration is missing")?;
        let registration: hook_runtime::Registration =
            serde_json::from_slice(&original_registration)
                .map_err(|_| "Invalid hook registration")?;
        registration.validate().map_err(|e| e.to_string())?;
        if registration.directory != directory
            || registration.path() != receipt.registration
            || receipt.after_sha.len() != 64
            || !receipt.after_sha.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Installation belongs to another registration or instance".into());
        }
        let expected = directory
            .join("hook-backups")
            .join(format!("{}-{}.json", registration.id, receipt.after_sha));
        if receipt.backup != expected || receipt_path != expected.with_extension("receipt.json") {
            return Err("Installation receipt path does not match its registration".into());
        }
        let backup_bytes = read(&receipt.backup)?.ok_or("Installation backup is missing")?;
        let backup: Value =
            serde_json::from_slice(&backup_bytes).map_err(|_| "Invalid installation backup")?;
        if backup["profile"] != json!(receipt.profile) || backup["after_sha"] != receipt.after_sha {
            return Err("Installation backup does not match receipt".into());
        }
        let original_profile = read(&receipt.profile)?.ok_or("Installed profile is missing")?;
        if hash(&original_profile) != receipt.after_sha {
            return Err("Profile changed after installation; removal preserves manual edits and requires review outside this operation".into());
        }
        let removed_profile: Option<Vec<u8>> = serde_json::from_value(backup["before"].clone())
            .map_err(|_| "Invalid original profile backup")?;
        let mut disabled = registration;
        disabled.options = Default::default();
        disabled.goal_id = None;
        let disabled_registration = serde_json::to_vec_pretty(&disabled)
            .map_err(|_| "Cannot encode disabled registration")?;
        Ok(Self {
            receipt_path: receipt_path.into(),
            receipt_bytes,
            backup_path: receipt.backup,
            backup_bytes,
            removal: Removal {
                version: 1,
                directory: directory.into(),
                profile: receipt.profile,
                registration: receipt.registration,
                original_profile,
                removed_profile,
                original_registration,
                disabled_registration,
            },
        })
    }
    pub fn approval(&self) -> String {
        hash(
            &serde_json::to_vec(&(
                &self.receipt_path,
                &self.receipt_bytes,
                &self.backup_path,
                &self.backup_bytes,
                &self.removal,
            ))
            .expect("removal plan"),
        )
    }
    pub fn review(&self) -> Value {
        json!({
            "operation": "remove_managed_hooks",
            "profile": self.removal.profile,
            "registration": self.removal.registration,
            "restore_previous_profile": self.removal.removed_profile.is_some(),
            "approval": self.approval(),
            "effect": concat!(
                "Restore the pre-install profile and disable this registration's delivery, memory and goal features. ",
                "Restart or reload the harness to discard cached handlers. Queued deliveries are retained."
            )
        })
    }
    pub fn apply(self, approval: &str) -> Result<PathBuf> {
        if self.approval() != approval {
            return Err("Removal approval does not match review".into());
        }
        let directory = &self.removal.directory;
        let _active = hook_runtime::active_guard(directory).map_err(|e| e.to_string())?;
        let _lock = hook_install::lock(directory)?;
        let parent = self
            .removal
            .profile
            .parent()
            .ok_or("Profile has no parent")?;
        let _profile_lock = if parent != directory {
            Some(hook_install::lock(parent)?)
        } else {
            None
        };
        if read(&self.receipt_path)?.as_ref() != Some(&self.receipt_bytes)
            || read(&self.backup_path)?.as_ref() != Some(&self.backup_bytes)
            || read(&self.removal.profile)?.as_ref() != Some(&self.removal.original_profile)
            || read(&self.removal.registration)?.as_ref()
                != Some(&self.removal.original_registration)
        {
            return Err("Profile, registration or receipt changed; review removal again".into());
        }
        let path = directory
            .join("hook-removals")
            .join(format!("{approval}.json"));
        let bytes =
            serde_json::to_vec(&self.removal).map_err(|_| "Cannot encode removal receipt")?;
        match read(&path)? {
            Some(old) if old != bytes => return Err("Removal receipt conflicts".into()),
            Some(_) => (),
            None => layouts::write_private(&path, &bytes)?,
        }
        layouts::write_private(
            &self.removal.registration,
            &self.removal.disabled_registration,
        )?;
        restore_profile(&self.removal.profile, self.removal.removed_profile.as_deref())
            .map_err(|_| {
                "Registration features are disabled but profile removal is unconfirmed; inspect the saved removal receipt before retrying"
            })?;
        Ok(path)
    }
}
fn restore_profile(path: &Path, bytes: Option<&[u8]>) -> Result<()> {
    match bytes {
        Some(bytes) => layouts::write_private(path, bytes),
        None => fs::remove_file(path).map_err(|_| "Cannot remove managed profile".into()),
    }
}
pub fn rollback(path: &Path) -> Result<()> {
    let bytes = read(path)?.ok_or("Removal receipt is missing")?;
    let removal: Removal = serde_json::from_slice(&bytes).map_err(|_| "Invalid removal receipt")?;
    let registration: hook_runtime::Registration =
        serde_json::from_slice(&removal.original_registration)
            .map_err(|_| "Invalid original registration")?;
    registration.validate().map_err(|e| e.to_string())?;
    if removal.version != 1
        || removal.directory != registration.directory
        || removal.registration != registration.path()
        || path.parent() != Some(removal.directory.join("hook-removals").as_path())
    {
        return Err("Removal receipt does not match its instance".into());
    }
    let _active = hook_runtime::active_guard(&removal.directory).map_err(|e| e.to_string())?;
    let _lock = hook_install::lock(&removal.directory)?;
    let parent = removal.profile.parent().ok_or("Profile has no parent")?;
    let _profile_lock = if parent != removal.directory {
        Some(hook_install::lock(parent)?)
    } else {
        None
    };
    if read(&removal.profile)? != removal.removed_profile
        || read(&removal.registration)?.as_ref() != Some(&removal.disabled_registration)
    {
        return Err("Removed profile or registration changed; preserve subsequent edits".into());
    }
    layouts::write_private(&removal.profile, &removal.original_profile)?;
    layouts::write_private(&removal.registration, &removal.original_registration).map_err(|_| {
        "Profile restored but registration restoration is unconfirmed; inspect before retrying"
            .into()
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) struct Fixture {
        pub(crate) directory: PathBuf,
        pub(crate) receipt: PathBuf,
        pub(crate) profile: PathBuf,
        registration: PathBuf,
    }
    impl Fixture {
        pub(crate) fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "daedalus-uninstall-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let profile = directory.join("native/settings.json");
            let registration: hook_runtime::Registration = serde_json::from_value(json!({
                "version": 1,
                "id": "fixture",
                "directory": directory,
                "scope": {
                    "instance": "fixture",
                    "terminal": {"backend":"tmux", "endpoint":"/fixture.sock", "pane":"%1", "generation":"generation"},
                    "harness": "claude",
                    "conversation": "session"
                },
                "goal_id": "goal",
                "harness_process": null,
                "server_emitter": null,
                "transcript": null,
                "events": ["Stop"],
                "options": {"telegram_delivery":true, "telegram_progress":true, "notifications":true, "hindsight":true},
                "activity_lease_ms": 1000,
                "pending_timeout_ms": 1000,
                "hook_timeout_ms": 1000
            })).unwrap();
            let registration_path = registration.path();
            layouts::write_private(
                &registration_path,
                &serde_json::to_vec(&registration).unwrap(),
            )
            .unwrap();
            let original = b"{\"unrelated\":true}";
            let installed = b"{\"unrelated\":true,\"managed\":true}";
            layouts::write_private(&profile, installed).unwrap();
            let after_sha = hash(installed);
            let backup = directory
                .join("hook-backups")
                .join(format!("fixture-{after_sha}.json"));
            layouts::write_private(
                &backup,
                &serde_json::to_vec(
                    &json!({"profile":profile,"before":original.to_vec(),"after_sha":after_sha}),
                )
                .unwrap(),
            )
            .unwrap();
            let receipt = backup.with_extension("receipt.json");
            layouts::write_private(
                &receipt,
                &serde_json::to_vec(&Receipt {
                    profile: profile.clone(),
                    backup,
                    after_sha,
                    registration: registration_path.clone(),
                })
                .unwrap(),
            )
            .unwrap();
            Self {
                directory,
                receipt,
                profile,
                registration: registration_path,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    #[test]
    fn preview_is_inert_and_reviewed_removal_preserves_original_handlers_and_rolls_back() {
        let f = Fixture::new();
        let before = fs::read(&f.profile).unwrap();
        let registered = fs::read(&f.registration).unwrap();
        assert_eq!(list(&f.directory, "claude").unwrap().len(), 1);
        let plan = Plan::prepare(&f.directory, &f.receipt).unwrap();
        assert_eq!(fs::read(&f.profile).unwrap(), before);
        let approval = plan.approval();
        let receipt = plan.apply(&approval).unwrap();
        assert_eq!(fs::read(&f.profile).unwrap(), b"{\"unrelated\":true}");
        let disabled = hook_runtime::inspect(&f.registration).unwrap();
        assert!(
            !disabled.options.telegram_delivery
                && !disabled.options.hindsight
                && disabled.goal_id.is_none()
        );
        rollback(&receipt).unwrap();
        assert_eq!(fs::read(&f.profile).unwrap(), before);
        assert_eq!(fs::read(&f.registration).unwrap(), registered);
    }
    #[test]
    fn changed_profile_registration_and_wrong_approval_never_overwrite() {
        let f = Fixture::new();
        let original = fs::read(&f.profile).unwrap();
        let plan = Plan::prepare(&f.directory, &f.receipt).unwrap();
        let approval = plan.approval();
        layouts::write_private(&f.profile, b"manual edit").unwrap();
        assert!(plan.apply(&approval).is_err());
        assert_eq!(fs::read(&f.profile).unwrap(), b"manual edit");
        assert!(Plan::prepare(&f.directory, &f.receipt).is_err());
        layouts::write_private(&f.profile, &original).unwrap();
        assert!(
            Plan::prepare(&f.directory, &f.receipt)
                .unwrap()
                .apply("wrong")
                .is_err()
        );
        let plan = Plan::prepare(&f.directory, &f.receipt).unwrap();
        let approval = plan.approval();
        let mut bytes = fs::read(&f.registration).unwrap();
        bytes.push(b' ');
        layouts::write_private(&f.registration, &bytes).unwrap();
        assert!(plan.apply(&approval).is_err());
        assert_eq!(fs::read(&f.profile).unwrap(), original);
    }
    #[test]
    fn rollback_preserves_subsequent_edits_and_receipt_symlinks_are_rejected() {
        let f = Fixture::new();
        let plan = Plan::prepare(&f.directory, &f.receipt).unwrap();
        let approval = plan.approval();
        let receipt = plan.apply(&approval).unwrap();
        layouts::write_private(&f.profile, b"new user settings").unwrap();
        assert!(rollback(&receipt).is_err());
        assert_eq!(fs::read(&f.profile).unwrap(), b"new user settings");
        let link = f.directory.join("link.receipt.json");
        std::os::unix::fs::symlink(&f.receipt, &link).unwrap();
        assert!(Plan::prepare(&f.directory, &link).is_err());
    }
}
