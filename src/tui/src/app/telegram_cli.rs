//! Reviewed local Telegram administration for noninteractive clients.
use super::telegram_admin::{self, Reply, Request};
use crate::core::layouts;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
pub const HELP: &str = "daedalus telegram status --config-dir ABS\ndaedalus telegram start|stop --config-dir ABS [--apply --approve DIGEST]\ndaedalus telegram bindings --config-dir ABS --registration ABS\ndaedalus telegram bind --config-dir ABS --registration ABS --chat ID --actor ID [--apply --approve DIGEST]\nChanges preview by default. Apply requires the unchanged review digest and revalidates configuration, service or binding identity. Start/stop request a transition; use status to observe it. Tokens are never command-line arguments.\n";
#[derive(Debug)]
pub struct Command {
    directory: PathBuf,
    verb: String,
    registration: Option<PathBuf>,
    chat: Option<i64>,
    actor: Option<i64>,
    approval: Option<String>,
}
pub fn parse(args: &[String]) -> Result<Option<Command>> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return Ok(None);
    }
    let verb = args[0].as_str();
    if !["status", "start", "stop", "bindings", "bind"].contains(&verb) {
        return Err("Unknown Telegram administration command".into());
    }
    let mut flags = BTreeMap::new();
    let mut apply = false;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--apply" && !apply {
            apply = true;
            i += 1;
            continue;
        }
        if ![
            "--config-dir",
            "--registration",
            "--chat",
            "--actor",
            "--approve",
        ]
        .contains(&args[i].as_str())
            || i + 1 >= args.len()
            || flags.insert(args[i].clone(), args[i + 1].clone()).is_some()
        {
            return Err("Unknown, missing or repeated Telegram option".into());
        }
        i += 2;
    }
    let directory = PathBuf::from(flags.remove("--config-dir").ok_or("Missing --config-dir")?);
    layouts::check_path(&directory)?;
    let allowed: &[&str] = match verb {
        "start" | "stop" => &["--approve"],
        "bindings" => &["--registration"],
        "bind" => &["--registration", "--chat", "--actor", "--approve"],
        _ => &[],
    };
    if flags.keys().any(|k| !allowed.contains(&k.as_str()))
        || (apply && !["start", "stop", "bind"].contains(&verb))
        || (apply != flags.contains_key("--approve"))
    {
        return Err("Options do not match Telegram operation".into());
    }
    let registration = flags.remove("--registration").map(PathBuf::from);
    if let Some(path) = &registration {
        layouts::check_path(path)?;
    }
    let chat = flags
        .remove("--chat")
        .map(|v| v.parse::<i64>().map_err(|_| "Invalid chat identity"))
        .transpose()?;
    let actor = flags
        .remove("--actor")
        .map(|v| v.parse::<i64>().map_err(|_| "Invalid actor identity"))
        .transpose()?;
    if (["bindings", "bind"].contains(&verb) && registration.is_none())
        || (verb == "bind" && (chat.is_none_or(|v| v == 0) || actor.is_none_or(|v| v <= 0)))
    {
        return Err("Bindings need an exact registration; bind also needs a nonzero chat and positive actor".into());
    }
    let approval = flags.remove("--approve");
    if approval
        .as_ref()
        .is_some_and(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("Approval must be the SHA-256 digest from preview".into());
    }
    Ok(Some(Command {
        directory,
        verb: verb.into(),
        registration,
        chat,
        actor,
        approval,
    }))
}
pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    let Some(command) = parse(args)? else {
        return write!(out, "{HELP}").map_err(|_| "Cannot write Telegram help".into());
    };
    run_with(command, telegram_admin::execute, out)
}
pub fn run_with(
    command: Command,
    mut execute: impl FnMut(&Path, Request) -> Result<Reply>,
    out: &mut impl Write,
) -> Result<()> {
    let request = match command.verb.as_str() {
        "status" => Request::Inspect,
        "start" | "stop" => Request::Service {
            start: command.verb == "start",
        },
        "bindings" => Request::Bindings {
            registration: command.registration.ok_or("Registration missing")?,
        },
        "bind" => Request::Bind {
            registration: command.registration.ok_or("Registration missing")?,
            chat: command.chat.ok_or("Chat missing")?,
            actor: command.actor.ok_or("Actor missing")?,
        },
        _ => return Err("Unknown Telegram operation".into()),
    };
    let mutation = ["start", "stop", "bind"].contains(&command.verb.as_str());
    let value = match execute(&command.directory, request)? {
        Reply::Data(value) if !mutation => value,
        Reply::Plan(plan) if mutation => {
            if let Some(approval) = command.approval {
                if approval != plan.approval() {
                    return Err(
                        "Telegram state changed since preview; review the new plan before applying"
                            .into(),
                    );
                }
                match execute(&command.directory, Request::Apply { plan, approval })? {
                    Reply::Data(value) => value,
                    _ => return Err("Unexpected Telegram apply response".into()),
                }
            } else {
                plan.review()
            }
        }
        _ => return Err("Unexpected Telegram administration response".into()),
    };
    serde_json::to_writer_pretty(&mut *out, &value).map_err(|_| "Cannot write Telegram output")?;
    writeln!(out).map_err(|_| "Cannot write Telegram output".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let directory = std::env::temp_dir().join(format!(
                "daedalus-telegram-cli-{}-{unique}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let token = directory.join("token");
            layouts::write_private(&token, b"123:offline_fixture_only").unwrap();
            let config = json!({"version":1,"instance":"fixture","bot_id":123,"token_file":token,"grants":[],"listen_chats":[],"registrations":[],"hook_bindings":[]});
            layouts::write_private(
                &directory.join("telegram-service.json"),
                &serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            Self(directory)
        }
        fn command(&self, verb: &str, extra: &[&str]) -> Command {
            let mut args = vec![
                verb.to_string(),
                "--config-dir".into(),
                self.0.to_string_lossy().into_owned(),
            ];
            args.extend(extra.iter().map(|v| v.to_string()));
            parse(&args).unwrap().unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn service_preview_never_applies_and_changed_state_rejects_old_digest() {
        let fixture = Fixture::new();
        let mut output = Vec::new();
        run_with(
            fixture.command("start", &[]),
            |directory, request| {
                assert!(matches!(&request, Request::Service { start: true }));
                telegram_admin::execute(directory, request)
            },
            &mut output,
        )
        .unwrap();
        let preview: Value = serde_json::from_slice(&output).unwrap();
        let approval = preview["approval"].as_str().unwrap();
        // Change configuration after the reviewed preview; no transition executes.
        let path = fixture.0.join("telegram-service.json");
        let mut bytes = layouts::read_private(&path).unwrap().unwrap();
        bytes.push(b'\n');
        layouts::write_private(&path, &bytes).unwrap();
        let result = run_with(
            fixture.command("start", &["--apply", "--approve", approval]),
            |directory, request| {
                assert!(!matches!(&request, Request::Apply { .. }));
                telegram_admin::execute(directory, request)
            },
            &mut Vec::new(),
        );
        assert!(result.unwrap_err().contains("changed since preview"));
    }
    #[test]
    fn reviewed_apply_is_forwarded_once_and_preserves_observed_transition() {
        let fixture = Fixture::new();
        let Reply::Plan(plan) =
            telegram_admin::execute(&fixture.0, Request::Service { start: true }).unwrap()
        else {
            panic!("plan expected")
        };
        let approval = plan.approval();
        let mut calls = 0;
        let mut output = Vec::new();
        run_with(
            fixture.command("start", &["--apply", "--approve", &approval]),
            |directory, request| {
                calls += 1;
                match request {
                    Request::Service { start: true } => {
                        telegram_admin::execute(directory, Request::Service { start: true })
                    }
                    Request::Apply {
                        plan,
                        approval: given,
                    } => {
                        assert_eq!(given, approval);
                        assert_eq!(given, plan.approval());
                        Ok(Reply::Data(json!({"state":"starting"})))
                    }
                    _ => panic!("unexpected request"),
                }
            },
            &mut output,
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap()["state"],
            "starting"
        );
    }
    #[test]
    fn status_and_binding_queries_do_not_apply() {
        let fixture = Fixture::new();
        run_with(
            fixture.command("status", &[]),
            |_, request| {
                assert!(matches!(request, Request::Inspect));
                Ok(Reply::Data(json!({"configured":false})))
            },
            &mut Vec::new(),
        )
        .unwrap();
        let registration = fixture.0.join("registration.json");
        run_with(
            fixture.command(
                "bindings",
                &["--registration", registration.to_str().unwrap()],
            ),
            |_, request| {
                let Request::Bindings {
                    registration: received,
                } = request
                else {
                    panic!("bindings expected")
                };
                assert_eq!(received, registration);
                Ok(Reply::Data(json!({"recipients":[]})))
            },
            &mut Vec::new(),
        )
        .unwrap();
    }
    #[test]
    fn binding_preview_preserves_exact_recipient_and_registration() {
        let fixture = Fixture::new();
        let registration = fixture.0.join("registration.json");
        let result = run_with(
            fixture.command(
                "bind",
                &[
                    "--registration",
                    registration.to_str().unwrap(),
                    "--chat",
                    "-42",
                    "--actor",
                    "7",
                ],
            ),
            |_, request| {
                let Request::Bind {
                    registration: received,
                    chat,
                    actor,
                } = request
                else {
                    panic!("bind preview expected")
                };
                assert_eq!(received, registration);
                assert_eq!((chat, actor), (-42, 7));
                Err("fixture: live registration unavailable".into())
            },
            &mut Vec::new(),
        );
        assert_eq!(
            result.unwrap_err(),
            "fixture: live registration unavailable"
        );
        let result = run_with(
            fixture.command("stop", &[]),
            |_, request| {
                assert!(matches!(request, Request::Service { start: false }));
                Err("fixture: no verified process".into())
            },
            &mut Vec::new(),
        );
        assert_eq!(result.unwrap_err(), "fixture: no verified process");
    }
    #[test]
    fn parser_rejects_unreviewed_or_mismatched_options() {
        let directory = std::env::temp_dir();
        let base = directory.to_str().unwrap();
        for extras in [
            vec!["start", "--apply"],
            vec!["stop", "--approve", "abcd"],
            vec!["status", "--apply", "--approve", "abcd"],
            vec!["start", "--token", "secret"],
            vec![
                "bind",
                "--registration",
                base,
                "--chat",
                "0",
                "--actor",
                "1",
            ],
            vec!["bindings"],
            vec!["start", "--apply", "--apply"],
        ] {
            let mut args = extras.into_iter().map(str::to_string).collect::<Vec<_>>();
            args.extend(["--config-dir".into(), base.into()]);
            assert!(parse(&args).is_err());
        }
    }
}
