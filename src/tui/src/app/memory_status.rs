//! Explicit local memory receipt inspection and provider status refresh.
//! Reading status never recalls content, resubmits retention or polls Telegram.
use super::{diagnostics, hook_runtime, memory, telegram_config};
use crate::{
    adapters::hindsight::{Http, OperationState, StatusApi},
    core::{
        journal::{self, Delivery, Journal},
        layouts,
    },
};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, String>;
const HELP: &str = "daedalus memory list --config-dir ABS [--after ID]\ndaedalus memory status --config-dir ABS --key MEMORY_KEY\ndaedalus memory refresh --config-dir ABS --key MEMORY_KEY\nList/status read local receipts only. Refresh queries the configured Hindsight operation status and saves its correlated receipt. It never retries or uploads memory. Provider errors and original memory contents are not printed.\n";

pub fn run(args: &[String], out: &mut impl Write) -> Result<()> {
    if args.is_empty() || args == ["--help"] {
        return write!(out, "{HELP}").map_err(|_| "Cannot write memory help".into());
    }
    let verb = args[0].as_str();
    if !matches!(verb, "list" | "status" | "refresh") {
        return Err("Unknown memory command".into());
    }
    let mut flags = std::collections::BTreeMap::new();
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        if !matches!(flag.as_str(), "--config-dir" | "--key" | "--after") {
            return Err("Unknown memory option".into());
        }
        let value = rest.next().ok_or("Missing memory option value")?;
        if flags.insert(flag.as_str(), value.as_str()).is_some() {
            return Err("Repeated memory option".into());
        }
    }
    let directory = Path::new(*flags.get("--config-dir").ok_or("Missing --config-dir")?);
    layouts::check_path(directory)?;
    let key = flags.get("--key").copied();
    if (verb == "list" && key.is_some())
        || (verb != "list" && (key.is_none() || flags.contains_key("--after")))
    {
        return Err("Options do not match memory operation".into());
    }
    if let Some(key) = key {
        validate_key(key)?;
    }
    let after = flags.get("--after").map_or(Ok(0), |v| {
        v.parse::<i64>().map_err(|_| "Invalid receipt cursor")
    })?;
    if after < 0 {
        return Err("Invalid receipt cursor".into());
    }
    let result = if verb == "refresh" {
        refresh(directory, key.unwrap())?
    } else {
        let rows = diagnostics::with_database(&directory.join("delivery.sqlite3"), |con| {
            journal::read_delivery_page(con, "memory:", after, key, 50)
                .map_err(|_| "Cannot inspect memory receipts".into())
        })?
        .unwrap_or_default();
        let rows: Vec<_> = rows
            .iter()
            .filter(|row| row.origin["kind"] == "verified_memory_hook")
            .map(summary)
            .collect();
        if verb == "status" {
            rows.into_iter().next().ok_or("Memory receipt not found")?
        } else {
            json!({"receipts":rows,"next_after":rows.last().and_then(|r|r["id"].as_i64()),"network_checked":false})
        }
    };
    serde_json::to_writer(&mut *out, &result).map_err(|_| "Cannot write memory status")?;
    writeln!(out).map_err(|_| "Cannot write memory status".into())
}
fn validate_key(key: &str) -> Result<()> {
    let suffix = key
        .strip_prefix("memory:")
        .ok_or("Invalid memory receipt key")?;
    if suffix.len() != 64 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid memory receipt key".into());
    }
    Ok(())
}
fn summary(row: &Delivery) -> Value {
    let receipt = row.receipt.as_ref();
    json!({"id":row.id,"key":row.dedup_key,"state":row.state,"operation":if row.payload["recall"]==true {"recall"}else{"retain"},"remote_state":receipt.and_then(|r|r.get("remote_state")),"observed_at":receipt.and_then(|r|r.get("observed_at")),"network_checked":false})
}
fn refresh(directory: &Path, key: &str) -> Result<Value> {
    let _active =
        hook_runtime::active_guard(directory).map_err(|_| "Memory instance is retired or busy")?;
    let _lock = telegram_config::configuration_lock(directory)?;
    let config = memory::load(directory)?.ok_or("Memory is not configured")?;
    let mut journal = Journal::open(&directory.join("delivery.sqlite3"))
        .map_err(|_| "Cannot open memory journal")?;
    let row = journal
        .delivery_for_key("outbound", key)
        .map_err(|_| "Cannot inspect memory receipt")?
        .ok_or("Memory receipt not found")?;
    validate_receipt(&config, &row)?;
    if row.state != "accepted" {
        return Ok(summary(&row));
    }
    let mut api = Http::new(
        &config.endpoint,
        config.key()?,
        Duration::from_millis(config.timeout_ms),
    )
    .map_err(|e| e.to_string())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Clock unavailable")?
        .as_secs_f64();
    observe(&mut api, &mut journal, &config, &row, now)
}
fn validate_receipt<'a>(
    config: &'a memory::Config,
    row: &'a Delivery,
) -> Result<(&'a str, &'a str)> {
    if row.direction != "outbound"
        || row.origin["kind"] != "verified_memory_hook"
        || row.origin["endpoint"] != config.endpoint
        || row.payload["recall"] != false
    {
        return Err("Receipt is not a retained operation at the configured endpoint".into());
    }
    let binding: memory::Binding = serde_json::from_value(row.origin["binding"].clone())
        .map_err(|_| "Invalid memory receipt binding")?;
    let binding = config
        .bindings
        .iter()
        .find(|candidate| {
            candidate.registration_id == binding.registration_id
                && candidate.scope == binding.scope
                && candidate.bank == binding.bank
        })
        .ok_or("Memory binding changed; no status request sent")?;
    let operation = row.payload["operation_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("Invalid memory operation identity")?;
    let receipt = row.receipt.as_ref().ok_or("Missing acceptance receipt")?;
    if receipt["operation_id"] != operation || receipt["bank"] != binding.bank {
        return Err("Acceptance receipt does not match memory operation".into());
    }
    Ok((&binding.bank, operation))
}
fn observe(
    api: &mut impl StatusApi,
    journal: &mut Journal,
    config: &memory::Config,
    row: &Delivery,
    now: f64,
) -> Result<Value> {
    if !now.is_finite() || now < 0.0 || row.state != "accepted" {
        return Err("Invalid memory observation".into());
    }
    let (bank, operation) = validate_receipt(config, row)?;
    let remote = api.operation(bank, operation).map_err(|e| e.to_string())?;
    if remote.bank != bank || remote.operation_id != operation {
        return Err("Uncorrelated memory status; receipt unchanged".into());
    }
    let state = match remote.state {
        OperationState::Completed => "delivered",
        OperationState::Failed => "blocked",
        OperationState::Cancelled => "cancelled",
        OperationState::Pending | OperationState::Processing | OperationState::NotFound => {
            "accepted"
        }
    };
    let receipt = json!({"bank":bank,"operation_id":operation,"state":state,"remote_state":remote.state,"observed_at":now});
    journal
        .settle_accepted(row, state, &receipt)
        .map_err(|_| "Memory receipt changed; refresh local status")?;
    let mut updated = row.clone();
    updated.state = state.into();
    updated.receipt = Some(receipt);
    let mut result = summary(&updated);
    result["network_checked"] = json!(true);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::hindsight::{self, Operation};
    struct Mock {
        calls: usize,
        result: std::result::Result<Operation, hindsight::Error>,
    }
    impl StatusApi for Mock {
        fn operation(
            &mut self,
            _: &str,
            _: &str,
        ) -> std::result::Result<Operation, hindsight::Error> {
            self.calls += 1;
            self.result.clone()
        }
    }
    fn fixture(state: OperationState) -> (memory::Config, Journal, Delivery, Mock) {
        let config:memory::Config=serde_json::from_value(json!({
            "version":1,"enabled":true,"endpoint":"http://127.0.0.1:8888","credential_file":null,
            "bindings":[{"registration_id":"fixture","bank":"bank","scope":{"instance":"instance","terminal":{"backend":"tmux","endpoint":"/tmp/synthetic.sock","pane":"%1","generation":"generation"},"conversation":"session","harness":"claude"}}],
            "recall_query":"project","timeout_ms":1000
        })).unwrap();
        config.validate().unwrap();
        let mut journal = Journal::memory().unwrap();
        let key = format!("memory:{}", "a".repeat(64));
        let payload =
            json!({"recall":false,"operation_id":"op","content":"private assistant text"});
        let origin = json!({"kind":"verified_memory_hook","endpoint":config.endpoint,"binding":config.bindings[0]});
        journal
            .enqueue("outbound", &key, &payload, &key, Some(&origin), 1.0)
            .unwrap();
        let pending = journal
            .begin("outbound", &key, "memory", 2.0, 30.0)
            .unwrap()
            .unwrap();
        journal
            .finish(
                &pending.delivery_id,
                pending.lease_token.as_deref().unwrap(),
                "accepted",
                Some(&json!({"bank":"bank","operation_id":"op","state":"accepted"})),
                None,
                0.0,
                3.0,
            )
            .unwrap();
        let row = journal.delivery_for_key("outbound", &key).unwrap().unwrap();
        let mock = Mock {
            calls: 0,
            result: Ok(Operation {
                bank: "bank".into(),
                operation_id: "op".into(),
                state,
            }),
        };
        (config, journal, row, mock)
    }
    #[test]
    fn accepted_requests_settle_only_on_correlated_terminal_status() {
        for (state, expected) in [
            (OperationState::Pending, "accepted"),
            (OperationState::Processing, "accepted"),
            (OperationState::NotFound, "accepted"),
            (OperationState::Completed, "delivered"),
            (OperationState::Failed, "blocked"),
            (OperationState::Cancelled, "cancelled"),
        ] {
            let (config, mut journal, row, mut api) = fixture(state);
            let result = observe(&mut api, &mut journal, &config, &row, 10.0).unwrap();
            assert_eq!(result["state"], expected);
            assert_eq!(result["network_checked"], true);
            assert!(!result.to_string().contains("private assistant text"));
            assert_eq!(
                journal
                    .delivery_for_key("outbound", &row.dedup_key)
                    .unwrap()
                    .unwrap()
                    .state,
                expected
            );
            assert_eq!(api.calls, 1);
            assert!(
                observe(&mut api, &mut journal, &config, &row, 11.0).is_err(),
                "stale receipt must not overwrite new status"
            );
        }
    }
    #[test]
    fn failure_or_changed_binding_never_replays_or_changes_acceptance() {
        let (mut config, mut journal, row, mut api) = fixture(OperationState::Completed);
        config.bindings[0].bank = "other".into();
        assert!(observe(&mut api, &mut journal, &config, &row, 10.0).is_err());
        assert_eq!(api.calls, 0);
        config.bindings[0].bank = "bank".into();
        for response in [
            Err(hindsight::Error::Uncertain),
            Ok(Operation {
                bank: "other".into(),
                operation_id: "op".into(),
                state: OperationState::Completed,
            }),
            Ok(Operation {
                bank: "bank".into(),
                operation_id: "other".into(),
                state: OperationState::Completed,
            }),
        ] {
            api.result = response;
            assert!(observe(&mut api, &mut journal, &config, &row, 10.0).is_err());
            assert_eq!(
                journal
                    .delivery_for_key("outbound", &row.dedup_key)
                    .unwrap(),
                Some(row.clone())
            );
        }
    }
    #[test]
    fn cli_requires_exact_memory_keys_and_explicit_refresh() {
        for command in [
            vec!["status", "--config-dir", "/tmp/unused", "--key", "other"],
            vec!["list", "--config-dir", "/tmp/unused", "--key", "memory:abc"],
            vec!["refresh", "--config-dir", "/tmp/unused"],
        ] {
            assert!(
                run(
                    &command.into_iter().map(String::from).collect::<Vec<_>>(),
                    &mut Vec::new()
                )
                .is_err()
            );
        }
    }
}
