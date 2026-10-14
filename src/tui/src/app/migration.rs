//! Import into a separate private instance. The original is the rollback copy.
//! Preparing captures inert state; applying never starts a feeder or a harness.
use super::setup::SetupPlan;
use crate::{
    adapters::sqlite,
    core::{checkpoints::Registry, layouts},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::{ffi::OsStrExt, fs::DirBuilderExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

type Result<T> = std::result::Result<T, String>;
static NEXT: AtomicU64 = AtomicU64::new(0);
const RECEIPT: &str = "migration.json";

#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub files: Vec<String>,
    pub includes_credentials: bool,
    pub notices: Vec<String>,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Manifest {
    version: u32,
    source: PathBuf,
    activation: String,
    hashes: BTreeMap<String, String>,
}
#[derive(Serialize)]
pub struct Applied {
    pub destination: PathBuf,
    pub changed: bool,
    pub requires_cutover: bool,
}
pub struct ImportPlan {
    pub preview: Preview,
    stage: Stage,
    destination: PathBuf,
    manifest: Manifest,
}
struct Stage(PathBuf, bool);
impl Stage {
    fn create(path: PathBuf) -> Result<Self> {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "Cannot create private import staging directory")?;
        Ok(Self(path, true))
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        if self.1 {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

impl ImportPlan {
    pub fn prepare(source: &Path, destination: &Path, credentials: bool) -> Result<Self> {
        Self::prepare_with_memory_credentials(source, destination, credentials, credentials)
    }
    pub(crate) fn prepare_with_memory_credentials(
        source: &Path,
        destination: &Path,
        credentials: bool,
        memory_credentials: bool,
    ) -> Result<Self> {
        layouts::check_path(source)?;
        layouts::check_path(destination)?;
        if source == destination
            || destination.starts_with(source)
            || source.starts_with(destination)
        {
            return Err("Import needs a separate destination outside the source instance".into());
        }
        if !source.is_dir() {
            return Err("Import source must be an instance directory".into());
        }
        trusted_directory(source, true)?;
        let parent = destination
            .parent()
            .ok_or("Import destination needs a parent directory")?;
        if !parent.is_dir() {
            return Err("Create the destination parent directory before importing".into());
        }
        trusted_directory(parent, false)?;
        let stage_path = parent.join(format!(
            ".daedalus-import-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let stage = Stage::create(stage_path)?;
        let settings = SetupPlan::import(&stage.0, &source.join("control.json"))?;
        let mut notices = settings.preview.notices.clone();
        // The settings-only limitation is superseded by this enclosing import.
        notices.retain(|n| !n.starts_with("Routes, checkpoints"));
        let mut config = settings.config().clone();
        let mut files = BTreeMap::<String, Vec<u8>>::new();
        if let Some(bytes) = layouts::read_private(&source.join("telegram.env"))? {
            let values = telegram_env(&bytes)?;
            let mut policy = BTreeMap::new();
            for key in ["CONTROL_IDS", "FEED_ALL_GROUPS"] {
                if let Some(value) = values.get(key) {
                    let ids = value
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|id| !id.is_empty())
                        .map(|id| {
                            id.parse::<i64>()
                                .map_err(|_| "Invalid Telegram policy identity")
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    policy.insert(key, ids);
                }
            }
            if !policy.is_empty() {
                files.insert(
                    "legacy-telegram-policy.json".into(),
                    serde_json::to_vec_pretty(&json!({"requires_review":true,"policy":policy}))
                        .map_err(|_| "Cannot preserve Telegram policy")?,
                );
            }
            for (target, key) in [
                (&mut config.telegram.owner_id, "OWNER_ID"),
                (&mut config.telegram.bot_id, "BOT_ID"),
                (&mut config.telegram.username, "BOT_USERNAME"),
            ] {
                if let Some(value) = values.get(key) {
                    let value = if key == "BOT_USERNAME" {
                        value.trim_start_matches('@')
                    } else {
                        value
                    };
                    if target.as_ref().is_some_and(|old| old != value) {
                        return Err(
                            "Telegram identity conflicts between settings and token file".into(),
                        );
                    }
                    *target = Some(value.into());
                }
            }
            if credentials && let Some(token) = values.get("TELEGRAM_BOT_TOKEN") {
                files.insert(
                    "telegram.env".into(),
                    format!("TELEGRAM_BOT_TOKEN={token}\n").into_bytes(),
                );
            }
        }
        let native_service_path = source.join("telegram-service.json");
        if fs::symlink_metadata(&native_service_path).is_ok() {
            use super::telegram_config::ServiceConfig;
            let mut service = ServiceConfig::load(&native_service_path)?;
            let registrations = service.check_registrations()?;
            if config
                .telegram
                .bot_id
                .as_ref()
                .is_some_and(|id| id != &service.bot_id.to_string())
            {
                return Err(
                    "Native Telegram service and control settings disagree about bot identity"
                        .into(),
                );
            }
            config.telegram.bot_id = Some(service.bot_id.to_string());
            if credentials {
                let token = service.read_token()?;
                if let Some(legacy) = files.get("telegram.env") {
                    let values = telegram_env(legacy)?;
                    if values
                        .get("TELEGRAM_BOT_TOKEN")
                        .is_some_and(|value| value != &token)
                    {
                        return Err("Native and legacy Telegram credentials conflict".into());
                    }
                }
                files.insert(
                    "telegram-token.txt".into(),
                    format!("{token}\n").into_bytes(),
                );
            }
            service.token_file = destination.join("telegram-token.txt");
            service.registrations.clear();
            for (_, mut registration) in registrations {
                if registration.directory != source {
                    return Err(
                        "Native hook registration belongs to another instance directory".into(),
                    );
                }
                registration.directory = destination.into();
                if let Some(transcript) = &registration.transcript
                    && let Ok(relative) = transcript.strip_prefix(source)
                {
                    registration.transcript = Some(destination.join(relative));
                    notices.push("An instance-local hook transcript path was relocated without copying conversation history; re-register that transcript before using its hooks.".into());
                }
                registration
                    .validate()
                    .map_err(|_| "Relocated hook registration is invalid")?;
                let name = format!("hooks/{}.json", registration.id);
                service.registrations.push(destination.join(&name));
                files.insert(
                    name,
                    serde_json::to_vec_pretty(&registration)
                        .map_err(|_| "Cannot encode relocated hook registration")?,
                );
            }
            service.validate()?;
            notices.push(format!("Native Telegram service preserved {} explicit grants and {} hook registrations. No grants were inferred. Token contents {}. Native routes and recipient revisions require review before cutover; harness hooks still need installation against the destination registration paths.", service.grants.len(), service.registrations.len(), if credentials {"copied by explicit credential selection"} else {"not copied; provision the new private token file"}));
            files.insert(
                "telegram-service.json".into(),
                serde_json::to_vec_pretty(&service)
                    .map_err(|_| "Cannot encode imported Telegram service")?,
            );
        }
        files.insert("control.json".into(), config.encode()?);
        if let Some(bytes) = layouts::read_private(&source.join("tg_offset"))? {
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| "Invalid legacy Telegram polling offset")?
                .trim();
            let offset = text
                .parse::<i64>()
                .map_err(|_| "Invalid legacy Telegram polling offset")?;
            if offset < 0 {
                return Err("Legacy Telegram polling offset must not be negative".into());
            }
            files.insert("legacy-polling.json".into(),serde_json::to_vec_pretty(&json!({"next_offset":offset,"source":"tg_offset","namespace":config.agent_name,"bot_id":config.telegram.bot_id,"requires_cutover":true})).map_err(|_| "Cannot preserve legacy polling offset")?);
        }
        for name in ["allowed_ids.json", "mention_only_chats.json"] {
            if let Some(bytes) = layouts::read_private(&source.join(name))? {
                let policy: Value =
                    serde_json::from_slice(&bytes).map_err(|_| "Invalid legacy Telegram policy")?;
                if !(policy.is_object() || (name == "mention_only_chats.json" && policy.is_array()))
                {
                    return Err("Invalid legacy Telegram policy structure".into());
                }
                if name == "mention_only_chats.json" {
                    let chats = mention_chats(&policy)?;
                    if !chats.is_empty() {
                        let bot = config.telegram.bot_id.as_deref()
                            .filter(|id| id.parse::<i64>().is_ok_and(|id| id > 0))
                            .ok_or("Legacy mention-only policy requires an explicit bot identity; configure it before import")?;
                        files.insert("input-policy-review.json".into(), serde_json::to_vec_pretty(&json!({
                            "version":1,"bot":bot,"quiet_chats":chats,"listen":false,
                            "scope_review":"Apply only to the explicitly selected native instance and this bot; existing native overrides take precedence",
                            "grants":"unchanged","source":"mention_only_chats.json"
                        })).map_err(|_| "Cannot encode input-policy review")?);
                        notices.push(format!("{} legacy mention-only chat overrides will remain quiet for bot {}. No send grants or listen permission are added; existing native per-chat overrides take precedence.", chats.len(), bot));
                    }
                }
                files.insert(
                    format!("legacy-{name}"),
                    serde_json::to_vec_pretty(&json!({"requires_review":true,"policy":policy}))
                        .map_err(|_| "Cannot preserve legacy policy")?,
                );
            }
        }
        if let Some(bytes) = layouts::read_private(&source.join("pending_feed.jsonl"))? {
            files.insert("legacy-pending-feed.jsonl".into(), bytes);
            notices.push("Legacy pending input is retained as uncertain evidence, never as automatically retryable delivery.".into());
        }
        if source.join("layouts.json").exists() {
            let layout = layouts::Layouts::load(source).map_err(|_| "Invalid saved layouts")?;
            files.insert(
                "layouts.json".into(),
                serde_json::to_vec_pretty(&layout).map_err(|_| "Cannot encode imported layouts")?,
            );
        }
        let mut registry = Registry::load(source).map_err(|_| "Invalid saved checkpoints")?;
        for checkpoint in &mut registry.checkpoints {
            if checkpoint.context_file.is_empty() {
                continue;
            }
            let context = Path::new(&checkpoint.context_file);
            if !context.starts_with(source.join("checkpoints")) {
                return Err("Checkpoint context is outside the source checkpoint directory; relocate it explicitly before import".into());
            }
            let bytes =
                layouts::read_private(context)?.ok_or("A saved checkpoint context is missing")?;
            let name = format!("checkpoints/{:x}.md", Sha256::digest(&bytes));
            checkpoint.context_file = destination
                .join(&name)
                .to_str()
                .ok_or("Import path must be UTF-8")?
                .into();
            files.insert(name, bytes);
            if files.values().map(Vec::len).sum::<usize>() > 64 * 1024 * 1024 {
                return Err("Checkpoint import exceeds 64 MiB state limit".into());
            }
        }
        if !registry.checkpoints.is_empty() {
            files.insert(
                "dashboard-checkpoints.json".into(),
                serde_json::to_vec_pretty(&registry)
                    .map_err(|_| "Cannot encode imported checkpoints")?,
            );
        }
        if let Some(bytes) = layouts::read_private(&source.join("jobs.json"))? {
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| "Invalid saved route configuration")?;
            let active = value.get("active").cloned().unwrap_or(json!({}));
            if !active.is_object() {
                return Err("Saved route map must be an object".into());
            }
            // Preserve bindings for explicit exact-identity reconciliation. No
            // imported alias is interpreted as a current terminal or authority.
            let bindings = value
                .pointer("/meta/chat_bindings")
                .cloned()
                .unwrap_or(json!({}));
            let pins = value
                .pointer("/meta/chat_pins")
                .cloned()
                .unwrap_or(json!({}));
            if !bindings.is_object() || !pins.is_object() {
                return Err("Saved route metadata must contain objects".into());
            }
            files.insert(
                "legacy-routes.json".into(),
                serde_json::to_vec_pretty(
                    &json!({"active":active,"meta":{"chat_bindings":bindings,"chat_pins":pins}}),
                )
                .map_err(|_| "Cannot encode saved routes")?,
            );
        }
        import_memory(
            source,
            destination,
            memory_credentials,
            &mut files,
            &mut notices,
        )?;
        for (name, bytes) in &files {
            layouts::write_private(&stage.0.join(name), bytes)?;
        }
        for name in [
            "delivery.sqlite3",
            "armonia.sqlite3",
            "goals.sqlite3",
            "hook-observations.sqlite3",
            "telegram-controls.sqlite3",
            "session-renames.sqlite3",
            "telegram-stream.sqlite3",
            "routes.sqlite3",
        ] {
            let path = source.join(name);
            if fs::symlink_metadata(&path).is_ok() {
                let saved_name = if name == "routes.sqlite3" {
                    "legacy-native-routes.sqlite3"
                } else {
                    name
                };
                // Snapshot goal/readiness stores without opening their runtime
                // owners: startup recovery would mutate status during preview.
                sqlite::snapshot(&path, &stage.0.join(saved_name), Duration::from_secs(10))
                    .map_err(str::to_owned)?;
                if name == "telegram-stream.sqlite3" {
                    let count = disable_imported_streams(&stage.0.join(saved_name))?;
                    notices.push(format!("{count} Telegram stream subscriptions retained disabled with advanced epochs and cleared active turns. Original scopes and audit records are retained; explicitly rearm after cutover."));
                }
                if name == "session-renames.sqlite3" {
                    let unresolved = audit_session_renames(&stage.0.join(saved_name))?;
                    notices.push(format!("Session rename journal preserved with {unresolved} uncertain operations. Original instance, bot, actor and terminal identities remain unchanged; no operation is replayed or enabled by import."));
                }
                notices.push(format!(
                    "{name}: committed SQLite snapshot retained; no delivery or route is activated."
                ));
            }
        }
        if let (Some(bot), Some(jobs)) = (
            config.telegram.bot_id.as_deref(),
            files.get("legacy-routes.json"),
        ) {
            use crate::core::routes;
            use rusqlite::{Connection, OpenFlags};
            let legacy_path = stage.0.join("armonia.sqlite3");
            let legacy = if legacy_path.exists() {
                let connection = Connection::open_with_flags(
                    &legacy_path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
                )
                .map_err(|_| "Cannot inspect legacy identity snapshot")?;
                connection
                    .execute_batch("PRAGMA trusted_schema=OFF;PRAGMA query_only=ON;")
                    .map_err(|_| "Cannot restrict legacy identity snapshot")?;
                match routes::legacy_snapshot(&connection) {
                    Ok(rows) => rows,
                    Err(_) => {
                        notices.push("Legacy identity schema is unavailable; saved aliases remain unresolved until manually rebound.".into());
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            let preview = routes::preview_import(
                jobs,
                destination.to_str().ok_or("Destination must be UTF-8")?,
                bot,
                &legacy,
                &[],
                &[],
            )
            .map_err(|_| "Cannot prepare exact route review")?;
            layouts::write_private(
                &stage.0.join("route-review.json"),
                &serde_json::to_vec_pretty(&preview).map_err(|_| "Cannot encode route review")?,
            )?;
            let route_path = stage.0.join("routes.sqlite3");
            layouts::write_private(&route_path, b"")?;
            let connection = Connection::open_with_flags(
                &route_path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|_| "Cannot create imported route registry")?;
            let mut registry = routes::Registry::from_connection(connection)
                .map_err(|_| "Cannot initialize imported route registry")?;
            registry
                .stage_import(&preview)
                .map_err(|_| "Cannot retain unresolved route review")?;
        }
        let imported_instance = files
            .get("telegram-service.json")
            .map(|bytes| {
                serde_json::from_slice::<super::telegram_config::ServiceConfig>(bytes)
                    .map(|service| service.instance)
                    .map_err(|_| "Invalid staged Telegram service")
            })
            .transpose()?
            .unwrap_or_else(|| config.agent_name.clone());
        if let Some(bot) = config.telegram.bot_id.as_deref() {
            apply_mention_policy_scope(&stage.0, &imported_instance, bot)?;
        }
        let hashes = hashes(&stage.0)?;
        let manifest = Manifest {
            version: 1,
            source: source.into(),
            activation: "requires_cutover".into(),
            hashes,
        };
        layouts::write_private(
            &stage.0.join(RECEIPT),
            &serde_json::to_vec_pretty(&manifest).map_err(|_| "Cannot encode migration receipt")?,
        )?;
        notices.extend([
            "Original instance is unchanged and remains the rollback copy. Do not run two feeders for the same bot.".into(),
            "Imported routes require exact live identity reconciliation. Hook scripts and account credentials are not copied.".into(),
            "Stop the source feeder and take a final snapshot before cutover. This import does not activate any service.".into(),
            "Custom automation and unrecognized gateway secrets are not imported; configure these explicitly.".into(),
        ]);
        let preview = Preview {
            source: source.into(),
            destination: destination.into(),
            files: manifest.hashes.keys().cloned().collect(),
            includes_credentials: files.contains_key("telegram.env")
                || files.contains_key("telegram-token.txt")
                || files.contains_key("hindsight-token.txt"),
            notices,
        };
        Ok(Self {
            preview,
            stage,
            destination: destination.into(),
            manifest,
        })
    }

    pub(crate) fn staging_path(&self) -> &Path {
        &self.stage.0
    }
    pub(crate) fn seal_cutover(&mut self) -> Result<()> {
        self.manifest.hashes = hashes(&self.stage.0)?;
        layouts::write_private(
            &self.stage.0.join(RECEIPT),
            &serde_json::to_vec_pretty(&self.manifest)
                .map_err(|_| "Cannot encode cutover manifest")?,
        )
    }
    pub fn apply(mut self) -> Result<Applied> {
        layouts::check_path(&self.destination)?;
        trusted_directory(
            self.destination.parent().ok_or("Missing import parent")?,
            false,
        )?;
        if self.destination.exists() {
            let previous = layouts::read_private(&self.destination.join(RECEIPT))?
                .ok_or("Destination already exists; select a new private instance directory")?;
            let previous: Manifest = serde_json::from_slice(&previous)
                .map_err(|_| "Invalid existing migration receipt")?;
            if previous == self.manifest && hashes(&self.destination)? == self.manifest.hashes {
                return Ok(Applied {
                    destination: self.destination.clone(),
                    changed: false,
                    requires_cutover: true,
                });
            }
            return Err("Destination or source snapshot changed; import to a new directory instead of overwriting active state".into());
        }
        install_directory(&self.stage.0, &self.destination)?;
        self.stage.1 = false;
        fs::File::open(self.destination.parent().unwrap())
            .and_then(|f| f.sync_all())
            .map_err(|_| "Import installed but directory synchronization failed")?;
        Ok(Applied {
            destination: self.destination.clone(),
            changed: true,
            requires_cutover: true,
        })
    }
}

fn trusted_directory(path: &Path, private: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let uid = unsafe { libc::geteuid() };
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|_| "Cannot inspect import directory ownership")?;
        if !metadata.is_dir()
            || (metadata.uid() != uid && metadata.uid() != 0)
            || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
        {
            return Err("Import directories require trusted ownership and no shared writable non-sticky ancestor".into());
        }
    }
    if private {
        let metadata = fs::metadata(path).map_err(|_| "Cannot inspect private import directory")?;
        if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err("Source instance directory must be private and owned by this user".into());
        }
    }
    Ok(())
}

/// This is a staged copy, never the source runtime database. Old stream epochs
/// must not authorize copied queued output or revive an earlier accepted turn.
fn disable_imported_streams(path: &Path) -> Result<usize> {
    let count=super::diagnostics::with_database(path,|con| {
        for table in ["stream_subscriptions","stream_inputs"] {
            let valid:bool=con.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=? AND rootpage>0)",[table],|r|r.get(0)).map_err(|_|"Cannot inspect stream journal schema")?;
            if !valid{return Err("Native stream journal schema is missing".into());}
        }
        let triggers:i64=con.query_row("SELECT count(*) FROM sqlite_schema WHERE type='trigger'",[],|r|r.get(0)).map_err(|_|"Cannot inspect stream journal schema")?;
        if triggers!=0{return Err("Stream journal triggers require explicit review".into());}
        let mut query=con.prepare("SELECT plan,epoch,enabled,after_id,prompt_id,input_after FROM stream_subscriptions LIMIT 1025").map_err(|_|"Invalid stream subscription schema")?;
        let rows=query.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?))).map_err(|_|"Cannot inspect stream subscriptions")?;
        let mut count=0;
        for row in rows {
            let (encoded,epoch,enabled,after,prompt,input)=row.map_err(|_|"Malformed stream subscription")?;
            if count>=1024 || encoded.len()>1048576 || epoch<=0 || epoch==i64::MAX || !matches!(enabled,0|1) || after<0 || prompt<0 || input<0 {return Err("Invalid stream subscription boundaries".into());}
            let plan:super::telegram_stream::Plan=serde_json::from_str(&encoded).map_err(|_|"Invalid native stream plan")?;
            plan.binding.route_scope.validate().map_err(|_|"Invalid stream route scope")?;
            plan.binding.goal_scope.validate().map_err(|_|"Invalid stream conversation scope")?;
            count+=1;
        }
        // Parse expected input columns without assigning them to a new instance.
        let mut inputs=con.prepare("SELECT hash,scope,actor,terminal,conversation,revision,used_event FROM stream_inputs LIMIT 10001").map_err(|_|"Invalid stream provenance schema")?;
        let mut rows=inputs.query([]).map_err(|_|"Cannot inspect stream provenance")?;
        let mut seen=0;
        while let Some(row)=rows.next().map_err(|_|"Cannot inspect stream provenance")? {
            if seen>=10000{return Err("Stream provenance exceeds import limit".into());}
            let scope:String=row.get(1).map_err(|_|"Invalid stream provenance scope")?;
            let terminal:String=row.get(3).map_err(|_|"Invalid stream provenance terminal")?;
            let scope:crate::core::routes::Scope=serde_json::from_str(&scope).map_err(|_|"Invalid stream provenance scope")?;
            scope.validate().map_err(|_|"Invalid stream provenance scope")?;
            let terminal:crate::core::identity::TerminalId=serde_json::from_str(&terminal).map_err(|_|"Invalid stream provenance terminal")?;
            terminal.validate().map_err(|_|"Invalid stream provenance terminal")?;
            seen+=1;
        }
        Ok(count)
    })?.ok_or("Stream snapshot is missing")?;
    let con = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "Cannot open staged stream snapshot")?;
    let deadline = std::time::Instant::now();
    con.progress_handler(
        1000,
        Some(move || deadline.elapsed() > Duration::from_secs(2)),
    )
    .map_err(|_| "Cannot bound staged stream update")?;
    con.execute_batch("PRAGMA trusted_schema=OFF;BEGIN IMMEDIATE;UPDATE stream_subscriptions SET enabled=0,epoch=epoch+1,turn=NULL;COMMIT;").map_err(|_|"Cannot disable imported stream subscriptions")?;
    Ok(count)
}

