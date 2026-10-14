//! Offline native-owner handoff. No bot, harness, or supervisor is started here.
//! Locks prove quiescence only for the native cooperating service/hook protocol.
use super::{
    migration::ImportPlan,
    telegram_config::{ServiceConfig, private_connection},
    telegram_service::Owner,
};
use crate::{
    adapters::sqlite,
    core::{identity::TerminalId, layouts, routes::Terminal},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    time::Duration,
};
pub type Result<T> = std::result::Result<T, String>;
const PROOF: &str = "cutover.json";
const RETIRED: &str = "cutover-retired.json";
const STARTED: &str = "cutover-started.json";
// Reviewed native_owner_v1 compatibility implementation at ee0ce3bd409d9d387a4d629af203858c55912c54.
// Pin protocol evidence independently of bundled Python source: editing or removing
// that source must never silently change the set of accepted handoff producers.
const LEGACY_GUARD_SHA256: &str =
    "bd4ad6624fb3628b102b1c09b2f20b239666380c76528e2de531d76d549090b6";
const LEGACY_FEEDER_SHA256: &str =
    "406849909be46065463c57e540b87dd03c75c09d5a2a5eb78737729423128115";
// Reviewed removal of raw console mirror; graceful handoff protocol unchanged.
const MIRRORLESS_FEEDER_SHA256: &str =
    "16681d71e7977d94379ab24723a555259477cb952425cfad0080e9f9c7a11dcf";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    version: u32,
    transaction: String,
    state: String,
    source: PathBuf,
    destination: PathBuf,
    backup: PathBuf,
    instance: String,
    bot_id: i64,
    manifest_sha256: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub verified_chats: Vec<String>,
    pub unresolved_chats: Vec<String>,
    pub approval: String,
    pub notices: Vec<String>,
}
struct Locks {
    _owner: Owner,
    _files: Vec<CutoverLock>,
}
fn private_dir(path: &Path) -> Result<()> {
    layouts::check_path(path)?;
    let m = fs::metadata(path).map_err(|_| "Instance directory missing")?;
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
        return Err("Instance directory must be private and owned".into());
    }
    Ok(())
}
struct CutoverLock(File);
impl Drop for CutoverLock {
    fn drop(&mut self) {
        // A concurrent subprocess can inherit the open file description before
        // exec closes CLOEXEC descriptors. Release ownership at guard exit.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
fn lock(path: &Path) -> Result<CutoverLock> {
    layouts::check_path(path)?;
    private_dir(path.parent().ok_or("Missing lock parent")?)?;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "Cannot open cutover lock")?;
    let m = f.metadata().map_err(|_| "Cannot inspect cutover lock")?;
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
        || unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
    {
        return Err("Instance is busy or cutover lock is unsafe".into());
    }
    Ok(CutoverLock(f))
}
fn acquire(source: &Path, destination: &Path, global: &Path, bot: i64) -> Result<Locks> {
    private_dir(source)?;
    private_dir(destination)?;
    let owner = Owner::acquire(source, global, bot)?;
    let mut files = vec![lock(&destination.join(".telegram-service.lock"))?];
    for directory in [source, destination] {
        for name in [".cutover.lock", ".telegram-config.lock", ".setup.lock"] {
            files.push(lock(&directory.join(name))?);
        }
    }
    Ok(Locks {
        _owner: owner,
        _files: files,
    })
}
fn read(path: &Path) -> Result<Vec<u8>> {
    layouts::check_path(path)?;
    let m = fs::symlink_metadata(path).map_err(|_| "Cutover state missing")?;
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
    {
        return Err("Cutover state must be private and owned".into());
    }
    layouts::read_private(path)?.ok_or("Cutover state missing".into())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn proof(path: &Path) -> Result<Proof> {
    let p: Proof = serde_json::from_slice(&read(path)?).map_err(|_| "Invalid cutover proof")?;
    if p.version != 1
        || p.transaction.len() != 64
        || !p.transaction.bytes().all(|b| b.is_ascii_hexdigit())
        || p.bot_id <= 0
    {
        return Err("Invalid cutover proof".into());
    }
    for path in [&p.source, &p.destination, &p.backup] {
        layouts::check_path(path)?;
    }
    Ok(p)
}
fn write_proof(path: &Path, p: &Proof) -> Result<()> {
    layouts::write_private(
        path,
        &serde_json::to_vec_pretty(p).map_err(|_| "Cannot encode cutover proof")?,
    )
}
fn sync_parent(path: &Path) -> Result<()> {
    File::open(path.parent().ok_or("Missing parent")?)
        .and_then(|f| f.sync_all())
        .map_err(|_| "Cannot sync cutover directory".into())
}
fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}
fn imported_source(destination: &Path) -> Result<PathBuf> {
    let value: Value = serde_json::from_slice(&read(&destination.join("migration.json"))?)
        .map_err(|_| "Invalid import manifest")?;
    if value["version"] != 1 || value["activation"] != "requires_cutover" {
        return Err("Unsupported import manifest".into());
    }
    let source = PathBuf::from(value["source"].as_str().ok_or("Import source missing")?);
    layouts::check_path(&source)?;
    Ok(source)
}
/// Root calls this while holding Owner, before opening polling or delivery.
/// Fresh instances pass without a cutover record. Retired sources fail closed.
pub fn activation_check(directory: &Path) -> Result<()> {
    if exists(&directory.join(RETIRED)) {
        return Err(
            "This instance was retired by cutover; use the destination or explicit rollback".into(),
        );
    }
    if exists(&directory.join(".cutover-transaction.json")) {
        return Err("Incomplete coordinated cutover requires recovery".into());
    }
    if !exists(&directory.join("migration.json")) {
        return Ok(());
    }
    let p = proof(&directory.join(PROOF))?;
    if p.state != "committed"
        || p.destination != directory
        || p.source != imported_source(directory)?
        || hash(&read(&directory.join("migration.json"))?) != p.manifest_sha256
    {
        return Err("Imported instance requires coordinated polling/route cutover".into());
    }
    let retired = proof(&p.source.join(RETIRED))?;
    if retired.state != "retired"
        || retired.transaction != p.transaction
        || retired.destination != directory
    {
        return Err("Source retirement proof does not match destination".into());
    }
    let config = ServiceConfig::load(&directory.join("telegram-service.json"))?;
    if config.bot_id != p.bot_id || config.instance != p.instance {
        return Err("Cutover bot or instance identity changed".into());
    }
    Ok(())
}
/// Mark the point after which rollback must be another coordinated migration:
/// source cursors cannot safely resume once the destination might have polled.
pub fn mark_activation(directory: &Path) -> Result<()> {
    activation_check(directory)?;
    if exists(&directory.join(PROOF)) {
        let p = proof(&directory.join(PROOF))?;
        layouts::write_private(
            &directory.join(STARTED),
            &serde_json::to_vec(
                &json!({"version":1,"transaction":p.transaction,"state":"activation_requested"}),
            )
            .map_err(|_| "Cannot encode activation marker")?,
        )?;
    }
    Ok(())
}

pub struct Plan {
    pub preview: Preview,
    import: ImportPlan,
    _locks: Locks,
    proof: Proof,
    destination_service: Vec<u8>,
    source_service: Vec<u8>,
    verified: Vec<Terminal>,
    source_token_sha: String,
    legacy: bool,
    proc_root: PathBuf,
    memory_fingerprint: String,
}
impl Plan {
    /// Observe must return fresh complete live evidence. Called after native
    /// service, hook and configuration locks have been acquired.
    pub fn prepare(
        destination: &Path,
        global: &Path,
        observe: impl FnMut() -> Result<Vec<Terminal>>,
    ) -> Result<Self> {
        Self::prepare_with_process_root(destination, global, Path::new("/proc"), observe)
    }
    fn prepare_with_process_root(
        destination: &Path,
        global: &Path,
        proc_root: &Path,
        mut observe: impl FnMut() -> Result<Vec<Terminal>>,
    ) -> Result<Self> {
        let source = imported_source(destination)?;
        if source == destination
            || source.starts_with(destination)
            || destination.starts_with(&source)
        {
            return Err("Cutover requires separate source and destination".into());
        }
        let selected = ServiceConfig::load(&destination.join("telegram-service.json"))?;
        let legacy = !exists(&source.join("telegram-service.json"));
        if !legacy {
            let current = ServiceConfig::load(&source.join("telegram-service.json"))?;
            if current.instance != selected.instance || current.bot_id != selected.bot_id {
                return Err(
                    "Native cutover requires the same explicit instance and bot identity".into(),
                );
            }
            if exists(&source.join("tg_offset")) {
                return Err(
                    "Mixed legacy/native ownership requires explicit lifecycle review".into(),
                );
            }
        }
        let locks = acquire(&source, destination, global, selected.bot_id)?;
        let legacy_evidence = if legacy {
            Some(legacy_stopped(&source, selected.bot_id, proc_root)?)
        } else {
            None
        };
        if exists(&source.join(RETIRED))
            || exists(&destination.join(PROOF))
            || exists(&destination.join(".cutover-transaction.json"))
        {
            return Err("Cutover already committed or incomplete; inspect/recover first".into());
        }
        let source_service = read(&source.join(if legacy {
            "legacy-stopped.json"
        } else {
            "telegram-service.json"
        }))?;
        let destination_service = read(&destination.join("telegram-service.json"))?;
        let source_token = source_token(&source, legacy, selected.bot_id)?;
        let source_token_sha = hash(source_token.as_bytes());
        if source_token != selected.read_token()? {
            return Err("Source and destination token identities disagree".into());
        }
        let memory_fingerprint = memory_fingerprint(destination)?;
        let mut import =
            ImportPlan::prepare_with_memory_credentials(&source, destination, true, false)?;
        let stage = import.staging_path().to_path_buf();
        // Final snapshot must not broaden optional credential consent. Keep the
        // destination's reviewed memory configuration and its own provisioned key.
        if let Some(mut memory) = super::memory::load(destination)? {
            let staged_key = stage.join("hindsight-token.txt");
            if exists(&staged_key) {
                fs::remove_file(&staged_key)
                    .map_err(|_| "Cannot preserve memory credential selection")?;
            }
            if let Some(path) = &memory.credential_file {
                if let Some(bytes) = super::telegram_config::read_private(path, 4096)? {
                    layouts::write_private(&staged_key, &bytes)?;
                }
                memory.credential_file = Some(destination.join("hindsight-token.txt"));
            }
            layouts::write_private(
                &stage.join("hindsight.json"),
                &serde_json::to_vec_pretty(&memory)
                    .map_err(|_| "Cannot preserve selected memory config")?,
            )?;
        } else {
            for name in [
                "hindsight.json",
                "hindsight-token.txt",
                "memory-review.json",
            ] {
                if exists(&stage.join(name)) {
                    fs::remove_file(stage.join(name))
                        .map_err(|_| "Cannot retain optional memory selection")?;
                }
            }
        }

        // Retain explicitly reviewed destination grants and policy, but use final
        // native registrations relocated by the final source snapshot.
        let mut service = if legacy {
            let mut reviewed = selected.clone();
            let token = destination.join("telegram-token.txt");
            layouts::write_private(&stage.join("telegram-token.txt"), source_token.as_bytes())?;
            reviewed.token_file = token;
            // Preserve explicitly enrolled destination registrations, never infer legacy identity.
            for (path, registration) in selected.check_registrations()? {
                let relative = path
                    .strip_prefix(destination)
                    .map_err(|_| "Destination hook registration must be inside its instance")?;
                layouts::write_private(
                    &stage.join(relative),
                    &serde_json::to_vec_pretty(&registration)
                        .map_err(|_| "Invalid registration")?,
                )?;
            }
            reviewed.hook_bindings.clear();
            reviewed
        } else {
            ServiceConfig::load(&stage.join("telegram-service.json"))?
        };
        service.grants = selected.grants.clone();
        service.listen_chats = selected.listen_chats.clone();
        let live = observe()?;
        let (verified_chats, unresolved_chats, verified) = if legacy {
            reconcile_legacy_routes(&stage, &service)?
        } else {
            reconcile_routes(&stage, &service, &live)?
        };
        super::migration::apply_mention_policy(&stage, &service)?;
        let route_db = private_connection(&stage.join("routes.sqlite3"))?;
        service.hook_bindings.retain(|binding| {
            if !verified_chats.contains(&binding.route_scope.chat)
                || !service.grants.iter().any(|grant| grant.chat.to_string() == binding.route_scope.chat
                    && grant.actor.to_string() == binding.actor
                    && grant.permissions.contains(&super::telegram_config::Access::Send)) { return false; }
            route_db.query_row("SELECT c.identity,c.revision,t.conversation FROM chat_routes c JOIN route_terminals t ON t.identity=c.identity WHERE c.instance=? AND c.bot=? AND c.chat=?",
                rusqlite::params![binding.route_scope.instance,binding.route_scope.bot,binding.route_scope.chat],
                |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,Option<String>>(2)?)))
                .is_ok_and(|(identity,revision,conversation)| serde_json::from_str::<TerminalId>(&identity).ok().as_ref() == Some(&binding.goal_scope.terminal)
                    && revision == binding.route_revision && conversation.as_deref() == Some(binding.goal_scope.conversation.as_str()))
        });
        drop(route_db);
        service.validate()?;
        layouts::write_private(
            &stage.join("telegram-service.json"),
            &serde_json::to_vec_pretty(&service).map_err(|_| "Cannot encode cutover service")?,
        )?;
        if let Some(evidence) = &legacy_evidence {
            for name in [
                "legacy-owner-protocol.json",
                "legacy-stopped.json",
                "legacy-handoff-inputs.json",
            ] {
                layouts::write_private_limit(
                    &stage.join(name),
                    &super::telegram_config::read_private(&source.join(name), 32 * 1024 * 1024)?
                        .ok_or("Legacy evidence missing")?,
                    32 * 1024 * 1024,
                )?;
            }
            // Legacy delivery and command encodings are retained for review only.
            // They must never become retryable native commands merely through import.
            if exists(&stage.join("delivery.sqlite3")) {
                fs::rename(
                    stage.join("delivery.sqlite3"),
                    stage.join("legacy-delivery.sqlite3"),
                )
                .map_err(|_| "Cannot retain legacy delivery evidence")?;
            }
            let journal = crate::core::journal::Journal::open(&stage.join("delivery.sqlite3"))
                .map_err(|_| "Cannot initialize native journal")?;
            drop(journal);
            // Only this offline migration may seed a cursor without fabricating
            // an admitted native event: the matching durable legacy stop proof
            // is the acknowledgment receipt retained alongside the journal.
            private_connection(&stage.join("delivery.sqlite3"))?
                .execute(
                    "INSERT INTO cursors(channel,bot,offset) VALUES('telegram',?,?)",
                    rusqlite::params![
                        service.bot_id.to_string(),
                        evidence["next_offset"]
                            .as_i64()
                            .ok_or("Missing final legacy cursor")?
                    ],
                )
                .map_err(|_| "Cannot initialize final legacy polling cursor")?;
        } else if !exists(&stage.join("delivery.sqlite3")) {
            return Err(
                "Native source needs a durable delivery journal for final cursor snapshot".into(),
            );
        }
        let con = private_connection(&stage.join("delivery.sqlite3"))?;
        con.execute("UPDATE inbox SET state='unknown',error='cutover retained uncertain processing control' WHERE state='processing'",[]).map_err(|_|"Invalid native event journal")?;
        con.execute("UPDATE delivery SET state='unknown',error='cutover retained uncertain in-flight delivery' WHERE state='intent'",[]).map_err(|_|"Invalid native delivery journal")?;
        let (attachments_copied, attachments_blocked) =
            relocate_attachments(&con, &source, destination, &stage)?;
        drop(con);
        import.seal_cutover()?;
        let manifest_sha256 = hash(&read(&stage.join("migration.json"))?);
        let transaction = hash(
            &serde_json::to_vec(&(
                &source,
                destination,
                &manifest_sha256,
                &destination_service,
                &verified_chats,
                &unresolved_chats,
            ))
            .map_err(|_| "Cannot encode cutover review")?,
        );
        let backup = destination
            .parent()
            .ok_or("Missing destination parent")?
            .join(format!(".daedalus-rollback-{transaction}"));
        if exists(&backup) {
            return Err("Cutover rollback path already exists".into());
        }
        let proof = Proof {
            version: 1,
            transaction: transaction.clone(),
            state: "committed".into(),
            source: source.clone(),
            destination: destination.into(),
            backup,
            instance: service.instance.clone(),
            bot_id: service.bot_id,
            manifest_sha256,
        };
        write_proof(&stage.join(PROOF), &proof)?;
        let mut preview=Preview{source,destination:destination.into(),verified_chats,unresolved_chats,approval:transaction,notices:vec!["Final snapshot taken while native service and hooks are locked; no services started.".into(),"Only exact live terminal generation and conversation matches retain routes. Unresolved routes and their hook recipient bindings are disabled.".into(),"Original source is retired on commit. A private destination rollback snapshot is retained. Rollback is allowed only before destination activation.".into(),"Harness hooks must be installed against relocated destination registration files before feeding resumes.".into(),format!("Queued attachments: {attachments_copied} verified references relocated; {attachments_blocked} unsafe drafts blocked. Uncertain evidence unchanged.")]};
        if legacy {
            preview.notices.push("Legacy acknowledged coalesced and uncertain inputs are retained privately without replay. The legacy delivery journal is review-only; native polling resumes at the verified final cursor. All legacy routes require explicit identity rebinding; no labels are trusted.".into());
        }
        Ok(Self {
            preview,
            import,
            _locks: locks,
            proof,
            destination_service,
            source_service,
            verified,
            source_token_sha,
            legacy,
            proc_root: proc_root.into(),
            memory_fingerprint,
        })
    }
    pub fn apply(
        self,
        approval: &str,
        mut observe: impl FnMut() -> Result<Vec<Terminal>>,
    ) -> Result<PathBuf> {
        if memory_fingerprint(&self.proof.destination)? != self.memory_fingerprint {
            return Err("Memory configuration or credential changed after review".into());
        }
        if approval != self.preview.approval {
            return Err("Approval does not match the final cutover plan".into());
        }
        if read(&self.proof.destination.join("telegram-service.json"))? != self.destination_service
            || read(&self.proof.source.join(if self.legacy {
                "legacy-stopped.json"
            } else {
                "telegram-service.json"
            }))? != self.source_service
        {
            return Err("Service configuration changed after review".into());
        }
        if hash(source_token(&self.proof.source, self.legacy, self.proof.bot_id)?.as_bytes())
            != self.source_token_sha
        {
            return Err("Source token changed after review".into());
        }
        if self.legacy {
            legacy_stopped(&self.proof.source, self.proof.bot_id, &self.proc_root)?;
        }
        let live = observe()?;
        for expected in &self.verified {
            if live
                .iter()
                .filter(|row| {
                    row.identity == expected.identity
                        && row.conversation == expected.conversation
                        && row.harness == expected.harness
                        && row.available
                })
                .count()
                != 1
            {
                return Err("Verified terminal changed after cutover review".into());
            }
        }
        let mut pending = self.proof.clone();
        pending.state = "preparing".into();
        write_proof(&self.proof.source.join(RETIRED), &pending)?;
        write_proof(
            &self.proof.destination.join(".cutover-transaction.json"),
            &pending,
        )?;
        rename_new(&self.proof.destination, &self.proof.backup)?;
        sync_parent(&self.proof.backup)?;
        rename_new(self.import.staging_path(), &self.proof.destination)?;
        sync_parent(&self.proof.destination)?;
        pending.state = "retired".into();
        write_proof(&self.proof.source.join(RETIRED), &pending)?;
        Ok(self.proof.backup.clone())
    }
}
fn memory_fingerprint(directory: &Path) -> Result<String> {
    let config =
        super::telegram_config::read_private(&directory.join("hindsight.json"), 1024 * 1024)?;
    let token = if let Some(memory) = super::memory::load(directory)? {
        memory
            .credential_file
            .as_ref()
            .map(|path| super::telegram_config::read_private(path, 4096))
            .transpose()?
            .flatten()
    } else {
        None
    };
    Ok(hash(
        &serde_json::to_vec(&(config, token)).map_err(|_| "Cannot snapshot memory selection")?,
    ))
}
fn source_token(source: &Path, legacy: bool, bot: i64) -> Result<String> {
    if !legacy {
        return ServiceConfig::load(&source.join("telegram-service.json"))?.read_token();
    }
    let bytes = read(&source.join("telegram.env"))?;
    let values = super::migration::telegram_env(&bytes)?;
    let token = values
        .get("TELEGRAM_BOT_TOKEN")
        .ok_or("Legacy token missing")?;
    super::telegram_config::validate_token(bot, token.as_bytes())
}
fn legacy_processes_absent(source: &Path, proc_root: &Path) -> Result<()> {
    let entries = fs::read_dir(proc_root).map_err(|_| "Cannot verify legacy process quiescence")?;
    let known = [
        "tg_feeder",
        "outbound_flusher",
        "tg_stream",
        "tg_live",
        "tg_status",
        "frame_verify",
        "pre_tool_use",
        "turnstate",
        "tg_autosend",
        "tg_verbose",
        "session_identity",
        "notify",
        "memory_recall",
        "memory_persist",
        "harness_memory",
        "record_task",
        "praxis_mode",
        "goal_loop",
    ];
    for (count, entry) in entries.enumerate() {
        if count > 100_000 {
            return Err("Process inventory exceeds verification limit".into());
        }
        let entry = entry.map_err(|_| "Cannot inspect process inventory")?;
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let dir = entry.path();
        let meta = match fs::metadata(&dir) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Cannot inspect process owner".into()),
        };
        if meta.uid() != unsafe { libc::geteuid() } {
            continue;
        }
        let command = match fs::read(dir.join("cmdline")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Cannot verify legacy process command".into()),
        };
        let relevant = command.split(|b| *b == 0).take(5).any(|arg| {
            let path = Path::new(std::ffi::OsStr::from_bytes(arg));
            path.file_stem().is_some_and(|name| {
                known.iter().any(|candidate| {
                    name == *candidate || name.to_string_lossy().ends_with(&format!("_{candidate}"))
                })
            })
        });
        if !relevant {
            continue;
        }
        let environment = match fs::read(dir.join("environ")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Cannot verify legacy process scope".into()),
        };
        let mut scope = None;
        for value in environment.split(|b| *b == 0) {
            if let Some(value) = value.strip_prefix(b"CONFIG_DIR=") {
                scope = Some(PathBuf::from(std::ffi::OsStr::from_bytes(value)));
                break;
            }
        }
        let scoped = scope
            .as_deref()
            .map(fs::canonicalize)
            .transpose()
            .map_err(|_| "Cannot resolve legacy process configuration scope")?;
        if scoped.as_deref().is_none_or(|path| path == source) {
            return Err(
                "A legacy feeder or hook process still needs shutdown by its original supervisor"
                    .into(),
            );
        }
    }
    Ok(())
}
fn legacy_stopped(source: &Path, bot: i64, proc_root: &Path) -> Result<Value> {
    legacy_processes_absent(source, proc_root)?;
    let owner: Value = serde_json::from_slice(&read(&source.join("legacy-owner-protocol.json"))?)
        .map_err(|_| "Invalid legacy owner evidence")?;
    let stopped: Value = serde_json::from_slice(&read(&source.join("legacy-stopped.json"))?)
        .map_err(|_| "Legacy feeder must be gracefully stopped by its original supervisor")?;
    for key in [
        "version",
        "protocol",
        "directory",
        "bot_id",
        "pid",
        "run_id",
        "feeder_script",
        "feeder_sha256",
        "guard_sha256",
    ] {
        if owner.get(key).is_none() || owner[key] != stopped[key] {
            return Err("Legacy stop proof does not match its latest owner run".into());
        }
    }
    if owner["version"] != 1
        || owner["protocol"] != "native_owner_v1"
        || owner["directory"].as_str() != source.to_str()
        || owner["bot_id"] != bot
        || stopped["state"] != "stopped"
        || stopped["next_offset"]
            .as_i64()
            .is_none_or(|offset| offset < 0)
        || owner["run_id"]
            .as_str()
            .is_none_or(|id| id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("Legacy ownership proof is incomplete".into());
    }
    let inputs = super::telegram_config::read_private(
        &source.join("legacy-handoff-inputs.json"),
        32 * 1024 * 1024,
    )?
    .ok_or("Legacy pending input evidence missing")?;
    let payload: Value =
        serde_json::from_slice(&inputs).map_err(|_| "Invalid legacy pending input evidence")?;
    if payload["run_id"] != owner["run_id"]
        || payload["replay"] != "forbidden"
        || stopped["inputs_sha256"] != hash(&inputs)
        || std::str::from_utf8(&read(&source.join("tg_offset"))?)
            .ok()
            .and_then(|value| value.trim().parse::<i64>().ok())
            != stopped["next_offset"].as_i64()
    {
        return Err("Legacy final cursor or input evidence changed".into());
    }
    // Runtime ownership files alone cannot establish that the running legacy code
    // implements the protocol. Match the reviewed compatibility implementation.
    if owner["guard_sha256"] != LEGACY_GUARD_SHA256
        || ![LEGACY_FEEDER_SHA256, MIRRORLESS_FEEDER_SHA256]
            .iter()
            .any(|sha| owner["feeder_sha256"].as_str() == Some(*sha))
    {
        return Err("Legacy feeder must run the installed graceful-handoff compatibility version before stopping".into());
    }
    Ok(stopped)
}
fn reconcile_legacy_routes(
    stage: &Path,
    service: &ServiceConfig,
) -> Result<(Vec<String>, Vec<String>, Vec<Terminal>)> {
    use crate::core::routes::{self, Registry};
    // Preserve aliases as review plans. No terminal generation is reconstructed
    // from a display name, process PID, or stale registry timestamp.
    let bytes = if exists(&stage.join("legacy-routes.json")) {
        read(&stage.join("legacy-routes.json"))?
    } else {
        b"{}".to_vec()
    };
    let preview = routes::preview_import(
        &bytes,
        &service.instance,
        &service.bot_id.to_string(),
        &[],
        &[],
        &[],
    )
    .map_err(|_| "Cannot retain legacy route review")?;
    let unresolved = preview
        .routes
        .iter()
        .map(|row| row.scope.chat.clone())
        .collect();
    let path = stage.join("routes.sqlite3");
    if exists(&path) {
        fs::remove_file(&path).map_err(|_| "Cannot reset staged legacy routes")?;
    }
    let mut registry = Registry::from_connection(private_connection(&path)?)
        .map_err(|_| "Cannot initialize routes")?;
    registry
        .stage_import(&preview)
        .map_err(|_| "Cannot stage legacy route review")?;
    layouts::write_private(
        &stage.join("route-review.json"),
        &serde_json::to_vec_pretty(&preview).map_err(|_| "Cannot encode route review")?,
    )?;
    Ok((vec![], unresolved, vec![]))
}
fn rename_new(from: &Path, to: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let a =
        std::ffi::CString::new(from.as_os_str().as_bytes()).map_err(|_| "Invalid rename path")?;
    let b = std::ffi::CString::new(to.as_os_str().as_bytes()).map_err(|_| "Invalid rename path")?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err("Cannot publish cutover; retained transaction requires recovery".into());
    }
    Ok(())
}
fn relocate_attachments(
    con: &rusqlite::Connection,
    source: &Path,
    destination: &Path,
    stage: &Path,
) -> Result<(usize, usize)> {
    let rows = {
        let mut statement = con.prepare("SELECT id,payload FROM delivery WHERE direction='inbound' AND state IN ('pending','retryable') ORDER BY id").map_err(|_| "Cannot inspect queued attachments")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| "Cannot read queued attachments")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| "Cannot decode queued attachments")?
    };
    let (mut copied, mut blocked, mut total) = (0, 0, 0usize);
    for (id, encoded) in rows {
        let mut frame: super::telegram::Frame =
            serde_json::from_str(&encoded).map_err(|_| "Invalid native queued frame")?;
        let Some(attachment) = frame.attachment.as_mut() else {
            continue;
        };
        let verified = (|| -> Result<Vec<u8>> {
            if copied >= 256
                || attachment.sha256.len() != 64
                || !attachment.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                || attachment.path
                    != source
                        .join("attachments")
                        .join(format!("{}.bin", attachment.sha256))
            {
                return Err("Attachment reference does not match private content store".into());
            }
            let bytes = super::telegram_config::read_private(&attachment.path, 20 * 1024 * 1024)?
                .ok_or("Attachment missing")?;
            if bytes.len() != attachment.bytes
                || hash(&bytes) != attachment.sha256
                || total + bytes.len() > 128 * 1024 * 1024
            {
                return Err("Attachment content or import budget mismatch".into());
            }
            Ok(bytes)
        })();
        match verified {
            Ok(bytes) => {
                let relative =
                    PathBuf::from("attachments").join(format!("{}.bin", attachment.sha256));
                layouts::write_private_limit(&stage.join(&relative), &bytes, 20 * 1024 * 1024)?;
                total += bytes.len();
                copied += 1;
                attachment.path = destination.join(relative);
                let encoded =
                    serde_json::to_string(&frame).map_err(|_| "Cannot encode relocated frame")?;
                con.execute("UPDATE delivery SET payload=?,payload_sha=? WHERE id=? AND state IN ('pending','retryable')",rusqlite::params![encoded,hash(encoded.as_bytes()),id]).map_err(|_| "Cannot relocate queued attachment")?;
            }
            Err(_) => {
                blocked += 1;
                con.execute("UPDATE delivery SET state='blocked',error='cutover attachment missing, invalid, or over budget; review before retry' WHERE id=? AND state IN ('pending','retryable')",[id]).map_err(|_| "Cannot block unsafe attachment")?;
            }
        }
    }
    Ok((copied, blocked))
}
fn reconcile_routes(
    stage: &Path,
    config: &ServiceConfig,
    live: &[Terminal],
) -> Result<(Vec<String>, Vec<String>, Vec<Terminal>)> {
    let mut identities = BTreeSet::new();
    for row in live {
        row.identity.validate().map_err(str::to_owned)?;
        if !identities
            .insert(serde_json::to_string(&row.identity).map_err(|_| "Invalid live identity")?)
        {
            return Err("Ambiguous live inventory".into());
        }
    }
    let source = stage.join("legacy-native-routes.sqlite3");
    let destination = stage.join("routes.sqlite3");
    if !source.exists() {
        return Err("Native source route database is missing".into());
    }
    if destination.exists() {
        fs::remove_file(&destination).map_err(|_| "Cannot replace staged route registry")?;
    }
    sqlite::snapshot(&source, &destination, Duration::from_secs(10)).map_err(str::to_owned)?;
    let mut con = private_connection(&destination)?;
    let rows = {
        let mut query=con.prepare("SELECT c.chat,c.identity,c.revision,t.conversation,t.harness FROM chat_routes c LEFT JOIN route_terminals t ON t.identity=c.identity WHERE c.instance=? AND c.bot=? ORDER BY c.chat").map_err(|_|"Invalid native route registry")?;
        query
            .query_map(
                rusqlite::params![config.instance, config.bot_id.to_string()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .map_err(|_| "Cannot read source routes")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| "Cannot decode source routes")?
    };
    let tx = con.transaction().map_err(|_| "Cannot reconcile routes")?;
    tx.execute(
        "DELETE FROM chat_routes WHERE instance<>? OR bot<>?",
        rusqlite::params![config.instance, config.bot_id.to_string()],
    )
    .map_err(|_| "Cannot isolate routes")?;
    tx.execute("DELETE FROM route_grants", [])
        .map_err(|_| "Cannot clear copied grants")?;
    tx.execute("UPDATE route_terminals SET available=0", [])
        .map_err(|_| "Cannot reset copied liveness")?;
    let (mut verified_chats, mut unresolved, mut verified) = (vec![], vec![], vec![]);
    for (chat, encoded, revision, conversation, harness) in rows {
        let id: TerminalId =
            serde_json::from_str(&encoded).map_err(|_| "Invalid saved route identity")?;
        let matching = live.iter().find(|row| {
            row.identity == id
                && row.conversation == conversation
                && row.harness == harness
                && row.available
        });
        let permitted = config.grants.iter().any(|g| g.chat.to_string() == chat);
        if let Some(row) = matching.filter(|_| permitted && revision > 0) {
            tx.execute(
                "UPDATE route_terminals SET available=1 WHERE identity=?",
                [encoded],
            )
            .map_err(|_| "Cannot verify route")?;
            verified_chats.push(chat);
            verified.push(row.clone());
        } else {
            tx.execute(
                "DELETE FROM chat_routes WHERE instance=? AND bot=? AND chat=?",
                rusqlite::params![config.instance, config.bot_id.to_string(), chat],
            )
            .map_err(|_| "Cannot quarantine unresolved route")?;
            unresolved.push(chat);
        }
    }
    tx.commit()
        .map_err(|_| "Cannot commit route reconciliation")?;
    drop(con);
    Ok((verified_chats, unresolved, verified))
}
/// Recovery/rollback is explicit and remains offline. Once activation is
/// requested, restoring old polling cursors is forbidden.
pub fn rollback(source: &Path, global: &Path, approval: &str) -> Result<()> {
    let p = proof(&source.join(RETIRED))?;
    if p.transaction != approval || p.source != source {
        return Err("Rollback approval does not match transaction".into());
    }
    if exists(&p.destination.join(STARTED)) {
        return Err(
            "Destination activation requested; rollback requires a new coordinated migration"
                .into(),
        );
    }
    let owner = Owner::acquire(source, global, p.bot_id)?;
    let source_guard = lock(&source.join(".cutover.lock"))?;
    let mut guards = vec![];
    if p.destination.exists() {
        guards.push(lock(&p.destination.join(".telegram-service.lock"))?);
        guards.push(lock(&p.destination.join(".cutover.lock"))?);
    }
    if p.backup.exists() {
        let marker = proof(&p.backup.join(".cutover-transaction.json"))?;
        if marker.transaction != p.transaction {
            return Err("Rollback snapshot identity mismatch".into());
        }
        if p.destination.exists() {
            let current = proof(&p.destination.join(PROOF))?;
            if current.transaction != p.transaction {
                return Err("Destination transaction mismatch".into());
            }
            let abandoned = p.backup.with_extension("abandoned");
            rename_new(&p.destination, &abandoned)?;
        }
        rename_new(&p.backup, &p.destination)?;
        let mut restored = p.clone();
        restored.state = "rollback_restored".into();
        write_proof(&source.join(RETIRED), &restored)?;
        fs::remove_file(p.destination.join(".cutover-transaction.json"))
            .map_err(|_| "Cannot finish destination rollback")?;
        sync_parent(&p.destination)?;
    } else if exists(&p.destination.join(".cutover-transaction.json")) {
        let marker = proof(&p.destination.join(".cutover-transaction.json"))?;
        if marker.transaction != p.transaction {
            return Err("Rollback transaction mismatch".into());
        }
        fs::remove_file(p.destination.join(".cutover-transaction.json"))
            .map_err(|_| "Cannot clear pending transaction")?;
    } else if p.state != "preparing" && p.state != "rollback_restored" {
        return Err("Rollback snapshot missing".into());
    }
    if p.state == "rollback_restored"
        && (exists(&p.destination.join(PROOF)) || imported_source(&p.destination)? != source)
    {
        return Err("Restored destination does not match rollback transaction".into());
    }
    fs::remove_file(source.join(RETIRED)).map_err(|_| "Cannot restore source activation")?;
    File::open(source)
        .and_then(|f| f.sync_all())
        .map_err(|_| "Cannot sync source recovery")?;
    drop((guards, source_guard, owner));
    Ok(())
}

/// Explicit offline CLI. Applying requires the digest from an immediately
/// preceding preview; changed source snapshots require a new review.
pub fn run(args: &[String], out: &mut impl std::io::Write) -> Result<()> {
    use super::telegram_commands::Operations;
    if args.is_empty() || args == ["--help"] {
        writeln!(out,"daedalus telegram cutover preview --config-dir ABS\ndaedalus telegram cutover apply --config-dir ABS --approve DIGEST\ndaedalus telegram cutover rollback --source ABS --approve DIGEST\nNative owners and the version-matched legacy graceful-handoff bridge are supported. Stop the source service and hooks through their original supervisor before review; no process is stopped automatically. Legacy routes require explicit rebinding and uncertain input is retained without replay.")
            .map_err(|_| "Cannot write cutover help")?;
        return Ok(());
    }
    let verb = args[0].as_str();
    let mut flags = std::collections::BTreeMap::new();
    let mut i = 1;
    while i < args.len() {
        let value = args.get(i + 1).ok_or("Missing cutover argument")?;
        if !["--config-dir", "--source", "--approve"].contains(&args[i].as_str())
            || flags.insert(args[i].as_str(), value.as_str()).is_some()
        {
            return Err("Unknown or duplicate cutover argument".into());
        }
        i += 2;
    }
    let global = PathBuf::from(format!("/tmp/daedalus-service-{}", unsafe {
        libc::geteuid()
    }));
    if verb == "rollback" {
        if flags.len() != 2 {
            return Err("Rollback requires --source and --approve".into());
        }
        rollback(
            Path::new(*flags.get("--source").ok_or("--source is required")?),
            &global,
            flags.get("--approve").ok_or("--approve is required")?,
        )?;
        writeln!(out, "{{\"state\":\"rolled_back\",\"activated\":false}}")
            .map_err(|_| "Cannot write rollback outcome")?;
        return Ok(());
    }
    if !matches!(verb, "preview" | "apply")
        || flags.contains_key("--source")
        || (verb == "preview" && flags.len() != 1)
        || (verb == "apply" && flags.len() != 2)
    {
        return Err("Use cutover preview|apply --config-dir ABS [--approve DIGEST]".into());
    }
    let directory = PathBuf::from(
        *flags
            .get("--config-dir")
            .ok_or("--config-dir is required")?,
    );
    let config = super::config::ControlConfig::parse(&read(&directory.join("control.json"))?)?;
    let service = ServiceConfig::load(&directory.join("telegram-service.json"))?;
    let mut operations = super::telegram_commands::NativeOperations {
        instance: service.instance,
        directory: directory.clone(),
        config,
        terminals: crate::adapters::terminals::Terminals::default(),
        environment: std::env::vars().collect(),
    };
    let plan = Plan::prepare(&directory, &global, || operations.inventory())?;
    serde_json::to_writer_pretty(&mut *out, &plan.preview)
        .map_err(|_| "Cannot write cutover review")?;
    if verb == "apply" {
        let approval = flags.get("--approve").ok_or("--approve is required")?;
        let backup = plan.apply(approval, || operations.inventory())?;
        writeln!(
            out,
            "\n{}",
            json!({"state":"committed","activated":false,"rollback":backup})
        )
        .map_err(|_| "Cannot write cutover outcome")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{
            config::ControlConfig,
            setup::SetupPlan,
            telegram_config::{Access, Grant, ServicePlan},
        },
        core::{
            identity::Backend,
            journal::Journal,
            routes::{Permission, Registry, Scope},
        },
    };
    use std::os::unix::fs::DirBuilderExt;
    struct Fixture {
        root: PathBuf,
        source: PathBuf,
        destination: PathBuf,
        global: PathBuf,
        terminal: Terminal,
    }
    impl Fixture {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("daedalus-cutover-{}-{nonce}", std::process::id()));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let source = root.join("source");
            let destination = root.join("destination");
            let global = root.join("locks");
            SetupPlan::fresh(&source, ControlConfig::fresh(&root).unwrap())
                .unwrap()
                .apply()
                .unwrap();
            let token = source.join("token");
            layouts::write_private(&token, b"123:synthetic_fixture").unwrap();
            let config = ServiceConfig {
                version: 1,
                instance: "example".into(),
                bot_id: 123,
                token_file: token,
                grants: vec![Grant {
                    chat: 42,
                    actor: 7,
                    permissions: vec![Access::Read, Access::Send, Access::Manage],
                }],
                listen_chats: BTreeSet::new(),
                registrations: vec![],
                hook_bindings: vec![],
            };
            let plan = ServicePlan::fresh(&source.join("telegram-service.json"), config).unwrap();
            plan.apply(&plan.approval_token()).unwrap();
            let terminal = Terminal {
                identity: TerminalId {
                    backend: Backend::Herdr,
                    endpoint: "example".into(),
                    pane: "pane".into(),
                    generation: "generation".into(),
                },
                label: "Agent".into(),
                workspace: "/fixture".into(),
                harness: "claude".into(),
                conversation: Some("conversation".into()),
                available: true,
            };
            let mut routes = Registry::from_connection(
                private_connection(&source.join("routes.sqlite3")).unwrap(),
            )
            .unwrap();
            let scope = Scope {
                instance: "example".into(),
                bot: "123".into(),
                chat: "42".into(),
            };
            routes
                .set_grant(
                    &scope,
                    "7",
                    &[Permission::Read, Permission::Send, Permission::Manage],
                )
                .unwrap();
            routes
                .inventory(
                    Backend::Herdr,
                    "example",
                    std::slice::from_ref(&terminal),
                    true,
                    true,
                )
                .unwrap();
            routes
                .select(
                    &scope,
                    "7",
                    &terminal.identity,
                    std::slice::from_ref(&terminal.identity),
                    0,
                    true,
                    true,
                )
                .unwrap();
            drop(routes);
            let mut journal = Journal::open(&source.join("delivery.sqlite3")).unwrap();
            journal
                .admit_and_advance("telegram", "123", 7, &json!({"update_id":7}), 1.0)
                .unwrap();
            drop(journal);
            ImportPlan::prepare(&source, &destination, true)
                .unwrap()
                .apply()
                .unwrap();
            Self {
                root,
                source,
                destination,
                global,
                terminal,
            }
        }
        fn prepare(&self) -> Plan {
            Plan::prepare(&self.destination, &self.global, || {
                Ok(vec![self.terminal.clone()])
            })
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn cutover_does_not_expand_memory_credential_consent() {
        let f = Fixture::new();
        let source_key = f.source.join("memory-token");
        layouts::write_private(&source_key, b"source-fixture-secret").unwrap();
        let mut config = json!({"version":1,"enabled":true,"endpoint":"http://127.0.0.1:8888","credential_file":source_key,"bindings":[],"recall_query":"fixture query","timeout_ms":1000});
        layouts::write_private(
            &f.source.join("hindsight.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        config["credential_file"] = json!(f.destination.join("hindsight-token.txt"));
        layouts::write_private(
            &f.destination.join("hindsight.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let plan = f.prepare();
        assert!(
            !plan
                .import
                .staging_path()
                .join("hindsight-token.txt")
                .exists()
        );
        let approval = plan.preview.approval.clone();
        layouts::write_private(
            &f.destination.join("hindsight-token.txt"),
            b"changed-after-review",
        )
        .unwrap();
        assert!(
            plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
                .unwrap_err()
                .contains("Memory")
        );
        assert!(!exists(&f.source.join(RETIRED)));
        fs::remove_file(f.destination.join("hindsight-token.txt")).unwrap();
        let plan = f.prepare();
        let approval = plan.preview.approval.clone();
        plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
            .unwrap();
        assert!(!f.destination.join("hindsight-token.txt").exists());
        assert!(
            super::super::memory::load(&f.destination)
                .unwrap()
                .unwrap()
                .enabled
        );
    }
    fn make_legacy(f: &Fixture) -> PathBuf {
        fs::remove_file(f.source.join("telegram-service.json")).unwrap();
        layouts::write_private(
            &f.source.join("telegram.env"),
            b"TELEGRAM_BOT_TOKEN=123:synthetic_fixture\n",
        )
        .unwrap();
        layouts::write_private(&f.source.join("tg_offset"), b"91").unwrap();
        layouts::write_private(
            &f.source.join("jobs.json"),
            br#"{"active":{"42":"old display"},"meta":{}}"#,
        )
        .unwrap();
        let owner = json!({"version":1,"protocol":"native_owner_v1","directory":f.source,"bot_id":123,"pid":987654321,"run_id":"a".repeat(64),"feeder_script":"/fixture/tg_feeder.py","feeder_sha256":LEGACY_FEEDER_SHA256,"guard_sha256":LEGACY_GUARD_SHA256});
        let inputs=serde_json::to_vec(&json!({"version":1,"run_id":owner["run_id"],"coalesced":[{"items":[["fixture private input",90]]}],"uncertain":[{"update":{"update_id":89},"state":"unknown"}],"background_remaining":1,"replay":"forbidden"})).unwrap();
        layouts::write_private(
            &f.source.join("legacy-owner-protocol.json"),
            &serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
        layouts::write_private(&f.source.join("legacy-handoff-inputs.json"), &inputs).unwrap();
        let mut stop = owner;
        stop["state"] = json!("stopped");
        stop["next_offset"] = json!(91);
        stop["inputs_sha256"] = json!(hash(&inputs));
        stop["background_remaining"] = json!(1);
        layouts::write_private(
            &f.source.join("legacy-stopped.json"),
            &serde_json::to_vec(&stop).unwrap(),
        )
        .unwrap();
        let proc = f.root.join("proc");
        fs::create_dir(&proc).unwrap();
        proc
    }
    #[test]
    fn cutover_guard_releases_ownership_with_an_inherited_descriptor() {
        let f = Fixture::new();
        let path = f.source.join(".fixture-cutover.lock");
        let held = lock(&path).unwrap();
        let inherited = held.0.try_clone().unwrap();
        assert!(lock(&path).is_err());
        drop(held);
        let acquired = lock(&path).unwrap();
        drop(inherited);
        assert!(lock(&path).is_err());
        drop(acquired);
        assert!(lock(&path).is_ok());
    }
    #[test]
    fn legacy_graceful_cutover_keeps_uncertain_input_and_final_cursor_without_replay() {
        let f = Fixture::new();
        let proc = make_legacy(&f);
        let plan = Plan::prepare_with_process_root(&f.destination, &f.global, &proc, || {
            Ok(vec![f.terminal.clone()])
        })
        .unwrap();
        assert_eq!(plan.preview.unresolved_chats, vec!["42"]);
        assert!(plan.preview.verified_chats.is_empty());
        let approval = plan.preview.approval.clone();
        plan.apply(&approval, || Ok(vec![])).unwrap();
        activation_check(&f.destination).unwrap();
        let journal = Journal::open(&f.destination.join("delivery.sqlite3")).unwrap();
        assert_eq!(journal.cursor("telegram", "123", 0).unwrap(), 91);
        let db = private_connection(&f.destination.join("delivery.sqlite3")).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM inbox", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let input = read(&f.destination.join("legacy-handoff-inputs.json")).unwrap();
        assert!(
            String::from_utf8(input)
                .unwrap()
                .contains("fixture private input")
        );
        assert!(f.destination.join("legacy-delivery.sqlite3").exists());
        assert_eq!(
            ServiceConfig::load(&f.destination.join("telegram-service.json"))
                .unwrap()
                .read_token()
                .unwrap(),
            "123:synthetic_fixture"
        );
    }
    #[test]
    fn matching_receipts_from_unreviewed_legacy_code_are_rejected() {
        let f = Fixture::new();
        let proc = make_legacy(&f);
        for field in ["guard_sha256", "feeder_sha256"] {
            let owner_path = f.source.join("legacy-owner-protocol.json");
            let stop_path = f.source.join("legacy-stopped.json");
            let original_owner = read(&owner_path).unwrap();
            let original_stop = read(&stop_path).unwrap();
            for path in [&owner_path, &stop_path] {
                let mut receipt: Value = serde_json::from_slice(&read(path).unwrap()).unwrap();
                receipt[field] = json!("0".repeat(64));
                layouts::write_private(path, &serde_json::to_vec(&receipt).unwrap()).unwrap();
            }
            assert!(
                legacy_stopped(&f.source, 123, &proc)
                    .unwrap_err()
                    .contains("compatibility version")
            );
            layouts::write_private(&owner_path, &original_owner).unwrap();
            layouts::write_private(&stop_path, &original_stop).unwrap();
        }
        legacy_stopped(&f.source, 123, &proc).unwrap();
        for name in ["legacy-owner-protocol.json", "legacy-stopped.json"] {
            let path = f.source.join(name);
            let mut receipt: Value = serde_json::from_slice(&read(&path).unwrap()).unwrap();
            receipt["feeder_sha256"] = json!(MIRRORLESS_FEEDER_SHA256);
            layouts::write_private(&path, &serde_json::to_vec(&receipt).unwrap()).unwrap();
        }
        legacy_stopped(&f.source, 123, &proc).unwrap();
    }
    #[test]
    fn legacy_stop_proof_rejects_restarts_changed_cursor_and_live_old_hooks() {
        let f = Fixture::new();
        let proc = make_legacy(&f);
        legacy_stopped(&f.source, 123, &proc).unwrap();
        layouts::write_private(&f.source.join("tg_offset"), b"92").unwrap();
        assert!(legacy_stopped(&f.source, 123, &proc).is_err());
        layouts::write_private(&f.source.join("tg_offset"), b"91").unwrap();
        let process = proc.join("8888");
        fs::create_dir(&process).unwrap();
        fs::write(
            process.join("cmdline"),
            b"python3\0/fixture/tg_stream.py\0--watch\0",
        )
        .unwrap();
        let mut environment = b"CONFIG_DIR=".to_vec();
        environment.extend_from_slice(f.source.as_os_str().as_bytes());
        environment.push(0);
        fs::write(process.join("environ"), environment).unwrap();
        assert!(legacy_stopped(&f.source, 123, &proc).is_err());
        fs::remove_dir_all(process).unwrap();
        let mut owner: Value =
            serde_json::from_slice(&read(&f.source.join("legacy-owner-protocol.json")).unwrap())
                .unwrap();
        owner["run_id"] = json!("b".repeat(64));
        layouts::write_private(
            &f.source.join("legacy-owner-protocol.json"),
            &serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
        assert!(legacy_stopped(&f.source, 123, &proc).is_err());
    }
    #[test]
    fn native_final_snapshot_preserves_cursor_verified_routes_and_private_rollback() {
        let f = Fixture::new();
        assert!(activation_check(&f.source).is_ok());
        assert!(activation_check(&f.destination).is_err());
        let mut journal = Journal::open(&f.source.join("delivery.sqlite3")).unwrap();
        journal
            .admit_and_advance("telegram", "123", 12, &json!({"update_id":12}), 2.0)
            .unwrap();
        drop(journal);
        drop(super::super::telegram_rename::Store::open(&f.source).unwrap());
        let callbacks_path = f.source.join("telegram-controls.sqlite3");
        drop(
            super::super::telegram_commands::CallbackStore::from_connection(
                private_connection(&callbacks_path).unwrap(),
            )
            .unwrap(),
        );
        let callbacks = private_connection(&callbacks_path).unwrap();
        callbacks.execute("INSERT INTO telegram_controls(token,issue_key,claim,expires_ms,state) VALUES('fixture','fixture','{}',100,'consumed_unknown')",[]).unwrap();
        drop(callbacks);
        let first = f.prepare();
        let first_approval = first.preview.approval.clone();
        drop(first);
        let plan = f.prepare();
        assert_eq!(plan.preview.approval, first_approval);
        assert_eq!(plan.preview.verified_chats, vec!["42"]);
        assert!(plan.preview.unresolved_chats.is_empty());
        assert!(!exists(&f.source.join(RETIRED)));
        let approval = plan.preview.approval.clone();
        let backup = plan
            .apply(&approval, || Ok(vec![f.terminal.clone()]))
            .unwrap();
        assert!(backup.is_dir());
        assert_eq!(fs::metadata(&backup).unwrap().mode() & 0o777, 0o700);
        activation_check(&f.destination).unwrap();
        assert!(activation_check(&f.source).is_err());
        let rename_snapshot =
            private_connection(&f.destination.join("session-renames.sqlite3")).unwrap();
        assert_eq!(
            rename_snapshot
                .query_row("SELECT count(*) FROM session_renames", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(rename_snapshot);
        let callbacks =
            private_connection(&f.destination.join("telegram-controls.sqlite3")).unwrap();
        assert_eq!(
            callbacks
                .query_row(
                    "SELECT state FROM telegram_controls WHERE token='fixture'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "consumed_unknown"
        );
        drop(callbacks);
        let journal = Journal::open(&f.destination.join("delivery.sqlite3")).unwrap();
        assert_eq!(journal.cursor("telegram", "123", 0).unwrap(), 13);
        drop(journal);
        let connection = private_connection(&f.destination.join("routes.sqlite3")).unwrap();
        let row: (i64, i64) = connection
            .query_row(
                "SELECT revision,pinned FROM chat_routes WHERE chat='42'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (1, 1));
        drop(connection);
        rollback(&f.source, &f.global, &approval).unwrap();
        activation_check(&f.source).unwrap();
        assert!(activation_check(&f.destination).is_err());
    }
    #[test]
    fn active_owner_and_hook_writer_prevent_final_snapshot() {
        let f = Fixture::new();
        let owner = Owner::acquire(&f.source, &f.global, 123).unwrap();
        assert!(
            Plan::prepare(&f.destination, &f.global, || panic!(
                "must not observe while source runs"
            ))
            .is_err()
        );
        drop(owner);
        let hook = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(f.source.join(".cutover.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(hook.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) },
            0
        );
        assert!(
            Plan::prepare(&f.destination, &f.global, || panic!(
                "must not observe while hook writes"
            ))
            .is_err()
        );
        drop(hook);
        assert!(!exists(&f.source.join(RETIRED)));
    }
    #[test]
    fn recycled_generation_is_quarantined_and_stale_approval_has_no_retirement() {
        let f = Fixture::new();
        let mut changed = f.terminal.clone();
        changed.identity.generation = "replacement".into();
        let plan = Plan::prepare(&f.destination, &f.global, || Ok(vec![changed.clone()])).unwrap();
        assert_eq!(plan.preview.unresolved_chats, vec!["42"]);
        assert!(plan.preview.verified_chats.is_empty());
        drop(plan);
        let plan = f.prepare();
        let approval = plan.preview.approval.clone();
        assert!(plan.apply(&approval, || Ok(vec![changed.clone()])).is_err());
        assert!(!exists(&f.source.join(RETIRED)));
        assert!(!exists(&f.destination.join(PROOF)));
    }
    #[test]
    fn activation_marker_forbids_restoring_old_polling_cursor() {
        let f = Fixture::new();
        let plan = f.prepare();
        let approval = plan.preview.approval.clone();
        plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
            .unwrap();
        mark_activation(&f.destination).unwrap();
        assert!(rollback(&f.source, &f.global, &approval).is_err());
        assert!(activation_check(&f.source).is_err());
        activation_check(&f.destination).unwrap();
    }
    #[test]
    fn incomplete_prepare_marker_can_be_recovered_without_deleting_instances() {
        let f = Fixture::new();
        let plan = f.prepare();
        let mut pending = plan.proof.clone();
        pending.state = "preparing".into();
        write_proof(&f.source.join(RETIRED), &pending).unwrap();
        write_proof(&f.destination.join(".cutover-transaction.json"), &pending).unwrap();
        drop(plan);
        assert!(activation_check(&f.source).is_err());
        assert!(activation_check(&f.destination).is_err());
        rollback(&f.source, &f.global, &pending.transaction).unwrap();
        assert!(f.source.is_dir());
        assert!(f.destination.is_dir());
        activation_check(&f.source).unwrap();
    }
    #[test]
    fn recovery_finishes_after_rollback_restore_was_already_published() {
        let f = Fixture::new();
        let plan = f.prepare();
        let mut p = plan.proof.clone();
        p.state = "rollback_restored".into();
        write_proof(&f.source.join(RETIRED), &p).unwrap();
        drop(plan);
        rollback(&f.source, &f.global, &p.transaction).unwrap();
        activation_check(&f.source).unwrap();
        assert!(activation_check(&f.destination).is_err());
    }
    #[test]
    fn queued_attachments_relocate_only_after_private_content_verification() {
        use crate::app::telegram::{Attachment, Frame};
        let f = Fixture::new();
        let bytes = b"synthetic file";
        let sha = hash(bytes);
        let path = f.source.join("attachments").join(format!("{sha}.bin"));
        layouts::write_private(&path, bytes).unwrap();
        let mut journal = Journal::open(&f.source.join("delivery.sqlite3")).unwrap();
        let mut frame = Frame {
            scope: Scope {
                instance: "example".into(),
                bot: "123".into(),
                chat: "42".into(),
            },
            actor: "7".into(),
            message_id: 5,
            update_id: 8,
            terminal: f.terminal.identity.clone(),
            route_revision: 1,
            conversation: f.terminal.conversation.clone(),
            text: "file".into(),
            attachment: Some(Attachment {
                path: path.clone(),
                sha256: sha.clone(),
                bytes: bytes.len(),
                name: "fixture.txt".into(),
            }),
        };
        let target = serde_json::to_string(&frame.terminal).unwrap();
        journal
            .enqueue(
                "inbound",
                &target,
                &serde_json::to_value(&frame).unwrap(),
                "file-valid",
                None,
                1.0,
            )
            .unwrap();
        frame.attachment.as_mut().unwrap().sha256 = "0".repeat(64);
        journal
            .enqueue(
                "inbound",
                &target,
                &serde_json::to_value(&frame).unwrap(),
                "file-invalid",
                None,
                1.0,
            )
            .unwrap();
        drop(journal);
        let plan = f.prepare();
        let approval = plan.preview.approval.clone();
        plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
            .unwrap();
        let journal = Journal::open(&f.destination.join("delivery.sqlite3")).unwrap();
        let row = journal
            .delivery_for_key("inbound", "file-valid")
            .unwrap()
            .unwrap();
        assert_eq!(
            row.payload["attachment"]["path"],
            json!(f.destination.join("attachments").join(format!("{sha}.bin")))
        );
        assert_eq!(
            journal
                .delivery_for_key("inbound", "file-invalid")
                .unwrap()
                .unwrap()
                .state,
            "blocked"
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
