//! Native CLI aliases sharing exact-target application operations.
use std::io::Write;
type Result<T> = std::result::Result<T, String>;
#[derive(Debug, PartialEq)]
pub enum Family {
    Terminal,
    Checkpoint,
    Route,
    Diagnostics,
}
pub fn translate(args: &[String]) -> Result<(Family, Vec<String>)> {
    let verb = args.first().ok_or("Use job --help")?.as_str();
    if ["model", "effort", "plan"].contains(&verb) {
        let mut native = vec!["control".into(), "--command".into(), verb.into()];
        native.extend_from_slice(&args[1..]);
        return Ok((Family::Terminal, native));
    }
    if ["resume", "fork", "handoff"].contains(&verb) {
        // Keep every native checkpoint identity/profile requirement. Never revive
        // a legacy display-name target or guess the latest conversation.
        if args[1..].iter().any(|value| value == "--operation") {
            return Err("The job command fixes its checkpoint operation; omit --operation".into());
        }
        let mut native = vec!["spawn".into(), "--operation".into(), verb.into()];
        native.extend_from_slice(&args[1..]);
        return Ok((Family::Checkpoint, native));
    }
    let (family,native)=match verb {
        "list"|"seats"|"snapshot"|"status"=>(Family::Terminal,"list"),
        "new"=>(Family::Terminal,"launch"),
        "send"|"delegate"=>(Family::Terminal,"send"),"cmd"=>(Family::Terminal,"command"),
        "capture"=>(Family::Terminal,"capture"),"wait"=>(Family::Terminal,"wait"),
        "stop"=>(Family::Terminal,"interrupt"),"kill"=>(Family::Terminal,"close"),
        "keys"=>(Family::Terminal,"keys"),"attach"=>(Family::Terminal,"attach"),
        "use"=>(Family::Route,"select"),"active"=>(Family::Route,"active"),"sessions"=>(Family::Route,"list"),
        "checkpoint"|"save"=>(Family::Checkpoint,"save"),"checkpoints"=>(Family::Checkpoint,"list"),
        "health"=>(Family::Diagnostics,""),
        "mode"=>return Err("This harness control has no verified native capability adapter. Use its own interactive controls; no keys were sent.".into()),
        "reconcile"=>return Err("Legacy job-registry reconciliation is retired. Native inventory observes live terminals and rejects stale generations; use job list or route candidates.".into()),
        "register"=>return Err("Legacy worker registration is retired. Register exact harness lifecycle identity using hook profile-preview and hook profile-apply; no display-name worker registry is created.".into()),
        "shard"|"gc"|"gc-plan"=>return Err("Automatic deployment sharding and terminal garbage collection are retired. Use reviewed checkpoint fork/handoff and exact-target terminal close operations.".into()),
        "failover"|"recover"=>return Err("Legacy provider recovery is disabled. Use an explicitly configured harness profile and verified checkpoint operation.".into()),
        _=>return Err("Unknown job command; use job --help".into()),
    };
    let mut result = Vec::new();
    if !native.is_empty() {
        result.push(native.into());
    }
    result.extend_from_slice(&args[1..]);
    if family == Family::Diagnostics && !result.iter().any(|v| v == "--json") {
        result.push("--json".into());
    }
    Ok((family, result))
}
pub fn run_job(args: &[String], out: &mut impl Write) -> Result<()> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return writeln!(out,"Native job commands: list, seats, snapshot, new, send, cmd, capture, wait, stop, kill, keys, attach, use, active, sessions, checkpoint, save, checkpoints, resume, fork, handoff, delegate, status, health, model, effort, plan.\nAll require --config-dir ABS. Terminal actions require --target ABS with exact observation JSON; mutations preview unless --apply.\nmodel/effort/plan use terminal control with --registration, optional --arguments, and --apply --expect-sha from preview.\nnew uses terminal launch options; wait requires --registration ABS. use uses route select options and an explicit review digest.\nsave stores a named checkpoint; resume/fork/handoff require --key, --target and --harness. delegate sends --text to the explicit --target, without an implicit orchestrator. status lists live inventory.\nSee terminal --help, checkpoint --help and route --help. Legacy positional display-name targets are not resolved.").map_err(|_|"Cannot write job help".into());
    }
    let (family, args) = translate(args)?;
    match family {
        Family::Terminal => super::terminal_cli::run(&args, out),
        Family::Checkpoint => super::session_cli::run(&args, out),
        Family::Route => super::route_cli::run(&args, out),
        Family::Diagnostics => super::diagnostics::run(&args, out),
    }
}
pub fn run_attach(args: &[String], out: &mut impl Write) -> Result<()> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return writeln!(out,"daedalus tma --config-dir ABS --target ABS [--apply]\nNative project attachment uses an exact terminal observation file. A reviewed --apply opens an independent native tmux/Herdr view; the source terminal keeps running.").map_err(|_|"Cannot write attach help".into());
    }
    let mut options = vec!["attach".into()];
    options.extend_from_slice(args);
    super::terminal_cli::run(&options, out)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn model_effort_and_plan_share_verified_terminal_control() {
        for command in ["model", "effort", "plan"] {
            let (family, options) = translate(&args(&[
                command,
                "--target",
                "/tmp/exact-pane",
                "--registration",
                "/tmp/exact-hook",
            ]))
            .unwrap();
            assert_eq!(family, Family::Terminal);
            assert_eq!(&options[..3], &["control", "--command", command]);
            assert_eq!(
                &options[3..],
                &[
                    "--target",
                    "/tmp/exact-pane",
                    "--registration",
                    "/tmp/exact-hook"
                ]
            );
        }
        assert!(translate(&args(&["mode"])).is_err());
    }

    #[test]
    fn checkpoint_aliases_require_native_identity_and_never_guess_resume() {
        for operation in ["resume", "fork", "handoff"] {
            let (family, options) = translate(&args(&[
                operation,
                "--config-dir",
                "/tmp/control",
                "--key",
                "checkpoint:key",
                "--target",
                "/tmp/exact-pane",
                "--harness",
                "claude",
            ]))
            .unwrap();
            assert_eq!(family, Family::Checkpoint);
            assert_eq!(&options[..3], &["spawn", "--operation", operation]);
            assert!(
                super::super::session_cli::parse(&options)
                    .unwrap()
                    .is_some()
            );
            assert!(
                super::super::session_cli::parse(
                    &translate(&args(&[operation, "--config-dir", "/tmp/control"]))
                        .unwrap()
                        .1
                )
                .is_err()
            );
            assert!(translate(&args(&[operation, "--operation", "resume"])).is_err());
        }
        assert_eq!(
            translate(&args(&["save"])).unwrap(),
            (Family::Checkpoint, args(&["save"]))
        );
        assert_eq!(
            translate(&args(&["delegate", "--text", "literal request"])).unwrap(),
            (
                Family::Terminal,
                args(&["send", "--text", "literal request"])
            )
        );
        assert_eq!(
            translate(&args(&["status"])).unwrap(),
            (Family::Terminal, args(&["list"]))
        );
    }
    #[test]
    fn deployment_automation_is_retired_instead_of_promising_pending_code() {
        for command in ["reconcile", "register", "shard", "gc", "gc-plan"] {
            let error = translate(&args(&[command])).unwrap_err();
            assert!(error.contains("retired"), "{command}: {error}");
            assert!(!error.contains("migration"));
        }
    }
    #[test]
    fn aliases_preserve_literal_arguments_and_never_invent_targets() {
        assert_eq!(
            translate(&args(&[
                "send",
                "--text",
                "$(literal)",
                "--target",
                "/tmp/pane"
            ]))
            .unwrap(),
            (
                Family::Terminal,
                args(&["send", "--text", "$(literal)", "--target", "/tmp/pane"])
            )
        );
        assert_eq!(
            translate(&args(&["stop", "--target", "/tmp/pane"]))
                .unwrap()
                .1[0],
            "interrupt"
        );
        assert_eq!(
            translate(&args(&["use", "--chat", "42"])).unwrap(),
            (Family::Route, args(&["select", "--chat", "42"]))
        );
        assert!(translate(&args(&["mode", "--target", "/tmp/pane"])).is_err());
        assert!(
            translate(&args(&["reconcile"]))
                .unwrap_err()
                .contains("retired")
        );
    }
}
