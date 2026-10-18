//! Native checkpoint CLI sharing the existing session application operations.
use super::{
    backend::{NativeSource, Source},
    config::ControlConfig,
    session_actions::{SaveRequest, SessionStore},
    session_menu::{Answer, Request},
};
use crate::{
    adapters::{
        terminal_actions::Placement,
        terminals::{self, Terminals},
    },
    core::{checkpoints::Operation, layouts},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write, path::PathBuf};
pub type Result<T> = std::result::Result<T, String>;
pub const HELP: &str = "daedalus checkpoint list --config-dir ABS\ndaedalus checkpoint show|remove --config-dir ABS --key CHECKPOINT_KEY [--apply]\ndaedalus checkpoint save --config-dir ABS --target ABS --name NAME [--sid ID] [--include-output] [--apply]\ndaedalus checkpoint spawn --config-dir ABS --key CHECKPOINT_KEY --target ABS --operation resume|fork|handoff --harness ID [--placement split|window] [--apply]\nTarget is a private JSON file containing one exact terminal observation from inventory, including generation. Labels are never resolved. Save/remove/spawn preview by default; --apply authorizes that operation. Spawn requires installed harness capability evidence.\n";
#[derive(Debug, PartialEq)]
pub struct Command {
    verb: String,
    directory: PathBuf,
    options: BTreeMap<String, String>,
    apply: bool,
    include_output: bool,
}
pub fn parse(args: &[String]) -> Result<Option<Command>> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return Ok(None);
    }
    let verb = args[0].as_str();
    if !["list", "show", "remove", "rm", "save", "spawn"].contains(&verb) {
        return Err("Unknown checkpoint command; use checkpoint --help".into());
    }
    let mut options = BTreeMap::new();
    let (mut apply, mut include_output) = (false, false);
    let mut index = 1;
    while index < args.len() {
        let flag = &args[index];
        if flag == "--apply" && !apply {
            apply = true;
            index += 1;
            continue;
        }
        if flag == "--include-output" && !include_output {
            include_output = true;
            index += 1;
            continue;
        }
        if ![
            "--config-dir",
            "--key",
            "--target",
            "--name",
            "--sid",
            "--operation",
            "--harness",
            "--placement",
        ]
        .contains(&flag.as_str())
            || index + 1 >= args.len()
            || options
                .insert(flag.clone(), args[index + 1].clone())
                .is_some()
        {
            return Err("Unknown, missing or duplicate checkpoint option".into());
        }
        index += 2;
    }
    let directory = PathBuf::from(
        options
            .remove("--config-dir")
            .ok_or("Missing --config-dir")?,
    );
    layouts::check_path(&directory)?;
    let (required, allowed): (&[&str], &[&str]) = match verb {
        "list" => (&[], &[]),
        "show" | "remove" | "rm" => (&["--key"], &["--key"]),
        "save" => (&["--target", "--name"], &["--target", "--name", "--sid"]),
        _ => (
            &["--key", "--target", "--operation", "--harness"],
            &[
                "--key",
                "--target",
                "--operation",
                "--harness",
                "--placement",
            ],
        ),
    };
    if required.iter().any(|key| !options.contains_key(*key))
        || options.keys().any(|key| !allowed.contains(&key.as_str()))
        || (include_output && verb != "save")
        || (apply && ["list", "show"].contains(&verb))
    {
        return Err("Options do not match checkpoint operation".into());
    }
    if options.get("--name").is_some_and(|name| {
        name.trim().is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control)
    }) {
        return Err("Checkpoint name must contain 1 to 100 printable characters".into());
    }
    if options
        .get("--harness")
        .is_some_and(|id| crate::adapters::harnesses::tool(id).is_none())
    {
        return Err("Unknown harness adapter".into());
    }
    if let Some(path) = options.get("--target") {
        layouts::check_path(std::path::Path::new(path))?;
    }
    if options
        .get("--operation")
        .is_some_and(|op| !["resume", "fork", "handoff"].contains(&op.as_str()))
        || options
            .get("--placement")
            .is_some_and(|p| !["split", "window"].contains(&p.as_str()))
    {
        return Err("Invalid checkpoint operation or placement".into());
    }
    Ok(Some(Command {
        verb: verb.into(),
        directory,
        options,
        apply,
        include_output,
    }))
}
pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    let Some(command) = parse(args)? else {
        return write!(out, "{HELP}").map_err(|_| "Cannot write checkpoint help".into());
    };
    let mut source = NativeSource {
        directory: command.directory.clone(),
        terminals: Terminals::default(),
        last_snapshot: json!({}),
    };
    run_with(command, &mut source, out)
}
pub fn run_with(command: Command, source: &mut impl Source, out: &mut impl Write) -> Result<()> {
    let store = SessionStore {
        directory: command.directory.clone(),
    };
    let key = command
        .options
        .get("--key")
        .map(String::as_str)
        .unwrap_or("");
    let value = match command.verb.as_str() {
        "list" => serde_json::to_value(store.list()?).map_err(|_| "Cannot encode checkpoints")?,
        "show" => serde_json::to_value(store.get(key)?).map_err(|_| "Cannot encode checkpoint")?,
        "remove" | "rm" => {
            let row = store.get(key)?;
            if command.apply {
                store.remove(key)?;
            }
            json!({"operation":"remove","key":row.key,"applied":command.apply,"native_history_unchanged":true})
        }
        "save" | "spawn" => {
            let config = ControlConfig::parse(
                &layouts::read_private(&command.directory.join("control.json"))?
                    .ok_or("Configuration missing")?,
            )?;
            let path = PathBuf::from(&command.options["--target"]);
            let pane: Value = serde_json::from_slice(
                &layouts::read_private(&path)?.ok_or("Target observation missing")?,
            )
            .map_err(|_| "Invalid target JSON")?;
            let identity = terminals::identity(&pane)?;
            if !command.apply {
                if command.verb == "spawn" {
                    store.get(key)?;
                }
                json!({"operation":command.verb,"target":identity,"key":key,"apply":false,"notice":"Apply revalidates exact terminal generation. No terminal operation has run."})
            } else if command.verb == "save" {
                let request = SaveRequest {
                    name: command.options["--name"].clone(),
                    sid: command.options.get("--sid").cloned().unwrap_or_default(),
                    include_output: command.include_output,
                };
                match source.session(&config, Request::Save { pane, request })? {
                    Answer::Saved(row) => {
                        serde_json::to_value(row).map_err(|_| "Cannot encode checkpoint")?
                    }
                    _ => return Err("Unexpected checkpoint save response".into()),
                }
            } else {
                let checkpoint = store.get(key)?;
                let target = match source.session(
                    &config,
                    Request::Capabilities(command.options["--harness"].clone()),
                )? {
                    Answer::Capabilities(value) => value,
                    _ => return Err("Missing native capability evidence".into()),
                };
                let operation = match command.options["--operation"].as_str() {
                    "resume" => Operation::Resume,
                    "fork" => Operation::NativeFork,
                    _ => Operation::Handoff,
                };
                let placement = if command
                    .options
                    .get("--placement")
                    .is_some_and(|v| v == "window")
                {
                    Placement::Window
                } else {
                    Placement::Split
                };
                match source.session(
                    &config,
                    Request::Launch {
                        checkpoint: Box::new(checkpoint.checkpoint),
                        key: Some(checkpoint.key),
                        operation,
                        target,
                        parent: pane,
                        source: None,
                        placement,
                    },
                )? {
                    Answer::Receipt(receipt) => {
                        serde_json::to_value(receipt).map_err(|_| "Cannot encode launch receipt")?
                    }
                    _ => return Err("Unexpected checkpoint launch response".into()),
                }
            }
        }
        _ => return Err("Unsupported checkpoint operation".into()),
    };
    serde_json::to_writer_pretty(&mut *out, &value)
        .map_err(|_| "Cannot write checkpoint result")?;
    writeln!(out).map_err(|_| "Cannot write checkpoint result".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn parser_rejects_ambiguous_and_inapplicable_options() {
        assert!(
            parse(&args(&[
                "list",
                "--config-dir",
                crate::test_support::path("/tmp/example"),
                "--apply"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "save",
                "--config-dir",
                crate::test_support::path("/tmp/example"),
                "--target",
                "relative",
                "--name",
                "a"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "show",
                "--config-dir",
                crate::test_support::path("/tmp/example"),
                "--key",
                "one",
                "--key",
                "two"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "spawn",
                "--config-dir",
                crate::test_support::path("/tmp/example"),
                "--key",
                "k",
                "--target",
                crate::test_support::path("/tmp/private/pane"),
                "--operation",
                "shell",
                "--harness",
                "claude"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "save",
                "--config-dir",
                crate::test_support::path("/tmp/example"),
                "--target",
                crate::test_support::path("/tmp/private/pane"),
                "--name",
                "a",
                "--include-output"
            ]))
            .is_ok()
        );
    }
    struct Fake {
        calls: usize,
    }
    impl Source for Fake {
        fn refresh(&mut self, _: &ControlConfig) -> Result<super::super::backend::Observations> {
            panic!("CLI must not discover ambient terminals")
        }
        fn preview(&mut self, _: &Value, _: usize) -> Result<String> {
            panic!("CLI must not capture ambient terminals")
        }
        fn save_layouts(&mut self, _: &layouts::Layouts) -> Result<()> {
            panic!("CLI must not write layouts")
        }
        fn session(&mut self, _: &ControlConfig, request: Request) -> Result<Answer> {
            self.calls += 1;
            match request {
                Request::Save { pane, request } => {
                    assert_eq!(pane["terminal_id"], "generation");
                    assert_eq!(request.name, "fixture");
                    Err("fixture stale target refused".into())
                }
                _ => panic!("unexpected operation"),
            }
        }
    }
    #[test]
    fn preview_is_inert_and_apply_uses_exact_target_shared_operation() {
        let directory = std::env::temp_dir().join(format!(
            "daedalus-session-cli-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        super::super::setup::SetupPlan::fresh(
            &directory,
            ControlConfig::fresh(std::path::Path::new(crate::test_support::path("/tmp"))).unwrap(),
        )
        .unwrap()
        .apply()
        .unwrap();
        let target = directory.join("target.json");
        layouts::write_private(&target,&serde_json::to_vec(&json!({"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation"})).unwrap()).unwrap();
        let values = args(&[
            "save",
            "--config-dir",
            directory.to_str().unwrap(),
            "--target",
            target.to_str().unwrap(),
            "--name",
            "fixture",
        ]);
        let mut fake = Fake { calls: 0 };
        let mut out = vec![];
        run_with(parse(&values).unwrap().unwrap(), &mut fake, &mut out).unwrap();
        assert_eq!(fake.calls, 0);
        let mut applied = values;
        applied.push("--apply".into());
        assert!(
            run_with(parse(&applied).unwrap().unwrap(), &mut fake, &mut vec![])
                .unwrap_err()
                .contains("stale")
        );
        assert_eq!(fake.calls, 1);
        let checkpoint = crate::core::checkpoints::Checkpoint {
            name: "fixture".into(),
            harness: "claude".into(),
            sid: "sid".into(),
            cwd: crate::test_support::path("/tmp").into(),
            profile: String::new(),
            origin: "fixture".into(),
            created_at: String::new(),
            context_file: String::new(),
            source: None,
        };
        crate::core::checkpoints::Registry {
            checkpoints: vec![checkpoint],
        }
        .save(&directory)
        .unwrap();
        let store = SessionStore {
            directory: directory.clone(),
        };
        let key = store.list().unwrap()[0].key.clone();
        let values = args(&[
            "remove",
            "--config-dir",
            directory.to_str().unwrap(),
            "--key",
            &key,
        ]);
        run_with(parse(&values).unwrap().unwrap(), &mut fake, &mut vec![]).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        let mut applied = values;
        applied.push("--apply".into());
        run_with(parse(&applied).unwrap().unwrap(), &mut fake, &mut vec![]).unwrap();
        assert!(store.list().unwrap().is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