/// Validate an inert snapshot without opening the runtime Store (which initializes schema).
fn audit_session_renames(path: &Path) -> Result<usize> {
    super::diagnostics::with_database(path, |con| {
        let table: bool = con.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='session_renames' AND rootpage>0)", [], |row|row.get(0))
            .map_err(|_|"Cannot inspect session rename schema")?;
        let triggers: i64 = con.query_row("SELECT count(*) FROM sqlite_schema WHERE type='trigger'", [], |row|row.get(0))
            .map_err(|_|"Cannot inspect session rename schema")?;
        if !table || triggers != 0 {return Err("Session rename journal requires a native table without triggers".into());}
        let columns = con.prepare("PRAGMA table_info(session_renames)")
            .map_err(|_|"Cannot inspect session rename columns")?
            .query_map([], |row| Ok((row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,i64>(5)?)))
            .map_err(|_|"Cannot inspect session rename columns")?
            .collect::<rusqlite::Result<Vec<_>>>().map_err(|_|"Invalid session rename columns")?;
        let expected = ["operation","scope","actor","plan","state"];
        if columns.len()!=expected.len() || columns.iter().zip(expected).enumerate().any(|(i,((name,kind,pk),expected))|name!=expected || kind.to_uppercase()!="TEXT" || *pk!=i64::from(i==0)) {
            return Err("Unsupported session rename journal schema".into());
        }
        let mut query=con.prepare("SELECT operation,scope,actor,plan,state FROM session_renames LIMIT 10001")
            .map_err(|_|"Cannot inspect session rename records")?;
        let rows=query.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)))
            .map_err(|_|"Cannot read session rename records")?;
        let mut uncertain=0;
        for (index,row) in rows.enumerate() {
            let (operation,scope,actor,encoded,state)=row.map_err(|_|"Invalid session rename record")?;
            if index>=10000 || encoded.len()>1048576 || scope.len()>16384 || actor.len()>128 || actor.is_empty() || !matches!(state.as_str(),"intent"|"unknown"|"verified") {
                return Err("Session rename journal requires explicit review".into());
            }
            let scope:crate::core::routes::Scope=serde_json::from_str(&scope).map_err(|_|"Invalid rename scope")?;
            scope.validate().map_err(|_|"Invalid rename scope")?;
            let plan:super::telegram_rename::Plan=serde_json::from_str(&encoded).map_err(|_|"Invalid rename plan")?;
            super::telegram_rename::operation(&operation)?;
            super::telegram_rename::name(&plan.name)?;
            if plan.scope!=scope || plan.actor!=actor || plan.operation!=operation {
                return Err("Session rename record and plan identities disagree".into());
            }
            uncertain+=usize::from(state!="verified");
        }
        Ok(uncertain)
    })?.ok_or_else(||"Session rename journal snapshot is missing".into())
}

