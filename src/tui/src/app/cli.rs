//! Settings CLI; preview is the default and writing requires `--apply`.
use super::{config::ControlConfig, migration::ImportPlan, setup::SetupPlan};
use serde_json::json;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::PathBuf,
};

pub const HELP: &str = concat!(
    "Daedalus configuration\n\n  daedalus config init --config-dir ABS --workspace ABS [--apply]\n  daedalus config import --config-dir ABS --from ABS [--apply]\n  daedalus config validate --from ABS\n  daedalus config import-instance --config-dir NEW_ABS --from OLD_DIR_ABS [--credentials] [--apply]\n\nImport-instance stages settings, checkpoints, layouts, viewer history, legacy routes\nand SQLite snapshots in a separate instance. Telegram, memory and gateway credential files\nrequire --credentials. Native harness sign-in files are not copied. Polling\noffsets and permissions remain proposals for cutover; no service starts.\n\nInit and import print a JSON preview unless --apply is supplied. Existing\nconfiguration directories must be private (0700 on Unix). Import currently\nhandles portable control.json settings only. It does not migrate routes,\ncheckpoints, layouts, delivery databases, tokens, or custom hooks, and does\nnot install dependencies or sign in to harness accounts.\n",
    "\nInstance, interface, runtime and native profile settings:\n  daedalus config show --config-dir ABS\n  daedalus config set --config-dir ABS --key SETTING --value VALUE [--apply]\n\nconfig show lists saved values and editable settings. Instance lists use JSON arrays,\nmetadata bindings use a JSON object with unique keys, and null clears a Telegram hint.\nThese portable hints do not change service identities, credentials or grants. profiles.TOOL accepts an existing\nabsolute directory, or inherit to clear its saved override. config set prints the exact\nold/new value and a file preview; --apply saves with a backup. Settings edited from\nanother process apply on the next launch or instance reopen.\n"
);

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Show {
        directory: PathBuf,
    },
    Set {
        directory: PathBuf,
        key: String,
        value: String,
        apply: bool,
    },
    ImportInstance {
        directory: PathBuf,
        source: PathBuf,
        credentials: bool,
        apply: bool,
    },
    Init {
        directory: PathBuf,
        workspace: PathBuf,
        apply: bool,
    },
    Import {
        directory: PathBuf,
        source: PathBuf,
        apply: bool,
    },
    Validate {
        source: PathBuf,
    },
}

/// Arguments begin after `config`. Errors deliberately omit argument values.
pub fn parse(args: &[String]) -> Result<Command, String> {
    if args.is_empty() || (args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h" | "help"))
    {
        return Ok(Command::Help);
    }
    let verb = args[0].as_str();
    if matches!(verb, "show" | "set") {
        return parse_setting_command(args);
    }
    if !matches!(verb, "init" | "import" | "import-instance" | "validate") {
        return Err("Unknown configuration command; use config --help".into());
    }
    let mut directory = None;
    let mut workspace = None;
    let mut source = None;
    let mut apply = false;
    let mut credentials = false;
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        if flag == "--credentials" && verb == "import-instance" && !credentials {
            credentials = true;
            index += 1;
            continue;
        }
        if flag == "--apply" {
            if apply || verb == "validate" {
                return Err("Invalid or duplicate --apply option".into());
            }
            apply = true;
            index += 1;
            continue;
        }
        let target = match flag {
            "--config-dir" if verb != "validate" => &mut directory,
            "--workspace" if verb == "init" => &mut workspace,
            "--from" if verb != "init" => &mut source,
            _ => return Err("Unknown or inapplicable configuration option".into()),
        };
        if target.is_some() {
            return Err("Duplicate configuration option".into());
        }
        index += 1;
        let value = args
            .get(index)
            .ok_or("Missing configuration option value")?;
        let path = PathBuf::from(value);
        if !path.is_absolute() || value.chars().any(char::is_control) {
            return Err(
                "Configuration paths must be absolute and contain no control characters".into(),
            );
        }
        *target = Some(path);
        index += 1;
    }
    match verb {
        "import-instance" => Ok(Command::ImportInstance {
            directory: directory.ok_or("Missing --config-dir")?,
            source: source.ok_or("Missing --from")?,
            credentials,
            apply,
        }),
        "init" => Ok(Command::Init {
            directory: directory.ok_or("Missing --config-dir")?,
            workspace: workspace.ok_or("Missing --workspace")?,
            apply,
        }),
        "import" => Ok(Command::Import {
            directory: directory.ok_or("Missing --config-dir")?,
            source: source.ok_or("Missing --from")?,
            apply,
        }),
        "validate" => Ok(Command::Validate {
            source: source.ok_or("Missing --from")?,
        }),
        _ => unreachable!(),
    }
}

