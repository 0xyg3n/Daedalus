//! Source-selected admission for retained snapshots and cumulative processing.
use super::runtime_settings::Settings;
use crate::{
    adapters::sqlite,
    core::{json_preflight, layouts},
};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    ops::Deref,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;
const BYTES: &str =
    "Instance import retained-byte budget reached; review instance_import_limit_bytes";
const ENTRIES: &str = "Instance import entry budget reached; review instance_import_entry_limit";
const VALUES: &str =
    "Instance import JSON work budget reached; review instance_import_json_value_limit";
const WORK: &str =
    "Instance import byte-work budget reached; review instance_import_work_limit_bytes";
const TIME: &str = "Instance import deadline reached; review instance_import_timeout_ms";
const FRAME: &str =
    "Queued delivery frame exceeds import limit; review instance_import_frame_limit_bytes";

pub(crate) struct Budget {
    pub(crate) deadline: Instant,
    limit_bytes: u64,
    entry_limit: u64,
    values_left: u64,
    work_left: u64,
    frame_limit_bytes: u64,
    attachment_limit: u64,
    attachment_limit_bytes: u64,
    failed: Cell<Option<&'static str>>,
}
struct Inventory {
    files: Vec<(PathBuf, u64)>,
    bytes: u64,
    entries: u64,
}
impl Budget {
    pub(crate) fn new(settings: &Settings) -> Result<Self> {
        settings.validate()?;
        Ok(Self {
            deadline: Instant::now() + Duration::from_millis(settings.instance_import_timeout_ms),
            limit_bytes: settings.instance_import_limit_bytes,
            entry_limit: settings.instance_import_entry_limit,
            values_left: settings.instance_import_json_value_limit,
            work_left: settings.instance_import_work_limit_bytes,
            frame_limit_bytes: settings.instance_import_frame_limit_bytes,
            attachment_limit: settings.instance_import_attachment_limit,
            attachment_limit_bytes: settings.instance_import_attachment_limit_bytes,
            failed: Cell::new(None),
        })
    }
    fn refuse<T>(&self, error: &'static str) -> Result<T> {
        self.failed.set(Some(error));
        Err(error.into())
    }
    pub(crate) fn check_time(&self) -> Result<()> {
        if let Some(error) = self.failed.get() {
            return Err(error.into());
        }
        if Instant::now() >= self.deadline {
            return self.refuse(TIME);
        }
        Ok(())
    }
    pub(crate) fn work(&mut self, bytes: u64) -> Result<()> {
        self.check_time()?;
        self.work_left = match self.work_left.checked_sub(bytes) {
            Some(left) => left,
            None => return self.refuse(WORK),
        };
        Ok(())
    }
    pub(crate) fn attachment_limits(&self) -> Result<(u64, u64)> {
        self.check_time()?;
        Ok((self.attachment_limit, self.attachment_limit_bytes))
    }
    pub(crate) fn frame(&mut self, bytes: &[u8]) -> Result<()> {
        self.check_time()?;
        if bytes.len() as u64 > self.frame_limit_bytes {
            return self.refuse(FRAME);
        }
        self.work(bytes.len() as u64)?;
        self.json(Path::new("queued-frame.json"), bytes)
    }
    pub(crate) fn json(&mut self, name: &Path, bytes: &[u8]) -> Result<()> {
        if name.extension().and_then(|s| s.to_str()) == Some("json") {
            json_preflight::check(bytes, || {
                self.check_time()?;
                self.values_left = match self.values_left.checked_sub(1) {
                    Some(left) => left,
                    None => return self.refuse(VALUES),
                };
                Ok(())
            })
            .map_err(|error| {
                if error == json_preflight::INVALID {
                    "Invalid or ambiguous staged import JSON".into()
                } else {
                    error
                }
            })?;
        }
        self.check_time()
    }
    fn admit(&self, bytes: u64, entries: u64) -> Result<()> {
        self.check_time()?;
        if bytes > self.limit_bytes {
            return self.refuse(BYTES);
        }
        if entries > self.entry_limit {
            return self.refuse(ENTRIES);
        }
        Ok(())
    }
    fn inventory(&self, root: &Path) -> Result<Inventory> {
        self.check_time()?;
        let mut inventory = Inventory {
            files: Vec::new(),
            bytes: 0,
            entries: 0,
        };
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            self.check_time()?;
            for entry in fs::read_dir(directory).map_err(|_| "Cannot inspect import snapshot")? {
                self.check_time()?;
                let entry = entry.map_err(|_| "Cannot inspect import snapshot entry")?;
                let path = entry.path();
                let meta = fs::symlink_metadata(&path)
                    .map_err(|_| "Cannot inspect import snapshot metadata")?;
                inventory.entries = inventory.entries.checked_add(1).ok_or(ENTRIES)?;
                if meta.is_dir() {
                    self.admit(inventory.bytes, inventory.entries)?;
                    pending.push(path);
                } else if meta.is_file() {
                    if meta.len() > sqlite::MAX_DATABASE_BYTES {
                        return Err("Invalid migration file".into());
                    }
                    inventory.bytes = inventory.bytes.checked_add(meta.len()).ok_or(BYTES)?;
                    self.admit(inventory.bytes, inventory.entries)?;
                    inventory.files.push((path, meta.len()));
                } else {
                    return Err("Invalid migration file".into());
                }
            }
        }
        self.check_time()?;
        Ok(inventory)
    }
    pub(crate) fn inspect(&self, root: &Path) -> Result<()> {
        self.inventory(root).map(|_| ())
    }
    pub(crate) fn write(
        &mut self,
        root: &Path,
        path: &Path,
        bytes: &[u8],
        per_file: u64,
    ) -> Result<()> {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "Staged write is outside import snapshot")?;
        if relative
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
        {
            return Err("Invalid staged write path".into());
        }
        layouts::check_path(path)?;
        let inventory = self.inventory(root)?;
        let old = fs::symlink_metadata(path).map(|m| m.len()).unwrap_or(0);
        let mut added = if path.exists() { 0 } else { 1 };
        let mut parent = path.parent();
        while let Some(directory) = parent {
            if directory == root || directory.exists() {
                break;
            }
            added += 1;
            parent = directory.parent();
        }
        self.admit(
            inventory
                .bytes
                .saturating_sub(old)
                .checked_add(bytes.len() as u64)
                .ok_or(BYTES)?,
            inventory.entries.checked_add(added).ok_or(ENTRIES)?,
        )?;
        self.work(bytes.len() as u64)?;
        self.json(path, bytes)?;
        layouts::write_private_limit(path, bytes, per_file)?;
        self.check_time()
    }
    pub(crate) fn snapshot(
        &mut self,
        root: &Path,
        source: &Path,
        destination: &Path,
        options: &sqlite::SnapshotOptions,
    ) -> Result<()> {
        let inventory = self.inventory(root)?;
        self.admit(
            inventory.bytes,
            inventory.entries.checked_add(1).ok_or(ENTRIES)?,
        )?;
        let remaining = self.limit_bytes.saturating_sub(inventory.bytes);
        if remaining < 4096 {
            return self.refuse(BYTES);
        }
        if self.work_left < 4096 {
            return self.refuse(WORK);
        }
        let mut bounded = options.clone();
        bounded.limit_bytes = bounded.limit_bytes.min(remaining).min(self.work_left);
        bounded.timeout = bounded
            .timeout
            .min(self.deadline.saturating_duration_since(Instant::now()));
        self.check_time()?;
        if let Err(error) = sqlite::snapshot(source, destination, &bounded) {
            self.check_time()?;
            if error.contains("limit") && bounded.limit_bytes < options.limit_bytes {
                return self.refuse(if self.work_left < remaining {
                    WORK
                } else {
                    BYTES
                });
            }
            return Err(error.into());
        }
        self.work(
            fs::metadata(destination)
                .map_err(|_| "Cannot inspect copied import database")?
                .len(),
        )?;
        self.inspect(root)
    }
    pub(crate) fn hashes(&mut self, root: &Path) -> Result<BTreeMap<String, String>> {
        let inventory = self.inventory(root)?;
        let mut result = BTreeMap::new();
        for (path, size) in inventory.files {
            self.check_time()?;
            layouts::check_path(&path)?;
            #[cfg(unix)]
            let mut file = {
                use std::os::unix::fs::OpenOptionsExt;
                fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)
                    .map_err(|_| "Cannot hash import snapshot")?
            };
            #[cfg(windows)]
            let mut file = crate::platform::open_private(&path, false, false)
                .map_err(|_| "Cannot hash private import snapshot")?;
            check_file(&file, size)?;
            let mut hash = Sha256::new();
            let mut count = 0u64;
            let is_json = path.extension().and_then(|s| s.to_str()) == Some("json");
            let mut json = Vec::new();
            let mut chunk = [0; 65536];
            loop {
                self.check_time()?;
                let maximum = chunk
                    .len()
                    .min(usize::try_from(self.work_left.saturating_add(1)).unwrap_or(usize::MAX));
                let n = file
                    .read(&mut chunk[..maximum])
                    .map_err(|_| "Cannot read import snapshot hash")?;
                if n == 0 {
                    break;
                }
                self.work(n as u64)?;
                count = count.checked_add(n as u64).ok_or(BYTES)?;
                if count > size {
                    return Err("Import snapshot changed during hashing".into());
                }
                hash.update(&chunk[..n]);
                if is_json {
                    json.extend_from_slice(&chunk[..n]);
                }
            }
            if count != size {
                return Err("Import snapshot changed during hashing".into());
            }
            if is_json {
                self.json(&path, &json)?;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "Invalid import snapshot path")?
                .to_str()
                .ok_or("Import snapshot path must be UTF-8")?
                .to_owned();
            result.insert(relative, format!("{:x}", hash.finalize()));
        }
        self.check_time()?;
        Ok(result)
    }
}
fn check_file(file: &File, size: u64) -> Result<()> {
    let meta = file
        .metadata()
        .map_err(|_| "Cannot inspect import snapshot file")?;
    if !meta.is_file() || meta.len() != size {
        return Err("Import snapshot file changed".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 || meta.nlink() != 1
        {
            return Err("Import snapshot files must be private and owned".into());
        }
    }
    Ok(())
}
pub(crate) struct Payloads {
    pub(crate) rows: BTreeMap<String, Vec<u8>>,
    pub(crate) budget: Budget,
    bytes: u64,
}
impl Payloads {
    pub(crate) fn new(settings: &Settings) -> Result<Self> {
        Ok(Self {
            rows: BTreeMap::new(),
            budget: Budget::new(settings)?,
            bytes: 0,
        })
    }
    pub(crate) fn insert(&mut self, name: String, bytes: Vec<u8>) -> Result<()> {
        let previous = self.rows.get(&name).map_or(0, |b| b.len() as u64);
        let size = self
            .bytes
            .saturating_sub(previous)
            .checked_add(bytes.len() as u64)
            .ok_or(BYTES)?;
        self.budget.admit(
            size,
            self.rows.len() as u64 + u64::from(!self.rows.contains_key(&name)),
        )?;
        self.budget.work(bytes.len() as u64)?;
        self.budget.json(Path::new(&name), &bytes)?;
        self.rows.insert(name, bytes);
        self.bytes = size;
        Ok(())
    }
    pub(crate) fn into_parts(self) -> (BTreeMap<String, Vec<u8>>, Budget) {
        (self.rows, self.budget)
    }
}
impl Deref for Payloads {
    type Target = BTreeMap<String, Vec<u8>>;
    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "daedalus-import-budget-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn cumulative_json_and_byte_work_refusals_cannot_be_reset_by_another_phase() {
        let mut settings = Settings {
            instance_import_json_value_limit: 128,
            ..Default::default()
        };
        let mut budget = Budget::new(&settings).unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({"values":vec![0;60]})).unwrap();
        budget.json(Path::new("one.json"), &bytes).unwrap();
        budget.json(Path::new("two.json"), &bytes).unwrap();
        assert!(
            budget
                .json(Path::new("three.json"), &bytes)
                .unwrap_err()
                .contains("JSON work")
        );
        assert!(budget.check_time().is_err());
        settings.instance_import_work_limit_bytes = 4096;
        let mut budget = Budget::new(&settings).unwrap();
        budget.work(2048).unwrap();
        budget.work(2048).unwrap();
        assert!(budget.work(1).unwrap_err().contains("byte-work"));
        assert!(budget.work(0).is_err());
    }
    #[test]
    fn staged_entries_include_directories_and_expired_writes_never_create_files() {
        let t = Temp::new();
        let mut budget = Budget::new(&Settings {
            instance_import_entry_limit: 2,
            ..Default::default()
        })
        .unwrap();
        budget
            .write(&t.0, &t.0.join("nested/a.bin"), b"opaque evidence", 4096)
            .unwrap();
        assert!(
            budget
                .write(&t.0, &t.0.join("b.bin"), b"extra", 4096)
                .unwrap_err()
                .contains("entry budget")
        );
        assert!(!t.0.join("b.bin").exists());
        let mut budget = Budget::new(&Settings::default()).unwrap();
        budget.deadline = Instant::now();
        assert!(
            budget
                .write(&t.0, &t.0.join("late.bin"), b"late", 4096)
                .unwrap_err()
                .contains("deadline")
        );
        assert!(!t.0.join("late.bin").exists());
    }
    #[test]
    fn database_snapshot_uses_remaining_retained_budget_and_recovers_without_source_changes() {
        let t = Temp::new();
        let source = t.0.join("source.sqlite3");
        let stage = t.0.join("stage");
        fs::DirBuilder::new().mode(0o700).create(&stage).unwrap();
        let con = rusqlite::Connection::open(&source).unwrap();
        con.execute_batch(
            "CREATE TABLE inert(value BLOB);INSERT INTO inert VALUES(zeroblob(16384));",
        )
        .unwrap();
        drop(con);
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::read(&source).unwrap();
        let destination = stage.join("copy.sqlite3");
        let mut settings = Settings {
            instance_import_limit_bytes: 8192,
            ..Default::default()
        };
        let mut budget = Budget::new(&settings).unwrap();
        assert!(
            budget
                .snapshot(&stage, &source, &destination, &settings.snapshot_options())
                .unwrap_err()
                .contains("retained-byte")
        );
        assert!(!destination.exists());
        assert_eq!(fs::read(&source).unwrap(), before);
        settings.instance_import_limit_bytes = 65536;
        let mut budget = Budget::new(&settings).unwrap();
        budget
            .snapshot(&stage, &source, &destination, &settings.snapshot_options())
            .unwrap();
        assert_eq!(fs::read(&source).unwrap(), before);
        let copied = rusqlite::Connection::open(&destination).unwrap();
        assert_eq!(
            copied
                .query_row("SELECT length(value) FROM inert", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            16384
        );
    }
}
