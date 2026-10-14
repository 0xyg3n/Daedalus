//! Local route control reuses the same reviewed Telegram administration plan.
use super::{
    telegram_admin::{self, Reply, Request},
    telegram_config::ServiceConfig,
};
use crate::core::{
    layouts,
    routes::{Scope, Terminal},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, String>;
pub const HELP: &str = "daedalus route list --config-dir ABS\ndaedalus route active --config-dir ABS --chat ID\ndaedalus route candidates --config-dir ABS\ndaedalus route select --config-dir ABS --chat ID --actor ID --target ABS [--apply --approve DIGEST]\nSelect previews an exact candidate from route candidates. No route permissions are inferred; the actor must already have Manage access. Apply needs the unchanged preview digest and fresh terminal identity.\n";
#[derive(Debug)]
pub struct Command {
    pub directory: PathBuf,
    verb: String,
    chat: Option<i64>,
    actor: Option<i64>,
    target: Option<PathBuf>,
    approval: Option<String>,
}
pub fn parse(args: &[String]) -> Result<Option<Command>> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return Ok(None);
    }
    let verb = args[0].as_str();
    if !["list", "active", "candidates", "select"].contains(&verb) {
        return Err("Unknown route command; use route --help".into());
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
        if !["--config-dir", "--chat", "--actor", "--target", "--approve"]
            .contains(&args[i].as_str())
            || i + 1 >= args.len()
            || flags.insert(args[i].clone(), args[i + 1].clone()).is_some()
        {
            return Err("Unknown, missing or repeated route option".into());
        }
        i += 2;
    }
    let directory = PathBuf::from(flags.remove("--config-dir").ok_or("Missing --config-dir")?);
    layouts::check_path(&directory)?;
    let allowed: &[&str] = match verb {
        "select" => &["--chat", "--actor", "--target", "--approve"],
        "active" => &["--chat"],
        _ => &[],
    };
    if flags.keys().any(|k| !allowed.contains(&k.as_str()))
        || (apply && verb != "select")
        || (apply != flags.contains_key("--approve"))
    {
        return Err("Options do not match route operation".into());
    }
    let chat = flags
        .get("--chat")
        .map(|v| v.parse::<i64>().map_err(|_| "Invalid chat identity"))
        .transpose()?;
    let actor = flags
        .get("--actor")
        .map(|v| v.parse::<i64>().map_err(|_| "Invalid actor identity"))
        .transpose()?;
    if chat == Some(0)
        || actor.is_some_and(|v| v <= 0)
        || (["active", "select"].contains(&verb) && chat.is_none())
        || (verb == "select" && (actor.is_none() || !flags.contains_key("--target")))
    {
        return Err("Select needs exact chat, positive actor and target; active needs chat".into());
    }
    let target = flags.get("--target").map(PathBuf::from);
    if let Some(path) = &target {
        layouts::check_path(path)?;
    }
    Ok(Some(Command {
        directory,
        verb: verb.into(),
        chat,
        actor,
        target,
        approval: flags.remove("--approve"),
    }))
}
pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    let Some(command) = parse(args)? else {
        return write!(out, "{HELP}").map_err(|_| "Cannot write route help".into());
    };
    run_with(command, telegram_admin::execute, out)
}
pub fn run_with(
    command: Command,
    mut execute: impl FnMut(&Path, Request) -> Result<Reply>,
    out: &mut impl Write,
) -> Result<()> {
    let value = match command.verb.as_str() {
        "candidates" => match execute(&command.directory, Request::Routes)? {
            Reply::Data(value) => value,
            _ => return Err("Unexpected candidate response".into()),
        },
        "select" => {
            let target: Terminal = serde_json::from_slice(
                &layouts::read_private(command.target.as_ref().ok_or("Target missing")?)?
                    .ok_or("Target file missing")?,
            )
            .map_err(|_| "Target must be one exact route candidate JSON object")?;
            target
                .validate()
                .map_err(|_| "Invalid terminal candidate")?;
            let Reply::Plan(plan) = execute(
                &command.directory,
                Request::Route {
                    chat: command.chat.ok_or("Chat missing")?,
                    actor: command.actor.ok_or("Actor missing")?,
                    target: Box::new(target),
                },
            )?
            else {
                return Err("Missing route review plan".into());
            };
            if let Some(approval) = command.approval {
                if plan.approval() != approval {
                    return Err(
                        "Route changed since preview; review the new plan before applying".into(),
                    );
                }
                match execute(&command.directory, Request::Apply { plan, approval })? {
                    Reply::Data(value) => value,
                    _ => return Err("Unexpected route apply response".into()),
                }
            } else {
                plan.review()
            }
        }
        "list" | "active" => {
            let config = ServiceConfig::load(&command.directory.join("telegram-service.json"))?;
            super::diagnostics::with_database(&command.directory.join("routes.sqlite3"),|con|{
                if let Some(chat)=command.chat {
                    let scope=Scope{instance:config.instance.clone(),bot:config.bot_id.to_string(),chat:chat.to_string()};
                    let row=crate::core::routes::read_route(con,&scope).map_err(|_|"Cannot inspect saved route")?;
                    Ok(json!({"route":row,"live_verified":false}))
                } else {
                    let mut query=con.prepare("SELECT chat,identity,pinned,revision FROM chat_routes WHERE instance=? AND bot=? ORDER BY chat LIMIT 10001").map_err(|_|"Cannot inspect saved routes")?;
                    let rows=query.query_map(rusqlite::params![config.instance,config.bot_id.to_string()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,bool>(2)?,row.get::<_,i64>(3)?))).map_err(|_|"Cannot inspect saved routes")?.collect::<rusqlite::Result<Vec<_>>>().map_err(|_|"Invalid saved route")?;
                    if rows.len()>10000{return Err("Saved route result exceeds limit".into());}
                    let mut output=Vec::new();for(chat,identity,pinned,revision)in rows {let id:crate::core::identity::TerminalId=serde_json::from_str(&identity).map_err(|_|"Invalid saved route identity")?;id.validate().map_err(str::to_owned)?;output.push(json!({"chat":chat,"terminal":id,"pinned":pinned,"revision":revision}));}
                    Ok(json!({"routes":output,"live_verified":false}))
                }
            })?.unwrap_or_else(||if command.chat.is_some(){json!({"route":null,"live_verified":false})}else{json!({"routes":[],"live_verified":false})})
        }
        _ => return Err("Unknown route command".into()),
    };
    serde_json::to_writer_pretty(&mut *out, &value).map_err(|_| "Cannot write route output")?;
    writeln!(out).map_err(|_| "Cannot write route output".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn explicit_apply_requires_review_digest_and_identities() {
        assert!(
            parse(&args(&[
                "select",
                "--config-dir",
                "/tmp/fixture",
                "--chat",
                "42",
                "--actor",
                "7",
                "--target",
                "/tmp/target",
                "--apply"
            ]))
            .is_err()
        );
        assert!(parse(&args(&["active", "--config-dir", "/tmp/fixture"])).is_err());
        assert!(
            parse(&args(&[
                "list",
                "--config-dir",
                "/tmp/fixture",
                "--actor",
                "7"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "select",
                "--config-dir",
                "/tmp/fixture",
                "--chat",
                "42",
                "--actor",
                "0",
                "--target",
                "/tmp/target"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "select",
                "--config-dir",
                "/tmp/fixture",
                "--chat",
                "42",
                "--actor",
                "7",
                "--target",
                "/tmp/target"
            ]))
            .is_ok()
        );
    }
    #[test]
    fn route_preview_requires_grant_and_apply_consumes_exact_approval() {
        let root = std::env::temp_dir().join(format!(
            "daedalus-route-cli-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        super::super::setup::SetupPlan::fresh(
            &root,
            super::super::config::ControlConfig::fresh(Path::new("/tmp")).unwrap(),
        )
        .unwrap()
        .apply()
        .unwrap();
        let config = json!({"version":1,"instance":"fixture","bot_id":123,"token_file":root.join("token"),"grants":[{"chat":42,"actor":7,"permissions":["read","send","manage"]}],"listen_chats":[],"registrations":[],"hook_bindings":[]});
        layouts::write_private(
            &root.join("telegram-service.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let target = Terminal {
            identity: crate::core::identity::TerminalId {
                backend: crate::core::identity::Backend::Herdr,
                endpoint: "fixture".into(),
                pane: "pane".into(),
                generation: "generation".into(),
            },
            label: "fixture".into(),
            workspace: "/tmp".into(),
            harness: "claude".into(),
            conversation: Some("conversation".into()),
            available: true,
        };
        let target_path = root.join("target.json");
        layouts::write_private(&target_path, &serde_json::to_vec(&target).unwrap()).unwrap();
        let arguments = args(&[
            "select",
            "--config-dir",
            root.to_str().unwrap(),
            "--chat",
            "42",
            "--actor",
            "7",
            "--target",
            target_path.to_str().unwrap(),
        ]);
        let mut effects = 0;
        let mut execute = |directory: &Path, request: Request| match request {
            Request::Route {
                chat,
                actor,
                target,
            } => Ok(Reply::Plan(Box::new(telegram_admin::Plan::Route(
                Box::new(telegram_admin::RoutePlan::prepare(
                    directory,
                    chat,
                    actor,
                    *target,
                    std::slice::from_ref(&target_snapshot()),
                )?),
            )))),
            Request::Apply { plan, approval } => {
                assert_eq!(plan.approval(), approval);
                effects += 1;
                Ok(Reply::Data(json!({"fixture_applied":true})))
            }
            _ => panic!("unexpected administration operation"),
        };
        let mut denied = arguments.clone();
        let actor_index = denied.iter().position(|v| v == "--actor").unwrap() + 1;
        denied[actor_index] = "8".into();
        assert!(run_with(parse(&denied).unwrap().unwrap(), &mut execute, &mut vec![]).is_err());
        let mut output = vec![];
        run_with(
            parse(&arguments).unwrap().unwrap(),
            &mut execute,
            &mut output,
        )
        .unwrap();
        let preview: serde_json::Value = serde_json::from_slice(&output).unwrap();
        let approval = preview["approval"].as_str().unwrap();
        let mut wrong = arguments.clone();
        wrong.extend(args(&["--apply", "--approve", "wrong"]));
        assert!(run_with(parse(&wrong).unwrap().unwrap(), &mut execute, &mut vec![]).is_err());
        let mut applied = arguments;
        applied.extend(args(&["--apply", "--approve", approval]));
        run_with(parse(&applied).unwrap().unwrap(), &mut execute, &mut vec![]).unwrap();
        assert_eq!(effects, 1);
        let mut read_output = vec![];
        run_with(
            parse(&args(&["list", "--config-dir", root.to_str().unwrap()]))
                .unwrap()
                .unwrap(),
            |_, _| panic!("listing must not invoke transport"),
            &mut read_output,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&read_output).unwrap()["routes"],
            json!([])
        );
        assert!(!root.join("routes.sqlite3").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    fn target_snapshot() -> Terminal {
        Terminal {
            identity: crate::core::identity::TerminalId {
                backend: crate::core::identity::Backend::Herdr,
                endpoint: "fixture".into(),
                pane: "pane".into(),
                generation: "generation".into(),
            },
            label: "fixture".into(),
            workspace: "/tmp".into(),
            harness: "claude".into(),
            conversation: Some("conversation".into()),
            available: true,
        }
    }
}
