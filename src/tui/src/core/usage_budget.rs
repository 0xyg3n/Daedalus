//! Cumulative admission for local workspace-selection aggregates, never pane text.
use rusqlite::Connection;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub max_gap_ms: u64,
    pub lease_ms: u64,
    pub retention_days: u64,
    pub entries: u64,
    pub work_bytes: u64,
    pub timeout_ms: u64,
    pub sql_steps: u64,
    pub database_bytes: u64,
    pub output_bytes: u64,
    pub busy_ms: u64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            max_gap_ms: 30000,
            lease_ms: 30000,
            retention_days: 0,
            entries: 16384,
            work_bytes: 16777216,
            timeout_ms: 2000,
            sql_steps: 1000000,
            database_bytes: 67108864,
            output_bytes: 4194304,
            busy_ms: 25,
        }
    }
}
impl Policy {
    pub fn validate(self) -> Result<()> {
        if !(1..=86400000).contains(&self.max_gap_ms)
            || !(1..=86400000).contains(&self.lease_ms)
            || self.retention_days > 36500
            || !(1..=1048576).contains(&self.entries)
            || !(4096..=1073741824).contains(&self.work_bytes)
            || !(1..=60000).contains(&self.timeout_ms)
            || !(1..=100000000).contains(&self.sql_steps)
            || !(4096..=536870912).contains(&self.database_bytes)
            || !(1024..=67108864).contains(&self.output_bytes)
            || self.busy_ms > 5000
        {
            return Err("Invalid workspace usage policy".into());
        }
        Ok(())
    }
}
const KEYS: [&str; 6] = [
    "workspace_usage_timeout_ms",
    "workspace_usage_sql_step_limit",
    "workspace_usage_entry_limit",
    "workspace_usage_work_limit_bytes",
    "workspace_usage_database_limit_bytes",
    "workspace_usage_output_limit_bytes",
];
fn limit(reason: u8) -> String {
    format!(
        "Workspace usage limit reached; review {} in Workspace usage settings",
        KEYS[(reason - 1) as usize]
    )
}
struct SqlState {
    failure: AtomicU8,
    remaining: AtomicU64,
    deadline: Instant,
}
impl SqlState {
    fn consume_instruction(&self) -> bool {
        let mut left = self.remaining.load(Ordering::Relaxed);
        loop {
            if left == 0 {
                return false;
            }
            match self.remaining.compare_exchange_weak(
                left,
                left - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(observed) => left = observed,
            }
        }
    }
}
pub struct Guard {
    pub policy: Policy,
    state: Arc<SqlState>,
    entries: u64,
    bytes: u64,
}
impl Guard {
    pub fn new(policy: Policy, deadline: Option<Instant>) -> Result<Self> {
        policy.validate()?;
        let own = Instant::now() + Duration::from_millis(policy.timeout_ms);
        Ok(Self {
            policy,
            state: Arc::new(SqlState {
                failure: AtomicU8::new(0),
                remaining: AtomicU64::new(policy.sql_steps),
                deadline: deadline.map_or(own, |d| d.min(own)),
            }),
            entries: policy.entries,
            bytes: policy.work_bytes,
        })
    }
    pub fn check(&self) -> Result<()> {
        if Instant::now() >= self.state.deadline {
            self.state
                .failure
                .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
                .ok();
        }
        match self.state.failure.load(Ordering::Relaxed) {
            0 => Ok(()),
            reason => Err(limit(reason)),
        }
    }
    fn fail<T>(&self, reason: u8) -> Result<T> {
        self.state
            .failure
            .compare_exchange(0, reason, Ordering::Relaxed, Ordering::Relaxed)
            .ok();
        Err(limit(self.state.failure.load(Ordering::Relaxed)))
    }
    pub fn entries(&mut self, count: usize) -> Result<()> {
        self.check()?;
        match self.entries.checked_sub(count as u64) {
            Some(left) => {
                self.entries = left;
                Ok(())
            }
            None => self.fail(3),
        }
    }
    pub fn bytes(&mut self, count: usize) -> Result<()> {
        self.check()?;
        match self.bytes.checked_sub(count as u64) {
            Some(left) => {
                self.bytes = left;
                Ok(())
            }
            None => self.fail(4),
        }
    }
    pub fn sql<T>(&self, result: rusqlite::Result<T>) -> Result<T> {
        self.check()?;
        result.map_err(|e| {
            if e.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
                self.fail::<()>(5).unwrap_err()
            } else if matches!(
                e.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            ) {
                "Workspace usage database is busy; review workspace_usage_busy_wait_ms".into()
            } else {
                "Workspace usage database query failed".into()
            }
        })
    }
    /// Install before schema/configuration queries, on a connection dedicated to
    /// this operation. The handler counts all VM instructions, including triggers.
    pub fn configure(&self, con: &Connection) -> Result<()> {
        self.check()?;
        self.sql(
            con.busy_timeout(
                Duration::from_millis(self.policy.busy_ms).min(
                    self.state
                        .deadline
                        .saturating_duration_since(Instant::now()),
                ),
            ),
        )?;
        let state = self.state.clone();
        self.sql(con.progress_handler(
            1,
            Some(move || {
                if Instant::now() >= state.deadline {
                    state
                        .failure
                        .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
                        .ok();
                }
                if !state.consume_instruction() {
                    state
                        .failure
                        .compare_exchange(0, 2, Ordering::Relaxed, Ordering::Relaxed)
                        .ok();
                }
                state.failure.load(Ordering::Relaxed) != 0
            }),
        ))
    }
    pub fn unhook(con: &Connection) {
        let _ = con.progress_handler(0, None::<fn() -> bool>);
    }
    pub fn database_files(&self, path: &Path) -> Result<()> {
        self.check()?;
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            match std::fs::symlink_metadata(name) {
                Ok(meta) if meta.len() > self.policy.database_bytes => return self.fail(5),
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err("Cannot inspect workspace usage database".into()),
            }
            self.check()?;
        }
        Ok(())
    }
    pub fn database(&self, con: &Connection, writable: bool) -> Result<()> {
        let pages: u32 = self.sql(con.query_row("PRAGMA page_count", [], |r| r.get(0)))?;
        let size: u32 = self.sql(con.query_row("PRAGMA page_size", [], |r| r.get(0)))?;
        if size == 0
            || u64::from(pages)
                .checked_mul(u64::from(size))
                .is_none_or(|bytes| bytes > self.policy.database_bytes)
        {
            return self.fail(5);
        }
        if writable {
            self.sql(con.query_row(
                &format!(
                    "PRAGMA max_page_count={}",
                    self.policy.database_bytes / u64::from(size)
                ),
                [],
                |r| r.get::<_, u32>(0),
            ))?;
        }
        self.check()
    }
    pub fn encode<T: serde::Serialize>(&mut self, value: &T) -> Result<Vec<u8>> {
        struct Output<'a> {
            guard: &'a mut Guard,
            bytes: Vec<u8>,
        }
        impl std::io::Write for Output<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() as u64 > self.guard.policy.output_bytes - self.bytes.len() as u64 {
                    return Err(std::io::Error::other(self.guard.fail::<()>(6).unwrap_err()));
                }
                self.guard
                    .bytes(bytes.len())
                    .map_err(std::io::Error::other)?;
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut out = Output {
            guard: self,
            bytes: Vec::new(),
        };
        let result = serde_json::to_writer(&mut out, value);
        out.guard.check()?;
        result.map_err(|_| "Cannot encode workspace usage")?;
        Ok(out.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entry_work_output_and_outer_deadline_failures_remain_sticky() {
        let mut guard = Guard::new(
            Policy {
                entries: 1,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        guard.entries(1).unwrap();
        assert!(
            guard
                .entries(1)
                .unwrap_err()
                .contains("workspace_usage_entry_limit")
        );
        assert!(guard.bytes(0).is_err());
        let mut guard = Guard::new(
            Policy {
                work_bytes: 4096,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        guard.bytes(4000).unwrap();
        assert!(
            guard
                .bytes(97)
                .unwrap_err()
                .contains("workspace_usage_work_limit_bytes")
        );
        let mut guard = Guard::new(
            Policy {
                output_bytes: 1024,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(
            guard
                .encode(&"x".repeat(2048))
                .unwrap_err()
                .contains("workspace_usage_output_limit_bytes")
        );
        assert!(
            Guard::new(Policy::default(), Some(Instant::now()))
                .unwrap()
                .check()
                .unwrap_err()
                .contains("workspace_usage_timeout_ms")
        );
    }
    #[test]
    fn sql_instruction_and_logical_database_bounds_are_enforced() {
        let con = Connection::open_in_memory().unwrap();
        let guard = Guard::new(
            Policy {
                sql_steps: 1,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        guard.configure(&con).unwrap();
        assert!(
            guard
                .sql(con.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)))
                .unwrap_err()
                .contains("workspace_usage_sql_step_limit")
        );
        Guard::unhook(&con);
        assert_eq!(
            con.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        con.execute_batch("CREATE TABLE fixture(value); INSERT INTO fixture VALUES(1);")
            .unwrap();
        let guard = Guard::new(
            Policy {
                database_bytes: 4096,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        guard.configure(&con).unwrap();
        assert!(
            guard
                .database(&con, false)
                .unwrap_err()
                .contains("workspace_usage_database_limit_bytes")
        );
        Guard::unhook(&con);
        assert_eq!(
            con.query_row("SELECT value FROM fixture", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
