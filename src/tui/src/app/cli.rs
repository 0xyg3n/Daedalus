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

pub const HELP: &str = "Daedalus configuration\n\n  daedalus config init --config-dir ABS --workspace ABS [--apply]\n  daedalus config import --config-dir ABS --from ABS [--apply]\n  daedalus config validate --from ABS\n  daedalus config import-instance --config-dir NEW_ABS --from OLD_DIR_ABS [--credentials] [--apply]\n\nImport-instance stages settings, checkpoints, layouts, legacy routes and SQLite\nsnapshots in a separate instance. Telegram tokens require --credentials. Polling\noffsets and permissions remain proposals for cutover; no service starts.\n\nInit and import print a JSON preview unless --apply is supplied. Existing\nconfiguration directories must be private (0700 on Unix). Import currently\nhandles portable control.json settings only. It does not migrate routes,\ncheckpoints, layouts, delivery databases, tokens, or custom hooks, and does\nnot install dependencies or sign in to harness accounts.\n";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
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

pub fn run(args: &[String], output: &mut impl Write) -> Result<(), String> {
    let (plan, apply) = match parse(args)? {
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
                "/tmp/config",
                "--workspace",
                "/tmp/work"
            ]))
            .unwrap(),
            Command::Init {
                directory: "/tmp/config".into(),
                workspace: "/tmp/work".into(),
                apply: false
            }
        );
        assert!(matches!(
            parse(&args(&[
                "import",
                "--apply",
                "--from",
                "/tmp/source",
                "--config-dir",
                "/tmp/config"
            ]))
            .unwrap(),
            Command::Import { apply: true, .. }
        ));
        assert!(matches!(
            parse(&args(&["validate", "--from", "/tmp/config"])).unwrap(),
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
}
