//! Explicit local service configuration. Preview never reads token contents or starts a bot.
use super::hook_effects::Binding;
use crate::core::{layouts, routes::Permission};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::{
    fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    io::AsRawFd,
};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, String>;
const LIMIT: u64 = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Send,
    Manage,
}
impl Access {
    pub fn permission(self) -> Permission {
        match self {
            Self::Read => Permission::Read,
            Self::Send => Permission::Send,
            Self::Manage => Permission::Manage,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub chat: i64,
    pub actor: i64,
    pub permissions: Vec<Access>,
}
impl Grant {
    pub fn permissions(&self) -> Vec<Permission> {
        self.permissions.iter().map(|p| p.permission()).collect()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub version: u32,
    pub instance: String,
    pub bot_id: i64,
    pub token_file: PathBuf,
    #[serde(default)]
    pub grants: Vec<Grant>,
    #[serde(default)]
    pub listen_chats: BTreeSet<i64>,
    #[serde(default)]
    pub registrations: Vec<PathBuf>,
    #[serde(default)]
    pub hook_bindings: Vec<Binding>,
}
fn valid_path(path: &Path) -> Result<()> {
    layouts::check_path(path)
        .map_err(|_| "Expected an absolute path without symlinks or parent traversal".into())
}
impl ServiceConfig {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.bot_id <= 0
            || self.instance.is_empty()
            || self.instance.len() > 64
            || !self
                .instance
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err("Invalid service version, instance, or bot identity".into());
        }
        valid_path(&self.token_file)?;
        if self.grants.len() > 1000
            || self.listen_chats.len() > 1000
            || self.registrations.len() > 1024
            || self.hook_bindings.len() > 1024
        {
            return Err("Service configuration exceeds limits".into());
        }
        let mut seen = BTreeSet::new();
        for grant in &self.grants {
            let permissions: BTreeSet<_> = grant.permissions.iter().copied().collect();
            if grant.chat == 0
                || grant.actor <= 0
                || permissions.is_empty()
                || permissions.len() != grant.permissions.len()
                || !permissions.contains(&Access::Read)
                || !seen.insert((grant.chat, grant.actor))
            {
                return Err(
                    "Grants require unique chat/actor identities and explicit read permission"
                        .into(),
                );
            }
        }
        for chat in &self.listen_chats {
            if *chat >= 0
                || !self
                    .grants
                    .iter()
                    .any(|g| g.chat == *chat && g.permissions.contains(&Access::Send))
            {
                return Err("Listening requires an explicit group chat and sender grant".into());
            }
        }
        let mut paths = BTreeSet::new();
        for path in &self.registrations {
            valid_path(path)?;
            if !paths.insert(path) {
                return Err("Duplicate hook registration path".into());
            }
        }
        let mut bindings = BTreeSet::new();
        for binding in &self.hook_bindings {
            binding
                .goal_scope
                .validate()
                .map_err(|_| "Invalid hook scope")?;
            let chat = binding
                .route_scope
                .chat
                .parse::<i64>()
                .map_err(|_| "Invalid hook chat")?;
            let actor = binding
                .actor
                .parse::<i64>()
                .map_err(|_| "Invalid hook actor")?;
            if binding.registration_id.is_empty()
                || binding.registration_id.len() > 256
                || binding.registration_id.chars().any(char::is_control)
                || binding.goal_scope.instance != self.instance
                || binding.route_scope.instance != self.instance
                || binding.route_scope.bot != self.bot_id.to_string()
                || binding.route_revision <= 0
                || binding.reply_to.is_some_and(|id| id <= 0)
                || !bindings.insert((&binding.registration_id, chat, actor))
                || !self.grants.iter().any(|g| {
                    g.chat == chat
                        && g.actor == actor
                        && g.permissions.contains(&Access::Read)
                        && g.permissions.contains(&Access::Send)
                })
            {
                return Err("Hook recipient requires matching instance, bot, revision and explicit read/send grants".into());
            }
            if binding
                .hindsight_bank
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 256 || v.chars().any(char::is_control))
            {
                return Err("Invalid memory bank identity".into());
            }
        }
        Ok(())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = read_private(path, LIMIT)?.ok_or("Service configuration is missing")?;
        // PowerShell 5.1 and some Windows editors write a UTF-8 BOM. The
        // service reloads this file while it is running; rejecting the BOM
        // after an update makes that reload fail closed and drop the process.
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let config: Self =
            serde_json::from_slice(bytes).map_err(|_| "Invalid service configuration JSON")?;
        config.validate()?;
        Ok(config)
    }
    /// Read only at explicit local check or service start. Never serialize the returned secret.
    pub fn read_token(&self) -> Result<String> {
        self.validate()?;
        let bytes = read_private(&self.token_file, 1024)?.ok_or("Token file is missing")?;
        validate_token(self.bot_id, &bytes)
    }
    /// Validate configured hook files and recipient provenance without starting hooks.
    pub fn check_registrations(&self) -> Result<Vec<(PathBuf, super::hook_runtime::Registration)>> {
        self.validate()?;
        let mut registrations = Vec::new();
        let mut ids = BTreeSet::new();
        for path in &self.registrations {
            let registration = super::hook_runtime::inspect(path)
                .map_err(|_| "Configured hook registration is unavailable or invalid")?;
            if registration.scope.instance != self.instance || !ids.insert(registration.id.clone())
            {
                return Err(
                    "Configured hook registration has wrong instance or duplicate identity".into(),
                );
            }
            registrations.push((path.clone(), registration));
        }
        for binding in &self.hook_bindings {
            if !registrations.iter().any(|(_, registration)| {
                registration.id == binding.registration_id
                    && registration.scope == binding.goal_scope
            }) {
                return Err("Hook binding does not match a configured registration".into());
            }
        }
        Ok(registrations)
    }
    pub fn preview(&self) -> Value {
        json!({"version":self.version,"instance":self.instance,"bot_id":self.bot_id,"token":"external private file; contents omitted","grants":self.grants,"listen_chats":self.listen_chats,"registration_count":self.registrations.len(),"hook_binding_count":self.hook_bindings.len(),"notices":["No account, sender grant, or route is inferred from incoming Telegram messages.","Hook recipients remain subject to current route revision and conversation checks.","This plan does not start Telegram or register hooks."]})
    }
}
pub(crate) fn validate_token(bot_id: i64, bytes: &[u8]) -> Result<String> {
    if bot_id <= 0 || bytes.len() > 1024 {
        return Err("Invalid bot identity or token length".into());
    }
    let token = std::str::from_utf8(bytes)
        .map_err(|_| "Invalid token file")?
        .trim();
    let token = token.strip_prefix("TELEGRAM_BOT_TOKEN=").unwrap_or(token);
    // Only the exact literal single-variable form emitted by migration is
    // accepted. No shell syntax, expansion, extra variables or evaluation.
    if token.len() > 512 || token.chars().any(char::is_whitespace) {
        return Err(
            "Token requires a raw value or one literal TELEGRAM_BOT_TOKEN assignment".into(),
        );
    }
    let (bot, secret) = token.split_once(':').ok_or("Invalid token file")?;
    if bot != bot_id.to_string()
        || secret.is_empty()
        || !secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err("Token does not match configured bot identity".into());
    }
    Ok(token.into())
}
#[cfg(unix)]
pub(crate) fn read_private(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    valid_path(path)?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read private configuration file".into()),
    };
    let meta = file.metadata().map_err(|_| "Cannot inspect private file")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err(
            "Configuration and token files must be private regular files owned by this user".into(),
        );
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read private file")?;
    if bytes.len() as u64 > limit {
        return Err("Private file exceeds supported size".into());
    }
    Ok(Some(bytes))
}
pub struct ServicePlan {
    destination: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
    pub config: ServiceConfig,
}
impl ServicePlan {
    pub fn fresh(path: &Path, config: ServiceConfig) -> Result<Self> {
        let plan = Self::prepare(path, config)?;
        if plan.before.is_some() {
            return Err("Service configuration already exists; use import".into());
        }
        Ok(plan)
    }
    pub fn import(path: &Path, source: &Path) -> Result<Self> {
        Self::prepare(path, ServiceConfig::load(source)?)
    }
    pub(crate) fn prepare(path: &Path, config: ServiceConfig) -> Result<Self> {
        valid_path(path)?;
        if in_install_tree(path)
            || in_install_tree(&config.token_file)
            || config
                .registrations
                .iter()
                .any(|path| in_install_tree(path))
        {
            return Err("Telegram configuration cannot be stored under %LOCALAPPDATA%\\Daedalus. That directory is the install tree an update replaces. Use the config directory (%APPDATA%\\Daedalus or an explicit --config-dir outside the install tree).".into());
        }
        config.validate()?;
        let before = read_private(path, LIMIT)?;
        let after = serde_json::to_vec_pretty(&config)
            .map_err(|_| "Cannot encode service configuration")?;
        Ok(Self {
            destination: path.into(),
            before,
            after,
            config,
        })
    }
    pub fn review(&self) -> Value {
        json!({"configuration":self.config.preview(),"changes":self.before.as_deref()!=Some(&self.after),"backup_required":self.before.is_some(),"approval":self.approval_token()})
    }
    pub fn approval_token(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(&self.destination, &self.before, &self.after))
                    .expect("serializable plan")
            )
        )
    }
    pub fn apply(&self, approval: &str) -> Result<bool> {
        if approval != self.approval_token() {
            return Err("Service approval does not match plan".into());
        }
        valid_path(&self.destination)?;
        let directory = self
            .destination
            .parent()
            .ok_or("Missing service directory")?;
        let _lock = configuration_lock(directory)?;
        let current = read_private(&self.destination, LIMIT)?;
        if current.as_deref() == Some(&self.after) {
            return Ok(false);
        }
        if current != self.before {
            return Err("Service configuration changed; review a new plan".into());
        }
        if let Some(before) = &self.before {
            let backup = directory.join(format!(
                "telegram-service.backup-{:x}.json",
                Sha256::digest(before)
            ));
            match read_private(&backup, LIMIT)? {
                Some(existing) if existing != *before => {
                    return Err("Configuration backup conflicts".into());
                }
                Some(_) => (),
                None => layouts::write_private(&backup, before)
                    .map_err(|_| "Cannot save configuration backup")?,
            }
        }
        layouts::write_private(&self.destination, &self.after)
            .map_err(|_| "Cannot save service configuration")?;
        Ok(true)
    }
}
pub(crate) struct ConfigurationLock(fs::File);
#[cfg(unix)]
impl Drop for ConfigurationLock {
    fn drop(&mut self) {
        // A concurrent fork can inherit the open file description before exec.
        // Closing only our descriptor would leave that child holding this lock.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
#[cfg(unix)]
pub(crate) fn configuration_lock(directory: &Path) -> Result<ConfigurationLock> {
    valid_path(directory)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(|_| "Cannot create service directory")?;
    let meta = fs::metadata(directory).map_err(|_| "Cannot inspect service directory")?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("Service directory must be private and owned by this user".into());
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join(".telegram-config.lock"))
        .map_err(|_| "Cannot open service configuration lock")?;
    let meta = lock
        .metadata()
        .map_err(|_| "Cannot inspect configuration lock")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
        || unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
    {
        return Err("Service configuration is busy or unsafe".into());
    }
    Ok(ConfigurationLock(lock))
}
/// Local CLI only: no network requests or service activation.
pub fn run(args: &[String], writer: &mut impl Write) -> Result<()> {
    if args.is_empty() || args == ["--help"] || args == ["help"] {
        writeln!(writer, "daedalus telegram config init --file ABS --instance NAME --bot-id ID --token-file ABS [--owner ID --chat ID] [--apply]\ndaedalus telegram config import --file ABS --from ABS [--apply]\ndaedalus telegram config check --file ABS\nInit/import preview by default. --apply writes private configuration; it never starts Telegram.\nToken file: private raw bot token or literal TELEGRAM_BOT_TOKEN=value on one line. No shell evaluation.\nOwner grants are explicit; omitted --owner/--chat grants nobody access.")
            .map_err(|_| "Cannot write configuration help")?;
        return Ok(());
    }
    let verb = args
        .first()
        .map(String::as_str)
        .ok_or("Use telegram config check|init|import --file ABS")?;
    let mut flags = std::collections::BTreeMap::new();
    let mut apply = false;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--apply" {
            if apply {
                return Err("Duplicate --apply".into());
            }
            apply = true;
            i += 1;
            continue;
        }
        let value = args.get(i + 1).ok_or("Missing configuration argument")?;
        if ![
            "--file",
            "--from",
            "--instance",
            "--bot-id",
            "--token-file",
            "--owner",
            "--chat",
        ]
        .contains(&args[i].as_str())
            || flags.insert(args[i].as_str(), value.as_str()).is_some()
        {
            return Err("Unknown or duplicate configuration argument".into());
        }
        i += 2;
    }
    let path = Path::new(*flags.get("--file").ok_or("--file ABS is required")?);
    if verb == "check" {
        if apply || flags.len() != 1 {
            return Err("Check accepts only --file".into());
        }
        let config = ServiceConfig::load(path)?;
        config.read_token()?;
        config.check_registrations()?;
        serde_json::to_writer_pretty(
            writer,
            &json!({"valid":true,"configuration":config.preview(),"network_checked":false}),
        )
        .map_err(|_| "Cannot write configuration check")?;
        return Ok(());
    }
    let plan = match verb {
        "import" => {
            if flags.len() != 2 {
                return Err("Import requires --file and --from".into());
            }
            ServicePlan::import(
                path,
                Path::new(*flags.get("--from").ok_or("--from ABS is required")?),
            )?
        }
        "init" => {
            let get = |key| {
                flags.get(key).copied().ok_or("Init requires --instance --bot-id --token-file; optional --owner and --chat together")
            };
            let bot_id = get("--bot-id")?.parse().map_err(|_| "Invalid bot ID")?;
            let grants = match (flags.get("--owner"), flags.get("--chat")) {
                (None, None) => vec![],
                (Some(actor), Some(chat)) => vec![Grant {
                    chat: chat.parse().map_err(|_| "Invalid chat ID")?,
                    actor: actor.parse().map_err(|_| "Invalid owner ID")?,
                    permissions: vec![Access::Read, Access::Send, Access::Manage],
                }],
                _ => {
                    return Err(
                        "Specify both --owner and --chat for an explicit owner grant".into(),
                    );
                }
            };
            if flags.contains_key("--from") {
                return Err("Use import for an existing configuration".into());
            }
            ServicePlan::fresh(
                path,
                ServiceConfig {
                    version: 1,
                    instance: get("--instance")?.into(),
                    bot_id,
                    token_file: get("--token-file")?.into(),
                    grants,
                    listen_chats: BTreeSet::new(),
                    registrations: vec![],
                    hook_bindings: vec![],
                },
            )?
        }
        _ => return Err("Use telegram config check|init|import".into()),
    };
    serde_json::to_writer_pretty(&mut *writer, &plan.review())
        .map_err(|_| "Cannot write service plan")?;
    if apply {
        let changed = plan.apply(&plan.approval_token())?;
        let note = service_apply_note(path.parent());
        writeln!(writer, "\nApplied: {changed}; {note}.")
            .map_err(|_| "Cannot write service outcome")?
    }
    Ok(())
}