fn mention_chats(policy: &Value) -> Result<Vec<i64>> {
    fn chat(value: &Value) -> Result<i64> {
        let id = value
            .as_i64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
            .filter(|id| *id != 0)
            .ok_or("Legacy mention-only chat identity requires explicit review")?;
        Ok(id)
    }
    fn truthy(value: &Value) -> bool {
        match value {
            Value::Null => false,
            Value::Bool(value) => *value,
            Value::Number(value) => value.as_f64().is_some_and(|v| v != 0.0),
            Value::String(value) => !value.is_empty(),
            Value::Array(value) => !value.is_empty(),
            Value::Object(value) => !value.is_empty(),
        }
    }
    let mut chats = std::collections::BTreeSet::new();
    match policy {
        Value::Array(entries) if entries.len() <= 1000 => {
            for entry in entries {
                chats.insert(chat(entry)?);
            }
        }
        Value::Object(entries) if entries.len() <= 1000 => {
            for (key, value) in entries {
                if truthy(value) {
                    chats.insert(chat(&json!(key))?);
                }
            }
        }
        _ => {
            return Err("Legacy mention-only policy must contain at most 1000 chat entries".into());
        }
    }
    Ok(chats.into_iter().collect())
}

/// Offline staged import only. This does not grant permissions, activate routes,
/// enable listen mode, or replace an explicit native per-chat override.
pub(crate) fn apply_mention_policy(
    stage: &Path,
    service: &super::telegram_config::ServiceConfig,
) -> Result<()> {
    service.validate()?;
    apply_mention_policy_scope(stage, &service.instance, &service.bot_id.to_string())
}
fn apply_mention_policy_scope(stage: &Path, instance: &str, bot: &str) -> Result<()> {
    let Some(bytes) = layouts::read_private(&stage.join("input-policy-review.json"))? else {
        return Ok(());
    };
    let review: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid mention-only review")?;
    if review["version"] != 1
        || review["bot"] != bot
        || review["listen"] != false
        || instance.is_empty()
        || instance.len() > 128
        || instance.chars().any(char::is_control)
    {
        return Err(
            "Mention-only import scope changed; review the selected instance and bot".into(),
        );
    }
    let chats = mention_chats(&review["quiet_chats"])?;
    let path = stage.join("routes.sqlite3");
    if !path.exists() {
        layouts::write_private(&path, b"")?;
    }
    layouts::check_path(&path)?;
    let con = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "Cannot open staged input policy")?;
    // Use the shared schema initializer, then perform the inert import under one transaction.
    let registry = crate::core::routes::Registry::from_connection(con)
        .map_err(|_| "Cannot initialize staged input policy")?;
    drop(registry);
    let mut con = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "Cannot reopen staged input policy")?;
    con.execute_batch("PRAGMA trusted_schema=OFF")
        .map_err(|_| "Cannot restrict staged policy")?;
    let tx = con
        .transaction()
        .map_err(|_| "Cannot begin input-policy import")?;
    for chat in chats {
        tx.execute("INSERT INTO chat_input_policy(instance,bot,chat,listen,revision) VALUES(?,?,?,0,1) ON CONFLICT(instance,bot,chat) DO NOTHING",rusqlite::params![instance,bot,chat.to_string()])
            .map_err(|_|"Cannot preserve mention-only input policy")?;
    }
    tx.commit()
        .map_err(|_| "Cannot commit staged input policy")?;
    Ok(())
}

