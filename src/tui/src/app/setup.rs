//! Setup plans are inert until applied. Import never evaluates environment files or scripts.
use super::config::ControlConfig;
use serde::Serialize;
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::{
    fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    io::AsRawFd,
};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
const ALLOWED: [&str; 8] = [
    "version",
    "agent_name",
    "agent_home",
    "subscriptions",
    "telegram",
    "profiles",
    "tmux_sockets",
    "discovery",
];

#[derive(Clone, Debug, Serialize)]
pub struct SetupPreview {
    pub destination: PathBuf,
    pub source: Option<PathBuf>,
    pub changes: bool,
    pub backup_required: bool,
    pub ignored_fields: Vec<String>,
    pub notices: Vec<String>,
}

/// Holds the exact proposed bytes and expected destination, preventing stale previews.
pub struct SetupPlan {
    pub preview: SetupPreview,
    destination: PathBuf,
    config: ControlConfig,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
}

#[derive(Debug, Serialize)]
pub struct SetupResult {
    pub changed: bool,
    pub config_path: PathBuf,
    pub backup: Option<PathBuf>,
}

impl SetupPlan {
    /// Review a replacement of existing settings, retaining a backup on apply.
    pub fn edit(directory: &Path, config: ControlConfig) -> Result<Self, String> {
        let plan = Self::prepare(directory, config, None, vec![], vec![])?;
        if plan.before.is_none() {
            return Err("Configuration does not exist; use fresh setup instead".into());
        }
        Ok(plan)
    }

    pub fn fresh(directory: &Path, config: ControlConfig) -> Result<Self, String> {
        let plan = Self::prepare(directory, config, None, vec![], vec![])?;
        if plan.before.is_some() {
            return Err("Configuration already exists; use import or edit instead".into());
        }
        Ok(plan)
    }

    /// Imports only portable settings. Other state requires its own versioned migration.
    pub fn import(directory: &Path, source: &Path) -> Result<Self, String> {
        let bytes = read_regular(source)?.ok_or("Import source does not exist")?;
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid import JSON")?;
        let object = value
            .as_object_mut()
            .ok_or("Import must be a configuration object")?;
        let ignored: Vec<String> = object
            .keys()
            .filter(|key| !ALLOWED.contains(&key.as_str()))
            .cloned()
            .collect();
        for key in &ignored {
            object.remove(key);
        }
        // Connection settings are also allowlisted: tokens must not enter control.json.
        let mut ignored = ignored;
        if let Some(telegram) = object.get_mut("telegram").and_then(Value::as_object_mut) {
            let unknown: Vec<String> = telegram
                .keys()
                .filter(|key| !["owner_id", "bot_id", "username"].contains(&key.as_str()))
                .cloned()
                .collect();
            for key in unknown {
                telegram.remove(&key);
                ignored.push(format!("telegram.{key}"));
            }
        }
        let filtered = serde_json::to_vec(&value).map_err(|_| "Cannot process import")?;
        let config = ControlConfig::parse(&filtered)?;
        Self::prepare(directory, config, Some(source.to_path_buf()), ignored, vec![
            "Imports settings and native profile paths only; account sign-in remains with each harness.".into(),
            "Routes, checkpoints, layouts, delivery databases, token files and custom hooks are not migrated by this settings importer.".into(),
            "Original files remain unchanged. No imported commands are executed.".into(),
        ])
    }

    fn prepare(
        directory: &Path,
        config: ControlConfig,
        source: Option<PathBuf>,
        ignored_fields: Vec<String>,
        notices: Vec<String>,
    ) -> Result<Self, String> {
        validate_directory(directory)?;
        let destination = directory.join("control.json");
        let before = read_regular(&destination)?;
        let after = config.encode()?;
        let changes = before.as_deref() != Some(after.as_slice());
        let preview = SetupPreview {
            destination: destination.clone(),
            source,
            changes,
            backup_required: changes && before.is_some(),
            ignored_fields,
            notices,
        };
        Ok(Self {
            preview,
            destination,
            config,
            before,
            after,
        })
    }

    pub fn config(&self) -> &ControlConfig {
        &self.config
    }

    pub fn apply(self) -> Result<SetupResult, String> {
        let destination = &self.destination;
        let directory = destination
            .parent()
            .ok_or("Missing configuration directory")?;
        validate_directory(directory)?;
        ensure_private_directory(directory)?;
        let _lock = SetupLock::acquire(directory)?;
        if read_regular(destination)? != self.before {
            return Err("Configuration changed since preview; prepare setup again".into());
        }
        if self.before.as_deref() == Some(self.after.as_slice()) {
            return Ok(SetupResult {
                changed: false,
                config_path: destination.clone(),
                backup: None,
            });
        }
        let backup = if let Some(before) = &self.before {
            let backup = write_unique(directory, "control.backup", before)?;
            crate::platform::sync_directory(directory)
                .map_err(|_| "Cannot synchronize backup directory")?;
            Some(backup)
        } else {
            None
        };
        let temporary = write_unique(directory, "control.pending", &self.after)?;
        let _temporary_guard = RemoveFile(temporary.clone());
        crate::platform::replace_private(&temporary, destination)
            .map_err(|_| "Cannot replace configuration; backup retained")?;
        crate::platform::sync_directory(directory)
            .map_err(|_| "Configuration written but directory synchronization failed")?;
        Ok(SetupResult {
            changed: true,
            config_path: destination.clone(),
            backup,
        })
    }
}

