//! Offline provider override settings. A save never starts a provider.
use super::gateway::{self, Config, Plan};
use serde_json::json;
use std::{collections::BTreeMap, io::Write, path::Path};
type Result<T> = std::result::Result<T, String>;
pub const HELP: &str = "daedalus gateway config show --config-dir ABS\ndaedalus gateway config init --config-dir ABS --provider claude|codex --endpoint URL --model MODEL --credential-file ABS [--enabled true|false] [--apply]\ndaedalus gateway config set --config-dir ABS --key enabled|provider|endpoint|model|credential_file --value VALUE [--apply]\nInit starts disabled unless --enabled true is selected. Provider pairs the harness with its supported API format.\nChanges print a frozen review by default. --apply saves with a private backup and refuses changed files or retired instances.\nCredential entry accepts a private file path, never a token. Configuration commands do not contact a provider or launch a tool.\n";
pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    if args.is_empty() || args == ["--help"] || args == ["config"] || args == ["config", "--help"] {
        return write!(out, "{HELP}").map_err(|_| "Cannot write gateway help".into());
    }
    if args.first().is_none_or(|v| v != "config") {
        return Err("Use gateway config show, init or set".into());
    }
    let verb = args[1].as_str();
    if !["show", "init", "set"].contains(&verb) {
        return Err("Unknown gateway configuration command".into());
    }
    let mut flags = BTreeMap::new();
    let mut apply = false;
    let mut rest = args[2..].iter();
    while let Some(flag) = rest.next() {
        if flag == "--apply" {
            if apply || verb == "show" {
                return Err("Invalid or repeated gateway apply option".into());
            }
            apply = true;
            continue;
        }
        let allowed = flag == "--config-dir"
            || match verb {
                "init" => [
                    "--provider",
                    "--endpoint",
                    "--model",
                    "--credential-file",
                    "--enabled",
                ]
                .contains(&flag.as_str()),
                "set" => ["--key", "--value"].contains(&flag.as_str()),
                _ => false,
            };
        if !allowed {
            return Err("Unknown gateway configuration option".into());
        }
        let value = rest
            .next()
            .ok_or("Missing gateway configuration option value")?;
        if flags.insert(flag.as_str(), value.as_str()).is_some() {
            return Err("Repeated gateway configuration option".into());
        }
    }
    let value = |key: &str| -> Result<&str> {
        flags
            .get(key)
            .copied()
            .ok_or_else(|| format!("Missing {key}"))
    };
    let directory = Path::new(value("--config-dir")?);
    crate::core::layouts::check_path(directory)?;
    let current = gateway::load(directory)?;
    if verb == "show" {
        serde_json::to_writer(&mut *out,&json!({"configured":current.is_some(),"configuration":current,"editable_fields":gateway::FIELDS,"provider_pairs":{"claude":"anthropic_messages","codex":"openai_responses"},"network_verified":false})).map_err(|_|"Cannot write gateway settings")?;
        return writeln!(out).map_err(|_| "Cannot write gateway settings".into());
    }
    let updated = if verb == "init" {
        if current.is_some() {
            return Err("Gateway is already configured; use set".into());
        }
        let mut config = Config::fresh(value("--provider")?)?;
        for (key, flag) in [
            ("endpoint", "--endpoint"),
            ("model", "--model"),
            ("credential_file", "--credential-file"),
        ] {
            config = config.set(key, value(flag)?)?;
        }
        if let Some(enabled) = flags.get("--enabled") {
            config = config.set("enabled", enabled)?;
        }
        config
    } else {
        current
            .as_ref()
            .ok_or("Gateway is not configured; use init")?
            .set(value("--key")?, value("--value")?)?
    };
    let plan = Plan::prepare(directory, &current, updated)?;
    let review = plan.review();
    if apply {
        let approval = plan.approval();
        plan.apply(&approval)?;
    }
    serde_json::to_writer(
        &mut *out,
        &json!({"review":review,"saved":apply,"network_verified":false}),
    )
    .map_err(|_| "Cannot write gateway settings result")?;
    writeln!(out).map_err(|_| "Cannot write gateway settings result".into())
}
