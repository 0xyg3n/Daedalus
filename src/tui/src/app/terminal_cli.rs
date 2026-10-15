//! Exact-target local terminal operations. UI and CLI share execution adapters.
use super::{
    backend::{NativeSource, Source},
    config::ControlConfig,
    session_menu::{Answer, Request},
    workspace_actions::WorkspaceRequest,
};
use crate::{
    adapters::{
        terminal_actions::Action,
        terminals::{self, Terminals},
    },
    core::layouts,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write, path::PathBuf};
pub type Result<T> = std::result::Result<T, String>;
pub const HELP: &str = "daedalus terminal list --config-dir ABS\ndaedalus terminal capture --config-dir ABS --target ABS [--lines 80]\ndaedalus terminal send --config-dir ABS --target ABS --text TEXT [--apply]\ndaedalus terminal interrupt|close|focus|attach --config-dir ABS --target ABS [--apply]\ndaedalus terminal rename --config-dir ABS --target ABS --name NAME [--apply]\ndaedalus terminal launch --config-dir ABS --target ABS --harness ID [--placement split|window] [--name NAME] [--apply]\ndaedalus terminal wait --config-dir ABS --target ABS --registration ABS [--timeout-seconds 30]\nWait requires exact registered harness lifecycle evidence; unknown is never idle. Lifecycle observation may initialize private hook tables.\ndaedalus terminal command --config-dir ABS --target ABS --text /COMMAND [--apply]\ndaedalus terminal keys --config-dir ABS --target ABS --keys KEY_SEQUENCE [--apply]\ndaedalus terminal control --config-dir ABS --target ABS --registration ABS --command model|effort|plan [--arguments VALUE] [--apply --expect-sha PREVIEW_SHA]\nNative control previews verify the installed command contract; apply requires the unchanged review and idle lifecycle evidence.\nTargets are private JSON files containing exact inventory observations, never labels. Mutations preview by default. Attach uses the real terminal after --apply. Capture is read-only and may contain private terminal output.\n";
#[derive(Debug)]
pub struct Command {
    verb: String,
    directory: PathBuf,
    options: BTreeMap<String, String>,
    apply: bool,
}
pub fn parse(args: &[String]) -> Result<Option<Command>> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return Ok(None);
    }
    let verb = args[0].as_str();
    if ![
        "list",
        "capture",
        "send",
        "command",
        "control",
        "keys",
        "interrupt",
        "close",
        "focus",
        "attach",
        "rename",
        "launch",
        "wait",
    ]
    .contains(&verb)
    {
        return Err("Unknown terminal command; use terminal --help".into());
    }
    let mut options = BTreeMap::new();
    let mut apply = false;
    let mut i = 1;
    while i < args.len() {
        let key = &args[i];
        if key == "--apply" && !apply {
            apply = true;
            i += 1;
            continue;
        }
        if ![
            "--config-dir",
            "--target",
            "--text",
            "--keys",
            "--name",
            "--lines",
            "--harness",
            "--placement",
            "--registration",
            "--command",
            "--arguments",
            "--expect-sha",
            "--timeout-seconds",
        ]
        .contains(&key.as_str())
            || i + 1 >= args.len()
            || options.insert(key.clone(), args[i + 1].clone()).is_some()
        {
            return Err("Unknown, missing or repeated terminal option".into());
        }
        i += 2;
    }
    let directory = PathBuf::from(
        options
            .remove("--config-dir")
            .ok_or("Missing --config-dir")?,
    );
    layouts::check_path(&directory)?;
    let allowed: &[&str] = match verb {
        "list" => &[],
        "capture" => &["--target", "--lines"],
        "wait" => &["--target", "--registration", "--timeout-seconds"],
        "send" | "command" => &["--target", "--text"],
        "control" => &[
            "--target",
            "--registration",
            "--command",
            "--arguments",
            "--expect-sha",
        ],
        "keys" => &["--target", "--keys"],
        "rename" => &["--target", "--name"],
        "launch" => &["--target", "--harness", "--name", "--placement"],
        _ => &["--target"],
    };
    if options.keys().any(|key| !allowed.contains(&key.as_str()))
        || (verb != "list" && !options.contains_key("--target"))
        || (apply && ["list", "capture", "wait"].contains(&verb))
    {
        return Err("Options do not match terminal operation".into());
    }
    for (action, key) in [
        ("wait", "--registration"),
        ("control", "--registration"),
        ("control", "--command"),
        ("send", "--text"),
        ("command", "--text"),
        ("keys", "--keys"),
        ("rename", "--name"),
        ("launch", "--harness"),
    ] {
        if verb == action && !options.contains_key(key) {
            return Err("Required terminal option missing".into());
        }
    }
    if verb == "control" {
        crate::adapters::harness_commands::plan(
            "claude",
            crate::adapters::harness_commands::AUDITED_CLAUDE_VERSION,
            &options["--command"],
            options.get("--arguments").map(String::as_str).unwrap_or(""),
        )
        .map_err(|e| e.to_string())?;
        let approval = options.get("--expect-sha");
        if apply != approval.is_some()
            || approval.is_some_and(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("Native control apply requires --expect-sha from its preview".into());
        }
    }
    if let Some(path) = options.get("--registration") {
        layouts::check_path(std::path::Path::new(path))?;
    }
    if options
        .get("--timeout-seconds")
        .is_some_and(|v| v.parse::<usize>().map_or(true, |n| !(1..=300).contains(&n)))
    {
        return Err("Wait timeout must be between 1 and 300 seconds".into());
    }
    if let Some(path) = options.get("--target") {
        layouts::check_path(std::path::Path::new(path))?;
    }
    if options
        .get("--text")
        .is_some_and(|text| text.trim().is_empty() || text.len() > 64 * 1024 || text.contains('\0'))
    {
        return Err("Text must contain 1 to 65536 bytes without NUL".into());
    }
    if verb == "command"
        && options
            .get("--text")
            .is_some_and(|text| !text.starts_with('/') || text.contains(['\n', '\r']))
    {
        return Err("Harness command must be a single literal slash command".into());
    }
    if let Some(keys) = options.get("--keys") {
        let parts: Vec<_> = keys.split_whitespace().collect();
        if parts.is_empty()
            || parts.len() > 64
            || parts.iter().any(|key| {
                key.len() > 40
                    || key.starts_with('-')
                    || !key
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-+_".contains(&b))
            })
        {
            return Err("Keys require 1 to 64 literal named keys".into());
        }
    }
    if options.get("--name").is_some_and(|text| {
        text.trim().is_empty() || text.len() > 80 || text.chars().any(char::is_control)
    }) {
        return Err("Name must contain 1 to 80 printable bytes".into());
    }
    if options.get("--lines").is_some_and(|value| {
        value
            .parse::<usize>()
            .map_or(true, |n| !(1..=2000).contains(&n))
    }) {
        return Err("Capture lines must be between 1 and 2000".into());
    }
    if options
        .get("--placement")
        .is_some_and(|value| !["split", "window"].contains(&value.as_str()))
    {
        return Err("Placement must be split or window".into());
    }
    if options
        .get("--harness")
        .is_some_and(|id| id != "shell" && crate::adapters::harnesses::tool(id).is_none())
    {
        return Err("Unknown harness adapter".into());
    }
    Ok(Some(Command {
        verb: verb.into(),
        directory,
        options,
        apply,
    }))
}
pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    let Some(command) = parse(args)? else {
        return write!(out, "{HELP}").map_err(|_| "Cannot write terminal help".into());
    };
    let mut source = NativeSource {
        directory: command.directory.clone(),
        terminals: Terminals::default(),
        last_snapshot: json!({}),
    };
    run_with(command, &mut source, out)
}
pub fn run_with(command: Command, source: &mut impl Source, out: &mut impl Write) -> Result<()> {
    let config = ControlConfig::parse(
        &layouts::read_private(&command.directory.join("control.json"))?
            .ok_or("Configuration missing")?,
    )?;
    let value = if command.verb == "list" {
        source.refresh(&config)?.snapshot
    } else {
        let path = PathBuf::from(&command.options["--target"]);
        let pane: Value = serde_json::from_slice(
            &layouts::read_private(&path)?.ok_or("Target observation missing")?,
        )
        .map_err(|_| "Invalid target JSON")?;
        let identity = terminals::identity(&pane)?;
        if command.verb == "control" {
            let terminals = Terminals::default();
            let environment = crate::platform::environment();
            let request = super::harness_control::Request {
                directory: command.directory.clone(),
                registration: command.options["--registration"].clone().into(),
                target: pane,
                command: command.options["--command"].clone(),
                arguments: command
                    .options
                    .get("--arguments")
                    .cloned()
                    .unwrap_or_default(),
            };
            let plan = super::harness_control::prepare(request, &config, &terminals, &environment)?;
            if command.apply {
                let message = super::control_audit::perform(
                    &command.directory,
                    super::control_audit::Action::Terminal,
                    || {
                        super::harness_control::apply(
                            plan,
                            &command.options["--expect-sha"],
                            &config,
                            &terminals,
                            &environment,
                        )
                    },
                    |_| super::control_audit::State::Submitted,
                )?;
                json!({"message":message,"state":"submitted"})
            } else {
                plan.review()
            }
        } else if command.verb == "wait" {
            let registration = PathBuf::from(&command.options["--registration"]);
            let seconds = command
                .options
                .get("--timeout-seconds")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(30);
            let adapter = Terminals::default();
            let started = std::time::Instant::now();
            wait_with_deadline(
                seconds * 2,
                || {
                    let registered = super::hook_runtime::inspect(&registration)
                        .map_err(|_| "Hook registration is unavailable")?;
                    if registered.directory != command.directory
                        || registered.scope.terminal != identity
                    {
                        return Err(
                            "Registration does not match this exact target and instance".into()
                        );
                    }
                    let session = &pane["agent_session"];
                    let session_id = if session["kind"] == "id" {
                        session["value"].as_str()
                    } else {
                        session.as_str()
                    };
                    if session_id
                        .filter(|sid| !sid.is_empty())
                        .is_some_and(|sid| sid != registered.scope.conversation)
                    {
                        return Err(
                            "Registered conversation does not match target observation".into()
                        );
                    }
                    super::hook_runtime::verify_live_session(&registered, &adapter)
                        .map_err(|_| "Registered harness process or target changed")?;
                    let ready =
                        super::hook_runtime::ready_snapshot(&registration, &adapter.proc_root)
                            .map_err(|_| "Verified lifecycle observation is unavailable")?;
                    if ready.scope != registered.scope {
                        return Err("Registration changed while observing lifecycle".into());
                    }
                    Ok(ready.state)
                },
                || std::thread::sleep(std::time::Duration::from_millis(500)),
                || started.elapsed() >= std::time::Duration::from_secs(seconds as u64),
            )?
        } else if command.verb == "capture" {
            let lines = command
                .options
                .get("--lines")
                .and_then(|v| v.parse().ok())
                .unwrap_or(80);
            json!({"target":identity,"output":source.preview(&pane,lines)?})
        } else if !command.apply {
            json!({"operation":command.verb,"target":identity,"apply":false,"notice":"Exact terminal generation will be verified on apply. No terminal operation has run."})
        } else {
            if command.verb == "attach" {
                source.refresh(&config)?;
            }
            let request = match command.verb.as_str() {
                "attach" => Request::Native(pane),
                "launch" => Request::Workspace(Box::new(WorkspaceRequest::Organize {
                    pane,
                    operation: if command
                        .options
                        .get("--placement")
                        .is_some_and(|p| p == "window")
                    {
                        "new-group"
                    } else {
                        "split"
                    }
                    .into(),
                    label: command
                        .options
                        .get("--name")
                        .cloned()
                        .unwrap_or_else(|| command.options["--harness"].clone()),
                    tool: command.options["--harness"].clone(),
                })),
                operation => Request::Terminal {
                    pane,
                    action: match operation {
                        "send" | "command" => Action::Send(command.options["--text"].clone()),
                        "keys" => Action::Keys(
                            command.options["--keys"]
                                .split_whitespace()
                                .map(str::to_owned)
                                .collect(),
                        ),
                        "interrupt" => Action::Interrupt,
                        "close" => Action::Close,
                        "focus" => Action::Focus,
                        "rename" => Action::Rename(command.options["--name"].clone()),
                        _ => return Err("Unsupported terminal mutation".into()),
                    },
                },
            };
            match source.session(&config, request)? {
                Answer::Receipt(receipt) => {
                    serde_json::to_value(receipt).map_err(|_| "Cannot encode terminal receipt")?
                }
                Answer::Text(text) => json!({"message":text}),
                _ => return Err("Unexpected terminal response".into()),
            }
        }
    };
    serde_json::to_writer_pretty(&mut *out, &value).map_err(|_| "Cannot write terminal result")?;
    writeln!(out).map_err(|_| "Cannot write terminal result".into())
}
pub fn wait_with(
    samples: usize,
    observe: impl FnMut() -> Result<super::hook_runtime::Readiness>,
    pause: impl FnMut(),
) -> Result<Value> {
    wait_with_deadline(samples, observe, pause, || false)
}
fn wait_with_deadline(
    samples: usize,
    mut observe: impl FnMut() -> Result<super::hook_runtime::Readiness>,
    mut pause: impl FnMut(),
    mut expired: impl FnMut() -> bool,
) -> Result<Value> {
    use super::hook_runtime::Readiness;
    if !(1..=600).contains(&samples) {
        return Err("Invalid wait bound".into());
    }
    let mut last = Readiness::Unknown;
    for index in 0..samples {
        if expired() {
            break;
        }
        last = observe()?;
        if expired() {
            break;
        }
        if last == Readiness::Idle {
            return Ok(json!({"state":"idle","evidence":"verified registered lifecycle"}));
        }
        if index + 1 < samples {
            pause();
        }
    }
    Ok(
        json!({"state":"timeout","last_observed":last,"notice":"No verified idle observation arrived before the deadline."}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn parse_is_exact_and_operations_cannot_share_wrong_flags() {
        assert!(
            parse(&args(&[
                "send",
                "--config-dir",
                crate::test_support::path("/tmp/fixture"),
                "--target",
                crate::test_support::path("/tmp/pane")
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "capture",
                "--config-dir",
                crate::test_support::path("/tmp/fixture"),
                "--target",
                crate::test_support::path("/tmp/pane"),
                "--lines",
                "2001"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "launch",
                "--config-dir",
                crate::test_support::path("/tmp/fixture"),
                "--target",
                crate::test_support::path("/tmp/pane"),
                "--harness",
                "unknown"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "close",
                "--config-dir",
                crate::test_support::path("/tmp/fixture"),
                "--target",
                crate::test_support::path("/tmp/pane"),
                "--text",
                "no"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "attach",
                "--config-dir",
                crate::test_support::path("/tmp/fixture"),
                "--target",
                "relative"
            ]))
            .is_err()
        );
    }
    #[test]
    fn native_control_requires_exact_registration_and_preview_digest() {
        let base = args(&[
            "control",
            "--config-dir",
            crate::test_support::path("/tmp/fixture"),
            "--target",
            crate::test_support::path("/tmp/pane"),
            "--registration",
            crate::test_support::path("/tmp/hook"),
            "--command",
            "model",
            "--arguments",
            "test-model",
        ]);
        assert!(parse(&base).is_ok());
        let mut apply = base.clone();
        apply.push("--apply".into());
        assert!(parse(&apply).is_err());
        apply.extend(["--expect-sha".into(), "a".repeat(64)]);
        assert!(parse(&apply).is_ok());
        let mut unsupported = base;
        unsupported[8] = "mode".into();
        assert!(parse(&unsupported).is_err());
    }
    #[test]
    fn slow_idle_probe_after_deadline_reports_timeout_with_last_evidence() {
        let mut clock_calls = 0;
        let value = wait_with_deadline(
            2,
            || Ok(super::super::hook_runtime::Readiness::Idle),
            || panic!("deadline passed"),
            || {
                clock_calls += 1;
                clock_calls > 1
            },
        )
        .unwrap();
        assert_eq!(value["state"], "timeout");
        assert_eq!(value["last_observed"], "idle");
    }
    #[test]
    fn wait_never_infers_idle_from_unknown_or_retries_identity_errors() {
        use super::super::hook_runtime::Readiness;
        let mut pauses = 0;
        let value = wait_with(2, || Ok(Readiness::Unknown), || pauses += 1).unwrap();
        assert_eq!(value["state"], "timeout");
        assert_eq!(value["last_observed"], "unknown");
        assert_eq!(pauses, 1);
        let mut calls = 0;
        let value = wait_with(
            3,
            || {
                calls += 1;
                Ok(if calls == 2 {
                    Readiness::Idle
                } else {
                    Readiness::Busy
                })
            },
            || {},
        )
        .unwrap();
        assert_eq!(value["state"], "idle");
        assert_eq!(calls, 2);
        assert!(
            wait_with(
                3,
                || Err("target changed".into()),
                || panic!("must not retry identity mismatch")
            )
            .is_err()
        );
    }
    #[derive(Default)]
    struct Fake {
        effects: usize,
        captures: usize,
        refreshes: usize,
    }
    impl Source for Fake {
        fn refresh(&mut self, _: &ControlConfig) -> Result<super::super::backend::Observations> {
            self.refreshes += 1;
            Ok(Default::default())
        }
        fn preview(&mut self, pane: &Value, rows: usize) -> Result<String> {
            assert_eq!(pane["terminal_id"], "generation");
            assert_eq!(rows, 80);
            self.captures += 1;
            Ok("fixture output".into())
        }
        fn save_layouts(&mut self, _: &layouts::Layouts) -> Result<()> {
            panic!("unexpected layouts write")
        }
        fn session(&mut self, _: &ControlConfig, request: Request) -> Result<Answer> {
            self.effects += 1;
            match request {
                Request::Terminal {
                    pane,
                    action: Action::Send(text),
                } => {
                    assert_eq!(pane["terminal_id"], "generation");
                    assert_eq!(text, "literal $(no shell)");
                }
                Request::Workspace(request) => {
                    assert!(
                        matches!(*request,WorkspaceRequest::Organize{operation,tool,..} if operation=="split" && tool=="claude")
                    );
                }
                Request::Native(pane) => {
                    assert_eq!(pane["pane_id"], "pane");
                    assert_eq!(self.refreshes, 1);
                }
                _ => panic!("unexpected mutation"),
            }
            Err("fixture exact target stale".into())
        }
    }
    #[test]
    fn terminal_preview_never_mutates_and_apply_uses_shared_exact_operations() {
        let directory = std::env::temp_dir().join(format!(
            "daedalus-terminal-cli-{}-{}",
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
        let target = directory.join("pane.json");
        layouts::write_private(&target,&serde_json::to_vec(&json!({"backend":"herdr","session":"fixture","pane_id":"pane","terminal_id":"generation"})).unwrap()).unwrap();
        let mut fake = Fake::default();
        for (verb, extras) in [
            ("send", vec!["--text", "literal $(no shell)"]),
            ("launch", vec!["--harness", "claude"]),
            ("attach", vec![]),
        ] {
            let mut request = args(&[
                verb,
                "--config-dir",
                directory.to_str().unwrap(),
                "--target",
                target.to_str().unwrap(),
            ]);
            request.extend(args(&extras));
            let before = fake.effects;
            run_with(parse(&request).unwrap().unwrap(), &mut fake, &mut vec![]).unwrap();
            assert_eq!(fake.effects, before);
            request.push("--apply".into());
            assert!(
                run_with(parse(&request).unwrap().unwrap(), &mut fake, &mut vec![])
                    .unwrap_err()
                    .contains("stale")
            );
            assert_eq!(fake.effects, before + 1);
        }
        let capture = args(&[
            "capture",
            "--config-dir",
            directory.to_str().unwrap(),
            "--target",
            target.to_str().unwrap(),
        ]);
        run_with(parse(&capture).unwrap().unwrap(), &mut fake, &mut vec![]).unwrap();
        assert_eq!(fake.captures, 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