/// Explicit local setup completion may make its existing directory private.
/// Use the opened directory handle so a path replacement cannot redirect chmod.
#[cfg(unix)]
pub fn secure_local_directory(directory: &Path) -> Result<(), String> {
    validate_directory(directory)?;
    if directory.exists() {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(directory)
            .map_err(|_| "Cannot open configuration directory")?;
        let metadata = file
            .metadata()
            .map_err(|_| "Cannot inspect configuration directory")?;
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err("Configuration directory must be owned by the current user".into());
        }
        file.set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot make configuration directory private")?;
    }
    ensure_private_directory(directory)
}

fn ensure_private_directory(directory: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        crate::platform::ensure_private_dir(directory).map_err(|e| e.to_string())
    }
    #[cfg(unix)]
    match fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err("Configuration destination is not a directory".into());
            }
            #[cfg(unix)]
            if metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err("Existing configuration directory must be owned by the current user with mode 0700; choose a dedicated private directory".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(directory)
                .map_err(|_| "Cannot create private configuration directory")?;
            // Recheck after creation, including races with another creator.
            validate_directory(directory)?;
            return ensure_private_directory(directory);
        }
        Err(_) => return Err("Cannot inspect configuration directory".into()),
    }
    #[cfg(unix)]
    Ok(())
}

struct SetupLock(fs::File);
impl SetupLock {
    fn acquire(directory: &Path) -> Result<Self, String> {
        let path = directory.join(".setup.lock");
        let mut options = private_options();
        options.create(true);
        #[cfg(unix)]
        let file = options.open(path).map_err(|_| "Cannot open setup lock")?;
        #[cfg(windows)]
        let file = crate::platform::open_private(&path, true, true)
            .map_err(|_| "Cannot open setup lock")?;
        if !file
            .metadata()
            .map_err(|_| "Cannot inspect setup lock")?
            .is_file()
        {
            return Err("Setup lock must be a regular file".into());
        }
        #[cfg(unix)]
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("Setup is already running".into());
        }
        #[cfg(windows)]
        crate::platform::lock_exclusive(&file, true).map_err(|_| "Setup is already running")?;
        Ok(Self(file))
    }
}
impl Drop for SetupLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
        #[cfg(windows)]
        let _ = crate::platform::unlock(&self.0);
        // Keep the inode: unlinking would allow two independent locks.
    }
}

fn validate_directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .as_os_str()
            .to_string_lossy()
            .chars()
            .any(char::is_control)
        || path.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err("Configuration directory must be an absolute path without parent traversal or control characters".into());
    }
    #[cfg(windows)]
    crate::platform::check_path(path).map_err(|e| e.to_string())?;
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err(
                    "Configuration directory must not traverse symlinks or non-directories".into(),
                );
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot inspect configuration directory".into()),
        }
    }
    Ok(())
}

fn read_regular(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot inspect configuration file".into()),
        Ok(metadata) if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES => {
            return Err("Configuration must be a regular file of at most 1 MiB".into());
        }
        Ok(_) => (),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(unix)]
    let file = options
        .open(path)
        .map_err(|_| "Cannot read configuration file")?;
    #[cfg(windows)]
    let file = crate::platform::open_regular(path).map_err(|_| "Cannot read configuration file")?;
    if !file
        .metadata()
        .map_err(|_| "Cannot inspect opened configuration")?
        .is_file()
    {
        return Err("Configuration must be a regular file".into());
    }
    let mut bytes = vec![];
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read configuration file")?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("Configuration exceeds 1 MiB".into());
    }
    Ok(Some(bytes))
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options
}

