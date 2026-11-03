//! Shared admission for private checkpoint registries, tombstones and writes.
use super::{json_preflight, layouts};
use serde::Serialize;
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
const FILE: &str =
    "Checkpoint file limit reached; review checkpoint_file_limit_bytes in Terminals and discovery";
const WORK: &str = "Checkpoint byte-work limit reached; review checkpoint_work_limit_bytes in Terminals and discovery";
const ENTRIES: &str =
    "Checkpoint entry limit reached; review checkpoint_entry_limit in Terminals and discovery";
const VALUES: &str =
    "Checkpoint JSON value limit reached; review checkpoint_value_limit in Terminals and discovery";
const TIME: &str = "Checkpoint processing deadline reached; review checkpoint_timeout_ms in Terminals and discovery";
const INVALID: &str = "Invalid or ambiguous checkpoint JSON";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub file_bytes: u64,
    pub work_bytes: u64,
    pub entries: u64,
    pub values: u64,
    pub timeout_ms: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            file_bytes: 8388608,
            work_bytes: 67108864,
            entries: 65536,
            values: 1000000,
            timeout_ms: 2000,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        if !(4096..=67108864).contains(&self.file_bytes)
            || !(4096..=1073741824).contains(&self.work_bytes)
            || !(1..=1048576).contains(&self.entries)
            || !(128..=16777216).contains(&self.values)
            || !(1..=60000).contains(&self.timeout_ms)
        {
            return Err("Invalid checkpoint processing policy".into());
        }
        Ok(())
    }
}
pub struct Guard<'a> {
    pub policy: Policy,
    deadline: Instant,
    work_left: u64,
    entries_left: u64,
    values_left: u64,
    progress: Option<&'a dyn Fn() -> Result<()>>,
}
impl<'a> Guard<'a> {
    pub fn new(policy: Policy) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            deadline: Instant::now() + Duration::from_millis(policy.timeout_ms),
            work_left: policy.work_bytes,
            entries_left: policy.entries,
            values_left: policy.values,
            policy,
            progress: None,
        })
    }
    pub fn with_progress(mut self, progress: &'a dyn Fn() -> Result<()>) -> Self {
        self.progress = Some(progress);
        self
    }
    pub fn constrain_deadline(&mut self, deadline: Instant) {
        self.deadline = self.deadline.min(deadline);
    }
    pub fn time_left(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(TIME.into());
        }
        if let Some(progress) = self.progress {
            progress()?;
        }
        Ok(())
    }
    pub fn entries(&mut self, count: usize) -> Result<()> {
        self.time_left()?;
        self.entries_left = self.entries_left.checked_sub(count as u64).ok_or(ENTRIES)?;
        Ok(())
    }
    fn check_json(&mut self, bytes: &[u8]) -> Result<()> {
        json_preflight::check(bytes, || {
            self.time_left()?;
            self.values_left = self.values_left.checked_sub(1).ok_or(VALUES)?;
            Ok(())
        })
        .map_err(|e| {
            if e == json_preflight::INVALID {
                INVALID.into()
            } else {
                e
            }
        })?;
        self.time_left()
    }
    pub fn read(&mut self, path: &Path) -> Result<Option<Vec<u8>>> {
        self.time_left()?;
        let limit = self.policy.file_bytes.min(self.work_left);
        let bytes = layouts::read_private_limit(path, limit).map_err(|error| {
            if error == "State file exceeds size limit" {
                if limit < self.policy.file_bytes {
                    WORK.into()
                } else {
                    FILE.into()
                }
            } else {
                error
            }
        })?;
        if let Some(bytes) = &bytes {
            self.work_left = self.work_left.checked_sub(bytes.len() as u64).ok_or(WORK)?;
            self.check_json(bytes)?;
        }
        self.time_left()?;
        Ok(bytes)
    }
    /// Bound serialization as it writes; no full oversized Vec is built first.
    pub fn encode(&mut self, value: &impl Serialize) -> Result<Vec<u8>> {
        self.time_left()?;
        struct Writer<'a, 'b> {
            guard: &'a Guard<'b>,
            bytes: Vec<u8>,
            limit: u64,
            error: Option<String>,
        }
        impl io::Write for Writer<'_, '_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let result = self.guard.time_left().and_then(|()| {
                    if self.bytes.len() as u64 + bytes.len() as u64 > self.limit {
                        Err(if self.limit < self.guard.policy.file_bytes {
                            WORK
                        } else {
                            FILE
                        }
                        .into())
                    } else {
                        Ok(())
                    }
                });
                if let Err(error) = result {
                    self.error = Some(error);
                    return Err(io::Error::other(INVALID));
                }
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Writer {
            guard: self,
            bytes: Vec::new(),
            limit: self.policy.file_bytes.min(self.work_left),
            error: None,
        };
        if serde_json::to_writer(&mut writer, value).is_err() {
            return Err(writer.error.unwrap_or_else(|| INVALID.into()));
        }
        let bytes = writer.bytes;
        self.work_left = self.work_left.checked_sub(bytes.len() as u64).ok_or(WORK)?;
        self.check_json(&bytes)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoint_serialization_stops_before_oversized_output_and_honors_external_progress() {
        let mut guard = Guard::new(Policy {
            file_bytes: 4096,
            ..Policy::default()
        })
        .unwrap();
        assert!(
            guard
                .encode(&"x".repeat(8192))
                .unwrap_err()
                .contains("file limit")
        );
        let progress = || Err("outer operation expired".into());
        let mut guard = Guard::new(Policy::default())
            .unwrap()
            .with_progress(&progress);
        assert_eq!(
            guard.encode(&"small").unwrap_err(),
            "outer operation expired"
        );
        let mut guard = Guard::new(Policy {
            work_bytes: 4096,
            ..Policy::default()
        })
        .unwrap();
        guard.encode(&"x".repeat(3000)).unwrap();
        assert!(
            guard
                .encode(&"x".repeat(2000))
                .unwrap_err()
                .contains("byte-work limit")
        );
    }
}