fn service_apply_note(directory: Option<&Path>) -> String {
    let Some(directory) = directory else {
        return "service was not started".into();
    };
    if let Some(record) = running_service(directory) {
        if service_matches_current(&record) {
            return "the Telegram service is still running and was not restarted".into();
        }
        return "a pre-update Telegram service is still running and was not restarted".into();
    }
    match super::telegram_supervisor::inspect(directory) {
        Ok(observation) if observation.state == "stopped" => "service remains stopped".into(),
        Ok(observation) => {
            format!("service state is {} and was not changed", observation.state)
        }
        Err(_) => "service state could not be confirmed; it was not started".into(),
    }
}

/// `%LOCALAPPDATA%\Daedalus` is the Windows install and update tree.
/// Configuration belongs in `%APPDATA%\Daedalus` (or another directory outside that tree).
pub(crate) fn in_install_tree(path: &Path) -> bool {
    #[cfg(windows)]
    {
        let Ok(root) = crate::platform::user_local_data_directory() else {
            return false;
        };
        return windows_path_within(&root.join("Daedalus"), path);
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

#[cfg(windows)]
fn windows_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Prefix(prefix) => {
                let text = prefix.as_os_str().to_string_lossy();
                let text = text
                    .strip_prefix(r"\\?\")
                    .unwrap_or(text.as_ref())
                    .trim_end_matches('\\');
                Some(text.to_ascii_lowercase())
            }
            std::path::Component::Normal(part) => Some(part.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

#[cfg(windows)]
fn windows_path_within(root: &Path, path: &Path) -> bool {
    let root = windows_components(root);
    let path = windows_components(path);
    path.len() >= root.len() && path.iter().zip(&root).all(|(path, root)| path == root)
}

#[cfg(windows)]
pub(crate) fn same_directory(left: &Path, right: &Path) -> bool {
    if let (Ok(left), Ok(right)) = (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        return directory_components_equal(&left, &right);
    }
    directory_components_equal(left, right)
}

#[cfg(windows)]
fn directory_components_equal(left: &Path, right: &Path) -> bool {
    windows_components(left) == windows_components(right)
}

/// Live owner for this instance, proven by pid and creation time.
/// Missing or unverified records are not treated as a running service.
#[cfg(windows)]
pub(crate) fn running_service(directory: &Path) -> Option<super::telegram_supervisor::Record> {
    let bytes = read_private(&directory.join("telegram-runtime.json"), 8192).ok()??;
    let record: super::telegram_supervisor::Record = serde_json::from_slice(&bytes).ok()?;
    if record.version != 1 || !same_directory(&record.directory, directory) {
        return None;
    }
    let config = ServiceConfig::load(&directory.join("telegram-service.json")).ok()?;
    if record.instance != config.instance || record.bot_id != config.bot_id {
        return None;
    }
    let process = crate::platform::process_identity(record.pid).ok()?;
    let owner = crate::platform::current_identity().ok()?;
    if process.creation_time != record.start_ticks || process.owner != owner {
        return None;
    }
    daedalus_image(&process.executable).then_some(record)
}

#[cfg(not(windows))]
pub(crate) fn running_service(_directory: &Path) -> Option<super::telegram_supervisor::Record> {
    None
}

#[cfg(windows)]
fn daedalus_image(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    // Installers rename a running image to daedalus.exe.old before writing the new file.
    lower == "daedalus"
        || lower == "daedalus.exe"
        || lower == "daedalus-tui"
        || lower == "daedalus-tui.exe"
        || lower.starts_with("daedalus.exe.")
        || lower.starts_with("daedalus-tui.exe.")
}

/// True only when the live image is this process's executable and that file
/// was not created after the service started. Opening the recorded path is not
/// enough: Windows can replace the path while the old image keeps running.
#[cfg(windows)]
pub(crate) fn service_matches_current(record: &super::telegram_supervisor::Record) -> bool {
    let Ok(process) = crate::platform::process_identity(record.pid) else {
        return false;
    };
    if process.creation_time != record.start_ticks {
        return false;
    }
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    same_file(&process.executable, &current)
        && !file_replaced_after(&process.executable, process.creation_time)
}

#[cfg(not(windows))]
pub(crate) fn service_matches_current(_record: &super::telegram_supervisor::Record) -> bool {
    true
}

#[cfg(windows)]
fn same_file(left: &Path, right: &Path) -> bool {
    let identity = |path: &Path| {
        let file = crate::platform::open_regular(path).ok()?;
        crate::platform::file_identity(&file).ok()
    };
    identity(left).is_some() && identity(left) == identity(right)
}

#[cfg(windows)]
fn file_replaced_after(path: &Path, start_filetime: u64) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return true;
    };
    let Some(started) = filetime_to_system(start_filetime) else {
        return true;
    };
    let newer = |stamp: std::io::Result<std::time::SystemTime>| match stamp {
        Ok(stamp) => stamp
            .duration_since(started)
            .is_ok_and(|elapsed| elapsed > std::time::Duration::from_secs(2)),
        Err(_) => true,
    };
    // In-place replacement can keep the original creation time. A newer
    // modification time still means this path is not the image that started.
    newer(metadata.created()) || newer(metadata.modified())
}

#[cfg(windows)]
fn filetime_to_system(filetime: u64) -> Option<std::time::SystemTime> {
    const UNIX_OFFSET: u64 = 11_644_473_600 * 10_000_000;
    let ticks = filetime.checked_sub(UNIX_OFFSET)?;
    let seconds = ticks / 10_000_000;
    let sub = ticks % 10_000_000;
    Some(
        std::time::UNIX_EPOCH
            + std::time::Duration::new(seconds, u32::try_from(sub * 100).ok()?),
    )
}

pub(crate) fn preupdate_stop_digest(record: &super::telegram_supervisor::Record) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                record.pid,
                record.start_ticks,
                &record.instance,
                &record.directory,
                record.bot_id
            ))
            .expect("serializable stop identity")
        )
    )
}

