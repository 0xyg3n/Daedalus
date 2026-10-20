//! Local hook/goal CLI. Hook stdout is reserved for the harness response.
use super::{hook_install, hook_runtime, hooks};
use crate::core::{goal_store::Control, layouts};
use crate::{
    adapters::terminals::Terminals,
    core::identity::{Backend, TerminalId},
};
use serde_json::json;
use std::collections::BTreeMap;
use std::{
    io::{Read, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;

fn launch_plan_cli(args: &[String], writer: &mut impl Write) -> Result<()> {
    let mut source = None;
    let mut expected = None;
    let mut fields = args[1..].iter();
    while let Some(key) = fields.next() {
        let value = fields.next().ok_or("Launch option requires a value")?;
        match key.as_str() {
            "--from" if source.is_none() => source = Some(PathBuf::from(value)),
            "--expect-sha" if expected.is_none() => expected = Some(value.as_str()),
            _ => return Err("Unknown or duplicate launch option".into()),
        }
    }
    let apply = args[0] == "launch-apply";
    if apply != expected.is_some() {
        return Err("Only launch-apply requires --expect-sha from preview".into());
    }
    let path = source.ok_or("Missing --from private launch specification")?;
    let bytes = layouts::read_private(&path)?.ok_or("Launch specification is missing")?;
    let spec: super::hook_launch::Spec =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid launch specification")?;
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Invalid clock")?
            .as_millis(),
    )
    .map_err(|_| "Invalid clock")?;
    let plan = super::hook_launch::prepare(spec, now)?;
    let fingerprint = plan.fingerprint()?;
    if let Some(approval) = expected {
        let published = super::hook_launch::save(&plan, approval, now)?;
        write_json(
            writer,
            &json!({"launch_plan":published,"activated":false,"next":"Run daedalus hook launch --from with this plan in the reviewed terminal"}),
        )
    } else {
        write_json(
            writer,
            &json!({"fingerprint":fingerprint,"activated":false,"review":{"scope":plan.spec.registration.scope,
            "native_binary":plan.spec.native_binary,"entrypoint":plan.spec.entrypoint,"working_directory":plan.spec.working_directory,
            "created_hook_file":plan.native_hook_file(),"events":plan.spec.registration.events,"options":plan.spec.registration.options,"expires_ms":plan.spec.expires_ms,"arguments":plan.command()},"launch_plan":plan.path()}),
        )
    }
}

pub fn run(
    family: &str,
    args: &[String],
    reader: impl Read,
    writer: &mut impl Write,
) -> Result<()> {
    if args == ["--help"] || args == ["-h"] {
        let usage = if family == "hook" {
            "Usage: daedalus hook launch-cleanup-preview --from ABS_REVIEWED_PLAN\n       daedalus hook launch-cleanup --from ABS_REVIEWED_PLAN --expect-sha REVIEW_SHA\n       daedalus hook launch-preview --from ABS_PRIVATE_SPEC\n       daedalus hook launch-apply --from ABS_PRIVATE_SPEC --expect-sha REVIEW_SHA\n       daedalus hook launch --from ABS_REVIEWED_PLAN\n       daedalus hook run --registration ABS [--event-id ID] [--event-kind EVENT]\n       daedalus hook install --from ABS\n       daedalus hook inspect --registration ABS\n       daedalus hook profile-preview|profile-apply --harness NAME --instance ID --backend native|tmux|herdr --endpoint ID --pane ID --generation ID --conversation ID --pid PID --process-start TICKS --directory ABS --profile ABS --binary ABS [--transcript ABS] [--server-pid PID --server-start TICKS --server-binary ABS] --registration-id ID --version VERSION [--telegram true] [--progress true] [--hindsight true] [--notifications true] [--goal-id ID] [--expect-sha SHA]\n       daedalus hook profile-rollback --from ABS_RECEIPT\n       daedalus hook profile-remove-preview --from ABS_RECEIPT\n       daedalus hook profile-remove --from ABS_RECEIPT --approve DIGEST\n       daedalus hook profile-remove-rollback --from ABS_REMOVAL_RECEIPT\nProfile apply requires --expect-sha from a preview and writes a private rollback receipt. Run reads one bounded hook JSON payload from stdin. PreToolUse wrappers must specify --event-kind PreToolUse for blocking infrastructure failures."
        } else {
            "Usage: daedalus goal arm|pause|resume|complete|status --registration ABS\nArm reads the goal directive from stdin. Arming waits for a bound hook; it does not launch an agent."
        };
        return writeln!(writer, "{usage}").map_err(|_| "Cannot write help".into());
    }
    let command = args.first().ok_or("Missing subcommand; use --help")?;
    if family == "hook"
        && matches!(
            command.as_str(),
            "launch-cleanup-preview" | "launch-cleanup"
        )
    {
        let mut source = None;
        let mut approval = None;
        let mut fields = args[1..].iter();
        while let Some(key) = fields.next() {
            let value = fields.next().ok_or("Cleanup option requires a value")?;
            match key.as_str() {
                "--from" if source.is_none() => source = Some(PathBuf::from(value)),
                "--expect-sha" if approval.is_none() => approval = Some(value.as_str()),
                _ => return Err("Unknown or duplicate cleanup option".into()),
            }
        }
        let path = source.ok_or("Missing --from reviewed launch plan")?;
        if (command == "launch-cleanup") != approval.is_some() {
            return Err("Cleanup apply requires --expect-sha; preview takes no approval".into());
        }
        let proc_root = std::path::Path::new("/proc");
        return if let Some(sha) = approval {
            let removed = super::hook_launch::cleanup_apply(&path, sha, proc_root)?;
            write_json(
                writer,
                &json!({"removed_hook_file":removed,"session_records_preserved":true}),
            )
        } else {
            let review = super::hook_launch::cleanup_preview(&path, proc_root)?;
            write_json(
                writer,
                &json!({"fingerprint":review.fingerprint()?,"review":review,"removed":false}),
            )
        };
    }
    if family == "hook" && matches!(command.as_str(), "launch-preview" | "launch-apply") {
        return launch_plan_cli(args, writer);
    }
    if family == "hook" && command.starts_with("profile-") {
        return profile(args, writer, &Terminals::default());
    }
    let mut registration = None;
    let mut source = None;
    let mut event_id = None;
    let mut event_kind = None;
    let mut options = args[1..].iter();
    while let Some(option) = options.next() {
        let value = options.next().ok_or("Option requires a value")?;
        match option.as_str() {
            "--registration" if registration.is_none() => registration = Some(PathBuf::from(value)),
            "--from" if source.is_none() => source = Some(PathBuf::from(value)),
            "--event-id" if event_id.is_none() => event_id = Some(value.as_str()),
            "--event-kind" if event_kind.is_none() => {
                event_kind = Some(
                    serde_json::from_value::<hooks::EventKind>(json!(value))
                        .map_err(|_| "Invalid event kind")?,
                )
            }
            _ => return Err("Unknown or duplicate option".into()),
        }
    }
    if family == "hook" && command == "install" {
        if registration.is_some() || event_id.is_some() || event_kind.is_some() {
            return Err("Install accepts only --from".into());
        }
        let source = source.ok_or("Missing --from")?;
        let bytes = layouts::read_private(&source)?.ok_or("Registration source does not exist")?;
        let registration: hook_runtime::Registration =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid registration JSON")?;
        let path = hook_runtime::install(&registration).map_err(|e| e.to_string())?;
        return write_json(writer, &json!({"registration":path,"activated":false}));
    }
    if family == "hook" && command == "await-launch" {
        if registration.is_some() || event_id.is_some() || event_kind.is_some() {
            return Err("Await launch accepts only --from".into());
        }
        return super::pane_launch::await_launch(&source.ok_or("Missing launch ticket")?);
    }
    if family == "hook" && command == "launch" {
        if registration.is_some() || event_id.is_some() || event_kind.is_some() {
            return Err("Launch accepts only --from".into());
        }
        let source = source.ok_or("Missing --from")?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Invalid system clock")?
            .as_millis();
        return super::hook_launch::launch(
            &source,
            u64::try_from(now).map_err(|_| "Invalid system clock")?,
        );
    }
    if source.is_some() {
        return Err("--from is only valid for hook install or launch".into());
    }
    let path = registration.ok_or("Missing --registration")?;
    layouts::check_path(&path)?;
    if family == "hook" && command == "run" {
        let result = hook_runtime::run_expected(reader, writer, &path, event_id, event_kind);
        // Hook stdout is the provider protocol. Keep a private, payload-free
        // last-result record so the UI can explain a nonzero native exit.
        if !result
            .as_ref()
            .is_ok_and(|outcome| outcome.event_id == "unbound-codex-session")
        {
            let _ = hook_runtime::record_run_result(&path, event_kind, result.as_ref().err());
        }
        result.map_err(|e| e.to_string())?;
        return Ok(());
    }
    if event_id.is_some() || event_kind.is_some() {
        return Err("Event options are only valid for hook run".into());
    }
    if family == "hook" && command == "inspect" {
        return write_json(
            writer,
            &serde_json::to_value(hook_runtime::inspect(&path).map_err(|e| e.to_string())?)
                .map_err(|_| "Cannot encode registration")?,
        );
    }
    if family != "goal" {
        return Err("Unknown hook subcommand".into());
    }
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Invalid system clock")?
            .as_millis(),
    )
    .map_err(|_| "Invalid system clock")?;
    match command.as_str() {
        "arm" => {
            let bytes =
                hooks::read_payload(reader).map_err(|_| "Invalid or oversized goal directive")?;
            let directive =
                std::str::from_utf8(&bytes).map_err(|_| "Goal directive must be UTF-8")?;
            hook_runtime::arm(&path, directive, now).map_err(|e| e.to_string())?;
        }
        "pause" | "resume" | "complete" => {
            let action = match command.as_str() {
                "pause" => Control::Pause,
                "resume" => Control::Resume,
                _ => Control::Complete,
            };
            hook_runtime::control(&path, action, now).map_err(|e| e.to_string())?;
        }
        "status" => {}
        _ => return Err("Unknown goal subcommand".into()),
    }
    write_json(
        writer,
        &serde_json::to_value(hook_runtime::status(&path, now).map_err(|e| e.to_string())?)
            .map_err(|_| "Cannot encode goal status")?,
    )
}
/// Profile commands use typed flags so users never construct registration JSON.
/// GUI callers can invoke hook_install::preview/apply with the same Request.
pub(crate) fn profile(
    args: &[String],
    writer: &mut impl Write,
    terminals: &Terminals,
) -> Result<()> {
    let mut fields = BTreeMap::new();
    let mut iter = args[1..].iter();
    while let Some(key) = iter.next() {
        let value = iter.next().ok_or("Profile option requires a value")?;
        if fields.insert(key.as_str(), value.as_str()).is_some() {
            return Err("Duplicate profile option".into());
        }
    }
    if matches!(
        args[0].as_str(),
        "profile-remove-preview" | "profile-remove" | "profile-remove-rollback"
    ) {
        let path = PathBuf::from(fields.remove("--from").ok_or("Missing --from")?);
        if args[0] == "profile-remove-rollback" {
            if !fields.is_empty() {
                return Err("Removal rollback accepts only --from".into());
            }
            super::hook_uninstall::rollback(&path)?;
            return write_json(writer, &json!({"restored":true}));
        }
        let approval = fields.remove("--approve");
        if !fields.is_empty() || (args[0] == "profile-remove") != approval.is_some() {
            return Err(
                "Removal requires --approve from preview; preview accepts only --from".into(),
            );
        }
        let directory = path
            .parent()
            .and_then(|p| p.parent())
            .ok_or("Invalid managed receipt path")?;
        let plan = super::hook_uninstall::Plan::prepare(directory, &path)?;
        if let Some(approval) = approval {
            let receipt = plan.apply(approval)?;
            return write_json(
                writer,
                &json!({"removed":true,"receipt":receipt,"restart_or_reload_required":true}),
            );
        }
        return write_json(writer, &plan.review());
    }
    if args[0] == "profile-rollback" {
        if fields.len() != 1 {
            return Err("Rollback accepts only --from".into());
        }
        let path = PathBuf::from(fields.remove("--from").ok_or("Missing --from")?);
        let bytes = layouts::read_private(&path)?.ok_or("Rollback receipt does not exist")?;
        let receipt: hook_install::Receipt =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid rollback receipt")?;
        hook_install::rollback(&receipt)?;
        return write_json(
            writer,
            &json!({"rolled_back":true,"profile":receipt.profile}),
        );
    }
    if !matches!(args[0].as_str(), "profile-preview" | "profile-apply") {
        return Err("Unknown profile command".into());
    }
    let expected = fields.remove("--expect-sha").map(str::to_owned);
    if (args[0] == "profile-apply") != expected.is_some() {
        return Err("Apply requires --expect-sha; preview does not accept it".into());
    }
    fn take(fields: &mut BTreeMap<&str, &str>, key: &str) -> Result<String> {
        fields
            .remove(key)
            .map(str::to_owned)
            .ok_or_else(|| format!("Missing {key}"))
    }
    fn flag(fields: &mut BTreeMap<&str, &str>, key: &str) -> Result<bool> {
        match fields.remove(key) {
            None | Some("false") => Ok(false),
            Some("true") => Ok(true),
            _ => Err("Boolean profile options require true or false".into()),
        }
    }
    let harness = serde_json::from_value::<hooks::Harness>(json!(take(&mut fields, "--harness")?))
        .map_err(|_| "Unsupported native hook harness")?;
    let backend = serde_json::from_value::<Backend>(json!(take(&mut fields, "--backend")?))
        .map_err(|_| "Invalid terminal backend")?;
    let server_emitter = if let Some(pid) = fields.remove("--server-pid") {
        Some(
            hook_runtime::capture_server_emitter(
                hook_runtime::ProcessIdentity {
                    pid: pid.parse().map_err(|_| "Invalid server PID")?,
                    start_ticks: take(&mut fields, "--server-start")?
                        .parse()
                        .map_err(|_| "Invalid server generation")?,
                },
                &PathBuf::from(take(&mut fields, "--server-binary")?),
                &terminals.proc_root,
            )
            .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let request = hook_install::Request {
        harness,
        instance: take(&mut fields, "--instance")?,
        terminal: TerminalId {
            backend,
            endpoint: take(&mut fields, "--endpoint")?,
            pane: take(&mut fields, "--pane")?,
            generation: take(&mut fields, "--generation")?,
        },
        conversation: take(&mut fields, "--conversation")?,
        process: hook_runtime::ProcessIdentity {
            pid: take(&mut fields, "--pid")?
                .parse()
                .map_err(|_| "Invalid process ID")?,
            start_ticks: take(&mut fields, "--process-start")?
                .parse()
                .map_err(|_| "Invalid process generation")?,
        },
        server_emitter,
        directory: take(&mut fields, "--directory")?.into(),
        profile: take(&mut fields, "--profile")?.into(),
        binary: take(&mut fields, "--binary")?.into(),
        transcript: fields.remove("--transcript").map(PathBuf::from),
        registration_id: take(&mut fields, "--registration-id")?,
        harness_version: take(&mut fields, "--version")?,
        goal_id: fields.remove("--goal-id").map(str::to_owned),
        options: hook_runtime::Options {
            telegram_delivery: flag(&mut fields, "--telegram")?,
            telegram_progress: flag(&mut fields, "--progress")?,
            hindsight: flag(&mut fields, "--hindsight")?,
            notifications: flag(&mut fields, "--notifications")?,
        },
    };
    if !fields.is_empty() {
        return Err("Unknown profile option".into());
    }
    let preview = hook_install::preview(&request, terminals)?;
    if let Some(expected) = expected {
        if preview.after_sha != expected {
            return Err("Profile changed since preview; review a new preview".into());
        }
        let receipt = hook_install::apply(&preview, terminals)?;
        let path = receipt.backup.with_extension("receipt.json");
        layouts::write_private(
            &path,
            &serde_json::to_vec_pretty(&receipt).map_err(|_| "Cannot encode rollback receipt")?,
        )?;
        write_json(
            writer,
            &json!({"receipt":path,"installation":receipt,"profile_written":true,"reload":"provider_managed"}),
        )
    } else {
        write_json(
            writer,
            &serde_json::to_value(preview).map_err(|_| "Cannot encode preview")?,
        )
    }
}

fn write_json(writer: &mut impl Write, value: &serde_json::Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(|_| "Cannot write response")?;
    writeln!(writer).map_err(|_| "Cannot write response".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn invalid_options_never_read_hook_input() {
        for args in [
            vec![
                "run",
                "--registration",
                crate::test_support::path("/tmp/example"),
                "--registration",
                crate::test_support::path("/tmp/other"),
            ],
            vec!["run", "--unknown", "value"],
            vec!["run", "--registration"],
            vec![
                "install",
                "--from",
                crate::test_support::path("/tmp/example"),
                "--event-id",
                "1",
            ],
        ] {
            assert!(
                run(
                    "hook",
                    &args.into_iter().map(String::from).collect::<Vec<_>>(),
                    &b"{}"[..],
                    &mut Vec::new()
                )
                .is_err()
            );
        }
    }
    #[test]
    fn help_works_without_files_or_processes() {
        let mut output = Vec::new();
        run("goal", &["--help".into()], &b""[..], &mut output).unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("does not launch")
        );
    }
    #[test]
    fn removal_cli_requires_review_digest_and_can_restore() {
        let f = super::super::hook_uninstall::tests::Fixture::new();
        let mut output = Vec::new();
        run(
            "hook",
            &[
                "profile-remove-preview".into(),
                "--from".into(),
                f.receipt.to_string_lossy().into_owned(),
            ],
            std::io::empty(),
            &mut output,
        )
        .unwrap();
        let review: serde_json::Value = serde_json::from_slice(&output).unwrap();
        output.clear();
        run(
            "hook",
            &[
                "profile-remove".into(),
                "--from".into(),
                f.receipt.to_string_lossy().into_owned(),
                "--approve".into(),
                review["approval"].as_str().unwrap().into(),
            ],
            std::io::empty(),
            &mut output,
        )
        .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&output).unwrap();
        output.clear();
        run(
            "hook",
            &[
                "profile-remove-rollback".into(),
                "--from".into(),
                result["receipt"].as_str().unwrap().into(),
            ],
            std::io::empty(),
            &mut output,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap()["restored"],
            true
        );
    }
}