fn parse_setting_command(args: &[String]) -> Result<Command, String> {
    let mut directory = None;
    let mut key = None;
    let mut value = None;
    let mut apply = false;
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        if flag == "--apply" && args[0] == "set" && !apply {
            apply = true;
            index += 1;
            continue;
        }
        let slot = match flag {
            "--config-dir" => &mut directory,
            "--key" if args[0] == "set" => &mut key,
            "--value" if args[0] == "set" => &mut value,
            _ => return Err("Unknown or duplicate settings option".into()),
        };
        if slot.is_some() {
            return Err("Duplicate settings option".into());
        }
        index += 1;
        *slot = Some(
            args.get(index)
                .ok_or("Missing settings option value")?
                .clone(),
        );
        index += 1;
    }
    let path = directory.ok_or("Missing --config-dir")?;
    let directory = PathBuf::from(&path);
    crate::core::layouts::check_path(&directory)
        .map_err(|_| "Use an absolute configuration path without symlinks or parent traversal")?;
    if args[0] == "show" {
        return Ok(Command::Show { directory });
    }
    Ok(Command::Set {
        directory,
        key: key.ok_or("Missing --key")?,
        value: value.ok_or("Missing --value")?,
        apply,
    })
}

pub fn run(args: &[String], output: &mut impl Write) -> Result<(), String> {
    let (plan, apply) = match parse(args)? {
        Command::Show { directory } => {
            let bytes =
                super::telegram_config::read_private(&directory.join("control.json"), 1024 * 1024)?
                    .ok_or("Configuration missing")?;
            let config = ControlConfig::parse(&bytes)?;
            return write_json(
                output,
                &json!({"configuration":config,"interface_settings":super::interface_settings::SETTINGS,
                "runtime_settings":super::runtime_settings::SETTINGS,
                "instance_settings":super::control_settings::FIELDS,
                "profile_settings":super::config::HARNESSES.iter().map(|tool|json!({"key":format!("profiles.{tool}"),"saved_override":config.profiles.get(*tool),"inherit_value":"inherit","description":"Existing absolute native profile root, or inherit to remove the saved override. Native account files are retained."})).collect::<Vec<_>>(),
                "note":"Saved settings; matching environment overrides and terminal capabilities still apply."}),
            );
        }
        Command::Set {
            directory,
            key,
            value,
            apply,
        } => {
            let bytes =
                super::telegram_config::read_private(&directory.join("control.json"), 1024 * 1024)?
                    .ok_or("Configuration missing")?;
            let expected = ControlConfig::parse(&bytes)?;
            let before = expected.setting_value(&key)?;
            let updated = expected.with_setting(&key, &value)?;
            let after = updated.setting_value(&key)?;
            let plan = SetupPlan::edit_if_current(&directory, &expected, updated)?;
            let preview =
                json!({"setting":key,"before":before,"after":after,"configuration":plan.preview});
            return if apply {
                write_json(output, &json!({"preview":preview,"result":plan.apply()?}))
            } else {
                write_json(output, &json!({"preview":preview}))
            };
        }
        Command::ImportInstance {
            directory,
            source,
            credentials,
            apply,
        } => {
            let plan = ImportPlan::prepare(&source, &directory, credentials)?;
            let preview = plan.preview.clone();
            return if apply {
                write_json(output, &json!({"preview":preview,"result":plan.apply()?}))
            } else {
                write_json(output, &json!({"preview":preview}))
            };
        }
        Command::Help => {
            return output
                .write_all(HELP.as_bytes())
                .map_err(|_| "Cannot write configuration help".into());
        }
        Command::Init {
            directory,
            workspace,
            apply,
        } => (
            SetupPlan::fresh(&directory, ControlConfig::fresh(&workspace)?)?,
            apply,
        ),
        Command::Import {
            directory,
            source,
            apply,
        } => (SetupPlan::import(&directory, &source)?, apply),
        Command::Validate { source } => {
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            let file = options
                .open(source)
                .map_err(|_| "Cannot open configuration file")?;
            let metadata = file
                .metadata()
                .map_err(|_| "Cannot inspect configuration file")?;
            if !metadata.is_file() || metadata.len() > 1024 * 1024 {
                return Err("Configuration must be a regular file of at most 1 MiB".into());
            }
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "Cannot read configuration file")?;
            if bytes.len() > 1024 * 1024 {
                return Err("Configuration exceeds 1 MiB".into());
            }
            ControlConfig::parse(&bytes)?;
            return write_json(
                output,
                &json!({"valid": true, "scope": "portable-settings"}),
            );
        }
    };
    if apply {
        // Include limitations even when the caller explicitly bypasses preview mode.
        let preview = plan.preview.clone();
        let result = plan.apply()?;
        write_json(output, &json!({"preview": preview, "result": result}))
    } else {
        write_json(output, &json!({"preview": plan.preview}))
    }
}

