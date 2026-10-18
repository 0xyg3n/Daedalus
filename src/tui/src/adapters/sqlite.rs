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
    path::Path,
    thread,
    time::{Duration, Instant},
};

pub type Result<T> = std::result::Result<T, &'static str>;
pub const MAX_DATABASE_BYTES: u64 = 256 * 1024 * 1024;

/// Destination must be a new file in an existing private directory. This never
/// replaces a database and never opens a source transaction for writing.
pub fn snapshot(source: &Path, destination: &Path, timeout: Duration) -> Result<()> {
    let start = Instant::now();
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
            || metadata.len() > MAX_DATABASE_BYTES
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
            > MAX_DATABASE_BYTES
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
    let source = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "Cannot open source database")?;
    source
        .progress_handler(10_000, Some(move || start.elapsed() >= timeout))
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
        .is_none_or(|size| size > MAX_DATABASE_BYTES)
    {
        return Err("Source database exceeds snapshot size limit");
    }
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
    #[cfg(windows)]
    crate::platform::prepare_sqlite(destination).map_err(|_| "Cannot protect snapshot journal")?;
    let mut guard = Incomplete(destination, true);
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
        .progress_handler(10_000, Some(move || start.elapsed() >= timeout))
        .map_err(|_| "Cannot bound snapshot verification work")?;
    {
        let backup =
            Backup::new(&source, &mut target).map_err(|_| "Cannot start SQLite snapshot")?;
        loop {
            if start.elapsed() >= timeout {
                return Err("Database snapshot timed out");
            }
            let result = backup.step(128).map_err(|_| "Database snapshot failed")?;
            let progress = backup.progress();
            if (progress.pagecount.max(0) as u64).saturating_mul(size) > MAX_DATABASE_BYTES {
                return Err("Source database grew beyond snapshot size limit");
            }
            match result {
                StepResult::Done => break,
                StepResult::More => (),
                StepResult::Busy | StepResult::Locked => thread::sleep(Duration::from_millis(5)),
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
    reserved
        .sync_all()
        .map_err(|_| "Cannot synchronize database snapshot")?;
    crate::platform::sync_directory(parent).map_err(|_| "Cannot synchronize snapshot directory")?;
    guard.1 = false;
    Ok(())
}

struct Incomplete<'a>(&'a Path, bool);
impl Drop for Incomplete<'_> {
    fn drop(&mut self) {
        if self.1 {
            let _ = fs::remove_file(self.0);
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut path = self.0.as_os_str().to_owned();
                path.push(suffix);
                let _ = fs::remove_file(path);
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
    fn committed_wal_is_included_and_uncommitted_changes_are_excluded() {
        let t = Temp::new();
        let path = t.0.join("source.sqlite3");
        let copy = t.0.join("copy.sqlite3");
        let source = Connection::open(&path).unwrap();
        source.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE receipts(id INTEGER,state TEXT); INSERT INTO receipts VALUES(1,'unknown'); BEGIN IMMEDIATE; INSERT INTO receipts VALUES(2,'pending');").unwrap();
        snapshot(&path, &copy, Duration::from_secs(2)).unwrap();
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
        assert!(snapshot(&source, &target, Duration::from_secs(1)).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        let link = t.0.join("link.db");
        symlink(&source, &link).unwrap();
        assert!(snapshot(&link, &t.0.join("other.db"), Duration::from_secs(1)).is_err());
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
        assert!(snapshot(&source, &target, Duration::ZERO).is_err());
        assert!(!target.exists());
        fs::write(&source, b"broken private data").unwrap();
        let error = snapshot(&source, &target, Duration::from_secs(1)).unwrap_err();
        assert!(!error.contains("private data"));
        assert!(!target.exists());
    }
}
