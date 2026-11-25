//! Offline native-owner handoff. No bot, harness, or supervisor is started here.
//! Locks prove quiescence only for the native cooperating service/hook protocol.
use super::{
    migration::ImportPlan,
    telegram_config::{ServiceConfig, private_connection},
    telegram_service::Owner,
};
use crate::core::{identity::TerminalId, layouts, routes::Terminal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::{
    collections::BTreeSet,
    fs::{self, File},
    path::{Path, PathBuf},
};
#[cfg(unix)]
use std::{
    fs::OpenOptions,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
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
#[cfg(unix)]
fn private_dir(path: &Path) -> Result<()> {
    layouts::check_path(path)?;
    let m = fs::metadata(path).map_err(|_| "Instance directory missing")?;
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
        return Err("Instance directory must be private and owned".into());
    }
    Ok(())
}
struct CutoverLock(File);
#[cfg(unix)]
impl Drop for CutoverLock {
    fn drop(&mut self) {
        // A concurrent subprocess can inherit the open file description before
        // exec closes CLOEXEC descriptors. Release ownership at guard exit.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
#[cfg(unix)]
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
        for name in [
            ".cutover.lock",
            ".telegram-config.lock",
            ".setup.lock",
            "gateway.lock",
        ] {
            files.push(lock(&directory.join(name))?);
        }
    }
    Ok(Locks {
        _owner: owner,
        _files: files,
    })
}
#[cfg(unix)]
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
    sync_directory(path.parent().ok_or("Missing parent")?)
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
    gateway_fingerprint: String,
    dashboard_before: Option<Vec<u8>>,
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
        let gateway_fingerprint = gateway_fingerprint(destination)?;
        let dashboard_before = layouts::read_private_limit(
            &destination.join("dashboard.json"),
            super::dashboard::LIMIT,
        )?;
        if dashboard_before.is_some() {
            super::dashboard::State::load(destination)?;
        }
        let mut import =
            ImportPlan::prepare_with_optional_credentials(&source, destination, true, false)?;
        let stage = import.staging_path().to_path_buf();
        // Global UI preferences belong to the reviewed destination, including
        // when the destination is also the platform's preference directory.
        if let Some(bytes) = &dashboard_before {
            import.write_staged(&stage.join("dashboard.json"), bytes, 8 * 1024 * 1024)?;
        }
        // Preserve the destination's separately reviewed provider selection and
        // provisioned key. The final snapshot never expands credential consent.
        if let Some(mut gateway) = super::gateway::load(destination)? {
            let staged_key = stage.join("gateway-token.txt");
            if exists(&staged_key) {
                fs::remove_file(&staged_key)
                    .map_err(|_| "Cannot preserve gateway credential selection")?;
            }
            if let Some(bytes) = super::gateway::private_bytes(&gateway.credential_file, 8192)? {
                import.write_staged(&staged_key, &bytes, 8 * 1024 * 1024)?;
            }
            gateway.credential_file = destination.join("gateway-token.txt");
            import.write_staged(
                &stage.join("gateway.json"),
                &gateway.encode()?,
                8 * 1024 * 1024,
            )?;
        } else {
            for name in ["gateway.json", "gateway-token.txt"] {
                if exists(&stage.join(name)) {
                    fs::remove_file(stage.join(name))
                        .map_err(|_| "Cannot retain optional gateway selection")?;
                }
            }
        }
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
                    import.write_staged(&staged_key, &bytes, 8 * 1024 * 1024)?;
                }
                memory.credential_file = Some(destination.join("hindsight-token.txt"));
            }
            import.write_staged(
                &stage.join("hindsight.json"),
                &memory.encode()?,
                8 * 1024 * 1024,
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
            import.write_staged(
                &stage.join("telegram-token.txt"),
                source_token.as_bytes(),
                8 * 1024 * 1024,
            )?;
            reviewed.token_file = token;
            // Preserve explicitly enrolled destination registrations, never infer legacy identity.
            for (path, registration) in selected.check_registrations()? {
                let relative = path
                    .strip_prefix(destination)
                    .map_err(|_| "Destination hook registration must be inside its instance")?;
                import.write_staged(
                    &stage.join(relative),
                    &serde_json::to_vec_pretty(&registration)
                        .map_err(|_| "Invalid registration")?,
                    8 * 1024 * 1024,
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
            reconcile_routes(&import, &service, &live)?
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
        import.write_staged(
            &stage.join("telegram-service.json"),
            &service.encode()?,
            8 * 1024 * 1024,
        )?;
        if let Some(evidence) = &legacy_evidence {
            for name in [
                "legacy-owner-protocol.json",
                "legacy-stopped.json",
                "legacy-handoff-inputs.json",
            ] {
                import.write_staged(
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
            relocate_attachments(&con, &source, destination, &import, &service)?;
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
        import.write_staged(
            &stage.join(PROOF),
            &serde_json::to_vec_pretty(&proof).map_err(|_| "Cannot encode cutover proof")?,
            8 * 1024 * 1024,
        )?;
        let mut preview=Preview{source,destination:destination.into(),verified_chats,unresolved_chats,approval:transaction,notices:vec!["Final snapshot taken while native service and hooks are locked; no services started.".into(),"Only exact live terminal generation and conversation matches retain routes. Unresolved routes and their hook recipient bindings are disabled.".into(),"Original source is retired on commit. A private destination rollback snapshot is retained. Rollback is allowed only before destination activation.".into(),"Harness hooks must be installed against relocated destination registration files before feeding resumes.".into(),format!("Queued attachments: {attachments_copied} verified references relocated; {attachments_blocked} missing, invalid or file-policy-disallowed drafts blocked. Uncertain evidence unchanged.")]};
        preview.notices.push(match super::gateway::load(destination)? {
            Some(gateway)=>format!("Destination gateway selection: enabled={}, harness={}, endpoint={}, model={}, credential file={}, credential present={}. This reviewed destination selection is retained by cutover.",gateway.enabled,gateway.harness,gateway.endpoint,gateway.model,gateway.credential_file.display(),gateway.credential_file.is_file()),
            None=>"Destination gateway selection: not configured; no source gateway is enabled by cutover.".into(),
        });
        preview.notices.extend(
            import
                .preview
                .notices
                .iter()
                .filter(|notice| notice.starts_with("Source import limits:"))
                .cloned(),
        );
        preview.notices.push(format!("Queued attachment per-file policy: {} bytes from the effective final service configuration. Unavailable, invalid or disallowed files become blocked drafts; reference or shared budget exhaustion refuses this cutover.",service.runtime.file_limit_bytes));
        if dashboard_before.is_some() {
            preview.notices.push("Destination global dashboard preferences, saved instances and discovery settings are retained byte-for-byte; source dashboard preferences are not imported.".into());
        }
        if stage.join("imported-route-review").exists() {
            preview.notices.push("The previous inert staged route database and its sidecars are retained privately in imported-route-review. These files are review-only and do not authorize routes.".into());
        }
        if stage.join("views.json").exists() {
            preview.notices.push("Final source viewer history is retained byte-for-byte under its saved processing limits. Live relationships still require fresh terminal identities; the previous destination history remains in the rollback snapshot. No viewer is launched.".into());
        }
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
            gateway_fingerprint,
            dashboard_before,
        })
    }
    pub fn apply(
        self,
        approval: &str,
        mut observe: impl FnMut() -> Result<Vec<Terminal>>,
    ) -> Result<PathBuf> {
        self.import.with_budget(|budget| budget.check_time())?;
        if layouts::read_private_limit(
            &self.proof.destination.join("dashboard.json"),
            super::dashboard::LIMIT,
        )? != self.dashboard_before
        {
            return Err("Dashboard preferences changed after cutover review".into());
        }
        if gateway_fingerprint(&self.proof.destination)? != self.gateway_fingerprint {
            return Err("Gateway configuration or credential changed after review".into());
        }
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
        let proof_bytes = serde_json::to_vec_pretty(&self.proof)
            .map_err(|_| "Cannot encode reviewed cutover proof")?;
        self.import.verify_stage(Some((PROOF, &proof_bytes)))?;
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
fn gateway_fingerprint(directory: &Path) -> Result<String> {
    let config = super::gateway::private_bytes(
        &directory.join("gateway.json"),
        super::gateway::CONFIG_LIMIT,
    )?;
    let token = super::gateway::load(directory)?
        .map(|gateway| super::gateway::private_bytes(&gateway.credential_file, 8192))
        .transpose()?
        .flatten();
    Ok(hash(
        &serde_json::to_vec(&(config, token)).map_err(|_| "Cannot snapshot gateway selection")?,
    ))
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
/// Legacy feeder and hook program stems, or `<prefix>_<stem>`.
#[cfg(unix)]
const LEGACY_PROGRAMS: &[&str] = &[
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
#[cfg(unix)]
fn legacy_program(arg: &std::ffi::OsStr) -> bool {
    Path::new(arg).file_stem().is_some_and(|name| {
        LEGACY_PROGRAMS.iter().any(|candidate| {
            name == *candidate || name.to_string_lossy().ends_with(&format!("_{candidate}"))
        })
    })
}
/// macOS reads the same evidence through libproc and KERN_PROCARGS2. An owned,
/// live process whose command or scope cannot be read fails closed.
#[cfg(target_os = "macos")]
fn legacy_processes_absent(source: &Path, proc_root: &Path) -> Result<()> {
    use crate::platform::{
        process_argv, process_environment, process_info, process_list, process_owner,
    };
    // Isolated fixtures keep their procfs layout and semantics.
    if proc_root != Path::new("/proc") {
        return legacy_processes_absent_procfs(source, proc_root);
    }
    let pids = process_list(proc_root);
    if pids.is_empty() {
        return Err("Cannot verify legacy process quiescence".into());
    }
    if pids.len() > 100_000 {
        return Err("Process inventory exceeds verification limit".into());
    }
    let uid = unsafe { libc::geteuid() };
    // Other users' processes are out of scope, as on Linux. A process of this
    // user that libproc cannot describe has an unknown scope: fail closed.
    let exists = |pid: u32| unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    let live = |pid: u32| -> Result<bool> {
        match process_info(proc_root, pid) {
            Some(info) => Ok(info.uid == uid && !matches!(info.state, 'Z')),
            None => match process_owner(pid) {
                Some(owner) if owner != uid => Ok(false),
                Some(_) => Err("Cannot verify legacy process identity".into()),
                None if exists(pid) => Err("Cannot verify legacy process identity".into()),
                None => Ok(false),
            },
        }
    };
    for pid in pids {
        if !live(pid)? {
            continue;
        }
        let Some(argv) = process_argv(proc_root, pid) else {
            if live(pid)? {
                return Err("Cannot verify legacy process command".into());
            }
            continue;
        };
        if !argv
            .iter()
            .take(5)
            .any(|arg| legacy_program(std::ffi::OsStr::new(arg)))
        {
            continue;
        }
        let Some(environment) = process_environment(proc_root, pid) else {
            if live(pid)? {
                return Err("Cannot verify legacy process scope".into());
            }
            continue;
        };
        let scope = environment
            .iter()
            .find_map(|value| value.strip_prefix("CONFIG_DIR="))
            .map(PathBuf::from);
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
#[cfg(all(unix, not(target_os = "macos")))]
fn legacy_processes_absent(source: &Path, proc_root: &Path) -> Result<()> {
    legacy_processes_absent_procfs(source, proc_root)
}
#[cfg(unix)]
fn legacy_processes_absent_procfs(source: &Path, proc_root: &Path) -> Result<()> {
    let entries = fs::read_dir(proc_root).map_err(|_| "Cannot verify legacy process quiescence")?;
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
        let relevant = command
            .split(|b| *b == 0)
            .take(5)
            .any(|arg| legacy_program(std::ffi::OsStr::from_bytes(arg)));
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
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn rename_new(_: &Path, _: &Path) -> Result<()> {
    Err("Atomic no-replace rename is currently supported on Linux and macOS".into())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_new(from: &Path, to: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let a =
        std::ffi::CString::new(from.as_os_str().as_bytes()).map_err(|_| "Invalid rename path")?;
    let b = std::ffi::CString::new(to.as_os_str().as_bytes()).map_err(|_| "Invalid rename path")?;
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let status = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_EXCL) };
    if status != 0 {
        return Err("Cannot publish cutover; retained transaction requires recovery".into());
    }
    Ok(())
}
fn relocate_attachments(
    con: &rusqlite::Connection,
    source: &Path,
    destination: &Path,
    import: &ImportPlan,
    service: &ServiceConfig,
) -> Result<(usize, usize)> {
    let stage = import.staging_path();
    let (reference_limit, reference_bytes) =
        import.with_budget(|budget| budget.attachment_limits())?;
    let file_limit = service.runtime.file_limit_bytes;
    let (mut copied, mut blocked, mut total) = (0usize, 0usize, 0u64);
    let mut after = None;
    loop {
        import.with_budget(|budget| budget.check_time())?;
        let next = {
            let sql = if after.is_some() {
                "SELECT id,payload FROM delivery WHERE direction='inbound' AND state IN ('pending','retryable') AND id>? ORDER BY id LIMIT 1"
            } else {
                "SELECT id,payload FROM delivery WHERE direction='inbound' AND state IN ('pending','retryable') ORDER BY id LIMIT 1"
            };
            let mut query = con
                .prepare_cached(sql)
                .map_err(|_| "Cannot inspect queued attachments")?;
            let mut rows = if let Some(id) = after {
                query.query([id])
            } else {
                query.query([])
            }
            .map_err(|_| "Cannot read queued attachments")?;
            if let Some(row) = rows
                .next()
                .map_err(|_| "Cannot inspect queued attachment row")?
            {
                let id = row
                    .get::<_, i64>(0)
                    .map_err(|_| "Invalid queued delivery identity")?;
                let value = row
                    .get_ref(1)
                    .map_err(|_| "Cannot inspect queued delivery payload")?;
                let text = value
                    .as_str()
                    .map_err(|_| "Queued delivery payload must be UTF-8 text")?;
                import.with_budget(|budget| budget.frame(text.as_bytes()))?;
                let payload: Value =
                    serde_json::from_str(text).map_err(|_| "Invalid native queued frame")?;
                if !payload.is_object() {
                    return Err("Native queued frame must be a JSON object".into());
                }
                let frame = super::telegram::Frame::deserialize(&payload)
                    .map_err(|_| "Invalid native queued frame")?;
                if frame.attachment.is_some() && !payload["attachment"].is_object() {
                    return Err("Native queued attachment must be a JSON object".into());
                }
                Some((id, frame, payload))
            } else {
                None
            }
        };
        let Some((id, frame, mut payload)) = next else {
            break;
        };
        after = Some(id);
        let Some(attachment) = frame.attachment.as_ref() else {
            continue;
        };
        let verified = (|| -> Result<(PathBuf, Vec<u8>)> {
            let relative = attachment.relative_path(source)?;
            if attachment.bytes as u64 > file_limit {
                return Err("Attachment exceeds effective service file limit".into());
            }
            import.with_budget(|budget| budget.work(attachment.bytes as u64))?;
            let bytes =
                super::telegram_config::read_private(&attachment.path, attachment.bytes as u64)?
                    .ok_or("Attachment missing")?;
            if bytes.len() != attachment.bytes || hash(&bytes) != attachment.sha256 {
                return Err("Attachment content mismatch".into());
            }
            Ok((relative, bytes))
        })();
        match verified {
            Ok((relative, bytes)) => {
                if copied as u64 >= reference_limit {
                    return Err("Cutover attachment reference budget reached; review instance_import_attachment_limit".into());
                }
                let next_total = total
                    .checked_add(bytes.len() as u64)
                    .ok_or("Cutover attachment reference size overflow")?;
                if next_total > reference_bytes {
                    return Err("Cutover attachment reference bytes reached; review instance_import_attachment_limit_bytes".into());
                }
                import.write_staged(&stage.join(&relative), &bytes, file_limit)?;
                total = next_total;
                copied += 1;
                payload["attachment"]["path"] = json!(destination.join(relative));
                let encoded =
                    serde_json::to_string(&payload).map_err(|_| "Cannot encode relocated frame")?;
                let changed=con.execute("UPDATE delivery SET payload=?,payload_sha=? WHERE id=? AND state IN ('pending','retryable')",rusqlite::params![encoded,hash(encoded.as_bytes()),id]).map_err(|_|"Cannot relocate queued attachment")?;
                if changed != 1 {
                    return Err("Queued attachment changed during import".into());
                }
            }
            Err(_) => {
                // Shared budget errors abort the operation, rather than turning
                // valid input into a blocked content draft.
                import.with_budget(|budget| budget.check_time())?;
                blocked += 1;
                let changed=con.execute("UPDATE delivery SET state='blocked',error='cutover attachment missing, invalid, or disallowed by service file policy; review before retry' WHERE id=? AND state IN ('pending','retryable')",[id]).map_err(|_|"Cannot block unsafe attachment")?;
                if changed != 1 {
                    return Err("Queued attachment changed during import".into());
                }
            }
        }
    }
    Ok((copied, blocked))
}

fn retain_staged_routes(stage: &Path) -> Result<()> {
    private_dir(stage)?;
    let mut previous = Vec::new();
    for name in [
        "routes.sqlite3",
        "routes.sqlite3-wal",
        "routes.sqlite3-shm",
        "routes.sqlite3-journal",
    ] {
        let path = stage.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                layouts::check_path(&path)?;
                if !metadata.is_file() {
                    return Err("Invalid staged route review artifact".into());
                }
                previous.push(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot inspect staged route review".into()),
        }
    }
    if previous.is_empty() {
        return Ok(());
    }
    let retained = stage.join("imported-route-review");
    layouts::check_path(&retained)?;
    if retained
        .try_exists()
        .map_err(|_| "Cannot inspect staged route review directory")?
    {
        return Err("Staged route review directory already exists".into());
    }
    #[cfg(unix)]
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&retained)
        .map_err(|_| "Cannot create private staged route review")?;
    #[cfg(windows)]
    crate::platform::ensure_private_dir(&retained)
        .map_err(|_| "Cannot create private staged route review")?;
    for path in previous {
        let name = path
            .file_name()
            .ok_or("Invalid staged route review filename")?;
        fs::rename(&path, retained.join(name))
            .map_err(|_| "Cannot retain staged route review artifact")?;
    }
    Ok(())
}
fn reconcile_routes(
    import: &ImportPlan,
    config: &ServiceConfig,
    live: &[Terminal],
) -> Result<(Vec<String>, Vec<String>, Vec<Terminal>)> {
    let stage = import.staging_path();
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
    let control = super::config::ControlConfig::parse(&read(&stage.join("control.json"))?)?;
    retain_staged_routes(stage)?;
    import.snapshot_database(&source, &destination, &control.runtime.snapshot_options())?;
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
    sync_directory(source).map_err(|_| "Cannot sync source recovery")?;
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
    let global = super::telegram_service::global_directory()?;
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
    let terminals = crate::adapters::terminals::Terminals::default().configured(&config.runtime)?;
    let mut operations = super::telegram_commands::NativeOperations {
        instance: service.instance,
        directory: directory.clone(),
        config,
        terminals,
        environment: crate::platform::environment(),
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

#[cfg(windows)]
fn private_dir(path: &Path) -> Result<()> {
    layouts::check_path(path)?;
    crate::platform::validate_private_dir(path)
        .map_err(|_| "Instance directory must have private owner-scoped access".into())
}
#[cfg(windows)]
impl Drop for CutoverLock {
    fn drop(&mut self) {
        let _ = crate::platform::unlock(&self.0);
    }
}
#[cfg(windows)]
fn lock(path: &Path) -> Result<CutoverLock> {
    layouts::check_path(path)?;
    private_dir(path.parent().ok_or("Missing lock parent")?)?;
    let file = crate::platform::open_private(path, true, true)
        .map_err(|_| "Cannot open private cutover lock")?;
    crate::platform::lock_exclusive(&file, true)
        .map_err(|_| "Instance is busy or cutover lock is unsafe")?;
    Ok(CutoverLock(file))
}
#[cfg(windows)]
fn read(path: &Path) -> Result<Vec<u8>> {
    layouts::read_private(path)?.ok_or("Cutover state missing".into())
}
#[cfg(windows)]
fn legacy_processes_absent(_: &Path, _: &Path) -> Result<()> {
    Err("Legacy Unix runtime retirement cannot be verified on Windows; retire and cut over that instance on its original host".into())
}
#[cfg(windows)]
fn rename_new(from: &Path, to: &Path) -> Result<()> {
    crate::platform::rename_new(from, to)
        .map_err(|_| "Cannot publish cutover; retained transaction requires recovery".into())
}
fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(windows)]
    {
        crate::platform::sync_directory(path)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;
    /// A real process named like a legacy hook blocks cutover of its own scope
    /// (or with no scope) and is ignored for another instance.
    #[test]
    fn live_legacy_process_is_found_through_libproc() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "daedalus-legacy-live-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let other = root.join("other");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&other).unwrap();
        // macOS hides the environment of Apple platform binaries, so the scoped
        // cases run this test binary as the helper; /bin/sleep shows the hidden case.
        let image = std::env::current_exe().unwrap();
        let program = root.join("tg_feeder");
        std::os::unix::fs::symlink(&image, &program).unwrap();
        let platform = root.join("platform").join("tg_feeder");
        fs::create_dir_all(platform.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/bin/sleep", &platform).unwrap();
        let run = |program: &Path, scope: Option<&Path>| {
            let mut command = if program == platform.as_path() {
                let mut command = std::process::Command::new(program);
                command.arg("30");
                command
            } else {
                crate::test_support::idle_process(program)
            };
            command.env_remove("CONFIG_DIR");
            if let Some(scope) = scope {
                command.env("CONFIG_DIR", scope);
            }
            let mut child = command.spawn().unwrap();
            // Wait until the kernel reports the new image's arguments.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !crate::platform::process_argv(Path::new("/proc"), child.id())
                .is_some_and(|argv| argv.first().is_some_and(|a| a.ends_with("tg_feeder")))
            {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let result = legacy_processes_absent(&source, Path::new("/proc"));
            // Whether macOS exposed the scope; some releases hide it for Apple binaries.
            let scope_visible =
                crate::platform::process_environment(Path::new("/proc"), child.id())
                    .is_some_and(|env| env.iter().any(|v| v.starts_with("CONFIG_DIR=")));
            child.kill().unwrap();
            child.wait().unwrap();
            (result, scope_visible)
        };
        assert!(
            run(&program, Some(&source))
                .0
                .unwrap_err()
                .contains("original supervisor")
        );
        assert!(
            run(&program, None)
                .0
                .unwrap_err()
                .contains("original supervisor")
        );
        run(&program, Some(&other)).0.unwrap();
        // An Apple platform binary's environment may be hidden. Hidden is no
        // evidence of another scope: fail closed; a visible scope is honored.
        match run(&platform, Some(&other)) {
            (result, true) => result.unwrap(),
            (result, false) => assert!(result.unwrap_err().contains("original supervisor")),
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, unix))]
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
            // macOS clocks tick in microseconds; parallel tests need a counter.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let nonce = crate::test_support::unique_nanos();
            let count = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "daedalus-cutover-{}-{count}-{nonce}",
                std::process::id()
            ));
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
                runtime: Default::default(),
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
        fn queue_file(
            &self,
            name: &str,
            bytes: &[u8],
            key: &str,
        ) -> super::super::telegram::Attachment {
            use super::super::telegram::{Files, Frame, PrivateFiles};
            let attachment = PrivateFiles {
                directory: self.source.join("attachments"),
            }
            .save(name, bytes)
            .unwrap();
            let frame = Frame {
                scope: Scope {
                    instance: "example".into(),
                    bot: "123".into(),
                    chat: "42".into(),
                },
                actor: "7".into(),
                message_id: 5,
                update_id: 8,
                terminal: self.terminal.identity.clone(),
                route_revision: 1,
                conversation: self.terminal.conversation.clone(),
                text: "file".into(),
                attachment: Some(attachment.clone()),
            };
            let mut journal = Journal::open(&self.source.join("delivery.sqlite3")).unwrap();
            journal
                .enqueue(
                    "inbound",
                    &serde_json::to_string(&frame.terminal).unwrap(),
                    &serde_json::to_value(frame).unwrap(),
                    key,
                    None,
                    1.0,
                )
                .unwrap();
            attachment
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn cutover_keeps_actual_private_file_producer_extensions_and_original_uncertain_evidence() {
        let f = Fixture::new();
        let names = [
            "notes.txt".to_owned(),
            "photo.PNG".to_owned(),
            format!("{}.png", "x".repeat(300)),
            "plain".to_owned(),
        ];
        let files: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let key = format!("producer-{index}");
                (
                    key.clone(),
                    f.queue_file(name, format!("private fixture {index}").as_bytes(), &key),
                )
            })
            .collect();
        let unknown = f.queue_file("unknown.txt", b"uncertain content", "uncertain-file");
        let intent = f.queue_file("intent.png", b"in-flight content", "intent-file");
        let source = private_connection(&f.source.join("delivery.sqlite3")).unwrap();
        source.execute("UPDATE delivery SET state='unknown',receipt='{\"fixture\":\"retained\"}' WHERE dedup_key='uncertain-file'",[]).unwrap();
        source.execute("UPDATE delivery SET state='intent',receipt='{\"fixture\":\"retained\"}' WHERE dedup_key='intent-file'",[]).unwrap();
        drop(source);
        let source_before = fs::read(f.source.join("delivery.sqlite3")).unwrap();
        let plan = f.prepare();
        let staged =
            private_connection(&plan.import.staging_path().join("delivery.sqlite3")).unwrap();
        for (key, attachment) in &files {
            let (state, payload): (String, String) = staged
                .query_row(
                    "SELECT state,payload FROM delivery WHERE dedup_key=?",
                    [key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let payload: Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(state, "pending", "A producer-created file was blocked");
            assert_eq!(
                payload["attachment"]["path"],
                json!(
                    f.destination
                        .join("attachments")
                        .join(attachment.path.file_name().unwrap())
                )
            );
        }
        for (key, file) in [("uncertain-file", &unknown), ("intent-file", &intent)] {
            let (state, payload, receipt): (String, String, String) = staged
                .query_row(
                    "SELECT state,payload,receipt FROM delivery WHERE dedup_key=?",
                    [key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(state, "unknown");
            assert_eq!(
                serde_json::from_str::<Value>(&payload).unwrap()["attachment"]["path"],
                json!(file.path)
            );
            assert_eq!(receipt, r#"{"fixture":"retained"}"#);
            assert!(
                !plan
                    .import
                    .staging_path()
                    .join("attachments")
                    .join(file.path.file_name().unwrap())
                    .exists()
            );
        }
        drop(staged);
        let approval = plan.preview.approval.clone();
        plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
            .unwrap();
        for (_, attachment) in &files {
            assert_eq!(
                fs::read(
                    f.destination
                        .join("attachments")
                        .join(attachment.path.file_name().unwrap())
                )
                .unwrap(),
                fs::read(&attachment.path).unwrap()
            );
        }
        assert_eq!(
            fs::read(f.source.join("delivery.sqlite3")).unwrap(),
            source_before
        );
        rollback(&f.source, &f.global, &approval).unwrap();
        assert_eq!(
            fs::read(f.source.join("delivery.sqlite3")).unwrap(),
            source_before
        );
    }
    #[test]
    fn cutover_preserves_unrelated_pending_frame_metadata_while_relocating_legacy_bin() {
        let f = Fixture::new();
        f.queue_file("plain", b"fixture", "metadata-file");
        let source = private_connection(&f.source.join("delivery.sqlite3")).unwrap();
        let raw: String = source
            .query_row(
                "SELECT payload FROM delivery WHERE dedup_key='metadata-file'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut payload: Value = serde_json::from_str(&raw).unwrap();
        payload["retained_metadata"] =
            json!({"labels":["private fixture","preserve"],"revision":7});
        let raw = serde_json::to_string(&payload).unwrap();
        source
            .execute(
                "UPDATE delivery SET payload=?,payload_sha=? WHERE dedup_key='metadata-file'",
                rusqlite::params![raw, hash(raw.as_bytes())],
            )
            .unwrap();
        drop(source);
        let plan = f.prepare();
        let staged =
            private_connection(&plan.import.staging_path().join("delivery.sqlite3")).unwrap();
        let raw: String = staged
            .query_row(
                "SELECT payload FROM delivery WHERE dedup_key='metadata-file'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let relocated: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(relocated["retained_metadata"], payload["retained_metadata"]);
        assert_eq!(relocated["scope"], payload["scope"]);
        assert_ne!(
            relocated["attachment"]["path"],
            payload["attachment"]["path"]
        );
    }
    #[test]
    fn cutover_attachment_reference_and_frame_caps_refuse_and_recover_without_source_changes() {
        for kind in ["count", "bytes", "frame"] {
            let f = Fixture::new();
            f.queue_file("one.txt", b"one", "one");
            f.queue_file("two.png", b"two", "two");
            if kind == "frame" {
                let con = private_connection(&f.source.join("delivery.sqlite3")).unwrap();
                let raw: String = con
                    .query_row(
                        "SELECT payload FROM delivery WHERE dedup_key='one'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                let mut frame: Value = serde_json::from_str(&raw).unwrap();
                frame["text"] = json!("x".repeat(2048));
                let raw = serde_json::to_string(&frame).unwrap();
                con.execute(
                    "UPDATE delivery SET payload=?,payload_sha=? WHERE dedup_key='one'",
                    rusqlite::params![raw, hash(raw.as_bytes())],
                )
                .unwrap();
            }
            let path = f.source.join("control.json");
            let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
            let expected = match kind {
                "count" => {
                    config.runtime.instance_import_attachment_limit = 1;
                    "instance_import_attachment_limit"
                }
                "bytes" => {
                    config.runtime.instance_import_attachment_limit_bytes = 5;
                    "instance_import_attachment_limit_bytes"
                }
                _ => {
                    config.runtime.instance_import_frame_limit_bytes = 1024;
                    "instance_import_frame_limit_bytes"
                }
            };
            layouts::write_private(&path, &config.encode().unwrap()).unwrap();
            let source = fs::read(f.source.join("delivery.sqlite3")).unwrap();
            let destination = fs::read(f.destination.join("control.json")).unwrap();
            let refused = Plan::prepare(&f.destination, &f.global, || Ok(vec![f.terminal.clone()]));
            assert!(refused.err().unwrap().contains(expected));
            assert!(!exists(&f.source.join(RETIRED)));
            assert!(!exists(&f.destination.join(PROOF)));
            assert_eq!(fs::read(f.source.join("delivery.sqlite3")).unwrap(), source);
            assert_eq!(
                fs::read(f.destination.join("control.json")).unwrap(),
                destination
            );
            config.runtime.instance_import_attachment_limit = 256;
            config.runtime.instance_import_attachment_limit_bytes = 134217728;
            config.runtime.instance_import_frame_limit_bytes = 262144;
            layouts::write_private(&path, &config.encode().unwrap()).unwrap();
            let plan = f.prepare();
            let approval = plan.preview.approval.clone();
            plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
                .unwrap();
            assert_eq!(fs::read(f.source.join("delivery.sqlite3")).unwrap(), source);
            rollback(&f.source, &f.global, &approval).unwrap();
            assert_eq!(
                fs::read(f.destination.join("control.json")).unwrap(),
                destination
            );
        }
    }
    #[test]
    fn cutover_attachment_read_budget_exhaustion_is_not_misclassified_as_a_blocked_draft() {
        let f = Fixture::new();
        let bytes = vec![b'x'; 2 * 1024 * 1024];
        f.queue_file("file.txt", &bytes, "work-quota");
        let import = ImportPlan::prepare(&f.source, &f.destination, true).unwrap();
        import
            .with_budget(|budget| budget.work(2147483648 - bytes.len() as u64))
            .unwrap();
        let con = private_connection(&import.staging_path().join("delivery.sqlite3")).unwrap();
        let service = ServiceConfig::load(&f.source.join("telegram-service.json")).unwrap();
        let result = relocate_attachments(&con, &f.source, &f.destination, &import, &service);
        assert!(result.err().unwrap().contains("byte-work"));
        let state: String = con
            .query_row(
                "SELECT state FROM delivery WHERE dedup_key='work-quota'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "pending");
        assert!(!exists(&f.source.join(RETIRED)));
        assert!(!exists(&f.destination.join(PROOF)));
    }
    #[test]
    fn cutover_respects_effective_service_attachment_size_limit_without_rewriting_source() {
        let f = Fixture::new();
        f.queue_file("plain", b"fixture exceeds four bytes", "size-policy");
        let path = f.source.join("telegram-service.json");
        let mut service = ServiceConfig::load(&path).unwrap();
        service.runtime.file_limit_bytes = 4;
        layouts::write_private(&path, &service.encode().unwrap()).unwrap();
        let before = fs::read(f.source.join("delivery.sqlite3")).unwrap();
        let plan = f.prepare();
        let staged = Journal::open(&plan.import.staging_path().join("delivery.sqlite3")).unwrap();
        assert_eq!(
            staged
                .delivery_for_key("inbound", "size-policy")
                .unwrap()
                .unwrap()
                .state,
            "blocked"
        );
        assert_eq!(fs::read(f.source.join("delivery.sqlite3")).unwrap(), before);
    }
    #[test]
    fn shared_import_deadline_reached_by_final_observer_prevents_source_retirement() {
        let f = Fixture::new();
        let plan = f.prepare();
        let approval = plan.preview.approval.clone();
        let stage = plan.import.staging_path().to_path_buf();
        let budget = std::sync::Arc::clone(&plan.import.budget);
        let before = fs::read(f.destination.join("control.json")).unwrap();
        let result = plan.apply(&approval, || {
            budget.lock().unwrap().deadline = std::time::Instant::now();
            Ok(vec![f.terminal.clone()])
        });
        assert!(result.err().unwrap().contains("deadline"));
        assert!(!stage.exists());
        assert!(!exists(&f.source.join(RETIRED)));
        assert!(!exists(&f.destination.join(PROOF)));
        assert_eq!(
            fs::read(f.destination.join("control.json")).unwrap(),
            before
        );
    }
    #[test]
    fn sealed_retained_route_review_survives_promotion_and_offline_rollback() {
        let f = Fixture::new();
        let jobs = br#"{"active":{"42":"unresolved legacy alias"}}"#;
        layouts::write_private(&f.source.join("jobs.json"), jobs).unwrap();
        drop(
            Registry::from_connection(
                private_connection(&f.destination.join("routes.sqlite3")).unwrap(),
            )
            .unwrap(),
        );
        let destination_routes = fs::read(f.destination.join("routes.sqlite3")).unwrap();
        let plan = f.prepare();
        let stage = plan.import.staging_path();
        let retained = stage.join("imported-route-review/routes.sqlite3");
        let retained_bytes = fs::read(&retained).unwrap();
        let connection = private_connection(&retained).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM route_imports", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM chat_routes", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(connection);
        let receipt: Value =
            serde_json::from_slice(&fs::read(stage.join("migration.json")).unwrap()).unwrap();
        assert_eq!(
            receipt["hashes"]["imported-route-review/routes.sqlite3"],
            hash(&retained_bytes)
        );
        let approval = plan.preview.approval.clone();
        let backup = plan
            .apply(&approval, || Ok(vec![f.terminal.clone()]))
            .unwrap();
        assert_eq!(
            fs::read(f.destination.join("imported-route-review/routes.sqlite3")).unwrap(),
            retained_bytes
        );
        assert_eq!(
            fs::read(backup.join("routes.sqlite3")).unwrap(),
            destination_routes
        );
        let active = private_connection(&f.destination.join("routes.sqlite3")).unwrap();
        assert_eq!(
            active
                .query_row(
                    "SELECT count(*) FROM chat_routes WHERE chat='42'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        drop(active);
        rollback(&f.source, &f.global, &approval).unwrap();
        assert_eq!(
            fs::read(f.destination.join("routes.sqlite3")).unwrap(),
            destination_routes
        );
        assert_eq!(
            fs::read(
                backup
                    .with_extension("abandoned")
                    .join("imported-route-review/routes.sqlite3")
            )
            .unwrap(),
            retained_bytes
        );
        assert_eq!(fs::read(f.source.join("jobs.json")).unwrap(), jobs);
        assert!(!exists(&f.source.join(RETIRED)));
    }
    #[test]
    fn reviewed_stage_drift_is_refused_before_source_retirement() {
        for change in ["content", "removed", "added", "receipt", "proof"] {
            let f = Fixture::new();
            let plan = f.prepare();
            let approval = plan.preview.approval.clone();
            let stage = plan.import.staging_path().to_path_buf();
            let source_control = fs::read(f.source.join("control.json")).unwrap();
            let destination_control = fs::read(f.destination.join("control.json")).unwrap();
            let destination_delivery = fs::read(f.destination.join("delivery.sqlite3")).unwrap();
            let result = plan.apply(&approval, || {
                // Drift during the final observation must be checked afterwards.
                match change {
                    "content" => {
                        let path = stage.join("control.json");
                        let mut config = ControlConfig::parse(&fs::read(&path).unwrap()).unwrap();
                        config.runtime.shell_cols += 1;
                        layouts::write_private(&path, &config.encode().unwrap()).unwrap();
                    }
                    "removed" => fs::remove_file(stage.join("control.json")).unwrap(),
                    "added" => layouts::write_private(&stage.join("extra/note.txt"), b"later note")
                        .unwrap(),
                    "receipt" | "proof" => {
                        let path = stage.join(if change == "receipt" {
                            "migration.json"
                        } else {
                            PROOF
                        });
                        let mut bytes = fs::read(&path).unwrap();
                        bytes.push(b'\n');
                        layouts::write_private(&path, &bytes).unwrap();
                    }
                    _ => unreachable!(),
                }
                Ok(vec![f.terminal.clone()])
            });
            assert!(
                result.is_err(),
                "Retired source with changed stage: {change}"
            );
            assert!(!stage.exists());
            assert!(!exists(&f.source.join(RETIRED)));
            assert!(!exists(&f.destination.join(PROOF)));
            assert_eq!(
                fs::read(f.source.join("control.json")).unwrap(),
                source_control
            );
            assert_eq!(
                fs::read(f.destination.join("control.json")).unwrap(),
                destination_control
            );
            assert_eq!(
                fs::read(f.destination.join("delivery.sqlite3")).unwrap(),
                destination_delivery
            );
        }
    }
    #[test]
    fn final_viewer_history_snapshot_survives_cutover_and_destination_rollback() {
        let f = Fixture::new();
        let source_history = br#"{"version":2,"views":[{"version":99,"unresolved":"retain exact source evidence"}]}"#;
        let destination_history = b"{\n\"version\":2,\"views\":[]\n}\n";
        layouts::write_private(&f.source.join("views.json"), source_history).unwrap();
        layouts::write_private(&f.destination.join("views.json"), destination_history).unwrap();
        let plan = f.prepare();
        let approval = plan.preview.approval.clone();
        assert_eq!(
            fs::read(plan.import.staging_path().join("views.json")).unwrap(),
            source_history
        );
        assert_eq!(
            fs::read(f.destination.join("views.json")).unwrap(),
            destination_history
        );
        let backup = plan
            .apply(&approval, || Ok(vec![f.terminal.clone()]))
            .unwrap();
        assert_eq!(
            fs::read(f.destination.join("views.json")).unwrap(),
            source_history
        );
        assert_eq!(
            fs::read(backup.join("views.json")).unwrap(),
            destination_history
        );
        assert_eq!(
            fs::read(f.source.join("views.json")).unwrap(),
            source_history
        );
        rollback(&f.source, &f.global, &approval).unwrap();
        assert_eq!(
            fs::read(f.destination.join("views.json")).unwrap(),
            destination_history
        );
        assert_eq!(
            fs::read(f.source.join("views.json")).unwrap(),
            source_history
        );
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
    #[test]
    fn cutover_retains_destination_global_preferences_and_rollback_restores_exact_bytes() {
        let f = Fixture::new();
        let destination=b"{\"unrelated\":\"private-fixture\",\"discovery\":{\"scan_processes\":false},\"instances\":[]}";
        layouts::write_private(&f.destination.join("dashboard.json"), destination).unwrap();
        layouts::write_private(
            &f.source.join("dashboard.json"),
            br#"{"discovery":{"instance_limit":4096}}"#,
        )
        .unwrap();
        let plan = f.prepare();
        assert_eq!(
            fs::read(plan.import.staging_path().join("dashboard.json")).unwrap(),
            destination
        );
        assert!(
            !serde_json::to_string(&plan.preview)
                .unwrap()
                .contains("private-fixture")
        );
        let approval = plan.preview.approval.clone();
        plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
            .unwrap();
        assert_eq!(
            fs::read(f.destination.join("dashboard.json")).unwrap(),
            destination
        );
        rollback(&f.source, &f.global, &approval).unwrap();
        assert_eq!(
            fs::read(f.destination.join("dashboard.json")).unwrap(),
            destination
        );
    }
    #[test]
    fn changed_global_preferences_refuse_cutover_before_source_retirement() {
        let f = Fixture::new();
        let plan = f.prepare();
        let approval = plan.preview.approval.clone();
        layouts::write_private(&f.destination.join("dashboard.json"), b"{}").unwrap();
        assert!(
            plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
                .unwrap_err()
                .contains("Dashboard preferences changed")
        );
        assert!(!f.source.join(RETIRED).exists());
        assert_eq!(
            fs::read(f.destination.join("dashboard.json")).unwrap(),
            b"{}"
        );
    }
    #[test]
    fn cutover_preserves_destination_gateway_choice_without_copying_unselected_source_key() {
        use super::super::gateway;
        let f = Fixture::new();
        let source_key = f.source.join("provider-key");
        layouts::write_private(&source_key, b"source-gateway-fixture-key").unwrap();
        let config = gateway::Config {
            version: 1,
            enabled: true,
            format: gateway::Format::AnthropicMessages,
            endpoint: "https://source.example.invalid".into(),
            model: "source-model".into(),
            credential_file: source_key,
            harness: "claude".into(),
        };
        layouts::write_private(&f.source.join("gateway.json"), &config.encode().unwrap()).unwrap();
        let selected = gateway::Config {
            enabled: false,
            endpoint: "https://selected.example.invalid".into(),
            model: "selected-model".into(),
            credential_file: f.destination.join("gateway-token.txt"),
            ..config
        };
        layouts::write_private(
            &f.destination.join("gateway.json"),
            &selected.encode().unwrap(),
        )
        .unwrap();
        let plan = f.prepare();
        assert!(
            !plan
                .import
                .staging_path()
                .join("gateway-token.txt")
                .exists()
        );
        let approval = plan.preview.approval.clone();
        plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
            .unwrap();
        assert_eq!(
            gateway::load(&f.destination).unwrap(),
            Some(selected.clone())
        );
        assert!(!f.destination.join("gateway-token.txt").exists());
        rollback(&f.source, &f.global, &approval).unwrap();
        assert_eq!(gateway::load(&f.destination).unwrap(), Some(selected));
    }
    #[test]
    fn cutover_refuses_changed_gateway_credentials_and_does_not_retire_source() {
        let f = Fixture::new();
        let key = f.destination.join("gateway-token.txt");
        layouts::write_private(&key, b"selected-fixture-key").unwrap();
        let config = super::super::gateway::Config {
            version: 1,
            enabled: true,
            format: super::super::gateway::Format::OpenaiResponses,
            endpoint: "https://selected.example.invalid".into(),
            model: "selected-model".into(),
            credential_file: key.clone(),
            harness: "codex".into(),
        };
        layouts::write_private(
            &f.destination.join("gateway.json"),
            &config.encode().unwrap(),
        )
        .unwrap();
        let plan = f.prepare();
        let approval = plan.preview.approval.clone();
        layouts::write_private(&key, b"changed-fixture-key").unwrap();
        assert!(
            plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
                .unwrap_err()
                .contains("Gateway configuration or credential changed")
        );
        assert!(!f.source.join(RETIRED).exists());
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
    fn native_route_replacement_preserves_staged_database_and_sidecars_for_review() {
        let f = Fixture::new();
        let import = ImportPlan::prepare(&f.source, &f.destination, true).unwrap();
        let stage = import.staging_path();
        let previous = stage.join("routes.sqlite3");
        let con = private_connection(&previous).unwrap();
        con.execute_batch(
            "CREATE TABLE retained(state TEXT); INSERT INTO retained VALUES('unconfirmed')",
        )
        .unwrap();
        drop(con);
        let before = fs::read(&previous).unwrap();
        for (suffix, bytes) in [
            ("-journal", b"".as_slice()),
            ("-wal", b"uncertain staged WAL".as_slice()),
            ("-shm", b"uncertain staged SHM".as_slice()),
        ] {
            layouts::write_private(&stage.join(format!("routes.sqlite3{suffix}")), bytes).unwrap();
        }
        let service = ServiceConfig::load(&stage.join("telegram-service.json")).unwrap();
        let result = reconcile_routes(&import, &service, std::slice::from_ref(&f.terminal));
        assert!(result.is_ok(), "{result:?}");
        let retained = stage.join("imported-route-review");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&retained).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::read(retained.join("routes.sqlite3")).unwrap(), before);
        for (suffix, bytes) in [
            ("-journal", b"".as_slice()),
            ("-wal", b"uncertain staged WAL".as_slice()),
            ("-shm", b"uncertain staged SHM".as_slice()),
        ] {
            assert_eq!(
                fs::read(retained.join(format!("routes.sqlite3{suffix}"))).unwrap(),
                bytes
            );
        }
        let con = private_connection(&stage.join("routes.sqlite3")).unwrap();
        assert_eq!(
            con.query_row(
                "SELECT count(*) FROM chat_routes WHERE chat='42'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert!(!exists(&f.source.join(RETIRED)));
        assert!(!exists(&f.destination.join(PROOF)));
    }
    #[test]
    fn final_snapshot_uses_source_runtime_limits_and_preserves_them_after_commit() {
        let f = Fixture::new();
        let control_path = f.source.join("control.json");
        let mut control = ControlConfig::parse(&read(&control_path).unwrap()).unwrap();
        let destination_control = read(&f.destination.join("control.json")).unwrap();
        let destination_delivery = fs::read(f.destination.join("delivery.sqlite3")).unwrap();
        control.runtime.database_snapshot_limit_bytes = 4096;
        layouts::write_private(&control_path, &control.encode().unwrap()).unwrap();
        let called = std::cell::Cell::new(false);
        let result = Plan::prepare(&f.destination, &f.global, || {
            called.set(true);
            Ok(vec![f.terminal.clone()])
        });
        assert!(result.is_err());
        assert!(!called.get());
        assert_eq!(
            read(&f.destination.join("control.json")).unwrap(),
            destination_control
        );
        assert_eq!(
            fs::read(f.destination.join("delivery.sqlite3")).unwrap(),
            destination_delivery
        );
        assert!(!exists(&f.source.join(RETIRED)));
        assert!(!exists(&f.destination.join(PROOF)));
        assert!(!fs::read_dir(&f.root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".daedalus-import-")
        }));
        control.runtime.database_snapshot_limit_bytes = 1048576;
        control.runtime.database_snapshot_timeout_ms = 2000;
        control.runtime.database_snapshot_pages_per_step = 1;
        control.runtime.database_snapshot_busy_wait_ms = 9;
        control.runtime.database_snapshot_progress_ops = 7;
        layouts::write_private(&control_path, &control.encode().unwrap()).unwrap();
        let plan = f.prepare();
        let approval = plan.preview.approval.clone();
        plan.apply(&approval, || Ok(vec![f.terminal.clone()]))
            .unwrap();
        assert_eq!(
            ControlConfig::parse(&read(&f.destination.join("control.json")).unwrap())
                .unwrap()
                .runtime,
            control.runtime
        );
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
