//! Native command planning. No account credentials are read and no process is started.
use crate::{app::config::HARNESSES, core::checkpoints::Capabilities};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone, Copy)]
pub struct Method {
    pub id: &'static str,
    pub title: &'static str,
    pub args: &'static [&'static str],
    /// The executor must prompt privately and send the value on stdin, never argv.
    pub secret_stdin: bool,
    pub environment: &'static [(&'static str, &'static str)],
    pub instruction: &'static str,
}
const fn method(id: &'static str, title: &'static str, args: &'static [&'static str]) -> Method {
    Method {
        id,
        title,
        args,
        secret_stdin: false,
        environment: &[],
        instruction: "Use the harness's native account sign-in.",
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Tool {
    pub id: &'static str,
    pub name: &'static str,
    pub executables: &'static [&'static str],
    pub profile_variable: &'static str,
    pub default_profile: &'static str,
    pub account_subdirectory: &'static str,
    pub scoped_xdg: bool,
    pub extra_binary_paths: &'static [&'static str],
    pub methods: &'static [Method],
}

pub fn tools() -> impl Iterator<Item = Tool> {
    HARNESSES
        .into_iter()
        .map(|id| tool(id).expect("Every registered harness has a native adapter"))
}

pub fn tool(id: &str) -> Option<Tool> {
    let (
        id,
        name,
        executables,
        profile_variable,
        default_profile,
        methods,
        account_subdirectory,
        scoped_xdg,
        extra_binary_paths,
    ): (
        _,
        _,
        &'static [&'static str],
        _,
        _,
        &'static [Method],
        _,
        _,
        &'static [&'static str],
    ) = match id {
        "claude" => {
            const {
                (
                    "claude",
                    "Claude Code",
                    &["claude"],
                    "CLAUDE_CONFIG_DIR",
                    ".claude",
                    &[
                        method(
                            "subscription",
                            "Claude subscription",
                            &["auth", "login", "--claudeai"],
                        ),
                        method(
                            "console",
                            "Anthropic Console",
                            &["auth", "login", "--console"],
                        ),
                        method("sso", "Company SSO", &["auth", "login", "--sso"]),
                    ],
                    "",
                    false,
                    &[],
                )
            }
        }
        "codex" => {
            const {
                (
                    "codex",
                    "Codex",
                    &["codex"],
                    "CODEX_HOME",
                    ".codex",
                    &[
                        method("chatgpt", "ChatGPT account", &["login"]),
                        method("device", "ChatGPT device code", &["login", "--device-auth"]),
                        Method {
                            secret_stdin: true,
                            ..method("api-key", "OpenAI API key", &["login", "--with-api-key"])
                        },
                    ],
                    "",
                    false,
                    &[],
                )
            }
        }
        "grok" => {
            const {
                (
                    "grok",
                    "Grok",
                    &["grok"],
                    "GROK_HOME",
                    ".grok",
                    &[
                        method("oauth", "Grok / X account", &["login", "--oauth"]),
                        method("device", "Grok device code", &["login", "--device-auth"]),
                    ],
                    "",
                    false,
                    &[],
                )
            }
        }
        "hermes" => {
            const {
                (
                    "hermes",
                    "Hermes",
                    &["hermes"],
                    "HERMES_HOME",
                    ".hermes",
                    &[
                        method(
                            "openai-codex",
                            "ChatGPT account",
                            &["auth", "add", "openai-codex", "--type", "oauth"],
                        ),
                        method(
                            "xai-oauth",
                            "Grok / X account",
                            &["auth", "add", "xai-oauth", "--type", "oauth"],
                        ),
                        method(
                            "nous",
                            "Nous Portal",
                            &["auth", "add", "nous", "--type", "oauth"],
                        ),
                        method(
                            "anthropic",
                            "Claude account",
                            &["auth", "add", "anthropic", "--type", "oauth"],
                        ),
                    ],
                    "",
                    false,
                    &[],
                )
            }
        }
        "cursor" => {
            const {
                (
                    "cursor",
                    "Cursor",
                    &["cursor-agent", "agent"],
                    "XDG_CONFIG_HOME",
                    ".config",
                    &[
                        method("browser", "Cursor account", &["login"]),
                        Method {
                            environment: &[("NO_OPEN_BROWSER", "1")],
                            ..method("link", "Cursor sign-in link", &["login"])
                        },
                    ],
                    "cursor",
                    true,
                    &[],
                )
            }
        }
        "pi" => {
            const {
                (
                    "pi",
                    "Pi",
                    &["pi"],
                    "PI_CODING_AGENT_DIR",
                    ".pi/agent",
                    &[Method {
                        instruction: "Open Pi, then type /login.",
                        ..method("login", "Pi sign-in", &[])
                    }],
                    "",
                    false,
                    &[],
                )
            }
        }
        "opencode" => {
            const {
                (
                    "opencode",
                    "OpenCode",
                    &["opencode"],
                    "XDG_DATA_HOME",
                    ".local/share",
                    &[method("login", "Choose a provider", &["auth", "login"])],
                    "opencode",
                    true,
                    &[".opencode/bin"],
                )
            }
        }
        _ => return None,
    };
    Some(Tool {
        id,
        name,
        executables,
        profile_variable,
        default_profile,
        methods,
        account_subdirectory,
        scoped_xdg,
        extra_binary_paths,
    })
}

