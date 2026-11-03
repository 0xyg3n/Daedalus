//! Explicit, offline memory settings; writes require --apply.
use super::memory::{self, Config, Plan};
use serde_json::json;
use std::{collections::BTreeMap, io::Write, path::Path};
type Result<T> = std::result::Result<T, String>;
pub const HELP: &str = "\nHindsight configuration (offline):\n  daedalus memory config show --config-dir ABS\n  daedalus memory config set --config-dir ABS --key enabled|endpoint|credential_file|recall_query|timeout_ms --value VALUE [--apply]\n  daedalus memory config bind --config-dir ABS --registration ABS --bank BANK [--apply]\n  daedalus memory config unbind --config-dir ABS --registration-id ID [--apply]\nSet the endpoint first for a new configuration; new configurations start disabled.\nUse none to clear the credential file. Values are literal text, including recall query line breaks.\nBind requires an exact installed memory-enabled registration; it never enables memory.\nChanges print a review by default. --apply saves privately with a backup and refuses stale reviews.\nNo command contacts Hindsight, starts hooks, or reads credential values into its output.\n";
pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    if args.is_empty() || args == ["--help"] {
        return write!(out, "{HELP}").map_err(|_| "Cannot write memory settings help".into());
    }
    let verb = args[0].as_str();
    if !["show", "set", "bind", "unbind"].contains(&verb) {
        return Err("Unknown memory configuration command".into());
    }
    let mut flags = BTreeMap::new();
    let mut apply = false;
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        if flag == "--apply" {
            if apply || verb == "show" {
                return Err("Invalid or repeated memory apply option".into());
            }
            apply = true;
            continue;
        }
        let allowed = flag == "--config-dir"
            || match verb {
                "set" => ["--key", "--value"].contains(&flag.as_str()),
                "bind" => ["--registration", "--bank"].contains(&flag.as_str()),
                "unbind" => flag == "--registration-id",
                _ => false,
            };
        if !allowed {
            return Err("Unknown memory configuration option".into());
        }
        let value = rest
            .next()
            .ok_or("Missing memory configuration option value")?;
        if flags.insert(flag.as_str(), value.as_str()).is_some() {
            return Err("Repeated memory configuration option".into());
        }
    }
    let directory = Path::new(*flags.get("--config-dir").ok_or("Missing --config-dir")?);
    crate::core::layouts::check_path(directory)?;
    let current = memory::load(directory)?;
    if verb == "show" {
        let fields:Vec<_>=memory::FIELDS.iter().map(|(key,label,description)|json!({"key":key,"label":label,"description":description})).collect();
        serde_json::to_writer(&mut *out,&json!({"configured":current.is_some(),"configuration":current,"editable_fields":fields,"bindings":"Use bind/unbind with exact installed registration identities","network_verified":false})).map_err(|_|"Cannot write memory settings")?;
        return writeln!(out).map_err(|_| "Cannot write memory settings".into());
    }
    let value = |key: &str| -> Result<&str> {
        flags
            .get(key)
            .copied()
            .ok_or_else(|| format!("Missing {key}"))
    };
    let mut updated = current.clone().unwrap_or_else(Config::fresh);
    let mut registration = None;
    match verb {
        "set" => updated = updated.set(value("--key")?, value("--value")?)?,
        "bind" => {
            let path = Path::new(value("--registration")?);
            let selected = super::hook_runtime::inspect(path)
                .map_err(|_| "Invalid installed memory registration")?;
            if selected.directory != directory || selected.path() != path {
                return Err("Memory registration belongs to another instance".into());
            }
            updated = updated.bind(&selected, value("--bank")?)?;
            registration = Some(selected);
        }
        "unbind" => {
            let id = value("--registration-id")?;
            let before = updated.bindings.len();
            updated
                .bindings
                .retain(|binding| binding.registration_id != id);
            if before == updated.bindings.len() {
                return Err("Memory binding not found".into());
            }
        }
        _ => unreachable!(),
    }
    let mut plan = Plan::update(directory, current.as_ref(), updated)?;
    if let Some(registration) = registration {
        plan = plan.guard_registration(&registration)?;
    }
    let review = plan.review();
    let result = if apply {
        let approval = plan.approval();
        Some(plan.apply(&approval)?)
    } else {
        None
    };
    serde_json::to_writer(
        &mut *out,
        &json!({"review":review,"saved":apply,"result":result,"network_verified":false}),
    )
    .map_err(|_| "Cannot write memory settings result")?;
    writeln!(out).map_err(|_| "Cannot write memory settings result".into())
}