fn write_json(output: &mut impl Write, value: &impl serde::Serialize) -> Result<(), String> {
    serde_json::to_writer_pretty(&mut *output, value)
        .map_err(|_| "Cannot write configuration result")?;
    output
        .write_all(b"\n")
        .map_err(|_| "Cannot write configuration result".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "daedalus-config-cli-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_all_commands_and_defaults_to_preview() {
        assert_eq!(
            parse(&args(&[
                "init",
                "--config-dir",
                crate::test_support::path("/tmp/config"),
                "--workspace",
                crate::test_support::path("/tmp/work")
            ]))
            .unwrap(),
            Command::Init {
                directory: crate::test_support::path("/tmp/config").into(),
                workspace: crate::test_support::path("/tmp/work").into(),
                apply: false
            }
        );
        assert!(matches!(
            parse(&args(&[
                "import",
                "--apply",
                "--from",
                crate::test_support::path("/tmp/source"),
                "--config-dir",
                crate::test_support::path("/tmp/config")
            ]))
            .unwrap(),
            Command::Import { apply: true, .. }
        ));
        assert!(matches!(
            parse(&args(&[
                "validate",
                "--from",
                crate::test_support::path("/tmp/config")
            ]))
            .unwrap(),
            Command::Validate { .. }
        ));
        assert_eq!(parse(&[]).unwrap(), Command::Help);
    }
    #[test]
    fn rejects_ambiguous_or_inapplicable_arguments_without_echoing_values() {
        for input in [
            vec!["init"],
            vec!["validate", "--apply"],
            vec!["validate", "--from", "/a", "--from", "/b"],
            vec!["init", "--config-dir", "relative"],
            vec!["import", "--from"],
            vec!["show", "--apply"],
            vec!["show", "--config-dir", "/a", "--key", "ui.colors"],
            vec!["set", "--config-dir", "/a", "--key", "ui.colors"],
            vec![
                "set",
                "--config-dir",
                "/a",
                "--key",
                "ui.colors",
                "--value",
                "on",
                "--apply",
                "--apply",
            ],
            vec!["secret-token"],
        ] {
            let error = parse(&args(&input)).unwrap_err();
            assert!(!error.contains("secret-token"));
        }
    }
    #[test]
    fn fresh_import_and_validation_work_through_cli() {
        let temp = Temp::new();
        let directory = temp.0.join("fresh");
        let target = temp.0.join("imported");
        let initial = args(&[
            "init",
            "--config-dir",
            directory.to_str().unwrap(),
            "--workspace",
            temp.0.to_str().unwrap(),
        ]);
        let mut output = Vec::new();
        run(&initial, &mut output).unwrap();
        assert!(!directory.exists());
        let preview: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(preview["preview"]["changes"], true);
        let mut applied = initial;
        applied.push("--apply".into());
        run(&applied, &mut Vec::new()).unwrap();
        let source = directory.join("control.json");
        let imported = args(&[
            "import",
            "--config-dir",
            target.to_str().unwrap(),
            "--from",
            source.to_str().unwrap(),
            "--apply",
        ]);
        run(&imported, &mut Vec::new()).unwrap();
        assert_eq!(
            fs::read(&source).unwrap(),
            fs::read(target.join("control.json")).unwrap()
        );
        let mut repeat = Vec::new();
        run(&imported, &mut repeat).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&repeat).unwrap()["result"]["changed"],
            false
        );
        let mut validation = Vec::new();
        run(
            &args(&["validate", "--from", source.to_str().unwrap()]),
            &mut validation,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&validation).unwrap()["valid"],
            true
        );
    }
    #[test]
    fn validation_does_not_echo_malformed_secrets() {
        let temp = Temp::new();
        let source = temp.0.join("bad.json");
        fs::write(&source, b"{\"secret-token\":}").unwrap();
        let mut output = Vec::new();
        let error = run(
            &args(&["validate", "--from", source.to_str().unwrap()]),
            &mut output,
        )
        .unwrap_err();
        assert!(!error.contains("secret-token"));
        assert!(output.is_empty());
    }
    #[test]
    fn runtime_edits_preview_apply_back_up_and_preserve_other_settings() {
        let temp = Temp::new();
        let directory = temp.0.join("instance");
        let mut initial = ControlConfig::fresh(&temp.0).unwrap();
        initial.ui.colors = false;
        SetupPlan::fresh(&directory, initial.clone())
            .unwrap()
            .apply()
            .unwrap();
        let path = directory.join("control.json");
        let before = fs::read(&path).unwrap();
        let mut command = args(&[
            "set",
            "--config-dir",
            directory.to_str().unwrap(),
            "--key",
            "runtime.harness_probe_timeout_ms",
            "--value",
            "700",
        ]);
        let mut output = vec![];
        run(&command, &mut output).unwrap();
        let review: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(review["preview"]["before"], 2000);
        assert_eq!(review["preview"]["after"], 700);
        assert_eq!(fs::read(&path).unwrap(), before);
        command.push("--apply".into());
        output.clear();
        run(&command, &mut output).unwrap();
        let result: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            fs::read(result["result"]["backup"].as_str().unwrap()).unwrap(),
            before
        );
        let mut expected = initial;
        expected.runtime.harness_probe_timeout_ms = 700;
        assert_eq!(
            ControlConfig::parse(&fs::read(&path).unwrap()).unwrap(),
            expected
        );
        command[6] = "0".into();
        output.clear();
        assert!(run(&command, &mut output).is_err());
        assert!(output.is_empty());
        assert_eq!(
            ControlConfig::parse(&fs::read(&path).unwrap()).unwrap(),
            expected
        );
        run(
            &args(&["show", "--config-dir", directory.to_str().unwrap()]),
            &mut output,
        )
        .unwrap();
        let shown: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            shown["runtime_settings"].as_array().unwrap().len(),
            super::super::runtime_settings::SETTINGS.len()
        );
        assert_eq!(
            shown["configuration"]["runtime"]["harness_probe_timeout_ms"],
            700
        );
    }
    #[test]
    fn interface_edits_preview_apply_backup_and_reject_bad_values() {
        let temp = Temp::new();
        let directory = temp.0.join("instance");
        let mut initial = ControlConfig::fresh(&temp.0).unwrap();
        initial.subscriptions.push("codex".into());
        SetupPlan::fresh(&directory, initial.clone())
            .unwrap()
            .apply()
            .unwrap();
        let path = directory.join("control.json");
        let before = fs::read(&path).unwrap();
        let mut command = args(&[
            "set",
            "--config-dir",
            directory.to_str().unwrap(),
            "--key",
            "ui.preview_interval_ms",
            "--value",
            "700",
        ]);
        let mut output = Vec::new();
        run(&command, &mut output).unwrap();
        let preview: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(preview["preview"]["before"], 2000);
        assert_eq!(preview["preview"]["after"], 700);
        assert_eq!(fs::read(&path).unwrap(), before);
        command.push("--apply".into());
        output.clear();
        run(&command, &mut output).unwrap();
        let applied: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            fs::read(applied["result"]["backup"].as_str().unwrap()).unwrap(),
            before
        );
        let mut expected = initial;
        expected.ui.preview_interval_ms = 700;
        assert_eq!(
            ControlConfig::parse(&fs::read(&path).unwrap()).unwrap(),
            expected
        );
        let saved = fs::read(&path).unwrap();
        command[6] = "secret-value".into();
        output.clear();
        assert!(
            !run(&command, &mut output)
                .unwrap_err()
                .contains("secret-value")
        );
        assert!(output.is_empty());
        assert_eq!(fs::read(&path).unwrap(), saved);
        output.clear();
        run(
            &args(&["show", "--config-dir", directory.to_str().unwrap()]),
            &mut output,
        )
        .unwrap();
        let shown: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(shown["configuration"]["ui"]["preview_interval_ms"], 700);
        assert_eq!(
            shown["interface_settings"].as_array().unwrap().len(),
            super::super::interface_settings::SETTINGS.len()
        );
    }
}
