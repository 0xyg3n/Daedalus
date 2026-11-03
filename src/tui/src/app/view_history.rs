//! Bounded, strict private viewer-history processing. Reads never rewrite state.
use super::{discovery_budget::json_size, runtime_settings::Settings};
use crate::core::layouts;
use serde::Serialize;
use serde_json::Value;
use std::{
    path::Path,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;
const TIME: &str = "Saved viewer history processing deadline reached; review Viewer history processing deadline in Terminals and discovery";
const VALUES: &str = "Saved viewer history value budget reached; review Viewer history JSON values in Terminals and discovery";
const BYTES: &str = "Saved viewer history exceeds its byte budget; review Viewer history file limit in Terminals and discovery";
const INVALID: &str = "Invalid or ambiguous viewer history JSON";

pub struct Guard {
    deadline: Instant,
    remaining: usize,
    pub file_limit: usize,
    failed: Option<&'static str>,
}
impl Guard {
    pub fn new(settings: &Settings) -> Result<Self> {
        settings.validate()?;
        Ok(Self {
            deadline: Instant::now()
                + Duration::from_millis(settings.terminal_view_history_timeout_ms),
            remaining: settings.terminal_view_history_value_limit as usize,
            file_limit: settings.terminal_view_history_limit_bytes as usize,
            failed: None,
        })
    }
    pub fn time_left(&self) -> Result<()> {
        if let Some(error) = self.failed {
            return Err(error.into());
        }
        if Instant::now() >= self.deadline {
            return Err(TIME.into());
        }
        Ok(())
    }
    fn step(&mut self) -> Result<()> {
        self.time_left()?;
        if self.remaining == 0 {
            self.failed = Some(VALUES);
            return Err(VALUES.into());
        }
        self.remaining -= 1;
        Ok(())
    }
    /// Count values and keys without constructing Value trees. Duplicate keys
    /// are refused before serde_json can overwrite an identity or version.
    pub fn check_json(&mut self, bytes: &[u8]) -> Result<()> {
        self.time_left()?;
        if let Err(error) = crate::core::json_preflight::check(bytes, || self.step()) {
            if error == crate::core::json_preflight::INVALID {
                self.failed = Some(INVALID);
                return Err(INVALID.into());
            }
            return Err(error);
        }
        self.time_left()
    }
    /// Called before admitting an atomic write. Filesystem calls can finish
    /// later; their actual result is retained rather than replaying a write.
    pub fn encode(&mut self, value: &impl Serialize) -> Result<Vec<u8>> {
        self.time_left()?;
        json_size(value, self.file_limit).map_err(|_| BYTES)?;
        self.time_left()?;
        let bytes = serde_json::to_vec(value).map_err(|_| INVALID)?;
        self.check_json(&bytes)?;
        Ok(bytes)
    }
}
pub(crate) struct Snapshot {
    pub bytes: Vec<u8>,
    rows: Vec<Value>,
}

/// One validated read supplies both exact transport bytes and observation rows.
/// Import preserves unresolved records; live identity admission remains separate.
pub(crate) fn snapshot(directory: &Path, guard: &mut Guard) -> Result<Option<Snapshot>> {
    guard.time_left()?;
    let bytes = layouts::read_private_limit(&directory.join("views.json"), guard.file_limit as u64)
        .map_err(|e| {
            if e == "State file exceeds size limit" {
                BYTES.into()
            } else {
                e
            }
        })?;
    guard.time_left()?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    guard.check_json(&bytes)?;
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    guard.time_left()?;
    let object = value.as_object().ok_or(INVALID)?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "version" | "views"))
        || value.get("version").is_some_and(|v| v != 2)
    {
        return Err("Unsupported viewer registry version or fields".into());
    }
    let Value::Array(rows) = value["views"].take() else {
        return Err(INVALID.into());
    };
    Ok(Some(Snapshot { bytes, rows }))
}

pub fn read(directory: &Path, guard: &mut Guard) -> Result<Vec<Value>> {
    Ok(snapshot(directory, guard)?.map_or_else(Vec::new, |saved| saved.rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn cumulative_values_keys_duplicates_trailers_and_late_processing_are_refused() {
        let settings = Settings {
            terminal_view_history_value_limit: 128,
            ..Default::default()
        };
        let mut guard = Guard::new(&settings).unwrap();
        let bytes = serde_json::to_vec(&json!({"items":vec![0;120]})).unwrap();
        guard.check_json(&bytes).unwrap();
        assert!(guard.check_json(br#"{"a":1,"b":2,"c":3}"#).is_err());
        for bytes in [
            br#"{"a":1,"a":2}"#.as_slice(),
            br#"{"nested":{"id":1,"id":2}}"#,
            br#"{"x":1} {}"#,
        ] {
            assert!(Guard::new(&settings).unwrap().check_json(bytes).is_err());
        }
        let mut guard = Guard::new(&settings).unwrap();
        guard.deadline = Instant::now();
        assert!(guard.check_json(b"{}").unwrap_err().contains("deadline"));
    }
    #[test]
    fn encoding_rejects_size_before_staging_the_durable_payload() {
        let settings = Settings {
            terminal_view_history_limit_bytes: 4096,
            ..Default::default()
        };
        let mut guard = Guard::new(&settings).unwrap();
        assert!(
            guard
                .encode(&json!({"large":"x".repeat(4096)}))
                .unwrap_err()
                .contains("byte budget")
        );
        assert!(
            Guard::new(&settings)
                .unwrap()
                .encode(&json!({"views":[]}))
                .is_ok()
        );
    }
}