fn hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(|_| "Cannot inspect migration files")? {
            let entry = entry.map_err(|_| "Cannot inspect migration file")?;
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| "Cannot inspect migration file")?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "Invalid migration path")?
                .to_str()
                .ok_or("Migration path must be UTF-8")?
                .to_owned();
            if relative == RECEIPT {
                continue;
            }
            if !metadata.is_file() || metadata.len() > sqlite::MAX_DATABASE_BYTES {
                return Err("Invalid migration file".into());
            }
            // All generated files are private and bounded; stream database hashes.
            use std::io::Read;
            use std::os::unix::fs::OpenOptionsExt;
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)
                .map_err(|_| "Cannot hash migration file")?;
            let mut hasher = Sha256::new();
            let mut reader = file.take(sqlite::MAX_DATABASE_BYTES + 1);
            let count = std::io::copy(&mut reader, &mut hasher)
                .map_err(|_| "Cannot hash migration file")?;
            if count > sqlite::MAX_DATABASE_BYTES {
                return Err("Migration file exceeds size limit".into());
            }
            result.insert(relative, format!("{:x}", hasher.finalize()));
        }
    }
    Ok(result)
}

fn import_memory(
    source: &Path,
    destination: &Path,
    credentials: bool,
    files: &mut BTreeMap<String, Vec<u8>>,
    notices: &mut Vec<String>,
) -> Result<()> {
    use super::{memory, telegram_config};
    let mut token: Option<Vec<u8>> = None;
    let mut config = if let Some(mut native) = memory::load(source)? {
        if let Some(path) = &native.credential_file {
            if credentials {
                token = Some(
                    telegram_config::read_private(path, 4096)?
                        .ok_or("Memory credential file missing")?,
                );
            }
            native.credential_file = Some(destination.join("hindsight-token.txt"));
        }
        notices.push(format!("Native memory enabled setting and {} exact registration/bank bindings preserved. Registration scopes are never inferred or rewritten; enroll matching destination hooks before use.",native.bindings.len()));
        if source.join("hindsight.env").exists() {
            notices.push(
                "Native hindsight.json takes precedence; legacy hindsight.env was not merged."
                    .into(),
            );
        }
        native
    } else {
        let Some(bytes) = telegram_config::read_private(&source.join("hindsight.env"), 64 * 1024)?
        else {
            return Ok(());
        };
        let values = memory_literals(&bytes)?;
        let endpoint = values
            .get("HINDSIGHT_URL")
            .filter(|s| !s.is_empty())
            .ok_or("Legacy memory endpoint must be explicit")?
            .clone();
        let switch = telegram_config::read_private(&source.join("memory_backend"), 1024)?;
        let enabled = match switch {
            Some(bytes) => std::str::from_utf8(&bytes)
                .map_err(|_| "Invalid memory backend selection")?
                .trim()
                .eq_ignore_ascii_case("hindsight"),
            None => false,
        };
        if let Some(key) = values
            .get("HINDSIGHT_API_KEY")
            .filter(|key| !key.is_empty())
            && credentials
        {
            token = Some(key.as_bytes().to_vec());
        }
        let credential_file = values
            .get("HINDSIGHT_API_KEY")
            .filter(|key| !key.is_empty())
            .map(|_| destination.join("hindsight-token.txt"));
        let bank = values.get("HINDSIGHT_BANK").filter(|bank| !bank.is_empty());
        files.insert("memory-review.json".into(),serde_json::to_vec_pretty(&json!({"version":1,"requires_review":true,"source":"hindsight.env","enabled":enabled,"explicit_bank":bank,"reason":"Legacy bank has no verified registration scope. Select exact hook registrations; no default bank mapping is inferred."})).map_err(|_| "Cannot encode memory review")?);
        notices.push("Legacy memory enabled selection and explicit bank retained for review. No registration mapping or default bank was inferred; memory has no authorized bindings until explicitly configured.".into());
        memory::Config {
            version: 1,
            enabled,
            endpoint,
            credential_file,
            bindings: vec![],
            recall_query: "Relevant context for the current session".into(),
            timeout_ms: 3000,
        }
    };
    config.validate()?;
    if let Some(bytes) = token {
        let key = std::str::from_utf8(&bytes)
            .map_err(|_| "Invalid memory credential")?
            .trim();
        if key.is_empty() || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("Memory credential must be one raw token".into());
        }
        files.insert("hindsight-token.txt".into(), key.as_bytes().to_vec());
        notices.push("Memory credential copied by explicit credential selection into a private destination token file.".into());
    } else if config.credential_file.is_some() {
        notices.push("Memory credential was not copied. Provision the private destination token file before enabling memory effects.".into());
    }
    // Normalize only the endpoint's trailing slash; bindings retain exact scope.
    config.endpoint = config.endpoint.trim_end_matches('/').into();
    files.insert(
        "hindsight.json".into(),
        serde_json::to_vec_pretty(&config).map_err(|_| "Cannot encode memory configuration")?,
    );
    Ok(())
}
fn memory_literals(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Invalid legacy memory UTF-8")?;
    let mut result = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, value) = line
            .split_once('=')
            .ok_or("Legacy memory needs literal assignments")?;
        let key = key.trim();
        if !["HINDSIGHT_URL", "HINDSIGHT_API_KEY", "HINDSIGHT_BANK"].contains(&key) {
            continue;
        }
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('\'') && value.ends_with('\''))
                || (value.starts_with('"') && value.ends_with('"')))
        {
            value = &value[1..value.len() - 1];
        }
        if value
            .chars()
            .any(|c| c.is_control() || "$`\\\"'".contains(c))
        {
            return Err(
                "Legacy memory values must be literal; shell expansion is not allowed".into(),
            );
        }
        if result
            .insert(key.into(), value.into())
            .is_some_and(|old| old != value)
        {
            return Err("Conflicting legacy memory assignment".into());
        }
    }
    Ok(result)
}

