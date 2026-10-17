//! Read-only projection of recorded Herdr selection, joined to current identities.
//! Discovery never samples usage or captures terminal contents.
use crate::adapters::terminals;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write, path::Path};
type Result<T> = std::result::Result<T, String>;
const HELP: &str = "daedalus workspace-usage --config-dir ABS --json\nRead recorded Herdr server-selection aggregates for currently discovered terminal identities. Does not record usage or capture pane text; selection does not prove human attention.\n";

struct InventoryOutput(Vec<u8>);
impl Write for InventoryOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > (4 * 1024 * 1024usize).saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "Workspace inventory exceeds output limit",
            ));
        }
        self.0.extend_from_slice(bytes);
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
    let mut inventory = InventoryOutput(Vec::new());
    super::inventory::run(args, &mut inventory)?;
    let snapshot: Value =
        serde_json::from_slice(&inventory.0).map_err(|_| "Invalid native inventory")?;
    let result = read(directory, &snapshot)?;
    serde_json::to_writer(&mut *output, &result).map_err(|_| "Cannot write workspace usage")?;
    writeln!(output).map_err(|_| "Cannot write workspace usage".into())
}

fn read(directory: &Path, snapshot: &Value) -> Result<Value> {
    let mut live = BTreeMap::new();
    for pane in snapshot["panes"].as_array().into_iter().flatten() {
        if pane["backend"] != "herdr" {
            continue;
        }
        let Ok(identity) = terminals::identity(pane) else {
            continue;
        };
        let Some(key) = super::workspace_usage::key(pane) else {
            continue;
        };
        live.insert(key, identity);
        if live.len() > 16384 {
            return Err("Workspace inventory exceeds limit".into());
        }
    }
    let records = super::diagnostics::with_database(
        &directory.join("workspace-usage.sqlite3"),
        |con| {
            let table: bool = con.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='workspace_usage' AND rootpage>0)", [], |row| row.get(0)).map_err(|_| "Cannot inspect usage schema")?;
            if !table {
                return Err("Workspace usage schema is unavailable".into());
            }
            let mut query = con
                .prepare(
                    "SELECT visits,seconds,last_selected FROM workspace_usage WHERE terminal=?",
                )
                .map_err(|_| "Cannot query workspace usage")?;
            let mut records = BTreeMap::new();
            for key in live.keys() {
                let record = query
                    .query_row([key], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, f64>(1)?,
                            row.get::<_, f64>(2)?,
                        ))
                    })
                    .optional()
                    .map_err(|_| "Invalid workspace usage record")?;
                if let Some((visits, seconds, last)) = record {
                    if visits < 0
                        || !seconds.is_finite()
                        || seconds < 0.0
                        || !last.is_finite()
                        || last < 0.0
                    {
                        return Err("Invalid workspace usage aggregate".into());
                    }
                    records.insert(
                        key.clone(),
                        json!({"visits":visits,"seconds":seconds,"last_selected":last}),
                    );
                }
            }
            Ok(records)
        },
    )?;
    let status = if records.is_some() {
        "available"
    } else {
        "missing"
    };
    let terminals = live
        .into_iter()
        .map(|(key, terminal)| {
            let usage = records.as_ref().and_then(|rows| rows.get(&key));
            json!({"key":key,"terminal":terminal,"recorded":usage.is_some(),"usage":usage})
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"version":1,"status":status,"observed_at":snapshot["observed_at"],"discovery_complete":snapshot["errors"].as_array().is_some_and(Vec::is_empty),"discovery_errors":snapshot["errors"].as_array().map_or(0,Vec::len),"measurement":"Herdr server selection observed by the local TUI; not human attention or agent execution time","terminals":terminals}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "daedalus-usage-api-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
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