/// Ask the running service to exit. The service accepts this only when pid,
/// creation time, directory and instance still match, so a recycled pid is inert.
pub(crate) fn write_cooperative_stop(record: &super::telegram_supervisor::Record) -> Result<()> {
    let payload = serde_json::to_vec(&json!({
        "pid": record.pid,
        "start_ticks": record.start_ticks,
        "instance": record.instance,
        "directory": record.directory
    }))
    .map_err(|_| "Cannot encode stop request")?;
    layouts::write_private(&record.directory.join("telegram-stop.json"), &payload)
}

/// Open only an owned private database under an owned private directory.
/// Call at service start, never from read-only configuration checks.
#[cfg(unix)]
pub fn private_connection(path: &Path) -> Result<rusqlite::Connection> {
    valid_path(path)?;
    let parent = path.parent().ok_or("Missing database directory")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|_| "Cannot create database directory")?;
    let meta = fs::metadata(parent).map_err(|_| "Cannot inspect database directory")?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("Database directory must be private and owned by this user".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "Cannot open private database")?;
    let meta = file
        .metadata()
        .map_err(|_| "Cannot inspect private database")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err("Database must be an owned private regular file without hard links".into());
    }
    file.sync_all()
        .map_err(|_| "Cannot sync private database")?;
    // SQLite may open journal/WAL sidecars independently of the main file.
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        match fs::symlink_metadata(PathBuf::from(name)) {
            Ok(meta)
                if !meta.is_file()
                    || meta.file_type().is_symlink()
                    || meta.uid() != unsafe { libc::geteuid() }
                    || meta.mode() & 0o077 != 0
                    || meta.nlink() != 1 =>
            {
                return Err("Database sidecar is not a private owned regular file".into());
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot inspect database sidecar".into()),
        }
    }
    // SQLite opens by name: require the checked inode still occupies this path.
    let named = fs::symlink_metadata(path).map_err(|_| "Cannot inspect database identity")?;
    if named.ino() != meta.ino() || named.dev() != meta.dev() || named.file_type().is_symlink() {
        return Err("Database identity changed".into());
    }
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "Cannot open database connection")?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "Cannot configure database")?;
    connection
        .execute_batch("PRAGMA synchronous=FULL;")
        .map_err(|_| "Cannot configure database")?;
    Ok(connection)
}
#[cfg(windows)]
pub(crate) fn read_private(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    valid_path(path)?;
    let file = match crate::platform::open_private(path, false, false) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot read owned private configuration file".into()),
    };
    crate::platform::validate_private_file(&file)
        .map_err(|_| "Configuration and token files must have private owner-scoped access")?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read private file")?;
    if bytes.len() as u64 > limit {
        return Err("Private file exceeds supported size".into());
    }
    Ok(Some(bytes))
}
#[cfg(windows)]
impl Drop for ConfigurationLock {
    fn drop(&mut self) {
        let _ = crate::platform::unlock(&self.0);
    }
}
#[cfg(windows)]
pub(crate) fn configuration_lock(directory: &Path) -> Result<ConfigurationLock> {
    valid_path(directory)?;
    crate::platform::ensure_private_dir(directory)
        .map_err(|_| "Cannot create private service directory")?;
    crate::platform::validate_private_dir(directory)
        .map_err(|_| "Service directory is not private and owned")?;
    let lock = crate::platform::open_private(&directory.join(".telegram-config.lock"), true, true)
        .map_err(|_| "Cannot open service configuration lock")?;
    crate::platform::validate_private_file(&lock)
        .map_err(|_| "Unsafe service configuration lock")?;
    crate::platform::lock_exclusive(&lock, true).map_err(|_| "Service configuration is busy")?;
    Ok(ConfigurationLock(lock))
}
#[cfg(windows)]
pub fn private_connection(path: &Path) -> Result<rusqlite::Connection> {
    valid_path(path)?;
    let parent = path.parent().ok_or("Missing database directory")?;
    crate::platform::ensure_private_dir(parent)
        .map_err(|_| "Cannot create private database directory")?;
    crate::platform::validate_private_dir(parent)
        .map_err(|_| "Database directory is not private and owned")?;
    let file = crate::platform::prepare_sqlite(path)
        .map_err(|_| "Cannot prepare private database and rollback journal")?;
    crate::platform::validate_private_file(&file)
        .map_err(|_| "Database is not a private owned regular file")?;
    let identity =
        crate::platform::file_identity(&file).map_err(|_| "Cannot inspect database identity")?;
    file.sync_all()
        .map_err(|_| "Cannot sync private database")?;
    let named = crate::platform::open_private(path, false, false)
        .map_err(|_| "Cannot recheck database identity")?;
    if crate::platform::file_identity(&named).map_err(|_| "Cannot recheck database identity")?
        != identity
    {
        return Err("Database identity changed".into());
    }
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "Cannot open database connection")?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "Cannot configure database")?;
    connection
        .execute_batch("PRAGMA temp_store=MEMORY;")
        .map_err(|_| "Cannot configure private database temporary storage")?;
    let mode: String = connection
        .query_row("PRAGMA journal_mode=TRUNCATE", [], |row| row.get(0))
        .map_err(|_| "Cannot select private rollback journal mode")?;
    if !mode.eq_ignore_ascii_case("truncate") {
        return Err(
            "Database requires private TRUNCATE journaling; existing state was not removed".into(),
        );
    }
    connection
        .execute_batch("PRAGMA synchronous=FULL;")
        .map_err(|_| "Cannot configure database durability")?;
    Ok(connection)
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    #[test]
    fn private_database_and_configuration_locks_require_owner_scoped_state() {
        let directory = std::env::temp_dir().join(format!(
            "daedalus-config-win-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        crate::platform::ensure_private_dir(&directory).unwrap();
        {
            let lock = configuration_lock(&directory).unwrap();
            assert!(configuration_lock(&directory).is_err());
            drop(lock);
            let _next = configuration_lock(&directory).unwrap();
            let database = directory.join("state.sqlite3");
            let connection = private_connection(&database).unwrap();
            let mode: String = connection
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            assert_eq!(mode.to_ascii_lowercase(), "truncate");
            connection
                .execute_batch("CREATE TABLE retained(id INTEGER PRIMARY KEY);")
                .unwrap();
            drop(connection);
            let journal = std::path::PathBuf::from(format!("{}-journal", database.display()));
            crate::platform::validate_private_file(
                &crate::platform::open_private(&journal, false, false).unwrap(),
            )
            .unwrap();
            assert!(!std::path::PathBuf::from(format!("{}-wal", database.display())).exists());
            assert!(!std::path::PathBuf::from(format!("{}-shm", database.display())).exists());
            crate::platform::validate_private_file(
                &crate::platform::open_private(&database, false, false).unwrap(),
            )
            .unwrap();
            let alias = directory.join("alias.sqlite3");
            std::fs::hard_link(&database, &alias).unwrap();
            assert!(private_connection(&database).is_err());
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("daedalus-service-{}-{nonce}", std::process::id()));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn config(directory: &Path) -> ServiceConfig {
        ServiceConfig {
            version: 1,
            instance: "example".into(),
            bot_id: 123,
            token_file: directory.join("token"),
            grants: vec![Grant {
                chat: 42,
                actor: 7,
                permissions: vec![Access::Read, Access::Send, Access::Manage],
            }],
            listen_chats: BTreeSet::new(),
            registrations: vec![],
            hook_bindings: vec![],
        }
    }
    #[test]
    fn fresh_is_inert_redacted_private_and_idempotent() {
        let temp = Temp::new();
        let path = temp.0.join("service.json");
        let config = config(&temp.0);
        let plan = ServicePlan::fresh(&path, config).unwrap();
        assert!(!path.exists());
        assert!(!plan.review().to_string().contains("token_file"));
        assert!(plan.apply("incorrect").is_err());
        assert!(plan.apply(&plan.approval_token()).unwrap());
        assert!(!plan.apply(&plan.approval_token()).unwrap());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(ServiceConfig::load(&path).unwrap().grants[0].actor, 7);
    }

    #[test]
    fn configuration_lock_releases_even_with_an_inherited_descriptor() {
        use std::os::fd::FromRawFd;
        let temp = Temp::new();
        let lock = configuration_lock(&temp.0).unwrap();
        let duplicate = unsafe { libc::fcntl(lock.0.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        assert!(duplicate >= 0);
        let _inherited = unsafe { fs::File::from_raw_fd(duplicate) };
        assert!(configuration_lock(&temp.0).is_err());
        drop(lock);
        assert!(configuration_lock(&temp.0).is_ok());
    }
    #[test]
    fn import_requires_private_files_preserves_source_and_backs_up() {
        let temp = Temp::new();
        let source = temp.0.join("source.json");
        let path = temp.0.join("service.json");
        let mut initial = config(&temp.0);
        let plan = ServicePlan::fresh(&path, initial.clone()).unwrap();
        plan.apply(&plan.approval_token()).unwrap();
        let before = fs::read(&path).unwrap();
        initial.grants.clear();
        layouts::write_private(&source, &serde_json::to_vec(&initial).unwrap()).unwrap();
        let source_before = fs::read(&source).unwrap();
        let plan = ServicePlan::import(&path, &source).unwrap();
        assert!(!plan.config.grants.iter().any(|g| g.actor == 7));
        plan.apply(&plan.approval_token()).unwrap();
        assert_eq!(fs::read(&source).unwrap(), source_before);
        let backup = temp.0.join(format!(
            "telegram-service.backup-{:x}.json",
            Sha256::digest(&before)
        ));
        assert_eq!(fs::read(backup).unwrap(), before);
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ServicePlan::import(&path, &source).is_err());
    }
    #[test]
    fn token_check_is_local_redacted_bound_and_never_creates_files() {
        let temp = Temp::new();
        let path = temp.0.join("service.json");
        let config = config(&temp.0);
        let plan = ServicePlan::fresh(&path, config.clone()).unwrap();
        plan.apply(&plan.approval_token()).unwrap();
        assert!(config.read_token().is_err());
        assert!(!config.token_file.exists());
        layouts::write_private(&config.token_file, b"123:fixture_secret\n").unwrap();
        let count = fs::read_dir(&temp.0).unwrap().count();
        let mut out = vec![];
        run(
            &[
                "check".into(),
                "--file".into(),
                path.to_str().unwrap().into(),
            ],
            &mut out,
        )
        .unwrap();
        let output = String::from_utf8(out).unwrap();
        assert!(!output.contains("fixture_secret"));
        assert_eq!(count, fs::read_dir(&temp.0).unwrap().count());
        layouts::write_private(
            &config.token_file,
            b"TELEGRAM_BOT_TOKEN=123:fixture_secret\n",
        )
        .unwrap();
        assert_eq!(config.read_token().unwrap(), "123:fixture_secret");
        for bytes in [
            b"TELEGRAM_BOT_TOKEN=$(false)".as_slice(),
            b"TELEGRAM_BOT_TOKEN=123:fixture_secret\nOWNER_ID=7",
            b"export TELEGRAM_BOT_TOKEN=123:fixture_secret",
            b"TELEGRAM_BOT_TOKEN='123:fixture_secret'",
        ] {
            layouts::write_private(&config.token_file, bytes).unwrap();
            assert!(config.read_token().is_err());
        }
        layouts::write_private(&config.token_file, b"999:other").unwrap();
        assert!(config.read_token().is_err());
    }
    #[test]
    fn cli_preview_and_explicit_apply_and_import() {
        let temp = Temp::new();
        let path = temp.0.join("service.json");
        let token = temp.0.join("token");
        let args = vec![
            "init".into(),
            "--file".into(),
            path.to_str().unwrap().into(),
            "--instance".into(),
            "example".into(),
            "--bot-id".into(),
            "123".into(),
            "--token-file".into(),
            token.to_str().unwrap().into(),
            "--owner".into(),
            "7".into(),
            "--chat".into(),
            "42".into(),
        ];
        run(&args, &mut vec![]).unwrap();
        assert!(!path.exists());
        let mut apply = args.clone();
        apply.push("--apply".into());
        run(&apply, &mut vec![]).unwrap();
        let dest = temp.0.join("copy.json");
        run(
            &[
                "import".into(),
                "--file".into(),
                dest.to_str().unwrap().into(),
                "--from".into(),
                path.to_str().unwrap().into(),
                "--apply".into(),
            ],
            &mut vec![],
        )
        .unwrap();
        assert_eq!(ServiceConfig::load(&dest).unwrap().grants[0].actor, 7);
    }
    #[test]
    fn invalid_grants_paths_and_databases_fail_closed() {
        let temp = Temp::new();
        let mut config = config(&temp.0);
        config.grants.push(config.grants[0].clone());
        assert!(config.validate().is_err());
        config.grants.pop();
        config.listen_chats.insert(-42);
        assert!(config.validate().is_err());
        config.listen_chats.clear();
        config
            .registrations
            .push(temp.0.join("missing-registration.json"));
        assert!(config.check_registrations().is_err());
        config.registrations.clear();
        let target = temp.0.join("target");
        layouts::write_private(&target, b"123:fixture").unwrap();
        let link = temp.0.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        config.token_file = link.clone();
        assert!(config.validate().is_err());
        assert!(private_connection(&link).is_err());
        let db = temp.0.join("routes.sqlite");
        drop(private_connection(&db).unwrap());
        assert_eq!(fs::metadata(&db).unwrap().mode() & 0o777, 0o600);
        let alias = temp.0.join("alias.sqlite");
        fs::hard_link(&db, &alias).unwrap();
        assert!(private_connection(&db).is_err());
        let separate = temp.0.join("separate.sqlite");
        let sidecar = temp.0.join("separate.sqlite-journal");
        std::os::unix::fs::symlink(&target, &sidecar).unwrap();
        assert!(private_connection(&separate).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"123:fixture");
    }
    #[test]
    fn load_accepts_a_utf8_bom_without_exposing_the_token_file() {
        let temp = Temp::new();
        let path = temp.0.join("service.json");
        let config = config(&temp.0);
        let plan = ServicePlan::fresh(&path, config).unwrap();
        plan.apply(&plan.approval_token()).unwrap();
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend(fs::read(&path).unwrap());
        layouts::write_private(&path, &bytes).unwrap();
        let loaded = ServiceConfig::load(&path).unwrap();
        assert_eq!(loaded.bot_id, 123);
        assert_eq!(loaded.instance, "example");
        assert!(!in_install_tree(&path));
        let rendered = serde_json::to_string(&loaded.preview()).unwrap();
        assert!(!rendered.contains("token_file"));
    }
    #[test]
    fn stale_review_rejected_without_overwriting_configuration() {
        let temp = Temp::new();
        let path = temp.0.join("service.json");
        let plan = ServicePlan::fresh(&path, config(&temp.0)).unwrap();
        layouts::write_private(&path, b"{}").unwrap();
        assert!(plan.apply(&plan.approval_token()).is_err());
        assert_eq!(fs::read(path).unwrap(), b"{}");
    }
    #[test]
    fn hook_recipient_must_match_exact_configured_registration_scope() {
        use super::super::{
            hook_runtime::{Options, Registration},
            hooks::{EventKind, Harness},
        };
        use crate::core::{
            goal_store::Scope,
            identity::{Backend, TerminalId},
            routes,
        };
        let temp = Temp::new();
        let registration = Registration {
            version: 1,
            id: "hook".into(),
            directory: temp.0.clone(),
            scope: Scope {
                instance: "example".into(),
                terminal: TerminalId {
                    backend: Backend::Herdr,
                    endpoint: "example".into(),
                    pane: "pane".into(),
                    generation: "generation".into(),
                },
                conversation: "session".into(),
                harness: Harness::Claude,
            },
            goal_id: None,
            harness_process: None,
            server_emitter: None,
            transcript: None,
            events: vec![EventKind::Stop],
            options: Options::default(),
            activity_lease_ms: 30000,
            pending_timeout_ms: 30000,
            hook_timeout_ms: 12000,
        };
        layouts::write_private(
            &registration.path(),
            &serde_json::to_vec(&registration).unwrap(),
        )
        .unwrap();
        let mut config = config(&temp.0);
        config.registrations.push(registration.path());
        config.hook_bindings.push(Binding {
            registration_id: "hook".into(),
            goal_scope: registration.scope.clone(),
            route_scope: routes::Scope {
                instance: "example".into(),
                bot: "123".into(),
                chat: "42".into(),
            },
            actor: "7".into(),
            route_revision: 1,
            reply_to: None,
            hindsight_bank: None,
        });
        assert_eq!(config.check_registrations().unwrap().len(), 1);
        config.grants[0]
            .permissions
            .retain(|permission| *permission != Access::Read);
        assert!(config.validate().is_err());
        config.grants[0].permissions.push(Access::Read);
        config.hook_bindings[0].goal_scope.conversation = "other-session".into();
        assert!(config.check_registrations().is_err());
        config.hook_bindings.clear();
        config.instance = "other-instance".into();
        assert!(config.check_registrations().is_err());
    }
}