pub(crate) fn telegram_env(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Telegram configuration is not UTF-8")?;
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if ![
            "TELEGRAM_BOT_TOKEN",
            "OWNER_ID",
            "BOT_ID",
            "BOT_USERNAME",
            "CONTROL_IDS",
            "FEED_ALL_GROUPS",
        ]
        .contains(&key)
        {
            continue;
        }
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('\'') && value.ends_with('\''))
                || (value.starts_with('"') && value.ends_with('"')))
        {
            value = &value[1..value.len() - 1];
        }
        let policy = matches!(key, "CONTROL_IDS" | "FEED_ALL_GROUPS");
        if (!policy && value.is_empty())
            || value
                .chars()
                .any(|c| (!policy && c.is_whitespace()) || "$`\\\"'".contains(c) || c.is_control())
        {
            return Err("Telegram configuration requires literal single-line values".into());
        }
        if values
            .insert(key.into(), value.into())
            .is_some_and(|old| old != value)
        {
            return Err("Conflicting Telegram configuration entries".into());
        }
    }
    Ok(values)
}

#[cfg(target_os = "linux")]
fn install_directory(source: &Path, destination: &Path) -> Result<()> {
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())
        .map_err(|_| "Invalid staging path")?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| "Invalid destination path")?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err("Cannot install import atomically; destination may already exist".into());
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn install_directory(_: &Path, _: &Path) -> Result<()> {
    Err("Atomic instance import is currently supported on Linux".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::config::ControlConfig, core::checkpoints::Checkpoint};
    use std::os::unix::fs::MetadataExt;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "daedalus-import-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
            Self(p)
        }
        fn source(&self) -> PathBuf {
            let p = self.0.join("source");
            SetupPlan::fresh(&p, ControlConfig::fresh(&self.0).unwrap())
                .unwrap()
                .apply()
                .unwrap();
            p
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn stream_import_disables_old_epochs_without_modifying_source_or_scopes() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        drop(super::super::telegram_stream::Store::open(&source).unwrap());
        let path = source.join("telegram-stream.sqlite3");
        let con = rusqlite::Connection::open(&path).unwrap();
        let terminal = json!({"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"old-generation"});
        let scope = json!({"instance":"old-instance","bot":"123","chat":"42"});
        let plan = json!({"mode":"work","binding":{"registration_id":"hook","goal_scope":{"instance":"old-instance","terminal":terminal,"conversation":"conversation","harness":"pi"},"route_scope":scope,"actor":"7","route_revision":1,"reply_to":null,"hindsight_bank":null}});
        con.execute("INSERT INTO stream_subscriptions(id,plan,epoch,enabled,after_id,prompt_id,turn,last_text,input_after,diagnostic) VALUES('fixture',?,4,1,8,9,'old-turn','previous-output',2,'active')",[plan.to_string()]).unwrap();
        con.execute(
            "INSERT INTO stream_inputs VALUES('hash',?,'7',?,'conversation',1,'event')",
            rusqlite::params![scope.to_string(), terminal.to_string()],
        )
        .unwrap();
        drop(con);
        let before = fs::read(&path).unwrap();
        let import = ImportPlan::prepare(&source, &destination, false).unwrap();
        assert!(
            import
                .preview
                .notices
                .iter()
                .any(|n| n.contains("subscriptions retained disabled"))
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        import.apply().unwrap();
        let con = rusqlite::Connection::open(destination.join("telegram-stream.sqlite3")).unwrap();
        let row: (String, i64, i64, Option<String>) = con
            .query_row(
                "SELECT plan,epoch,enabled,turn FROM stream_subscriptions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(row, (plan.to_string(), 5, 0, None));
        assert_eq!(
            con.query_row("SELECT scope FROM stream_inputs", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            scope.to_string()
        );
        drop(con);
        assert!(
            !ImportPlan::prepare(&source, &destination, false)
                .unwrap()
                .apply()
                .unwrap()
                .changed
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    #[test]
    fn malformed_stream_schema_cannot_be_imported_as_active_output() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        let path = source.join("telegram-stream.sqlite3");
        layouts::write_private(&path, b"").unwrap();
        let con = rusqlite::Connection::open(&path).unwrap();
        con.execute_batch(
            "CREATE TABLE stream_subscriptions(id TEXT);CREATE TABLE stream_inputs(hash TEXT);",
        )
        .unwrap();
        drop(con);
        assert!(ImportPlan::prepare(&source, &destination, false).is_err());
        assert!(!destination.exists());
    }
    #[test]
    fn rename_journal_is_preserved_without_replaying_or_rewriting_scopes() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        drop(super::super::telegram_rename::Store::open(&source).unwrap());
        let path = source.join("session-renames.sqlite3");
        let con = rusqlite::Connection::open(&path).unwrap();
        let scope = json!({"instance":"old-instance","bot":"123","chat":"42"});
        let operation = format!("sr:{}", "a".repeat(64));
        let plan = json!({"scope":scope,"actor":"7","operation":operation,"name":"renamed","pane":{},"before":{"endpoint":"/fixture/tmux.sock","session":"$1","server_generation":"fixture","label":"before","members":[],"clients":[]}});
        con.execute(
            "INSERT INTO session_renames VALUES(?,?,?,?,?)",
            rusqlite::params![
                operation,
                scope.to_string(),
                "7",
                plan.to_string(),
                "unknown"
            ],
        )
        .unwrap();
        drop(con);
        let before = fs::read(&path).unwrap();
        let import = ImportPlan::prepare(&source, &destination, false).unwrap();
        assert!(
            import
                .preview
                .notices
                .iter()
                .any(|n| n.contains("1 uncertain operations"))
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        import.apply().unwrap();
        let con = rusqlite::Connection::open(destination.join("session-renames.sqlite3")).unwrap();
        let stored: (String, String) = con
            .query_row("SELECT plan,state FROM session_renames", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(stored, (plan.to_string(), "unknown".into()));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(
            !ImportPlan::prepare(&source, &destination, false)
                .unwrap()
                .apply()
                .unwrap()
                .changed
        );
    }
    #[test]
    fn invalid_rename_journal_is_refused_without_fixing_source_schema() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        let path = source.join("session-renames.sqlite3");
        layouts::write_private(&path, b"").unwrap();
        let con = rusqlite::Connection::open(&path).unwrap();
        con.execute_batch("CREATE VIEW session_renames AS SELECT 'invented' AS operation;")
            .unwrap();
        drop(con);
        let before = fs::read(&path).unwrap();
        assert!(ImportPlan::prepare(&source, &destination, false).is_err());
        assert!(!destination.exists());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    #[test]
    fn mention_only_values_match_legacy_truthiness_without_inventing_listen_grants() {
        assert_eq!(
            mention_chats(&json!(["-42", -7, -42])).unwrap(),
            vec![-42, -7]
        );
        assert_eq!(mention_chats(&json!({"-42":true,"-43":false,"-44":0,"-45":"false","-46":[],"ignored":null,"-47":{}})).unwrap(),vec![-45,-42]);
        for policy in [
            json!([false]),
            json!([1.2]),
            json!([0]),
            json!({"bad":true}),
            json!("invalid"),
        ] {
            assert!(mention_chats(&policy).is_err());
        }
    }
    #[test]
    fn mention_only_requires_known_bot_and_keeps_source_unchanged() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        let original = b"[-42]";
        layouts::write_private(&source.join("mention_only_chats.json"), original).unwrap();
        assert!(ImportPlan::prepare(&source, &destination, false).is_err());
        assert!(!destination.exists());
        assert_eq!(
            fs::read(source.join("mention_only_chats.json")).unwrap(),
            original
        );
        assert!(!source.join("routes.sqlite3").exists());
    }
    #[test]
    fn mention_only_import_is_scoped_idempotent_and_preserves_native_overrides() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        let mut config =
            ControlConfig::parse(&fs::read(source.join("control.json")).unwrap()).unwrap();
        config.telegram.bot_id = Some("123".into());
        layouts::write_private(&source.join("control.json"), &config.encode().unwrap()).unwrap();
        layouts::write_private(
            &source.join("mention_only_chats.json"),
            br#"{"-42":true,"-43":false}"#,
        )
        .unwrap();
        let plan = ImportPlan::prepare(&source, &destination, false).unwrap();
        assert!(
            plan.preview
                .notices
                .iter()
                .any(|n| n.contains("No send grants or listen permission"))
        );
        assert!(!destination.exists());
        plan.apply().unwrap();
        let read = || rusqlite::Connection::open(destination.join("routes.sqlite3")).unwrap();
        let con = read();
        let row: (String, String, String, i64, i64) = con
            .query_row(
                "SELECT instance,bot,chat,listen,revision FROM chat_input_policy",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (config.agent_name.clone(), "123".into(), "-42".into(), 0, 1)
        );
        assert_eq!(
            con.query_row("SELECT count(*) FROM route_grants", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(con);
        assert!(
            !ImportPlan::prepare(&source, &destination, false)
                .unwrap()
                .apply()
                .unwrap()
                .changed
        );
        apply_mention_policy_scope(&destination, &config.agent_name, "123").unwrap();
        let con = read();
        con.execute("UPDATE chat_input_policy SET listen=1,revision=7", [])
            .unwrap();
        drop(con);
        apply_mention_policy_scope(&destination, &config.agent_name, "123").unwrap();
        assert_eq!(
            read()
                .query_row("SELECT listen,revision FROM chat_input_policy", [], |r| Ok(
                    (r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)
                ))
                .unwrap(),
            (1, 7)
        );
        assert!(apply_mention_policy_scope(&destination, &config.agent_name, "456").is_err());
        assert_eq!(
            fs::read(source.join("mention_only_chats.json")).unwrap(),
            br#"{"-42":true,"-43":false}"#
        );
    }
    #[test]
    fn native_memory_exact_bindings_and_credentials_are_explicit() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        let key = source.join("memory-secret");
        layouts::write_private(&key, b"fixture-memory-secret").unwrap();
        let binding = json!({"registration_id":"fixture-hook","scope":{"instance":"fixture","terminal":{"backend":"herdr","endpoint":"fixture","pane":"pane","generation":"generation"},"conversation":"conversation","harness":"claude"},"bank":"explicit-bank"});
        let config = json!({"version":1,"enabled":true,"endpoint":"http://127.0.0.1:8888","credential_file":key,"bindings":[binding],"recall_query":"fixture query","timeout_ms":1000});
        layouts::write_private(
            &source.join("hindsight.json"),
            &serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let plan = ImportPlan::prepare(&source, &destination, false).unwrap();
        assert!(!plan.preview.includes_credentials);
        assert!(
            !serde_json::to_string(&plan.preview)
                .unwrap()
                .contains("fixture-memory-secret")
        );
        plan.apply().unwrap();
        let imported = super::super::memory::load(&destination).unwrap().unwrap();
        assert!(imported.enabled);
        assert_eq!(
            serde_json::to_value(&imported.bindings[0]).unwrap(),
            binding
        );
        assert_eq!(
            imported.credential_file,
            Some(destination.join("hindsight-token.txt"))
        );
        assert!(!destination.join("hindsight-token.txt").exists());
        let another = t.0.join("with-key");
        let plan = ImportPlan::prepare(&source, &another, true).unwrap();
        assert!(plan.preview.includes_credentials);
        plan.apply().unwrap();
        assert_eq!(
            fs::read(another.join("hindsight-token.txt")).unwrap(),
            b"fixture-memory-secret"
        );
        assert_eq!(
            fs::metadata(another.join("hindsight-token.txt"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn legacy_memory_preserves_bank_without_inventing_scope_and_rejects_shell() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        layouts::write_private(&source.join("hindsight.env"),b"HINDSIGHT_URL=http://127.0.0.1:8888\nHINDSIGHT_BANK='explicit bank'\nHINDSIGHT_API_KEY=fixture-secret\n").unwrap();
        layouts::write_private(&source.join("memory_backend"), b"hindsight\n").unwrap();
        ImportPlan::prepare(&source, &destination, true)
            .unwrap()
            .apply()
            .unwrap();
        let config = super::super::memory::load(&destination).unwrap().unwrap();
        assert!(config.enabled);
        assert!(config.bindings.is_empty());
        let review: Value =
            serde_json::from_slice(&fs::read(destination.join("memory-review.json")).unwrap())
                .unwrap();
        assert_eq!(review["explicit_bank"], "explicit bank");
        assert_eq!(review["requires_review"], true);
        layouts::write_private(&source.join("memory_backend"), b"off").unwrap();
        layouts::write_private(
            &source.join("hindsight.env"),
            b"HINDSIGHT_URL=http://127.0.0.1:8888\n",
        )
        .unwrap();
        let disabled = t.0.join("disabled");
        ImportPlan::prepare(&source, &disabled, false)
            .unwrap()
            .apply()
            .unwrap();
        assert!(
            !super::super::memory::load(&disabled)
                .unwrap()
                .unwrap()
                .enabled
        );
        let review: Value =
            serde_json::from_slice(&fs::read(disabled.join("memory-review.json")).unwrap())
                .unwrap();
        assert!(review["explicit_bank"].is_null());
        assert!(memory_literals(b"HINDSIGHT_API_KEY=$(read /tmp/token)").is_err());
        assert!(memory_literals(b"HINDSIGHT_BANK=one\nHINDSIGHT_BANK=two").is_err());
    }
    #[test]
    fn preview_has_no_destination_and_repeat_import_is_idempotent() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        let before = fs::read(source.join("control.json")).unwrap();
        let plan = ImportPlan::prepare(&source, &destination, false).unwrap();
        assert!(!destination.exists());
        assert!(!plan.preview.includes_credentials);
        let result = plan.apply().unwrap();
        assert!(result.changed);
        assert!(result.requires_cutover);
        assert_eq!(fs::metadata(&destination).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::read(source.join("control.json")).unwrap(), before);
        assert!(
            !ImportPlan::prepare(&source, &destination, false)
                .unwrap()
                .apply()
                .unwrap()
                .changed
        );
        fs::write(destination.join("operator.txt"), "new work").unwrap();
        assert!(
            ImportPlan::prepare(&source, &destination, false)
                .unwrap()
                .apply()
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(destination.join("operator.txt")).unwrap(),
            "new work"
        );
    }
    #[test]
    fn layouts_checkpoint_context_and_exact_legacy_metadata_survive() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        let mut layout = layouts::Layouts::default();
        layout.current.mode = layouts::ViewMode::Tiles;
        layout.save(&source).unwrap();
        let context = source.join("checkpoints/context.md");
        layouts::write_private(&context, b"Synthetic saved context").unwrap();
        Registry {
            checkpoints: vec![Checkpoint {
                name: "saved".into(),
                harness: "claude".into(),
                sid: "example-session".into(),
                cwd: t.0.to_str().unwrap().into(),
                profile: String::new(),
                origin: "fixture".into(),
                created_at: String::new(),
                context_file: context.to_str().unwrap().into(),
                source: None,
            }],
        }
        .save(&source)
        .unwrap();
        let jobs = json!({"active":{"42":"alias"},"meta":{"chat_bindings":{"42":{"terminal_id":"exact-id"}},"chat_pins":{"42":"alias"}},"jobs":{"ignored":{"command":"must not execute"}}});
        layouts::write_private(
            &source.join("jobs.json"),
            &serde_json::to_vec(&jobs).unwrap(),
        )
        .unwrap();
        ImportPlan::prepare(&source, &destination, false)
            .unwrap()
            .apply()
            .unwrap();
        assert_eq!(
            layouts::Layouts::load(&destination).unwrap().current.mode,
            layouts::ViewMode::Tiles
        );
        let imported = Registry::load(&destination).unwrap();
        let imported_path = Path::new(&imported.checkpoints[0].context_file);
        assert!(imported_path.starts_with(destination.join("checkpoints")));
        assert_eq!(fs::read(imported_path).unwrap(), b"Synthetic saved context");
        let saved: Value =
            serde_json::from_slice(&fs::read(destination.join("legacy-routes.json")).unwrap())
                .unwrap();
        assert_eq!(saved["meta"], jobs["meta"]);
        assert!(saved.get("jobs").is_none());
    }
    #[test]
    fn credentials_require_explicit_inclusion_and_are_never_in_preview() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        layouts::write_private(&source.join("telegram.env"),b"export TELEGRAM_BOT_TOKEN='123:fixture-only'\nOWNER_ID=42\nBOT_ID=123\nBOT_USERNAME=example_bot\nPRIVATE_SCRIPT=$(exit 1)\n").unwrap();
        let plan = ImportPlan::prepare(&source, &destination, false).unwrap();
        assert!(!plan.stage.0.join("telegram.env").exists());
        drop(plan);
        let plan = ImportPlan::prepare(&source, &destination, true).unwrap();
        assert!(
            !serde_json::to_string(&plan.preview)
                .unwrap()
                .contains("fixture-only")
        );
        plan.apply().unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("telegram.env")).unwrap(),
            "TELEGRAM_BOT_TOKEN=123:fixture-only\n"
        );
        let config =
            ControlConfig::parse(&fs::read(destination.join("control.json")).unwrap()).unwrap();
        assert_eq!(config.telegram.owner_id.as_deref(), Some("42"));
    }
    #[test]
    fn staging_collision_and_invalid_state_never_delete_existing_files() {
        let t = Temp::new();
        let collision = t.0.join("occupied");
        fs::create_dir(&collision).unwrap();
        fs::write(collision.join("keep"), "keep").unwrap();
        assert!(Stage::create(collision.clone()).is_err());
        assert!(collision.join("keep").exists());
        let source = t.source();
        let destination = t.0.join("new");
        fs::write(source.join("layouts.json"), b"invalid private contents").unwrap();
        let error = ImportPlan::prepare(&source, &destination, false)
            .err()
            .unwrap();
        assert!(!error.contains("private contents"));
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(&t.0).unwrap().count(), 2);
    }
    #[test]
    fn snapshot_retains_uncertain_delivery_and_cursor_including_wal() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        let database = rusqlite::Connection::open(source.join("delivery.sqlite3")).unwrap();
        database.execute_batch("PRAGMA journal_mode=WAL;PRAGMA wal_autocheckpoint=0;CREATE TABLE delivery(id INTEGER,state TEXT);INSERT INTO delivery VALUES(1,'unknown');CREATE TABLE cursors(offset INTEGER);INSERT INTO cursors VALUES(99);").unwrap();
        ImportPlan::prepare(&source, &destination, false)
            .unwrap()
            .apply()
            .unwrap();
        let copy = rusqlite::Connection::open(destination.join("delivery.sqlite3")).unwrap();
        assert_eq!(
            copy.query_row("SELECT state FROM delivery", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "unknown"
        );
        assert_eq!(
            copy.query_row("SELECT offset FROM cursors", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            99
        );
        drop(copy);
        assert!(
            !ImportPlan::prepare(&source, &destination, false)
                .unwrap()
                .apply()
                .unwrap()
                .changed
        );
    }
    #[test]
    fn native_goal_import_is_inert_and_repeatable_without_recovery_writes() {
        use crate::{
            app::hooks::Harness,
            core::{
                goal_store::{Scope, Store},
                identity::{Backend, TerminalId},
            },
        };
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        let scope = Scope {
            instance: "example".into(),
            terminal: TerminalId {
                backend: Backend::Tmux,
                endpoint: "/tmp/fixture.sock".into(),
                pane: "%1".into(),
                generation: "fixture".into(),
            },
            conversation: "session".into(),
            harness: Harness::Claude,
        };
        let mut store = Store::from_connection(
            rusqlite::Connection::open(source.join("goals.sqlite3")).unwrap(),
            0,
        )
        .unwrap();
        store.arm("goal", &scope, "Synthetic task", 0).unwrap();
        store
            .stop(
                "goal",
                &scope,
                "event",
                br#"{"session_id":"session","hook_event_name":"Stop"}"#,
                1,
                30000,
            )
            .unwrap();
        let expected = store.get("goal", &scope).unwrap();
        drop(store);
        ImportPlan::prepare(&source, &destination, false)
            .unwrap()
            .apply()
            .unwrap();
        let copy = Store::attach_connection(
            rusqlite::Connection::open(destination.join("goals.sqlite3")).unwrap(),
        )
        .unwrap();
        assert_eq!(copy.get("goal", &scope).unwrap(), expected);
        drop(copy);
        assert!(
            !ImportPlan::prepare(&source, &destination, false)
                .unwrap()
                .apply()
                .unwrap()
                .changed
        );
        assert!(!destination.join("hooks").exists());
    }
    #[test]
    fn env_never_evaluates_expressions_or_accepts_conflicting_ids() {
        assert!(telegram_env(b"TELEGRAM_BOT_TOKEN=$(cat /tmp/anything)").is_err());
        assert!(telegram_env(b"OWNER_ID=1\nOWNER_ID=2").is_err());
    }

    #[test]
    fn policy_and_routes_are_staged_without_granting_permissions() {
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        let mut config =
            ControlConfig::parse(&fs::read(source.join("control.json")).unwrap()).unwrap();
        config.telegram.bot_id = Some("123".into());
        layouts::write_private(&source.join("control.json"), &config.encode().unwrap()).unwrap();
        layouts::write_private(
            &source.join("jobs.json"),
            br#"{"active":{"42":"display-only"}}"#,
        )
        .unwrap();
        layouts::write_private(&source.join("allowed_ids.json"), br#"{"42":"operator"}"#).unwrap();
        layouts::write_private(&source.join("mention_only_chats.json"), b"[-100123]").unwrap();
        layouts::write_private(
            &source.join("telegram.env"),
            b"CONTROL_IDS='42, 43'\nFEED_ALL_GROUPS='-100123 -100456'\n",
        )
        .unwrap();
        ImportPlan::prepare(&source, &destination, false)
            .unwrap()
            .apply()
            .unwrap();
        let review: Value =
            serde_json::from_slice(&fs::read(destination.join("route-review.json")).unwrap())
                .unwrap();
        assert_eq!(review["routes"][0]["candidate"], Value::Null);
        let connection = rusqlite::Connection::open(destination.join("routes.sqlite3")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM route_grants", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let policy: Value =
            serde_json::from_slice(&fs::read(destination.join("legacy-allowed_ids.json")).unwrap())
                .unwrap();
        assert_eq!(policy["requires_review"], true);
        assert_eq!(policy["policy"]["42"], "operator");
        let env_policy: Value = serde_json::from_slice(
            &fs::read(destination.join("legacy-telegram-policy.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(env_policy["requires_review"], true);
        assert_eq!(env_policy["policy"]["CONTROL_IDS"], json!([42, 43]));
        assert_eq!(
            env_policy["policy"]["FEED_ALL_GROUPS"],
            json!([-100123, -100456])
        );
        drop(connection);
        assert!(
            !ImportPlan::prepare(&source, &destination, false)
                .unwrap()
                .apply()
                .unwrap()
                .changed
        );
    }

    #[test]
    fn refuses_shared_source_and_invalid_offset_without_changing_them() {
        use std::os::unix::fs::PermissionsExt;
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("new");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ImportPlan::prepare(&source, &destination, false).is_err());
        assert_eq!(fs::metadata(&source).unwrap().mode() & 0o777, 0o777);
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(source.join("tg_offset"), b"-1").unwrap();
        assert!(ImportPlan::prepare(&source, &destination, false).is_err());
        assert!(!destination.exists());
    }
    #[test]
    fn native_service_import_relocates_private_configuration_without_activation() {
        use super::super::telegram_config::ServiceConfig;
        let t = Temp::new();
        let source = t.source();
        let destination = t.0.join("native-new");
        let registration = source.join("hooks/fixture.json");
        let scope = json!({"instance":"example","terminal":{"backend":"herdr","endpoint":"example","pane":"pane","generation":"generation"},"conversation":"session","harness":"claude"});
        layouts::write_private(&registration,&serde_json::to_vec(&json!({
            "version":1,"id":"fixture","directory":source,"scope":scope,"goal_id":null,
            "harness_process":null,"transcript":source.join("transcript.jsonl"),"events":["Stop"],
            "options":{"telegram_delivery":true,"telegram_progress":false,"notifications":false,"hindsight":false},
            "activity_lease_ms":30000,"pending_timeout_ms":30000,"hook_timeout_ms":10000
        })).unwrap()).unwrap();
        let token = source.join("private-token");
        layouts::write_private(&token, b"TELEGRAM_BOT_TOKEN=123:synthetic_import_fixture\n")
            .unwrap();
        let service = json!({"version":1,"instance":"example","bot_id":123,"token_file":token,
            "grants":[{"chat":42,"actor":7,"permissions":["read","send"]}],"listen_chats":[],
            "registrations":[registration],"hook_bindings":[{"registration_id":"fixture","goal_scope":scope,"route_scope":{"instance":"example","bot":"123","chat":"42"},"actor":"7","route_revision":4,"reply_to":null,"hindsight_bank":null}]});
        let bytes = serde_json::to_vec(&service).unwrap();
        layouts::write_private(&source.join("telegram-service.json"), &bytes).unwrap();
        let preview = ImportPlan::prepare(&source, &destination, false).unwrap();
        assert!(!destination.exists());
        assert!(!preview.preview.includes_credentials);
        assert!(!preview.stage.0.join("telegram-token.txt").exists());
        let staged = ServiceConfig::load(&preview.stage.0.join("telegram-service.json")).unwrap();
        assert_eq!(staged.token_file, destination.join("telegram-token.txt"));
        assert_eq!(staged.grants.len(), 1);
        assert_eq!(staged.grants[0].actor, 7);
        drop(preview);
        let plan = ImportPlan::prepare(&source, &destination, true).unwrap();
        assert!(plan.preview.includes_credentials);
        assert!(
            !serde_json::to_string(&plan.preview)
                .unwrap()
                .contains("synthetic_import_fixture")
        );
        plan.apply().unwrap();
        let imported = ServiceConfig::load(&destination.join("telegram-service.json")).unwrap();
        assert_eq!(
            imported.read_token().unwrap(),
            "123:synthetic_import_fixture"
        );
        let hooks = imported.check_registrations().unwrap();
        assert_eq!(hooks[0].0, destination.join("hooks/fixture.json"));
        assert_eq!(hooks[0].1.directory, destination);
        assert_eq!(
            hooks[0].1.transcript,
            Some(destination.join("transcript.jsonl"))
        );
        assert!(!destination.join("transcript.jsonl").exists());
        assert!(!destination.join(".telegram-service.lock").exists());
        assert!(!destination.join("telegram-controls.sqlite3").exists());
        assert_eq!(
            fs::read(source.join("telegram-service.json")).unwrap(),
            bytes
        );
        assert!(
            !ImportPlan::prepare(&source, &destination, true)
                .unwrap()
                .apply()
                .unwrap()
                .changed
        );
    }
}
