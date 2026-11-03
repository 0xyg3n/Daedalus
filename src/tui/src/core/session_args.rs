//! Launch-selector grammar for Claude Code, Grok and OpenCode. No process or I/O.
//! Audited against installed help: Claude2.1.317, Grok1.0.70, OpenCode1.19.5.
use super::layouts::Result;

#[derive(Default)]
pub(super) struct Launch<'a> {
    pub resume: Option<&'a str>,
    pub created: Option<&'a str>,
    pub forking: bool,
}
#[derive(Clone, Copy)]
enum Kind {
    Flag,
    Value,
    Optional,
    Many,
    Resume,
    Session,
    Created,
    Fork,
    Continue,
}
fn kind(harness: &str, name: &str) -> Option<Kind> {
    use Kind::*;
    Some(match (harness, name) {
        ("claude" | "grok", "--resume" | "-r") => Resume,
        ("claude" | "grok", "--session-id") | ("grok", "-s") => Created,
        ("claude" | "grok", "--fork-session") | ("opencode", "--fork" | "--no-fork") => Fork,
        (_, "--continue" | "-c") => Continue,
        ("opencode", "--session" | "-s") => Session,
        (
            "claude",
            "--add-dir" | "--allowedTools" | "--allowed-tools" | "--betas" | "--disallowedTools"
            | "--disallowed-tools" | "--file" | "--mcp-config" | "--tools",
        ) => Many,
        (
            "claude",
            "--cloud"
            | "--debug"
            | "-d"
            | "--from-pr"
            | "--prompt-suggestions"
            | "--remote-control"
            | "--teleport"
            | "--worktree"
            | "-w",
        ) => Optional,
        (
            "claude",
            "--agent"
            | "--agents"
            | "--append-system-prompt"
            | "--append-system-prompt-file"
            | "--autocompact"
            | "--debug-file"
            | "--effort"
            | "--environment"
            | "--fallback-model"
            | "--input-format"
            | "--json-schema"
            | "--max-budget-usd"
            | "--model"
            | "--name"
            | "-n"
            | "--output-format"
            | "--permission-mode"
            | "--permission-prompts"
            | "--plugin-dir"
            | "--plugin-url"
            | "--remote-control-session-name-prefix"
            | "--setting-sources"
            | "--settings"
            | "--system-prompt"
            | "--system-prompt-file"
            | "--system-prompt-snapshot",
        ) => Value,
        (
            "claude",
            "--allow-dangerously-skip-permissions"
            | "--ax-screen-reader"
            | "--bg"
            | "--background"
            | "--bare"
            | "--brief"
            | "--chrome"
            | "--dangerously-skip-permissions"
            | "--desktop"
            | "--disable-slash-commands"
            | "--exclude-dynamic-system-prompt-sections"
            | "--forward-subagent-text"
            | "--help"
            | "-h"
            | "--ide"
            | "--include-hook-events"
            | "--include-partial-messages"
            | "--no-chrome"
            | "--no-session-persistence"
            | "--print"
            | "-p"
            | "--replay-user-messages"
            | "--restricted"
            | "--safe-mode"
            | "--strict-mcp-config"
            | "--tmux"
            | "--verbose"
            | "--version"
            | "-v",
        ) => Flag,
        (
            "grok",
            "--add-dir"
            | "--agent"
            | "--agents"
            | "--allow"
            | "--allowedTools"
            | "--allowed-tools"
            | "--append-system-prompt"
            | "--cwd"
            | "--debug-file"
            | "--deny"
            | "--disallowedTools"
            | "--disallowed-tools"
            | "--json-schema"
            | "--leader-socket"
            | "--model"
            | "-m"
            | "--max-turns"
            | "--output-format"
            | "--single"
            | "-p"
            | "--permission-mode"
            | "--prompt-file"
            | "--prompt-json"
            | "--reasoning-effort"
            | "--effort"
            | "--rules"
            | "--sandbox"
            | "--system-prompt-override"
            | "--system-prompt"
            | "--tools"
            | "--worktree-ref"
            | "--ref",
        ) => Value,
        ("grok", "--worktree" | "-w") => Optional,
        (
            "grok",
            "--always-approve"
            | "--yolo"
            | "--dangerously-skip-permissions"
            | "--debug"
            | "--disable-web-search"
            | "--fullscreen"
            | "--help"
            | "-h"
            | "--include-partial-messages"
            | "--minimal"
            | "--no-alt-screen"
            | "--no-plan"
            | "--no-subagents"
            | "--oauth"
            | "--restore-code"
            | "--version"
            | "-v"
            | "--verbatim",
        ) => Flag,
        (
            "opencode",
            "--log-level" | "--port" | "--hostname" | "--mdns-domain" | "--model" | "-m"
            | "--prompt" | "--agent" | "--replay-limit" | "--dir" | "--password" | "-p"
            | "--username" | "-u",
        ) => Value,
        ("opencode", "--cors") => Many,
        (
            "opencode",
            "--help" | "-h" | "--version" | "-v" | "--print-logs" | "--pure" | "--mdns" | "--auto"
            | "--mini" | "--no-replay",
        ) => Flag,
        _ => return None,
    })
}
fn assign<'a>(slot: &mut Option<&'a str>, value: Option<&'a str>) -> Result<()> {
    if let Some(value) = value {
        if slot.is_some_and(|old| old != value) {
            return Err("Conflicting launch session selectors".into());
        }
        *slot = Some(value);
    }
    Ok(())
}
fn boolean(harness: &str, inline: Option<&str>, default: bool) -> Result<bool> {
    match (harness, inline) {
        (_, None) => Ok(default),
        ("opencode", Some("true")) => Ok(true),
        ("opencode", Some("false")) => Ok(false),
        _ => Err("Invalid launch boolean option".into()),
    }
}
/// Unknown option arity leaves the selection unresolved; its value could itself
/// resemble a selector or fork flag. Never scan through an unparsed option.
pub(super) fn parse<'a>(argv: &'a [String], harness: &str) -> Result<Option<Launch<'a>>> {
    let mut launch = Launch::default();
    let mut args = argv.iter().skip(1).peekable();
    let mut continuing = false;
    let mut fork = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') {
            continue;
        }
        let (name, inline) = if arg.starts_with("--") {
            arg.split_once('=')
                .map_or((arg.as_str(), None), |(name, value)| (name, Some(value)))
        } else if arg.len() > 2
            && arg
                .get(..2)
                .is_some_and(|name| kind(harness, name).is_some())
        {
            let value = &arg[2..];
            let value = if harness == "claude" {
                value
            } else {
                value.strip_prefix('=').unwrap_or(value)
            };
            (&arg[..2], Some(value))
        } else {
            (arg.as_str(), None)
        };
        let Some(kind) = kind(harness, name) else {
            return Ok(None);
        };
        match kind {
            Kind::Fork => {
                let value = boolean(harness, inline, true)?;
                let value = if name == "--no-fork" { !value } else { value };
                if fork.is_some_and(|old| old != value) {
                    return Err("Conflicting launch fork options".into());
                }
                fork = Some(value);
                launch.forking = value;
            }
            Kind::Continue => continuing |= boolean(harness, inline, true)?,
            Kind::Flag => {
                boolean(harness, inline, true)?;
            }
            Kind::Value | Kind::Session | Kind::Created => {
                let value = inline
                    .or_else(|| args.next().map(String::as_str))
                    .ok_or("Missing launch option value")?;
                match kind {
                    Kind::Session => assign(&mut launch.resume, Some(value))?,
                    Kind::Created => assign(&mut launch.created, Some(value))?,
                    _ => (),
                }
            }
            Kind::Resume | Kind::Optional => {
                let value = inline.or_else(|| {
                    args.peek().filter(|value| !value.starts_with('-'))?;
                    args.next().map(String::as_str)
                });
                if matches!(kind, Kind::Resume) {
                    assign(&mut launch.resume, value)?;
                }
            }
            Kind::Many => {
                let mut count = usize::from(inline.is_some());
                while args.peek().is_some_and(|value| !value.starts_with('-')) {
                    args.next();
                    count += 1;
                }
                if count == 0 {
                    return Err("Missing launch list option value".into());
                }
            }
        }
    }
    if continuing && launch.resume.is_some() {
        return Err("Conflicting continue and resume launch selectors".into());
    }
    Ok(Some(launch))
}