/// This is a complete child environment, not an overlay onto the launching agent.
/// Do not log its values; HOME, PATH and desktop connection variables may be private.
pub struct ProcessPlan {
    pub binary: PathBuf,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub secret_stdin: bool,
    pub instruction: &'static str,
}
impl fmt::Debug for ProcessPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessPlan")
            .field("binary", &self.binary)
            .field("args", &self.args)
            .field(
                "environment_keys",
                &self.environment.keys().collect::<Vec<_>>(),
            )
            .field("secret_stdin", &self.secret_stdin)
            .finish_non_exhaustive()
    }
}
impl ProcessPlan {
    /// Merely builds a Command. Native login must remain an explicit user action.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .args(&self.args)
            .env_clear()
            .envs(&self.environment)
            .current_dir(&self.cwd);
        command
    }
}

pub fn executable(tool: &Tool, environment: &BTreeMap<String, String>) -> Option<PathBuf> {
    let mut directories: Vec<PathBuf> =
        std::env::split_paths(environment.get("PATH").map(String::as_str).unwrap_or(""))
            .filter(|path| path.is_absolute())
            .collect();
    if let Some(home) = environment
        .get("HOME")
        .map(Path::new)
        .filter(|p| p.is_absolute())
    {
        directories.extend(tool.extra_binary_paths.iter().map(|path| home.join(path)));
    }
    tool.executables
        .iter()
        .flat_map(|name| {
            directories
                .iter()
                .map(move |directory| directory.join(name))
        })
        .find(|path| {
            std::fs::metadata(path).is_ok_and(|metadata| {
                #[cfg(unix)]
                {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                }
                #[cfg(not(unix))]
                {
                    metadata.is_file()
                }
            })
        })
}

pub fn profile_directory(
    tool: &Tool,
    environment: &BTreeMap<String, String>,
    profiles: &BTreeMap<String, String>,
) -> Result<PathBuf, String> {
    let path = if let Some(value) = profiles
        .get(tool.id)
        .or_else(|| environment.get(tool.profile_variable))
    {
        PathBuf::from(value)
    } else {
        PathBuf::from(
            environment
                .get("HOME")
                .ok_or("HOME is required to select a native profile")?,
        )
        .join(tool.default_profile)
    };
    validate_path(&path)?;
    Ok(path)
}

pub fn account_directory(
    tool: &Tool,
    environment: &BTreeMap<String, String>,
    profiles: &BTreeMap<String, String>,
) -> Result<PathBuf, String> {
    Ok(profile_directory(tool, environment, profiles)?.join(tool.account_subdirectory))
}