#[cfg(test)]
mod tests {
    use super::super::memory::configuration_fixture::Fixture;
    use super::*;
    fn call(f: &Fixture, rest: &[&str]) -> Result<serde_json::Value> {
        let mut args: Vec<String> = vec![
            "config".into(),
            rest[0].into(),
            "--config-dir".into(),
            f.root.to_str().unwrap().into(),
        ];
        args.extend(rest[1..].iter().map(|s| s.to_string()));
        let mut output = Vec::new();
        super::super::memory_status::run(&args, &mut output)?;
        Ok(serde_json::from_slice(&output).unwrap())
    }
    #[test]
    fn cli_previews_and_saves_all_fields_with_literal_query_and_private_backups() {
        let f = Fixture::new(true);
        let original = std::fs::read(f.root.join("hindsight.json")).unwrap();
        let show = call(&f, &["show"]).unwrap();
        assert_eq!(
            show["editable_fields"].as_array().unwrap().len(),
            memory::FIELDS.len()
        );
        let query = "  Literal\nquery with 界 and \\characters  ";
        for (key, value) in [
            ("endpoint", "http://localhost:8999"),
            ("credential_file", "none"),
            ("recall_query", query),
            ("timeout_ms", "8000"),
            ("enabled", "true"),
        ] {
            let preview = call(&f, &["set", "--key", key, "--value", value]).unwrap();
            assert_eq!(preview["saved"], false);
            let before = memory::load(&f.root).unwrap().unwrap();
            let saved = call(&f, &["set", "--key", key, "--value", value, "--apply"]).unwrap();
            assert_eq!(saved["saved"], true);
            let after = memory::load(&f.root).unwrap().unwrap();
            assert_eq!(after.bindings, before.bindings);
            if key == "recall_query" {
                assert_eq!(after.recall_query, query);
            }
        }
        assert_ne!(
            std::fs::read(f.root.join("hindsight.json")).unwrap(),
            original
        );
        assert!(!f.root.join("delivery.sqlite3").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for e in std::fs::read_dir(&f.root).unwrap() {
                let e = e.unwrap();
                if e.file_name().to_string_lossy().starts_with("hindsight") {
                    assert_eq!(e.metadata().unwrap().permissions().mode() & 0o777, 0o600);
                }
            }
        }
    }
    #[test]
    fn cli_binding_keeps_disabled_state_and_unrelated_scope_and_requires_exact_registration() {
        let f = Fixture::new(true);
        let path = f.registration.path();
        let preview = call(
            &f,
            &[
                "bind",
                "--registration",
                path.to_str().unwrap(),
                "--bank",
                "new-bank",
            ],
        )
        .unwrap();
        assert_eq!(preview["saved"], false);
        assert_eq!(memory::load(&f.root).unwrap().unwrap(), f.config);
        call(
            &f,
            &[
                "bind",
                "--registration",
                path.to_str().unwrap(),
                "--bank",
                "new-bank",
                "--apply",
            ],
        )
        .unwrap();
        let config = memory::load(&f.root).unwrap().unwrap();
        assert!(!config.enabled);
        assert_eq!(config.bindings[0].bank, "new-bank");
        assert_eq!(config.bindings[1], f.config.bindings[1]);
        call(
            &f,
            &["unbind", "--registration-id", "fixture-hook", "--apply"],
        )
        .unwrap();
        assert_eq!(
            memory::load(&f.root).unwrap().unwrap().bindings,
            vec![f.config.bindings[1].clone()]
        );
        let other = Fixture::new(true);
        assert!(
            call(
                &f,
                &[
                    "bind",
                    "--registration",
                    other.registration.path().to_str().unwrap(),
                    "--bank",
                    "wrong-instance",
                    "--apply"
                ]
            )
            .is_err()
        );
    }
    #[test]
    fn cli_new_configuration_starts_disabled_and_refuses_invalid_edits_without_overwrite() {
        let f = Fixture::new(false);
        call(
            &f,
            &[
                "set",
                "--key",
                "endpoint",
                "--value",
                "http://localhost:8999",
            ],
        )
        .unwrap();
        assert!(!f.root.join("hindsight.json").exists());
        call(
            &f,
            &[
                "set",
                "--key",
                "endpoint",
                "--value",
                "http://localhost:8999",
                "--apply",
            ],
        )
        .unwrap();
        let initial = memory::load(&f.root).unwrap().unwrap();
        assert!(!initial.enabled);
        assert!(initial.bindings.is_empty());
        for rest in [
            vec!["set", "--key", "timeout_ms", "--value", "99", "--apply"],
            vec![
                "set",
                "--key",
                "credential_file",
                "--value",
                "raw-token",
                "--apply",
            ],
            vec![
                "set", "--key", "enabled", "--value", "true", "--apply", "--apply",
            ],
            vec!["show", "--apply"],
            vec!["unbind", "--registration-id", "missing", "--apply"],
        ] {
            assert!(call(&f, &rest).is_err());
            assert_eq!(memory::load(&f.root).unwrap().unwrap(), initial);
        }
    }
}
