//! Consistent private SQLite snapshots, including committed WAL transactions.
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub type Result<T> = std::result::Result<T, &'static str>;
pub const MAX_DATABASE_BYTES: u64 = 256 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotOptions {
    pub timeout: Duration,
    pub limit_bytes: u64,
    pub pages_per_step: i32,
    pub busy_wait: Duration,
    pub progress_ops: i32,
}
impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            limit_bytes: MAX_DATABASE_BYTES,
            pages_per_step: 128,
            busy_wait: Duration::from_millis(5),
            progress_ops: 10_000,
        }
    }
}
impl SnapshotOptions {
    pub fn validate(&self) -> Result<()> {
        if self.timeout > Duration::from_secs(900)
            || !(4096..=MAX_DATABASE_BYTES).contains(&self.limit_bytes)
            || !(1..=4096).contains(&self.pages_per_step)
            || !(Duration::from_millis(1)..=Duration::from_secs(1)).contains(&self.busy_wait)
            || !(1..=1_000_000).contains(&self.progress_ops)
        {
            return Err("Invalid database snapshot limits");
        }
        Ok(())
    }
}
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}
fn deadline(start: Instant, timeout: Duration) -> Result<()> {
    if start.elapsed() >= timeout {
        Err("Database snapshot timed out")
    } else {
        Ok(())
    }
}