fn child_environment(
    tool: &Tool,
    environment: &BTreeMap<String, String>,
    profiles: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    // No provider tokens, Telegram routes, sibling harness profiles, loader injection,
    // shell startup hooks or permission-bypass variables cross this boundary.
    const RUNTIME: [&str; 18] = [
        "HOME",
        "PATH",
        "TERM",
        "COLORTERM",
        "LANG",
        "LANGUAGE",
        "TZ",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "SSH_AUTH_SOCK",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "TMP",
        "TEMP",
        "TERM_PROGRAM",
    ];
    let mut child: BTreeMap<String, String> = environment
        .iter()
        .filter(|(key, _)| RUNTIME.contains(&key.as_str()) || key.starts_with("LC_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let profile = profile_directory(tool, environment, profiles)?;
    // The default location is the tool's own default with the variable unset.
    // Setting it explicitly is not equivalent for every tool: Claude Code then
    // reads its global config from the profile instead of ~/.claude.json.
    let default = environment
        .get("HOME")
        .map(|home| Path::new(home).join(tool.default_profile));
    if default.as_deref() != Some(profile.as_path()) {
        child.insert(
            tool.profile_variable.into(),
            profile.to_str().ok_or("Profile path must be UTF-8")?.into(),
        );
    }
    Ok(child)
}

pub fn launch_plan(
    tool: &Tool,
    binary: &Path,
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    profiles: &BTreeMap<String, String>,
) -> Result<ProcessPlan, String> {
    validate_path(binary)?;
    validate_path(cwd)?;
    Ok(ProcessPlan {
        binary: binary.into(),
        args: vec![],
        environment: child_environment(tool, environment, profiles)?,
        cwd: cwd.into(),
        secret_stdin: false,
        instruction: "",
    })
}

pub fn login_plan(
    tool: &Tool,
    method_id: &str,
    binary: &Path,
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    profiles: &BTreeMap<String, String>,
) -> Result<ProcessPlan, String> {
    let method = tool
        .methods
        .iter()
        .find(|method| method.id == method_id)
        .ok_or("Unknown native login method")?;
    let mut plan = launch_plan(tool, binary, cwd, environment, profiles)?;
    plan.args = method.args.iter().map(|arg| (*arg).into()).collect();
    plan.environment.extend(
        method
            .environment
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into())),
    );
    plan.secret_stdin = method.secret_stdin;
    plan.instruction = method.instruction;
    Ok(plan)
}

/// Capability evidence comes from a successful, bounded native help query supplied
/// by the caller. Unsupported harnesses never acquire capabilities by matching text.
pub fn checkpoint_capabilities(
    tool: &Tool,
    binary: &Path,
    successful_help: Option<&str>,
) -> Result<Capabilities, String> {
    validate_path(binary)?;
    let words: Vec<&str> = successful_help
        .unwrap_or("")
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| matches!(c, ',' | '[' | ']' | '(' | ')')))
        .collect();
    let native = matches!(tool.id, "claude" | "codex" | "grok" | "pi");
    let resume = native
        && if tool.id == "pi" {
            words.contains(&"--session")
        } else if tool.id == "codex" {
            words.contains(&"resume")
        } else {
            words.contains(&"--resume")
        };
    let native_fork = resume
        && if tool.id == "pi" {
            words.contains(&"--fork")
        } else if tool.id == "codex" {
            words.contains(&"fork")
        } else {
            words.contains(&"--fork-session")
        };
    Ok(Capabilities {
        harness: tool.id.into(),
        binary: binary.into(),
        resume,
        native_fork,
        handoff: native && successful_help.is_some(),
    })
}

