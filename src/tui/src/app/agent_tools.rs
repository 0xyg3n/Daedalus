//! Bounded JSON entrypoint over existing native CLI operations. This is not a
//! shell, MCP server, authorization bypass or replacement for operation reviews.
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, Read, Write};

const INPUT_LIMIT: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const ARG_LIMIT: usize = 256;
const ARG_BYTES: usize = 128 * 1024;
const HELP: &str = "daedalus agent capabilities\ndaedalus agent call < request.json\nOne bounded JSON request: {\"tool\":\"FAMILY\",\"args\":[\"OPTIONS\"],\"input\":\"optional goal directive\"}. Existing previews, approval digests and explicit --apply remain required. Interactive/continuous operations require the ordinary CLI/TUI.\n";
type Result<T> = std::result::Result<T, String>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    tool: String,
    args: Vec<String>,
    #[serde(default)]
    input: Option<String>,
}
struct LimitedOutput {
    bytes: Vec<u8>,
    exceeded: bool,
}
impl LimitedOutput {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            exceeded: false,
        }
    }
}
impl Write for LimitedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.exceeded || bytes.len() > OUTPUT_LIMIT.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("Agent operation output exceeds limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn operations(tool: &str) -> Option<&'static [&'static str]> {
    Some(match tool {
        "inventory" | "diagnostics" => &[],
        "terminal" => &[
            "list",
            "capture",
            "send",
            "interrupt",
            "close",
            "focus",
            "rename",
            "launch",
            "wait",
            "command",
            "keys",
            "control",
        ],
        "checkpoint" => &["list", "show", "save", "remove", "rm", "spawn"],
        "route" => &["list", "active", "candidates", "select"],
        "hook" => &[
            "install",
            "inspect",
            "profile-preview",
            "profile-apply",
            "profile-rollback",
            "profile-remove-preview",
            "profile-remove",
            "profile-remove-rollback",
            "launch-preview",
            "launch-apply",
            "launch-cleanup-preview",
            "launch-cleanup",
        ],
        "goal" => &["arm", "pause", "resume", "complete", "status"],
        "memory" => &["list", "status", "refresh"],
        "config" => &["init", "import", "import-instance", "validate"],
        "agent-identity" => &["preview", "apply", "status"],
        "update" => &["check"],
        "telegram" => &[
            "status", "start", "stop", "bind", "bindings", "config", "cutover",
        ],
        _ => return None,
    })
}
fn capabilities() -> Value {
    let families = [
        "inventory",
        "terminal",
        "checkpoint",
        "route",
        "hook",
        "goal",
        "memory",
        "config",
        "diagnostics",
        "agent-identity",
        "update",
        "telegram",
    ];
    json!({"protocol":"daedalus.agent.v1","tools":families.into_iter().map(|tool|json!({"tool":tool,"operations":operations(tool).unwrap()})).collect::<Vec<_>>(),
        "limits":{"request_bytes":INPUT_LIMIT,"operation_output_bytes":OUTPUT_LIMIT,"arguments":ARG_LIMIT,"argument_bytes":ARG_BYTES},
        "reviews":"Native parsers, previews and approval requirements apply unchanged; no flags are added.",
        "interactive_handoff":["tui","setup","terminal attach","hook launch","hook run","telegram serve","native provider sign-in"],
        "notice":"Capabilities are entrypoint coverage, not provider/platform support. terminal wait retains its native timeout. Authorized operations may call installed tools; no generic command runner is exposed."})
}
fn validate(request: &Request) -> std::result::Result<(), &'static str> {
    if request.args.len() > ARG_LIMIT
        || request
            .args
            .iter()
            .any(|arg| arg.len() > 16384 || arg.contains('\0'))
        || request.args.iter().map(String::len).sum::<usize>() > ARG_BYTES
    {
        return Err("invalid_arguments");
    }
    let verbs = operations(&request.tool).ok_or("unsupported_tool")?;
    let help = request.args.len() == 1 && matches!(request.args[0].as_str(), "--help" | "-h");
    if !verbs.is_empty()
        && !help
        && request
            .args
            .first()
            .is_none_or(|verb| !verbs.contains(&verb.as_str()))
    {
        return Err("unsupported_operation");
    }
    if request.input.is_some()
        && !(request.tool == "goal" && request.args.first().is_some_and(|verb| verb == "arm"))
    {
        return Err("unsupported_input");
    }
    Ok(())
}
fn dispatch(request: Request, output: &mut LimitedOutput) -> Result<()> {
    match request.tool.as_str() {
        "inventory" => super::inventory::run(&request.args, output),
        "terminal" => super::terminal_cli::run(&request.args, output),
        "checkpoint" => super::session_cli::run(&request.args, output),
        "route" => super::route_cli::run(&request.args, output),
        "hook" | "goal" => super::hook_cli::run(
            &request.tool,
            &request.args,
            request.input.unwrap_or_default().as_bytes(),
            output,
        ),
        "memory" => super::memory_status::run(&request.args, output),
        "config" => super::cli::run(&request.args, output),
        "diagnostics" => super::diagnostics::run(&request.args, output),
        "agent-identity" => super::agent_identity::run(&request.args, output),
        "update" => super::update::run(&request.args, output),
        "telegram"
            if request
                .args
                .first()
                .is_some_and(|verb| matches!(verb.as_str(), "config" | "cutover")) =>
        {
            super::telegram_service::run(&request.args, output)
        }
        "telegram" => super::telegram_cli::run(&request.args, output),
        _ => Err("Unsupported agent tool".into()),
    }
}
fn failure(code: &str) -> Value {
    // Never echo the request, provider errors, partially written output or paths
    // from a malformed request. The native CLI remains available for diagnostics.
    json!({"version":1,"ok":false,"error":{"code":code,"message":"Agent operation did not complete. Check native command help and diagnostics; do not automatically retry mutations."}})
}
fn call(reader: impl Read) -> Value {
    let mut bytes = Vec::new();
    if reader
        .take((INPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return failure("input_unavailable");
    }
    if bytes.len() > INPUT_LIMIT {
        return failure("input_limit");
    }
    let request: Request = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => return failure("invalid_request"),
    };
    if let Err(code) = validate(&request) {
        return failure(code);
    }
    let mut output = LimitedOutput::new();
    let result = dispatch(request, &mut output);
    if output.exceeded {
        return failure("output_limit");
    }
    if result.is_err() {
        return failure("operation_failed");
    }
    let result = serde_json::from_slice::<Value>(&output.bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&output.bytes).into_owned()));
    json!({"version":1,"ok":true,"result":result})
}
pub fn run(args: &[String], reader: impl Read, output: &mut impl Write) -> Result<()> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return output
            .write_all(HELP.as_bytes())
            .map_err(|_| "Cannot write agent help".into());
    }
    let value = if args == ["capabilities"] {
        json!({"version":1,"ok":true,"result":capabilities()})
    } else if args == ["call"] {
        call(reader)
    } else {
        failure("unsupported_operation")
    };
    let mut encoded = LimitedOutput::new();
    if serde_json::to_writer(&mut encoded, &value).is_err() {
        encoded = LimitedOutput::new();
        serde_json::to_writer(&mut encoded, &failure("output_limit"))
            .map_err(|_| "Cannot encode agent response")?;
    }
    output
        .write_all(&encoded.bytes)
        .map_err(|_| "Cannot write agent response")?;
    output
        .write_all(b"\n")
        .map_err(|_| "Cannot write agent response".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn invoke(value: Value) -> Value {
        call(serde_json::to_vec(&value).unwrap().as_slice())
    }
    #[test]
    fn rejects_unknown_fields_multiple_requests_and_bounded_input_without_echo() {
        let error = invoke(json!({"tool":"config","args":[],"secret":"DO_NOT_ECHO"}));
        assert_eq!(error["error"]["code"], "invalid_request");
        assert!(!error.to_string().contains("DO_NOT_ECHO"));
        assert_eq!(call(&b"{}{}"[..])["error"]["code"], "invalid_request");
        assert_eq!(
            call(std::io::repeat(b' ').take((INPUT_LIMIT + 1) as u64))["error"]["code"],
            "input_limit"
        );
        assert_eq!(
            invoke(json!({"tool":"config","args":vec!["x";ARG_LIMIT+1]}))["error"]["code"],
            "invalid_arguments"
        );
    }
    #[test]
    fn interactive_and_unknown_operations_reject_before_files_or_transport() {
        for (tool, verb) in [
            ("terminal", "attach"),
            ("hook", "run"),
            ("hook", "launch"),
            ("telegram", "serve"),
            ("shell", "anything"),
            ("config", "unknown"),
        ] {
            let result = invoke(json!({"tool":tool,"args":[verb,"--config-dir","DO_NOT_ECHO"]}));
            assert_eq!(result["ok"], false);
            assert!(!result.to_string().contains("DO_NOT_ECHO"));
            assert_ne!(result["error"]["code"], "operation_failed");
        }
    }
    #[test]
    fn output_bound_is_enforced_without_partial_success() {
        let mut writer = LimitedOutput::new();
        writer.write_all(&vec![b'x'; OUTPUT_LIMIT]).unwrap();
        assert!(writer.write_all(b"overflow").is_err());
        assert!(writer.exceeded);
        assert_eq!(writer.bytes.len(), OUTPUT_LIMIT);
    }
    #[test]
    fn config_preview_matches_native_cli_and_never_injects_apply() {
        let directory = std::env::temp_dir().join(format!(
            "daedalus-agent-preview-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let args = vec![
            "init".into(),
            "--config-dir".into(),
            directory.to_string_lossy().into_owned(),
            "--workspace".into(),
            std::env::temp_dir().to_string_lossy().into_owned(),
        ];
        let mut direct = Vec::new();
        super::super::cli::run(&args, &mut direct).unwrap();
        let wrapped = invoke(json!({"tool":"config","args":args}));
        assert_eq!(wrapped["ok"], true);
        assert_eq!(
            wrapped["result"],
            serde_json::from_slice::<Value>(&direct).unwrap()
        );
        assert!(!directory.exists());
    }
}
