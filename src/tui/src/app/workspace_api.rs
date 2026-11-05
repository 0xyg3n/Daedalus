//! Read-only recorded Herdr selection joined to current terminal identities.
use crate::core::usage_budget::Guard;
#[cfg(test)]
use crate::core::usage_budget::Policy;
use serde_json::{Value, json};
use std::{io::Write, path::Path};
type Result<T> = std::result::Result<T, String>;
const HELP: &str = "daedalus workspace-usage --config-dir ABS --json\nRead recorded Herdr server-selection aggregates for current terminal identities. Does not record usage or capture pane text; selection does not prove human attention. Saved Workspace usage limits apply; inventory uses its own saved discovery policy.\n";
struct InventoryOutput {
    bytes: Vec<u8>,
    limit: u64,
    exceeded: bool,
}
impl Write for InventoryOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.limit.saturating_sub(self.bytes.len() as u64) {
            self.exceeded = true;
            return Err(std::io::Error::other(
                "Workspace inventory limit reached; review workspace_usage_output_limit_bytes",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn run(args: &[String], output: &mut impl Write) -> Result<()> {
    if args == ["--help"] || args == ["-h"] {
        return output
            .write_all(HELP.as_bytes())
            .map_err(|_| "Cannot write usage help".into());
    }
    let mut directory = None;
    let mut json_output = false;
    let mut options = args.iter();
    while let Some(option) = options.next() {
        match option.as_str() {
            "--config-dir" if directory.is_none() => {
                directory = Some(options.next().ok_or("Missing configuration directory")?)
            }
            "--json" if !json_output => json_output = true,
            _ => return Err("Invalid workspace-usage option; use --help".into()),
        }
    }
    let directory = Path::new(directory.ok_or("Missing --config-dir")?);
    if !directory.is_absolute() || !json_output {
        return Err("Use --config-dir ABS --json".into());
    }
    let config = super::config::ControlConfig::parse(
        &crate::core::layouts::read_private(&directory.join("control.json"))?
            .ok_or("Instance configuration is missing")?,
    )?;
    let policy = config.runtime.workspace_usage_policy();
    policy.validate()?;
    let mut inventory = InventoryOutput {
        bytes: Vec::new(),
        limit: policy.output_bytes,
        exceeded: false,
    };
    super::inventory::run(args, &mut inventory).map_err(|error| {
        if inventory.exceeded {
            "Workspace inventory limit reached; review workspace_usage_output_limit_bytes".into()
        } else {
            error
        }
    })?;
    let mut guard = Guard::new(policy, None)?;
    guard.bytes(inventory.bytes.len())?;
    let snapshot: Value =
        serde_json::from_slice(&inventory.bytes).map_err(|_| "Invalid native inventory")?;
    let result = read_with(
        directory,
        &snapshot,
        &mut guard,
        config.ui.workspace_usage_recording,
    )?;
    let mut bytes = guard.encode(&result)?;
    if bytes.len() as u64 >= policy.output_bytes {
        return Err(
            "Workspace usage limit reached; review workspace_usage_output_limit_bytes".into(),
        );
    }
    guard.bytes(1)?;
    bytes.push(b'\n');
    output
        .write_all(&bytes)
        .map_err(|_| "Cannot write workspace usage".into())
}
fn read_with(
    directory: &Path,
    snapshot: &Value,
    guard: &mut Guard,
    recording: bool,
) -> Result<Value> {
    let prepared = super::workspace_usage::prepare(snapshot, guard, false)?;
    let path = directory.join("workspace-usage.sqlite3");
    guard.database_files(&path)?;
    let database =
        super::diagnostics::open_database_with(&path, guard.policy.database_bytes, |con| {
            guard.configure(con)
        })
        .map_err(|error| guard.check().err().unwrap_or(error))?;
    let records=match database {
        None=>None,
        Some(database)=>Some(database.project(|con| {
            guard.database(con,false)?;
            let table:bool=guard.sql(con.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='workspace_usage' AND rootpage>0)",[],|r|r.get(0)))?;
            if !table {return Err("Workspace usage schema is unavailable".into());}
            super::workspace_usage::aggregates(con,&prepared.live,guard)
        })?),
    };
    let mut terminals = Vec::new();
    for (key, terminal) in prepared.live {
        guard.entries(1)?;
        let usage = records.as_ref().and_then(|rows| rows.get(&key));
        terminals
            .push(json!({"key":key,"terminal":terminal,"recorded":usage.is_some(),"usage":usage}));
    }
    guard.check()?;
    Ok(
        json!({"version":1,"status":if records.is_some(){"available"}else{"missing"},"recording_enabled":recording,"retention_days":guard.policy.retention_days,"observed_at":snapshot["observed_at"],"discovery_complete":snapshot["errors"].as_array().is_some_and(Vec::is_empty),"discovery_errors":snapshot["errors"].as_array().map_or(0,Vec::len),"measurement":"Herdr server selection observed by the local TUI; not human attention or agent execution time","terminals":terminals}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn read(directory: &Path, snapshot: &Value) -> Result<Value> {
        read_with(
            directory,
            snapshot,
            &mut Guard::new(Policy::default(), None)?,
            true,
        )
    }

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "daedalus-usage-api-{}-{}",
                std::process::id(),
                crate::test_support::unique_nanos()
            ));
            crate::core::layouts::write_private(&path.join("fixture"), b"fixture").unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn snapshot(generation: &str) -> Value {
        json!({"panes":[{"backend":"herdr","session":"fixture","pane_id":"p1","terminal_id":generation,"content":"NEVER_EXPOSE_TEXT"}],"errors":[],"observed_at":100})
    }

    #[test]
    fn saved_read_budgets_refuse_without_mutating_existing_aggregates() {
        let fixture = Fixture::new();
        let snapshot = snapshot("current");
        let path = fixture.0.join("workspace-usage.sqlite3");
        let key = super::super::workspace_usage::key(&snapshot["panes"][0]).unwrap();
        let con = super::super::telegram_config::private_connection(&path).unwrap();
        con.execute_batch("CREATE TABLE workspace_usage(terminal TEXT PRIMARY KEY,visits INTEGER,seconds REAL,last_selected REAL)").unwrap();
        con.execute("INSERT INTO workspace_usage VALUES(?,2,3.5,95)", [key])
            .unwrap();
        drop(con);
        let before = std::fs::read(&path).unwrap();
        for (policy, key) in [
            (
                Policy {
                    entries: 1,
                    ..Default::default()
                },
                "workspace_usage_entry_limit",
            ),
            (
                Policy {
                    sql_steps: 1,
                    ..Default::default()
                },
                "workspace_usage_sql_step_limit",
            ),
            (
                Policy {
                    database_bytes: 4096,
                    ..Default::default()
                },
                "workspace_usage_database_limit_bytes",
            ),
        ] {
            let mut guard = Guard::new(policy, None).unwrap();
            assert!(
                read_with(&fixture.0, &snapshot, &mut guard, false)
                    .unwrap_err()
                    .contains(key)
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        let result = read_with(
            &fixture.0,
            &snapshot,
            &mut Guard::new(Policy::default(), None).unwrap(),
            false,
        )
        .unwrap();
        assert_eq!(result["recording_enabled"], false);
        assert_eq!(result["terminals"][0]["usage"]["seconds"], 3.5);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    #[test]
    fn captured_inventory_and_encoded_response_limits_refuse_complete_buffers() {
        let mut output = InventoryOutput {
            bytes: vec![],
            limit: 4,
            exceeded: false,
        };
        output.write_all(b"123").unwrap();
        assert!(output.write_all(b"45").is_err());
        assert!(output.exceeded);
        assert_eq!(output.bytes, b"123");
        let fixture = Fixture::new();
        let mut data = snapshot("current");
        data["panes"]=Value::Array((0..20).map(|i|json!({"backend":"herdr","session":"fixture","pane_id":format!("p{i}"),"terminal_id":"generation","content":"NEVER_EXPOSE_TEXT"})).collect());
        let mut guard = Guard::new(
            Policy {
                output_bytes: 1024,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let result = read_with(&fixture.0, &data, &mut guard, false).unwrap();
        assert!(
            guard
                .encode(&result)
                .unwrap_err()
                .contains("workspace_usage_output_limit_bytes")
        );
        assert!(!fixture.0.join("workspace-usage.sqlite3").exists());
    }
    #[test]
    fn missing_usage_never_creates_database_or_claims_zero_usage() {
        let fixture = Fixture::new();
        let result = read(&fixture.0, &snapshot("first")).unwrap();
        assert_eq!(result["status"], "missing");
        assert_eq!(result["terminals"][0]["recorded"], false);
        assert_eq!(result["terminals"][0]["usage"], Value::Null);
        assert!(!fixture.0.join("workspace-usage.sqlite3").exists());
        assert!(!result.to_string().contains("NEVER_EXPOSE_TEXT"));
    }
    #[test]
    fn only_live_generations_are_returned_without_recording_or_writing() {
        let fixture = Fixture::new();
        let old = snapshot("old");
        let oldkey = super::super::workspace_usage::key(&old["panes"][0]).unwrap();
        let current = snapshot("current");
        let key = super::super::workspace_usage::key(&current["panes"][0]).unwrap();
        let path = fixture.0.join("workspace-usage.sqlite3");
        let con = super::super::telegram_config::private_connection(&path).unwrap();
        con.execute_batch("CREATE TABLE workspace_usage(terminal TEXT PRIMARY KEY,visits INTEGER,seconds REAL,last_selected REAL)").unwrap();
        con.execute("INSERT INTO workspace_usage VALUES(?,7,12.5,90)", [&oldkey])
            .unwrap();
        con.execute("INSERT INTO workspace_usage VALUES(?,2,3.5,95)", [&key])
            .unwrap();
        drop(con);
        let before = std::fs::read(&path).unwrap();
        let result = read(&fixture.0, &current).unwrap();
        assert_eq!(result["terminals"].as_array().unwrap().len(), 1);
        assert_eq!(result["terminals"][0]["key"], key);
        assert_eq!(result["terminals"][0]["usage"]["seconds"], 3.5);
        assert!(!result.to_string().contains(&oldkey));
        assert!(!result.to_string().contains("NEVER_EXPOSE_TEXT"));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
    #[test]
    fn incompatible_or_invalid_aggregates_fail_without_initializing_schema() {
        let fixture = Fixture::new();
        let path = fixture.0.join("workspace-usage.sqlite3");
        let snapshot = snapshot("current");
        let key = super::super::workspace_usage::key(&snapshot["panes"][0]).unwrap();
        let con = super::super::telegram_config::private_connection(&path).unwrap();
        drop(con);
        let before = std::fs::read(&path).unwrap();
        assert!(read(&fixture.0, &snapshot).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let con = super::super::telegram_config::private_connection(&path).unwrap();
        con.execute_batch("CREATE TABLE workspace_usage(terminal TEXT PRIMARY KEY,visits INTEGER,seconds REAL,last_selected REAL)").unwrap();
        con.execute("INSERT INTO workspace_usage VALUES(?,-1,2,3)", [&key])
            .unwrap();
        drop(con);
        assert!(read(&fixture.0, &snapshot).is_err());
    }
    #[test]
    fn invalid_options_reject_before_inventory_or_file_creation() {
        for options in [
            vec!["--config-dir", "relative", "--json"],
            vec!["--config-dir", "/unused", "--json", "--apply"],
            vec!["--json"],
        ] {
            assert!(
                run(
                    &options.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                    &mut Vec::new()
                )
                .is_err()
            );
        }
    }
}