fn write_unique(directory: &Path, prefix: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    for _ in 0..100 {
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(".{prefix}-{}-{id}", std::process::id()));
        let mut options = private_options();
        options.create_new(true);
        #[cfg(unix)]
        let opened = options.open(&path);
        #[cfg(windows)]
        let opened = crate::platform::create_private_new(&path);
        match opened {
            Ok(mut file) => {
                if file
                    .write_all(bytes)
                    .and_then(|()| file.sync_all())
                    .is_err()
                {
                    let _ = fs::remove_file(&path);
                    return Err("Cannot write private configuration file".into());
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("Cannot create private configuration file".into()),
        }
    }
    Err("Cannot allocate private configuration file".into())
}

struct RemoveFile(PathBuf);
impl Drop for RemoveFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "daedalus-setup-test-{}-{}",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn config() -> ControlConfig {
        ControlConfig::fresh(Path::new(crate::test_support::path(
            "/tmp/example workspace",
        )))
        .unwrap()
    }

    #[test]
    fn fresh_setup_is_private_and_round_trips() {
        let temp = Temp::new();
        let directory = temp.0.join("fresh");
        let plan = SetupPlan::fresh(&directory, config()).unwrap();
        assert!(!directory.exists());
        assert!(plan.preview.changes);
        let result = plan.apply().unwrap();
        assert!(result.changed);
        assert!(result.backup.is_none());
        assert_eq!(
            ControlConfig::parse(&fs::read(&result.config_path).unwrap()).unwrap(),
            config()
        );
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&result.config_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert!(SetupPlan::fresh(&directory, config()).is_err());
    }
    #[test]
    fn import_previews_backs_up_and_repeats_without_changes() {
        let temp = Temp::new();
        let directory = temp.0.join("target");
        SetupPlan::fresh(&directory, config())
            .unwrap()
            .apply()
            .unwrap();
        let old = fs::read(directory.join("control.json")).unwrap();
        let source = temp.0.join("old.json");
        let mut changed = config();
        changed.subscriptions.push("codex".into());
        changed
            .tmux_sockets
            .push(crate::test_support::path("/tmp/example/custom-socket").into());
        changed.discovery.automatic = false;
        changed
            .discovery
            .herdr_sessions
            .push("project-workspace".into());
        let mut value = serde_json::to_value(&changed).unwrap();
        value["private_script"] = Value::String("do not execute".into());
        value["telegram"]["token"] = Value::String("do not copy".into());
        fs::write(&source, serde_json::to_vec(&value).unwrap()).unwrap();
        let plan = SetupPlan::import(&directory, &source).unwrap();
        assert_eq!(
            plan.preview.ignored_fields,
            ["private_script", "telegram.token"]
        );
        assert!(plan.preview.backup_required);
        let result = plan.apply().unwrap();
        assert_eq!(fs::read(result.backup.unwrap()).unwrap(), old);
        let output = fs::read(directory.join("control.json")).unwrap();
        assert_eq!(ControlConfig::parse(&output).unwrap(), changed);
        assert!(
            !SetupPlan::import(&directory, &source)
                .unwrap()
                .apply()
                .unwrap()
                .changed
        );
        assert!(fs::read_to_string(source).unwrap().contains("do not copy"));
    }
    #[test]
    fn stale_preview_does_not_overwrite() {
        let temp = Temp::new();
        let directory = temp.0.join("target");
        let plan = SetupPlan::fresh(&directory, config()).unwrap();
        ensure_private_directory(&directory).unwrap();
        fs::write(directory.join("control.json"), b"concurrent change").unwrap();
        assert!(plan.apply().is_err());
        assert_eq!(
            fs::read(directory.join("control.json")).unwrap(),
            b"concurrent change"
        );
    }
    #[test]
    fn corrupt_import_leaves_destination_untouched() {
        let temp = Temp::new();
        let source = temp.0.join("corrupt.json");
        let destination = temp.0.join("target");
        fs::write(&source, b"not json secret-token").unwrap();
        let error = SetupPlan::import(&destination, &source).err().unwrap();
        assert!(!error.contains("secret-token"));
        assert!(!destination.exists());
    }
    #[test]
    fn locks_exclude_concurrent_setup_and_release_without_unlink() {
        let temp = Temp::new();
        let directory = temp.0.join("target");
        ensure_private_directory(&directory).unwrap();
        let lock = SetupLock::acquire(&directory).unwrap();
        assert!(SetupLock::acquire(&directory).is_err());
        drop(lock);
        assert!(directory.join(".setup.lock").exists());
        assert!(SetupLock::acquire(&directory).is_ok());
    }
    #[cfg(unix)]
    #[test]
    fn existing_shared_directory_is_not_chmodded() {
        let temp = Temp::new();
        fs::set_permissions(&temp.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            SetupPlan::fresh(&temp.0, config())
                .unwrap()
                .apply()
                .is_err()
        );
        assert_eq!(
            fs::metadata(&temp.0).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    #[cfg(unix)]
    #[test]
    fn refuses_symlink_destination_and_source() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new();
        let source = temp.0.join("source.json");
        fs::write(&source, config().encode().unwrap()).unwrap();
        symlink(&source, temp.0.join("link.json")).unwrap();
        assert!(SetupPlan::import(&temp.0.join("target"), &temp.0.join("link.json")).is_err());
        symlink(&temp.0, temp.0.join("linked-dir")).unwrap();
        assert!(SetupPlan::fresh(&temp.0.join("linked-dir/target"), config()).is_err());
    }
}

#[cfg(windows)]
pub fn secure_local_directory(directory: &Path) -> Result<(), String> {
    validate_directory(directory)?;
    crate::platform::secure_owned_dir(directory).map_err(|e| e.to_string())
}