/// Destination must be a new file in an existing private directory. This never
/// replaces a database or existing sidecars and never opens a source transaction
/// for writing. The deadline is cooperative: filesystem calls can take longer,
/// but a copy that finishes after its budget is refused and cleaned up.
pub fn snapshot(source: &Path, destination: &Path, options: &SnapshotOptions) -> Result<()> {
    let start = Instant::now();
    options.validate()?;
    let timeout = options.timeout;
    deadline(start, timeout)?;
    crate::core::layouts::check_path(source).map_err(|_| "Invalid database source path")?;
    crate::core::layouts::check_path(destination)
        .map_err(|_| "Invalid database destination path")?;
    #[cfg(unix)]
    {
        let source_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(source)
            .map_err(|_| "Cannot inspect source database")?;
        let metadata = source_file
            .metadata()
            .map_err(|_| "Cannot inspect source database")?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.len() > options.limit_bytes
        {
            return Err("Source database must be an owned regular file within the size limit");
        }
        let parent = destination
            .parent()
            .ok_or("Database destination needs a private directory")?;
        let metadata =
            fs::metadata(parent).map_err(|_| "Cannot inspect database destination directory")?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err("Database destination directory must be private and owned by this user");
        }
    }
    #[cfg(windows)]
    {
        let file = crate::platform::open_private(source, false, false)
            .map_err(|_| "Cannot inspect private source database")?;
        if file
            .metadata()
            .map_err(|_| "Cannot inspect source database")?
            .len()
            > options.limit_bytes
        {
            return Err("Source database exceeds limit");
        }
        crate::platform::validate_private_dir(
            destination
                .parent()
                .ok_or("Missing private destination directory")?,
        )
        .map_err(|_| "Destination must be private")?;
    }
    let parent = destination
        .parent()
        .ok_or("Missing private destination directory")?;
    for suffix in ["-wal", "-shm", "-journal"] {
        match fs::symlink_metadata(sidecar(destination, suffix)) {
            Ok(_) => return Err("Snapshot destination has existing SQLite sidecar files"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot inspect snapshot destination sidecar files"),
        }
    }
    let source = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "Cannot open source database")?;
    source
        .progress_handler(
            options.progress_ops,
            Some(move || start.elapsed() >= timeout),
        )
        .map_err(|_| "Cannot bound source database work")?;
    source
        .busy_timeout(Duration::ZERO)
        .map_err(|_| "Cannot bound source database access")?;
    source
        .execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")
        .map_err(|_| "Cannot restrict source database access")?;
    let pages = u64::from(
        source
            .query_row::<u32, _, _>("PRAGMA page_count", [], |r| r.get(0))
            .map_err(|_| "Invalid source database")?,
    );
    let size = u64::from(
        source
            .query_row::<u32, _, _>("PRAGMA page_size", [], |r| r.get(0))
            .map_err(|_| "Invalid source database")?,
    );
    if pages
        .checked_mul(size)
        .is_none_or(|size| size > options.limit_bytes)
    {
        return Err("Source database exceeds snapshot size limit");
    }
    deadline(start, timeout)?;
    #[cfg(unix)]
    let reserved = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(destination)
        .map_err(|_| "Snapshot destination already exists or is unavailable")?;
    #[cfg(windows)]
    let reserved = crate::platform::create_private_new(destination)
        .map_err(|_| "Snapshot destination already exists or unavailable")?;
    let mut guard = Incomplete(destination, true);
    #[cfg(windows)]
    crate::platform::prepare_sqlite(destination).map_err(|_| "Cannot protect snapshot journal")?;
    let mut target = Connection::open_with_flags(
        destination,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "Cannot open snapshot destination")?;
    #[cfg(windows)]
    target
        .execute_batch(
            "PRAGMA temp_store=MEMORY; PRAGMA journal_mode=TRUNCATE; PRAGMA synchronous=FULL;",
        )
        .map_err(|_| "Cannot protect snapshot writes")?;
    target
        .busy_timeout(Duration::ZERO)
        .map_err(|_| "Cannot bound snapshot access")?;
    target
        .progress_handler(
            options.progress_ops,
            Some(move || start.elapsed() >= timeout),
        )
        .map_err(|_| "Cannot bound snapshot verification work")?;
    {
        let backup =
            Backup::new(&source, &mut target).map_err(|_| "Cannot start SQLite snapshot")?;
        loop {
            deadline(start, timeout)?;
            let result = backup
                .step(options.pages_per_step)
                .map_err(|_| "Database snapshot failed")?;
            deadline(start, timeout)?;
            let progress = backup.progress();
            if (progress.pagecount.max(0) as u64).saturating_mul(size) > options.limit_bytes {
                return Err("Source database grew beyond snapshot size limit");
            }
            match result {
                StepResult::Done => break,
                StepResult::More => (),
                StepResult::Busy | StepResult::Locked => thread::sleep(
                    options
                        .busy_wait
                        .min(timeout.saturating_sub(start.elapsed())),
                ),
                _ => return Err("Unsupported SQLite snapshot state"),
            }
        }
    }
    let check: String = target
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(|_| "Snapshot integrity check failed")?;
    if check != "ok" {
        return Err("Snapshot integrity check failed");
    }
    deadline(start, timeout)?;
    // A backup of a WAL-mode source can retain its mode. Consolidate the new,
    // exclusively owned copy so the destination is a standalone database file.
    #[cfg(unix)]
    let consolidate = "PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;";
    #[cfg(windows)]
    let consolidate = "PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=TRUNCATE;";
    target
        .execute_batch(consolidate)
        .map_err(|_| "Cannot consolidate database snapshot")?;
    target
        .close()
        .map_err(|_| "Cannot close database snapshot")?;
    if reserved
        .metadata()
        .map_err(|_| "Cannot inspect completed database snapshot")?
        .len()
        > options.limit_bytes
    {
        return Err("Completed database snapshot exceeds size limit");
    }
    reserved
        .sync_all()
        .map_err(|_| "Cannot synchronize database snapshot")?;
    crate::platform::sync_directory(parent).map_err(|_| "Cannot synchronize snapshot directory")?;
    deadline(start, timeout)?;
    guard.1 = false;
    Ok(())
}

