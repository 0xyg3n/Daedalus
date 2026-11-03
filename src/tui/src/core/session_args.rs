//! Native launch-selector grammar. No process or I/O.
//! Audited help: Claude2.1.317, Grok1.0.70, OpenCode1.19.5, Pi1.1.0,
//! Hermes0.20.0, Cursor2026.10.22. Picker flags and values stay distinct.
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
    ContinueOptional,
    Picker,
    ForkParent,
    Ephemeral,
}
fn kind(harness: &str, name: &str) -> Option<Kind> {
    use Kind::*;
    Some(match (harness, name) {
        ("claude" | "grok", "--resume" | "-r") => Resume,
        ("claude" | "grok", "--session-id") | ("grok", "-s") => Created,
        ("claude" | "grok", "--fork-session") | ("opencode", "--fork" | "--no-fork") => Fork,
        ("hermes", "--resume" | "-r") => Session,
        ("hermes", "--continue" | "-c") => ContinueOptional,
        ("cursor", "--resume") => Resume,
        ("cursor", "--continue") => Continue,
        ("pi", "--resume" | "-r") => Picker,
        ("pi", "--session") => Session,
        ("pi", "--session-id") => Created,
        ("pi", "--fork") => ForkParent,
        ("pi", "--no-session") => Ephemeral,
        ("claude" | "grok" | "opencode" | "pi", "--continue" | "-c") => Continue,
        (
            "hermes",
            "--profile" | "-p" | "--oneshot" | "-z" | "--usage-file" | "--model" | "-m"
            | "--provider" | "--reasoning" | "--toolsets" | "-t" | "--in" | "--skills" | "-s",
        ) => Value,
        (
            "hermes",
            "--help"
            | "-h"
            | "--version"
            | "-V"
            | "--no-restore-cwd"
            | "--worktree"
            | "-w"
            | "--accept-hooks"
            | "--yolo"
            | "--pass-session-id"
            | "--ignore-user-config"
            | "--ignore-rules"
            | "--safe-mode"
            | "--tui"
            | "--native"
            | "--tui-native"
            | "--cli"
            | "--dev",
        ) => Flag,
        (
            "cursor",
            "--api-key" | "--header" | "-H" | "--endpoint" | "-e" | "--output-format" | "--mode"
            | "--model" | "--sandbox" | "--workspace" | "--add-dir" | "--plugin-dir"
            | "--worktree-base",
        ) => Value,
        ("cursor", "--worktree" | "-w") => Optional,
        (
            "cursor",
            "--version"
            | "-v"
            | "--print"
            | "-p"
            | "--stream-partial-output"
            | "--plan"
            | "--list-models"
            | "--force"
            | "-f"
            | "--yolo"
            | "--auto-review"
            | "--approve-mcps"
            | "--trust"
            | "--skip-worktree-setup"
            | "--help"
            | "-h",
        ) => Flag,
        (
            "pi",
            "--provider"
            | "--model"
            | "--api-key"
            | "--system-prompt"
            | "--append-system-prompt"
            | "--mode"
            | "--session-dir"
            | "--name"
            | "-n"
            | "--models"
            | "--tools"
            | "-t"
            | "--exclude-tools"
            | "-xt"
            | "--thinking"
            | "--extension"
            | "-e"
            | "--skill"
            | "--prompt-template"
            | "--theme"
            | "--use-theme"
            | "--tui-mode",
        ) => Value,
        ("pi", "--list-models") => Optional,
        ("pi", "--export") => Value,
        (
            "pi",
            "--print"
            | "-p"
            | "--no-tools"
            | "-nt"
            | "--no-builtin-tools"
            | "-nbt"
            | "--no-extensions"
            | "-ne"
            | "--no-mcp"
            | "--no-skills"
            | "-ns"
            | "--no-prompt-templates"
            | "-np"
            | "--no-themes"
            | "--no-context-files"
            | "-nc"
            | "--verbose"
            | "--approve"
            | "-a"
            | "--no-approve"
            | "-na"
            | "--offline"
            | "--help"
            | "-h"
            | "--version"
            | "-v",
        ) => Flag,
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
    let mut resuming = false;
    let mut ephemeral = false;
    let mut positional = false;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') {
            if !positional {
                positional = true;
                if harness == "hermes" && arg != "chat" {
                    return Ok(None);
                }
                if (harness == "pi"
                    && matches!(
                        arg.as_str(),
                        "install"
                            | "remove"
                            | "uninstall"
                            | "update"
                            | "list"
                            | "config"
                            | "auth"
                            | "mcp"
                    ))
                    || (harness == "cursor"
                        && matches!(
                            arg.as_str(),
                            "persist"
                                | "install-shell-integration"
                                | "uninstall-shell-integration"
                                | "login"
                                | "logout"
                                | "mcp"
                                | "plugin"
                                | "worker"
                                | "status"
                                | "whoami"
                                | "models"
                                | "team"
                                | "bedrock"
                                | "about"
                                | "update"
                                | "create-chat"
                                | "generate-rule"
                                | "rule"
                                | "ls"
                                | "help"
                        ))
                {
                    return Ok(None);
                }
            }
            continue;
        }
        let (name, inline) = if arg.starts_with("--") {
            arg.split_once('=')
                .map_or((arg.as_str(), None), |(name, value)| (name, Some(value)))
        } else if arg.len() > 2
            && harness != "pi"
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
        if matches!(harness, "pi" | "hermes" | "cursor")
            && (matches!(name, "--help" | "-h" | "--version" | "--list-models")
                || (harness == "hermes" && name == "-V")
                || (matches!(harness, "pi" | "cursor") && name == "-v")
                || (harness == "pi" && name == "--export"))
        {
            return Ok(None);
        }
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
            Kind::ContinueOptional => {
                if inline.is_none() && args.peek().is_some_and(|value| !value.starts_with('-')) {
                    args.next();
                }
                continuing = true;
            }
            Kind::Picker => {
                boolean(harness, inline, true)?;
                resuming = true;
            }
            Kind::Ephemeral => {
                boolean(harness, inline, true)?;
                ephemeral = true;
            }
            Kind::ForkParent => {
                let value = inline
                    .or_else(|| args.next().map(String::as_str))
                    .ok_or("Missing fork parent selector")?;
                assign(&mut launch.resume, Some(value))?;
                launch.forking = true;
            }
            Kind::Flag => {
                boolean(harness, inline, true)?;
            }
            Kind::Value | Kind::Session | Kind::Created => {
                let value = inline
                    .or_else(|| args.next().map(String::as_str))
                    .ok_or("Missing launch option value")?;
                match kind {
                    Kind::Session => {
                        assign(&mut launch.resume, Some(value))?;
                        resuming = true;
                    }
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
                    resuming = true;
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
    let pi_modes = u8::from(resuming || continuing)
        + u8::from(launch.forking || launch.created.is_some())
        + u8::from(ephemeral);
    if harness == "pi" && pi_modes > 1 {
        return Err("Conflicting Pi session selectors".into());
    }
    if (continuing && launch.resume.is_some())
        || (matches!(harness, "pi" | "hermes" | "cursor") && continuing && resuming)
    {
        return Err("Conflicting continue and resume launch selectors".into());
    }
    Ok(Some(launch))
}