fn validate_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .as_os_str()
            .to_string_lossy()
            .chars()
            .any(char::is_control)
    {
        return Err(
            "Native command paths must be absolute and contain no control characters".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn env() -> BTreeMap<String, String> {
        [
            ("HOME", "/tmp/test-home"),
            ("PATH", "/usr/bin"),
            ("TERM", "xterm-256color"),
            ("TELEGRAM_BOT_TOKEN", "private-token"),
            ("CODEX_HOME", "/tmp/sibling"),
            ("CLAUDE_CONFIG_DIR", "/tmp/claude-default"),
            ("LD_PRELOAD", "/tmp/injected"),
            ("OPENAI_API_KEY", "wrong-account"),
        ]
        .into_iter()
        .map(|(a, b)| (a.into(), b.into()))
        .collect()
    }
    #[test]
    fn every_registered_harness_has_native_launch_and_login() {
        let all: Vec<_> = tools().collect();
        assert_eq!(all.len(), HARNESSES.len());
        for tool in all {
            assert!(!tool.methods.is_empty());
            let plan = launch_plan(
                &tool,
                Path::new("/usr/bin/native"),
                Path::new("/tmp/work"),
                &env(),
                &BTreeMap::new(),
            )
            .unwrap();
            assert!(plan.args.is_empty());
            assert!(!plan.secret_stdin);
        }
        assert!(tool("unknown").is_none());
    }
    #[test]
    fn isolates_instance_credentials_and_normal_permissions() {
        let tool = tool("claude").unwrap();
        let plan = launch_plan(
            &tool,
            Path::new("/usr/bin/claude"),
            Path::new("/tmp/work"),
            &env(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(plan.environment["CLAUDE_CONFIG_DIR"], "/tmp/claude-default");
        for key in [
            "TELEGRAM_BOT_TOKEN",
            "CODEX_HOME",
            "LD_PRELOAD",
            "OPENAI_API_KEY",
        ] {
            assert!(!plan.environment.contains_key(key));
        }
        assert!(plan.args.is_empty());
        assert!(!format!("{plan:?}").contains("private-token"));
        let command = plan.command();
        assert_eq!(command.get_program(), "/usr/bin/claude");
        assert_eq!(command.get_args().count(), 0);
    }
    #[test]
    fn xdg_profiles_are_scoped_without_mutating_caller() {
        let environment = env();
        let profiles = [
            ("cursor".into(), "/tmp/cursor config".into()),
            ("opencode".into(), "/tmp/opencode-data".into()),
        ]
        .into();
        let cursor = tool("cursor").unwrap();
        let plan = login_plan(
            &cursor,
            "link",
            Path::new("/usr/bin/cursor-agent"),
            Path::new("/tmp"),
            &environment,
            &profiles,
        )
        .unwrap();
        assert_eq!(plan.environment["XDG_CONFIG_HOME"], "/tmp/cursor config");
        assert_eq!(plan.environment["NO_OPEN_BROWSER"], "1");
        assert!(!environment.contains_key("XDG_CONFIG_HOME"));
        assert_eq!(
            account_directory(&cursor, &environment, &profiles).unwrap(),
            Path::new("/tmp/cursor config/cursor")
        );
        let codex = launch_plan(
            &tool("codex").unwrap(),
            Path::new("/usr/bin/codex"),
            Path::new("/tmp"),
            &environment,
            &profiles,
        )
        .unwrap();
        assert!(!codex.environment.contains_key("XDG_CONFIG_HOME"));
    }
    #[test]
    fn api_key_is_an_input_requirement_never_an_argument() {
        let plan = login_plan(
            &tool("codex").unwrap(),
            "api-key",
            Path::new("/usr/bin/codex"),
            Path::new("/tmp"),
            &env(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(plan.args, ["login", "--with-api-key"]);
        assert!(plan.secret_stdin);
    }
    #[test]
    fn codex_fork_capability_matches_checkpoint_argument_plan() {
        use crate::core::checkpoints::{self, Checkpoint, Operation};
        let tool = tool("codex").unwrap();
        let caps = checkpoint_capabilities(
            &tool,
            Path::new("/usr/bin/codex"),
            Some("Commands:\n resume Resume a session\n fork Fork a session"),
        )
        .unwrap();
        let checkpoint = Checkpoint {
            name: "Test".into(),
            harness: "codex".into(),
            sid: "session-123".into(),
            cwd: std::env::temp_dir().to_str().unwrap().into(),
            profile: String::new(),
            origin: String::new(),
            created_at: String::new(),
            context_file: String::new(),
            source: None,
        };
        let plan = checkpoints::plan(
            &checkpoint,
            Operation::NativeFork,
            &caps,
            &std::env::temp_dir(),
        )
        .unwrap();
        assert_eq!(plan.args, ["fork", "session-123"]);
        let without_fork = checkpoint_capabilities(
            &tool,
            Path::new("/usr/bin/codex"),
            Some("Commands:\n resume Resume a session"),
        )
        .unwrap();
        assert!(
            checkpoints::plan(
                &checkpoint,
                Operation::NativeFork,
                &without_fork,
                &std::env::temp_dir()
            )
            .is_err()
        );
    }
    #[test]
    fn the_default_profile_leaves_the_profile_variable_unset() {
        // Claude Code keeps its global config in ~/.claude.json only while
        // CLAUDE_CONFIG_DIR is unset; setting it to ~/.claude moves that file.
        let mut environment = env();
        environment.remove("CLAUDE_CONFIG_DIR");
        environment.remove("CODEX_HOME");
        for id in ["claude", "codex"] {
            let tool = tool(id).unwrap();
            let default = format!("/tmp/test-home/{}", tool.default_profile);
            for profiles in [BTreeMap::new(), [(id.to_string(), default.clone())].into()] {
                let plan = launch_plan(
                    &tool,
                    Path::new("/usr/bin/native"),
                    Path::new("/tmp/work"),
                    &environment,
                    &profiles,
                )
                .unwrap();
                assert!(
                    !plan.environment.contains_key(tool.profile_variable),
                    "{id}: {profiles:?}"
                );
            }
            let other: BTreeMap<String, String> = [(id.to_string(), "/tmp/other".into())].into();
            let plan = launch_plan(
                &tool,
                Path::new("/usr/bin/native"),
                Path::new("/tmp/work"),
                &environment,
                &other,
            )
            .unwrap();
            assert_eq!(plan.environment[tool.profile_variable], "/tmp/other");
        }
    }
    #[test]
    fn paths_stay_literal_and_capabilities_require_evidence() {
        let profiles = [("claude".into(), "/tmp/$(touch nope);profile".into())].into();
        let tool = tool("claude").unwrap();
        let plan = launch_plan(
            &tool,
            Path::new("/tmp/bin with space/claude"),
            Path::new("/tmp"),
            &env(),
            &profiles,
        )
        .unwrap();
        assert_eq!(plan.command().get_program(), "/tmp/bin with space/claude");
        assert_eq!(
            plan.environment["CLAUDE_CONFIG_DIR"],
            "/tmp/$(touch nope);profile"
        );
        assert!(
            !checkpoint_capabilities(&tool, &plan.binary, None)
                .unwrap()
                .resume
        );
        assert!(
            checkpoint_capabilities(&tool, &plan.binary, Some("--resume ID --fork-session"))
                .unwrap()
                .native_fork
        );
        assert!(
            !checkpoint_capabilities(
                &super::tool("hermes").unwrap(),
                Path::new("/usr/bin/hermes"),
                Some("--resume --fork-session")
            )
            .unwrap()
            .native_fork
        );
    }
    #[test]
    fn pi_capabilities_require_native_session_flags_not_other_provider_flags() {
        let tool = tool("pi").unwrap();
        let binary = Path::new("/fixture/pi");
        let unknown = checkpoint_capabilities(&tool, binary, None).unwrap();
        assert!(!unknown.resume && !unknown.native_fork && !unknown.handoff);
        let foreign =
            checkpoint_capabilities(&tool, binary, Some("--resume --fork-session")).unwrap();
        assert!(!foreign.resume && !foreign.native_fork);
        let native = checkpoint_capabilities(
            &tool,
            binary,
            Some("--session <path|id> --fork <path|id> --session-dir <dir>"),
        )
        .unwrap();
        assert!(native.resume && native.native_fork && native.handoff);
    }
}
