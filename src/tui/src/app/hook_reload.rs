//! Reviewed refresh of managed profiles. Never infer that rewriting a file has
//! reloaded a running provider or granted native hook trust.
use super::{hook_install, hook_runtime};
use crate::{adapters::terminals::Terminals, core::layouts};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
struct Profile {
    path: PathBuf,
    before: Vec<u8>,
    after: Vec<u8>,
}
pub struct Plan {
    directory: PathBuf,
    binary: PathBuf,
    target: PathBuf,
    registrations: Vec<(PathBuf, Vec<u8>)>,
    profiles: Vec<Profile>,
    rows: Vec<Value>,
}
fn contains_registration(bytes: &[u8], r: &hook_runtime::Registration) -> Result<bool> {
    use super::hooks::Harness;
    let marker = format!("Daedalus managed hook: {}", r.id);
    match r.scope.harness {
        Harness::Pi => {
            Ok(bytes
                .starts_with(format!("// Daedalus managed Pi bridge v1: {}\n", r.id).as_bytes()))
        }
        Harness::Opencode => Ok(bytes
            .starts_with(format!("// Daedalus managed OpenCode bridge v2: {}\n", r.id).as_bytes())),
        Harness::Hermes => {
            let value: yaml_serde::Value =
                yaml_serde::from_slice(bytes).map_err(|_| "Invalid Hermes hook profile")?;
            Ok(value
                .get("hooks")
                .and_then(|v| v.as_mapping())
                .into_iter()
                .flat_map(|v| v.values())
                .filter_map(|v| v.as_sequence())
                .flatten()
                .any(|h| h.get("daedalus_managed").and_then(|v| v.as_str()) == Some(r.id.as_str())))
        }
        _ => {
            fn matches(value: &Value, marker: &str) -> bool {
                match value {
                    Value::Object(v) => {
                        v.get("statusMessage").and_then(Value::as_str) == Some(marker)
                            || v.get("command")
                                .and_then(Value::as_str)
                                .is_some_and(|v| v.ends_with(&format!(" # {marker}")))
                            || v.values().any(|v| matches(v, marker))
                    }
                    Value::Array(v) => v.iter().any(|v| matches(v, marker)),
                    _ => false,
                }
            }
            Ok(matches(
                &serde_json::from_slice::<Value>(bytes).map_err(|_| "Invalid hook profile")?,
                &marker,
            ))
        }
    }
}
fn instruction(harness: &str) -> &'static str {
    match harness {
        "claude" | "cursor" => {
            "The provider watches its profile. Confirm a fresh successful hook before using Telegram."
        }
        "codex" => {
            "Open /hooks in Codex to reload and review changed handlers. Native trust is not granted by Daedalus."
        }
        "pi" => {
            "Run /reload in the idle Pi session to reload its extension, then confirm a fresh hook."
        }
        "opencode" => {
            "Restart the OpenCode server that owns this plugin when its sessions are idle; a UI reconnect alone does not reload server plugins."
        }
        "hermes" | "grok" => {
            "Resume the saved conversation after restarting the harness, then re-enroll its new process before Telegram input."
        }
        _ => "This provider has no verified reload procedure.",
    }
}
impl Plan {
    pub fn review(&self) -> Value {
        json!({"operation":"reload_managed_hooks", "profiles":self.profiles.iter().map(|p|json!({"path":p.path,"changed":p.before!=p.after})).collect::<Vec<_>>(),"harnesses":self.rows,"notice":"Updates only managed handlers. Native approval and restart requirements remain visible; no session is killed and no model prompt is sent."})
    }
    pub fn apply(self, directory: &Path) -> Result<Value> {
        if self.directory != directory {
            return Err("Hook reload belongs to another instance".into());
        }
        let _active = hook_runtime::active_guard(directory).map_err(|e| e.to_string())?;
        let _lock = hook_install::lock(directory)?;
        let parents = self
            .profiles
            .iter()
            .filter_map(|p| p.path.parent())
            .filter(|p| *p != directory)
            .collect::<BTreeSet<_>>();
        let _profile_locks = parents
            .into_iter()
            .map(hook_install::lock_profile)
            .collect::<Result<Vec<_>>>()?;

        if std::fs::canonicalize(&self.binary).map_err(|_| "Hook executable disappeared")?
            != self.target
        {
            return Err("Daedalus changed since review; review reload again".into());
        }
        for (path, bytes) in &self.registrations {
            if layouts::read_private(path)?.as_ref() != Some(bytes) {
                return Err("Hook registration changed; review reload again".into());
            }
        }
        for p in &self.profiles {
            if hook_install::read_profile(&p.path)?.as_ref() != Some(&p.before) {
                return Err("Hook profile changed; review reload again".into());
            }
        }
        for p in &self.profiles {
            let key = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&json!([p.path, p.before, p.after]))
                        .map_err(|_| "Cannot encode reload backup")?
                )
            );
            let backup = directory.join("hook-reloads").join(format!("{key}.json"));
            layouts::write_private(&backup,&serde_json::to_vec(&json!({"profile":p.path,"before":p.before,"after_sha":format!("{:x}",Sha256::digest(&p.after))})).map_err(|_|"Cannot encode reload backup")?)?;
            hook_install::write_profile(&p.path,&p.after).map_err(|_|"Hook refresh stopped after a profile write failed; inspect private hook-reloads backups before retrying")?;
        }
        Ok(
            json!({"state":"profiles_refreshed", "runtime_verified":false,"harnesses":self.rows,"next":"Follow each provider's reload/review step. A fresh successful hook confirms activation."}),
        )
    }
}
pub fn prepare(directory: &Path, binary: PathBuf, terminals: &Terminals) -> Result<Plan> {
    hook_install::check_binary_path(&binary)?;
    let mut receipts = BTreeMap::<PathBuf, BTreeSet<PathBuf>>::new();
    let root = directory.join("hook-backups");
    layouts::check_path(&root)?;
    let entries = match std::fs::read_dir(&root) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(
                "No managed hook installations found. Install standard hooks first.".into(),
            );
        }
        Err(_) => return Err("Cannot read managed hook receipts".into()),
    };
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err("Managed hook receipt limit exceeded".into());
        }
        let path = entry.map_err(|_| "Cannot read hook receipt")?.path();
        if !path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.ends_with(".receipt.json"))
        {
            continue;
        }
        let bytes = layouts::read_private(&path)?.ok_or("Hook receipt disappeared")?;
        let receipt: hook_install::Receipt =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid managed hook receipt")?;
        receipts
            .entry(receipt.registration)
            .or_default()
            .insert(receipt.profile);
    }
    let mut profiles = BTreeMap::<PathBuf, Profile>::new();
    let mut registrations = Vec::new();
    let mut rows = Vec::new();
    for (path, locations) in receipts {
        let r = hook_runtime::inspect(&path).map_err(|e| e.to_string())?;
        if r.directory != directory {
            return Err("Hook receipt points to another instance".into());
        }
        if locations.len() != 1 {
            return Err("A registration has multiple profile locations; inspect its installation before reloading".into());
        }
        let profile = locations.into_iter().next().unwrap();
        let Some(current) = hook_install::read_profile(&profile)? else {
            continue;
        };
        // Removal receipts are retained for rollback; they must not resurrect hooks.
        if !contains_registration(&current, &r)? {
            continue;
        }

        let entry = if let Some(entry) = profiles.get_mut(&profile) {
            entry
        } else {
            let bytes = hook_install::read_profile(&profile)?
                .ok_or("Managed hook profile is missing; reinstall hooks")?;
            profiles.entry(profile.clone()).or_insert(Profile {
                path: profile,
                before: bytes.clone(),
                after: bytes,
            })
        };
        entry.after = hook_install::merge(Some(&entry.after), &r, &binary)?;
        let harness = serde_json::to_value(r.scope.harness).map_err(|_| "Invalid harness")?;
        rows.push(json!({"registration":r.id,"harness":harness,"terminal":r.scope.terminal,"process_verified":hook_runtime::verify_live_session(&r,terminals).is_ok(),"next":instruction(harness.as_str().unwrap_or(""))}));
        registrations.push((
            path.clone(),
            layouts::read_private(&path)?.ok_or("Hook registration disappeared")?,
        ));
    }
    if profiles.is_empty() {
        return Err("No managed hook installations found. Install standard hooks first.".into());
    }
    let target = std::fs::canonicalize(&binary).map_err(|_| "Cannot resolve hook executable")?;
    Ok(Plan {
        directory: directory.into(),
        binary,
        target,
        registrations,
        profiles: profiles.into_values().collect(),
        rows,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_recognizes_only_present_owned_handlers_for_every_adapter() {
        let mut r:hook_runtime::Registration=serde_json::from_value(json!({"version":1,"id":"fixture-hook","directory":crate::test_support::path("/tmp/hooks"),"scope":{"instance":"fixture","terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"original"},"conversation":"session","harness":"codex"},"goal_id":null,"harness_process":null,"server_emitter":null,"transcript":null,"events":["Stop"],"options":{"telegram_delivery":false,"telegram_progress":false,"notifications":false,"hindsight":false},"activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":12000})).unwrap();
        for capability in hook_install::capabilities() {
            r.scope.harness = serde_json::from_value(json!(capability.harness)).unwrap();
            let bytes = hook_install::merge(
                None,
                &r,
                Path::new(crate::test_support::path("/tmp/daedalus")),
            )
            .unwrap();
            assert!(
                contains_registration(&bytes, &r).unwrap(),
                "{}",
                capability.harness
            );
            assert!(
                !contains_registration(b"{}", &r).unwrap(),
                "removed {}",
                capability.harness
            );
        }
    }
    #[test]
    fn changed_profile_prevents_all_reload_writes() {
        let root = std::env::temp_dir().join(format!(
            "daedalus-reload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let a = root.join("a.json");
        let b = root.join("b.json");
        let binary = root.join("binary");
        layouts::write_private(&a, b"original-a").unwrap();
        layouts::write_private(&b, b"manual-edit").unwrap();
        layouts::write_private(&binary, b"fixture").unwrap();
        let plan = Plan {
            directory: root.clone(),
            target: binary.clone(),
            binary,
            registrations: vec![],
            rows: vec![],
            profiles: vec![
                Profile {
                    path: a.clone(),
                    before: b"original-a".to_vec(),
                    after: b"new-a".to_vec(),
                },
                Profile {
                    path: b.clone(),
                    before: b"original-b".to_vec(),
                    after: b"new-b".to_vec(),
                },
            ],
        };
        assert!(plan.apply(&root).unwrap_err().contains("profile changed"));
        assert_eq!(std::fs::read(&a).unwrap(), b"original-a");
        assert_eq!(std::fs::read(&b).unwrap(), b"manual-edit");
        assert!(!root.join("hook-reloads").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn every_native_adapter_has_explicit_activation_requirements() {
        for capability in hook_install::capabilities() {
            assert!(!instruction(capability.harness).contains("no verified"));
        }
        assert!(instruction("codex").contains("trust"));
        assert!(instruction("opencode").contains("server"));
    }
}
