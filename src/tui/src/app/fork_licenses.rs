//! Named native profile roots. Credentials remain inside the selected harness.
use crate::core::{checkpoint_budget, layouts};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub name: String,
    pub home: PathBuf,
    catalog: PathBuf,
    catalog_sha256: String,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read(
    path: &Path,
    settings: &super::runtime_settings::Settings,
) -> Result<Option<(Vec<u8>, Value)>> {
    let mut policy = settings.checkpoint_policy();
    policy.file_bytes = policy
        .file_bytes
        .min(super::config::MAX_CONFIG_BYTES as u64);
    let mut guard = checkpoint_budget::Guard::new(policy)?;
    let Some(bytes) = guard.read(path)? else {
        return Ok(None);
    };
    let value = serde_json::from_slice(&bytes).map_err(|_| "Invalid license catalog structure")?;
    Ok(Some((bytes, value)))
}
fn roots(value: &Value) -> Result<BTreeMap<String, PathBuf>> {
    let licenses = value
        .get("licenses")
        .and_then(Value::as_object)
        .ok_or("License catalog needs a licenses object")?;
    let mut result = BTreeMap::new();
    for (name, license) in licenses {
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err("Invalid native license name".into());
        }
        let home = license
            .get("home")
            .and_then(Value::as_str)
            .ok_or("Native license needs an absolute home directory")?;
        if home.chars().any(char::is_control) {
            return Err("Invalid native license directory".into());
        }
        let home = super::hooks::windows_native_path(Path::new(home));
        layouts::check_path(&home)?;
        result.insert(name.clone(), home);
    }
    Ok(result)
}
pub fn list(
    directory: &Path,
    harness: &str,
    environment: &BTreeMap<String, String>,
    settings: &super::runtime_settings::Settings,
) -> Result<Vec<Choice>> {
    if harness != "codex" {
        return Ok(Vec::new());
    }
    let explicit = environment.get("CODEX_LICENSE_FILE");
    let mut candidates = if let Some(path) = explicit {
        vec![PathBuf::from(path)]
    } else {
        let mut paths = vec![directory.join("codex-licenses.json")];
        if let Some(home) = environment
            .get("HOME")
            .or_else(|| environment.get("USERPROFILE"))
        {
            let legacy = Path::new(home).join(".config/daedalus/codex-licenses.json");
            if !paths.contains(&legacy) {
                paths.push(legacy);
            }
        }
        paths
    };
    for path in &mut candidates {
        *path = super::hooks::windows_native_path(path);
        layouts::check_path(path)?;
        if let Some((bytes, value)) = read(path, settings)? {
            let sha = digest(&bytes);
            return Ok(roots(&value)?
                .into_iter()
                .map(|(name, home)| Choice {
                    name,
                    home,
                    catalog: path.clone(),
                    catalog_sha256: sha.clone(),
                })
                .collect());
        }
    }
    if explicit.is_some() {
        Err("Configured CODEX_LICENSE_FILE does not exist".into())
    } else {
        Ok(Vec::new())
    }
}
impl Choice {
    pub(crate) fn verify(&self, settings: &super::runtime_settings::Settings) -> Result<()> {
        let (bytes, value) =
            read(&self.catalog, settings)?.ok_or("Reviewed license catalog disappeared")?;
        if digest(&bytes) != self.catalog_sha256
            || roots(&value)?.get(&self.name) != Some(&self.home)
        {
            return Err("License catalog changed; select the fork license again".into());
        }
        layouts::check_path(&self.home)?;
        if !self.home.is_dir() {
            return Err("Selected license home does not exist".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn catalog_choices_keep_native_roots_and_refuse_changed_review() {
        let root = std::env::temp_dir().join(format!(
            "daedalus-license-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        let catalog = root.join("codex-licenses.json");
        layouts::write_private(&catalog,&serde_json::to_vec(&json!({"default":"first","licenses":{"first":{"home":root,"email":"fixture@example.invalid","no_failover":true},"second":{"home":root}},"seats":{"fixture":"first"}})).unwrap()).unwrap();
        let settings = super::super::runtime_settings::Settings::default();
        let choices = list(&root, "codex", &BTreeMap::new(), &settings).unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].name, "first");
        assert_eq!(choices[0].home, root);
        choices[0].verify(&settings).unwrap();
        layouts::write_private(&catalog, b"{\"licenses\":{}}").unwrap();
        assert!(
            choices[0]
                .verify(&settings)
                .unwrap_err()
                .contains("changed")
        );
        fs_remove(&root);
    }
    #[test]
    fn explicit_missing_duplicate_and_relative_license_roots_are_refused() {
        let root = std::env::temp_dir().join(format!(
            "daedalus-license-invalid-{}-{}",
            std::process::id(),
            crate::test_support::unique_nanos()
        ));
        let catalog = root.join("codex-licenses.json");
        let environment = [(
            "CODEX_LICENSE_FILE".into(),
            catalog.to_string_lossy().into_owned(),
        )]
        .into();
        let settings = super::super::runtime_settings::Settings::default();
        assert!(list(&root, "codex", &environment, &settings).is_err());
        for bytes in [
            b"{\"licenses\":{\"a\":{},\"a\":{}}}".as_slice(),
            b"{\"licenses\":{\"a\":{\"home\":\"relative\"}}}",
        ] {
            layouts::write_private(&catalog, bytes).unwrap();
            assert!(list(&root, "codex", &environment, &settings).is_err());
        }
        assert!(
            list(&root, "claude", &environment, &settings)
                .unwrap()
                .is_empty()
        );
        fs_remove(&root);
    }
    fn fs_remove(path: &Path) {
        std::fs::remove_dir_all(path).unwrap();
    }
}