struct Incomplete<'a>(&'a Path, bool);
impl Drop for Incomplete<'_> {
    fn drop(&mut self) {
        if self.1 {
            let _ = fs::remove_file(self.0);
            for suffix in ["-wal", "-shm", "-journal"] {
                let _ = fs::remove_file(sidecar(self.0, suffix));
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::{DirBuilderExt, symlink},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "daedalus-snapshot-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn failed_snapshot_preserves_existing_sidecar_evidence() {
        let t = Temp::new();
        let source = t.0.join("source.db");
        Connection::open(&source)
            .unwrap()
            .execute_batch(
                "CREATE TABLE receipts(state TEXT); INSERT INTO receipts VALUES('unconfirmed')",
            )
            .unwrap();
        for suffix in ["-wal", "-shm", "-journal"] {
            let target = t.0.join(format!("copy{suffix}.db"));
            let sidecar = std::path::PathBuf::from(format!("{}{suffix}", target.display()));
            fs::write(&sidecar, b"retained uncertain evidence").unwrap();
            assert!(
                snapshot(
                    &source,
                    &target,
                    &SnapshotOptions {
                        timeout: Duration::ZERO,
                        ..Default::default()
                    }
                )
                .is_err()
            );
            assert!(!target.exists());
            assert_eq!(fs::read(&sidecar).unwrap(), b"retained uncertain evidence");
            let error = snapshot(&source, &target, &SnapshotOptions::default()).unwrap_err();
            assert!(error.contains("existing SQLite sidecar"));
            assert!(!target.exists());
            assert_eq!(fs::read(&sidecar).unwrap(), b"retained uncertain evidence");
        }
        let target = t.0.join("linked.db");
        let retained = t.0.join("retained-evidence");
        fs::write(&retained, b"retained uncertain evidence").unwrap();
        let link = super::sidecar(&target, "-wal");
        symlink(&retained, &link).unwrap();
        assert!(snapshot(&source, &target, &SnapshotOptions::default()).is_err());
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(retained).unwrap(), b"retained uncertain evidence");
        assert!(!target.exists());
    }
    #[test]
    fn configured_limit_counts_committed_wal_and_small_steps_keep_uncertain_rows() {
        let t = Temp::new();
        let source_path = t.0.join("source.db");
        let source = Connection::open(&source_path).unwrap();
        source.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE receipts(state TEXT,payload BLOB); INSERT INTO receipts VALUES('unconfirmed',zeroblob(65536));").unwrap();
        let before = fs::read(&source_path).unwrap();
        let wal = fs::read(sidecar(&source_path, "-wal")).unwrap();
        assert_eq!(before.len(), 4096);
        let target = t.0.join("copy.db");
        let mut options = SnapshotOptions {
            limit_bytes: 4096,
            pages_per_step: 1,
            progress_ops: 1,
            busy_wait: Duration::from_millis(11),
            ..Default::default()
        };
        assert!(snapshot(&source_path, &target, &options).is_err());
        assert!(!target.exists());
        assert_eq!(fs::read(&source_path).unwrap(), before);
        assert_eq!(fs::read(sidecar(&source_path, "-wal")).unwrap(), wal);
        options.limit_bytes = 131072;
        snapshot(&source_path, &target, &options).unwrap();
        let copied = Connection::open(&target).unwrap();
        assert_eq!(
            copied
                .query_row("SELECT state,length(payload) FROM receipts", [], |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?
                )))
                .unwrap(),
            ("unconfirmed".into(), 65536)
        );
        assert_eq!(fs::read(&source_path).unwrap(), before);
        assert_eq!(fs::read(sidecar(&source_path, "-wal")).unwrap(), wal);
        assert!(fs::metadata(&target).unwrap().len() <= options.limit_bytes);
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!sidecar(&target, suffix).exists());
        }
    }
    #[test]
    fn configured_short_deadline_removes_only_the_new_incomplete_copy() {
        let t = Temp::new();
        let source = t.0.join("source.db");
        Connection::open(&source).unwrap().execute_batch("CREATE TABLE receipts(state TEXT,payload BLOB); INSERT INTO receipts VALUES('unconfirmed',zeroblob(8388608));").unwrap();
        let original = fs::read(&source).unwrap();
        let target = t.0.join("copy.db");
        let options = SnapshotOptions {
            timeout: Duration::from_millis(1),
            pages_per_step: 1,
            progress_ops: 1,
            busy_wait: Duration::from_secs(1),
            ..Default::default()
        };
        assert!(snapshot(&source, &target, &options).is_err());
        assert!(!target.exists());
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!sidecar(&target, suffix).exists());
        }
        assert_eq!(fs::read(source).unwrap(), original);
    }
    #[test]
    fn invalid_direct_options_refuse_before_creating_or_inspecting_files() {
        let t = Temp::new();
        for options in [
            SnapshotOptions {
                limit_bytes: 0,
                ..Default::default()
            },
            SnapshotOptions {
                limit_bytes: MAX_DATABASE_BYTES + 1,
                ..Default::default()
            },
            SnapshotOptions {
                pages_per_step: 0,
                ..Default::default()
            },
            SnapshotOptions {
                progress_ops: 0,
                ..Default::default()
            },
            SnapshotOptions {
                busy_wait: Duration::ZERO,
                ..Default::default()
            },
            SnapshotOptions {
                timeout: Duration::from_secs(901),
                ..Default::default()
            },
        ] {
            assert_eq!(
                snapshot(
                    &t.0.join("missing-source"),
                    &t.0.join("missing-destination"),
                    &options
                ),
                Err("Invalid database snapshot limits")
            );
        }
        assert_eq!(fs::read_dir(&t.0).unwrap().count(), 0);
    }
    #[test]
    fn committed_wal_is_included_and_uncommitted_changes_are_excluded() {
        let t = Temp::new();
        let path = t.0.join("source.sqlite3");
        let copy = t.0.join("copy.sqlite3");
        let source = Connection::open(&path).unwrap();
        source.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE receipts(id INTEGER,state TEXT); INSERT INTO receipts VALUES(1,'unknown'); BEGIN IMMEDIATE; INSERT INTO receipts VALUES(2,'pending');").unwrap();
        snapshot(
            &path,
            &copy,
            &SnapshotOptions {
                timeout: Duration::from_secs(2),
                ..Default::default()
            },
        )
        .unwrap();
        let target = Connection::open(&copy).unwrap();
        assert_eq!(
            target
                .query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            target
                .query_row("SELECT state FROM receipts", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "unknown"
        );
        assert_eq!(fs::metadata(&copy).unwrap().mode() & 0o777, 0o600);
        source.execute_batch("ROLLBACK").unwrap();
    }
    #[test]
    fn never_replaces_existing_destination_or_follows_symlinks() {
        let t = Temp::new();
        let source = t.0.join("source.db");
        let target = t.0.join("target.db");
        Connection::open(&source)
            .unwrap()
            .execute_batch("CREATE TABLE a(id)")
            .unwrap();
        fs::write(&target, b"keep").unwrap();
        assert!(
            snapshot(
                &source,
                &target,
                &SnapshotOptions {
                    timeout: Duration::from_secs(1),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        let link = t.0.join("link.db");
        symlink(&source, &link).unwrap();
        assert!(snapshot(&link, &t.0.join("other.db"), &SnapshotOptions::default()).is_err());
    }
    #[test]
    fn timeout_and_corruption_leave_no_partial_snapshot() {
        let t = Temp::new();
        let source = t.0.join("source.db");
        let target = t.0.join("target.db");
        Connection::open(&source)
            .unwrap()
            .execute_batch("CREATE TABLE a(id)")
            .unwrap();
        assert!(
            snapshot(
                &source,
                &target,
                &SnapshotOptions {
                    timeout: Duration::ZERO,
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(!target.exists());
        fs::write(&source, b"broken private data").unwrap();
        let error = snapshot(
            &source,
            &target,
            &SnapshotOptions {
                timeout: Duration::from_secs(1),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(!error.contains("private data"));
        assert!(!target.exists());
    }
}
