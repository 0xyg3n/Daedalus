//! Service coordination. The journal owner never waits for Telegram long polling.
//! A worker cannot acknowledge another batch until the owner durably admits it.
use super::telegram::{Engine, Outbound, Result};
use crate::adapters::telegram::{Api, Error};
use crate::core::journal::Delivery;
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

type Batch = std::result::Result<Vec<Value>, Error>;

/// One outbound HTTP request at a time, independent of polling and feeder work.
/// The owner keeps the lease; the worker has no database or retry authority.
pub struct Sender {
    request: Option<SyncSender<Box<Delivery>>>,
    response: Receiver<std::result::Result<Value, Error>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    pending: Option<Box<Delivery>>,
    after: String,
    retry_at: f64,
}
impl Sender {
    pub fn spawn(mut api: impl Api + Send + 'static) -> Result<Self> {
        let (request, requests) = mpsc::sync_channel::<Box<Delivery>>(1);
        let (responses, response) = mpsc::sync_channel(1);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = thread::Builder::new()
            .name("telegram-send".into())
            .spawn(move || {
                while let Ok(row) = requests.recv() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let result = super::telegram::transmit(&mut api, &row);
                    if responses.send(result).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| "Cannot start Telegram delivery worker")?;
        Ok(Self {
            request: Some(request),
            response,
            stopped,
            worker: Some(worker),
            pending: None,
            after: String::new(),
            retry_at: 0.0,
        })
    }
    pub fn tick(&mut self, engine: &mut Engine, bot: i64, now: f64) -> Result<()> {
        if !now.is_finite() || now < 0.0 {
            return Err("Invalid service timestamp".into());
        }
        if let Some(row) = self.pending.as_ref() {
            match self.response.try_recv() {
                Ok(result) => {
                    if let Err(Error::RateLimited(delay)) = &result {
                        self.retry_at = now + (*delay).max(1) as f64;
                    }
                    engine.complete_send(row, result, now)?;
                    self.pending = None;
                }
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    return Err(
                        "Telegram delivery worker stopped; submission may be uncertain".into(),
                    );
                }
            }
        }
        if now < self.retry_at || self.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        let destinations = engine
            .journal
            .destinations("outbound", &self.after, 32)
            .map_err(|e| e.to_string())?;
        if destinations.is_empty() {
            self.after.clear();
            return Ok(());
        }
        let prefix = format!("telegram:{bot}:");
        for destination in destinations {
            self.after = destination.clone();
            let Some(chat) = destination
                .strip_prefix(&prefix)
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|id| *id != 0)
            else {
                continue;
            };
            if let Outbound::Ready(row) = engine.claim_send(chat, now)? {
                self.request
                    .as_ref()
                    .ok_or("Telegram delivery worker stopped")?
                    .try_send(row.clone())
                    .map_err(|_| "Telegram delivery worker unavailable")?;
                self.pending = Some(row);
                break;
            }
        }
        Ok(())
    }
    pub fn shutdown(mut self, engine: &mut Engine, clock: impl Fn() -> f64) -> Result<()> {
        self.stopped.store(true, Ordering::Release);
        self.request.take();
        self.worker
            .take()
            .ok_or("Delivery worker missing")?
            .join()
            .map_err(|_| "Telegram delivery worker failed")?;
        if let Some(row) = self.pending.take() {
            // Without evidence retain intent for reconciliation, never guess
            // that an interrupted request was not sent.
            if let Ok(result) = self.response.try_recv() {
                engine.complete_send(&row, result, clock())?;
            }
        }
        Ok(())
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.request.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub struct Poller {
    request: Option<SyncSender<i64>>,
    response: Receiver<Batch>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    pending: bool,
    retry_at: f64,
}

impl Poller {
    /// Construct only after verifying the transport's bot identity. Production
    /// transports must bound every request (BotApi uses a 60-second deadline).
    pub fn spawn(mut api: impl Api + Send + 'static) -> Result<Self> {
        let (request, requests) = mpsc::sync_channel(1);
        let (responses, response) = mpsc::sync_channel(1);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let worker = thread::Builder::new()
            .name("telegram-poll".into())
            .spawn(move || {
                while let Ok(offset) = requests.recv() {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let result = api.updates(offset, 50);
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    // Only one outstanding request; the channel cannot accumulate
                    // updates while storage is unavailable or the owner is busy.
                    if responses.send(result).is_err() {
                        break;
                    }
                }
            })
            .map_err(|_| "Cannot start Telegram polling worker")?;
        Ok(Self {
            request: Some(request),
            response,
            stopped,
            worker: Some(worker),
            pending: false,
            retry_at: 0.0,
        })
    }

    /// Returns without waiting on HTTP. Admission failure stops scheduling;
    /// callers must treat it as a service fault, not advance the polling offset.
    pub fn tick(&mut self, engine: &mut Engine, now: f64) -> Result<usize> {
        if !now.is_finite() || now < 0.0 {
            return Err("Invalid service timestamp".into());
        }
        let mut admitted = 0;
        if self.pending {
            match self.response.try_recv() {
                Ok(Ok(batch)) => {
                    // Keep pending=true if admission fails: no next request can
                    // accidentally acknowledge a partially persisted batch.
                    admitted = engine.admit_updates(batch, now)?;
                    self.pending = false;
                }
                Ok(Err(error)) => {
                    self.pending = false;
                    self.retry_at = now
                        + match error {
                            Error::RateLimited(delay) => delay.max(1) as f64,
                            Error::NotSent | Error::Uncertain => 5.0,
                            Error::Rejected(code) => {
                                self.stopped.store(true, Ordering::Release);
                                return Err(match code {
                                    409 => "Telegram polling conflict (409): another poller or webhook owns this bot. Stop the competing receiver before restarting Daedalus.".into(),
                                    401 => "Telegram token rejected (401): replace the bot token in Telegram settings.".into(),
                                    _ => format!("Telegram polling rejected ({code}); check the bot configuration."),
                                });
                            }
                            Error::Invalid(_) => {
                                self.stopped.store(true, Ordering::Release);
                                return Err("Telegram polling response was invalid; no updates were acknowledged.".into());
                            }
                        };
                }
                Err(TryRecvError::Empty) => return Ok(0),
                Err(TryRecvError::Disconnected) => {
                    return Err("Telegram polling worker stopped".into());
                }
            }
        }
        if !self.pending && now >= self.retry_at && !self.stopped.load(Ordering::Acquire) {
            self.request
                .as_ref()
                .ok_or("Telegram polling worker stopped")?
                .try_send(engine.cursor()?)
                .map_err(|_| "Telegram polling worker unavailable")?;
            self.pending = true;
        }
        Ok(admitted)
    }

    /// Waits for the current bounded HTTP request, without starting another.
    /// Never drop a detached poller and launch a second owner for the same bot.
    pub fn shutdown(mut self) -> Result<()> {
        self.stop();
        self.worker
            .take()
            .ok_or("Polling worker missing")?
            .join()
            .map_err(|_| "Telegram polling worker failed".to_owned())
    }
    fn stop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.request.take();
    }
}
impl Drop for Poller {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn clock() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Stable lock files are never unlinked: replacing a lock inode would allow a
/// second owner while the first still holds its descriptor.
pub struct Owner {
    _bot: OwnerLock,
    _instance: OwnerLock,
}
struct OwnerLock(std::fs::File);
#[cfg(unix)]
impl Drop for OwnerLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // A concurrent fork may retain a descriptor until exec even with
        // CLOEXEC. Explicitly release our ownership on this open description.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
#[cfg(unix)]
fn lock(path: &std::path::Path) -> Result<OwnerLock> {
    use std::os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    };
    crate::core::layouts::check_path(path)?;
    let parent = path.parent().ok_or("Missing service lock directory")?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|_| "Cannot create service lock directory")?;
    let meta = std::fs::metadata(parent).map_err(|_| "Cannot inspect service lock directory")?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("Service lock directory must be private and owned".into());
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "Cannot open service owner lock")?;
    let meta = file
        .metadata()
        .map_err(|_| "Cannot inspect service owner lock")?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err("Service owner lock is unsafe".into());
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Another service already owns this bot or instance".into());
    }
    Ok(OwnerLock(file))
}
impl Owner {
    pub fn acquire(
        directory: &std::path::Path,
        global: &std::path::Path,
        bot: i64,
    ) -> Result<Self> {
        if bot <= 0 {
            return Err("Invalid service bot identity".into());
        }
        let bot = lock(&global.join(format!("telegram-{bot}.lock")))?;
        let instance = lock(&directory.join(".telegram-service.lock"))?;
        Ok(Self {
            _bot: bot,
            _instance: instance,
        })
    }
}

fn apply_grants(engine: &mut Engine, config: &super::telegram_config::ServiceConfig) -> Result<()> {
    let grants = config
        .grants
        .iter()
        .map(|g| (g.chat.to_string(), g.actor.to_string(), g.permissions()))
        .collect::<Vec<_>>();
    engine
        .routes
        .replace_grants(&config.instance, &config.bot_id.to_string(), &grants)
        .map_err(|e| e.to_string())?;
    engine.listen_chats = config.listen_chats.clone();
    Ok(())
}

/// Reload only explicit local policy. Transport ownership and credentials remain
/// pinned until restart. No route, recipient, or grant is inferred from a pane.
fn refresh_policy(
    current: &super::telegram_config::ServiceConfig,
    next: &super::telegram_config::ServiceConfig,
    directory: &std::path::Path,
    engine: &mut Engine,
    feeder: &mut super::telegram_feeder::NativeFeeder,
) -> Result<Vec<(std::path::PathBuf, super::hook_runtime::Registration)>> {
    next.validate()?;
    if current.version != next.version
        || current.instance != next.instance
        || current.bot_id != next.bot_id
        || current.token_file != next.token_file
    {
        return Err(
            "Service transport identity changed; restart with the reviewed configuration".into(),
        );
    }
    let registrations = next.check_registrations()?;
    if registrations.iter().any(|(_, r)| r.directory != directory) {
        return Err("Hook registrations must use this instance's state directory".into());
    }
    // Validate the complete policy before replacing any grants. The loop does not
    // dispatch work between these assignments, and an error stops the service.
    engine.configure_hooks(
        registrations.iter().map(|(_, r)| r.clone()).collect(),
        &next.hook_bindings,
    )?;
    if serde_json::to_value(&current.grants).map_err(|_| "Invalid grants")?
        != serde_json::to_value(&next.grants).map_err(|_| "Invalid grants")?
        || current.listen_chats != next.listen_chats
    {
        apply_grants(engine, next)?;
    }
    feeder.registrations = next.registrations.clone();
    Ok(registrations)
}

#[derive(Default)]
struct Cursors {
    inbound: String,
    hooks: i64,
}
impl Cursors {
    fn hooks(
        &mut self,
        engine: &mut Engine,
        config: &super::telegram_config::ServiceConfig,
        registrations: &[(std::path::PathBuf, super::hook_runtime::Registration)],
        now: f64,
    ) -> Result<()> {
        let events = engine
            .journal
            .events_page("hooks", &config.instance, &["admitted"], self.hooks, 32)
            .map_err(|e| e.to_string())?;
        if events.is_empty() {
            self.hooks = 0;
            return Ok(());
        }
        for event in events {
            self.hooks = event.id;
            let Some((_, registration)) = registrations
                .iter()
                .find(|(_, r)| event.payload["registration_id"].as_str() == Some(r.id.as_str()))
            else {
                continue;
            };
            let bindings = config
                .hook_bindings
                .iter()
                .filter(|b| b.registration_id == registration.id)
                .cloned()
                .collect::<Vec<_>>();
            if bindings.is_empty() {
                continue;
            }
            // A revoked binding retains the event for explicit operator review.
            // The expander never dispatches transport or infers another recipient.
            let _ = super::hook_effects::expand_many(
                &mut engine.journal,
                &engine.routes,
                registration,
                &bindings,
                &event,
                now,
            );
        }
        Ok(())
    }
    fn feed(
        &mut self,
        engine: &mut Engine,
        feeder: &mut impl super::telegram::Feeder,
    ) -> Result<()> {
        let destinations = engine
            .journal
            .destinations("inbound", &self.inbound, 8)
            .map_err(|e| e.to_string())?;
        if destinations.is_empty() {
            self.inbound.clear();
            return Ok(());
        }
        for destination in destinations {
            self.inbound = destination.clone();
            let target = serde_json::from_str(&destination)
                .map_err(|_| "Invalid saved feeder destination")?;
            engine.feed_one(feeder, &target, clock)?;
        }
        Ok(())
    }
}

static STOP: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Release);
}

/// Explicit foreground service entrypoint. No caller starts this as a side
/// effect of setup, diagnostics or a TUI read operation.
/// Only getMe is retried here: it is read-only and cannot duplicate a send.
/// Ownership remains held and the service stays Starting until identity matches.
fn start_engine(
    engine: &mut Engine,
    api: &mut impl Api,
    mut wait: impl FnMut(std::time::Duration) -> Result<bool>,
) -> Result<bool> {
    use std::time::Duration;
    if !wait(Duration::ZERO)? {
        return Ok(false);
    }
    let mut backoff = 1u64;
    loop {
        match api.get_me() {
            Ok(identity) => {
                engine.start_verified(identity)?;
                return Ok(true);
            }
            Err(error) => {
                let delay = match error {
                    Error::NotSent | Error::Uncertain | Error::Rejected(500..=599) => backoff,
                    Error::RateLimited(delay) => delay.max(1),
                    error => return Err(error.to_string()),
                };
                backoff = backoff.saturating_mul(2).min(60);
                eprintln!(
                    "Telegram identity verification temporarily unavailable; retrying in {delay}s"
                );
                if !wait(Duration::from_secs(delay))? {
                    return Ok(false);
                }
            }
        }
    }
}

pub fn run(args: &[String], out: &mut impl std::io::Write) -> Result<()> {
    use super::{
        config::ControlConfig,
        telegram_commands::{CallbackStore, Dispatcher, NativeOperations},
        telegram_config::{self, ServiceConfig},
        telegram_feeder::NativeFeeder,
    };
    use crate::{
        adapters::{telegram::BotApi, terminals::Terminals},
        core::{journal::Journal, layouts, routes::Registry},
    };
    if args.first().is_some_and(|a| {
        matches!(
            a.as_str(),
            "status" | "discover" | "start" | "stop" | "bindings" | "bind"
        )
    }) {
        return super::telegram_cli::run(args, out);
    }
    if args.first().is_some_and(|a| a == "config") {
        return telegram_config::run(&args[1..], out);
    }
    if args.first().is_some_and(|a| a == "cutover") {
        return super::cutover::run(&args[1..], out);
    }
    if args.is_empty() || args == ["--help"] || args == ["help"] {
        writeln!(out,"daedalus telegram status|discover|start|stop --config-dir ABS [--apply --approve DIGEST]\ndaedalus telegram bindings|bind --config-dir ABS --registration ABS [options]\ndaedalus telegram serve --config-dir ABS\ndaedalus telegram config check|init|import --file ABS [options]\nDiscover saves private chat names without granting access; it requires the managed service stopped. serve is an explicit foreground service; setup never starts it.").map_err(|_|"Cannot write service help")?;
        return Ok(());
    }
    if args.len() != 3 || args[0] != "serve" || args[1] != "--config-dir" {
        return Err("Use telegram serve --config-dir ABS or telegram config --help".into());
    }
    let directory = std::path::PathBuf::from(&args[2]);
    layouts::check_path(&directory)?;
    super::cutover::activation_check(&directory)?;
    let path = directory.join("telegram-service.json");
    let mut config = ServiceConfig::load(&path)?;
    let mut registrations = config.check_registrations()?;
    if registrations.iter().any(|(_, r)| r.directory != directory) {
        return Err("Hook registrations must use this instance's state directory".into());
    }
    let control = ControlConfig::parse(
        &layouts::read_private(&directory.join("control.json"))?
            .ok_or("Run setup before starting Telegram")?,
    )?;
    let token = config.read_token()?;
    let global = global_directory()?;
    let owner = Owner::acquire(&directory, &global, config.bot_id)?;
    // Revalidate under ownership: a concurrent cutover may have retired this
    // source after the early read-only check.
    super::cutover::activation_check(&directory)?;
    #[cfg(windows)]
    let running_image = super::telegram_supervisor::capture_running_image()?;
    #[cfg(windows)]
    if let Some(executable) = super::telegram_supervisor::replacement_for(&running_image)? {
        // Drop ownership before the installed image starts, or it cannot lock.
        drop(owner);
        super::telegram_supervisor::spawn_installed_service(&directory, &executable)?;
        writeln!(out, "{{\"service\":\"telegram\",\"state\":\"stopped\"}}")
            .map_err(|_| "Cannot write service state")?;
        return Ok(());
    }
    super::telegram_supervisor::record(
        &directory,
        &config,
        super::telegram_supervisor::Phase::Starting,
    )?;
    let journal = Journal::open(&directory.join("delivery.sqlite3")).map_err(|e| e.to_string())?;
    let routes = Registry::from_connection(telegram_config::private_connection(
        &directory.join("routes.sqlite3"),
    )?)
    .map_err(|e| e.to_string())?;
    let mut engine = Engine::new(journal, routes, &config.instance, config.bot_id)?;
    engine.configure_hooks(
        registrations.iter().map(|(_, r)| r.clone()).collect(),
        &config.hook_bindings,
    )?;
    apply_grants(&mut engine, &config)?;
    let mut api = BotApi::new(&token, "https://api.telegram.org").map_err(|e| e.to_string())?;
    // Persist before the first network request, so rollback can never replay
    // an imported cursor after this destination may have contacted Telegram.
    super::cutover::mark_activation(&directory)?;
    engine.streams = Some(super::telegram_stream::Store::open(&directory)?);
    STOP.store(false, Ordering::Release);
    let _signals = ServiceSignals::install()?;
    let started = start_engine(&mut engine, &mut api, |delay| {
        let began = std::time::Instant::now();
        loop {
            if STOP.load(Ordering::Acquire)
                || super::telegram_supervisor::stop_requested(&directory)?
            {
                return Ok(false);
            }
            let remaining = delay.saturating_sub(began.elapsed());
            if remaining.is_zero() {
                return Ok(true);
            }
            thread::sleep(remaining.min(std::time::Duration::from_millis(100)));
        }
    })?;
    if !started {
        super::telegram_supervisor::record(
            &directory,
            &config,
            super::telegram_supervisor::Phase::Stopped,
        )?;
        return Ok(());
    }
    let mut dispatcher = Dispatcher {
        callbacks: CallbackStore::from_connection(telegram_config::private_connection(
            &directory.join("telegram-controls.sqlite3"),
        )?)?,
    };
    let mut operations = NativeOperations {
        instance: config.instance.clone(),
        directory: directory.clone(),
        config: control,
        terminals: Terminals::default(),
        environment: crate::platform::environment(),
    };
    let mut feeder = NativeFeeder {
        instance: config.instance.clone(),
        registrations: config.registrations.clone(),
        terminals: Terminals::default(),
    };
    let mut files = super::telegram::PrivateFiles {
        directory: directory.join("attachments"),
    };
    let mut poller =
        Poller::spawn(BotApi::new(&token, "https://api.telegram.org").map_err(|e| e.to_string())?)?;
    let mut sender =
        Sender::spawn(BotApi::new(&token, "https://api.telegram.org").map_err(|e| e.to_string())?)?;
    let mut feedback = super::telegram_feedback::Feedback::spawn(
        BotApi::feedback(&token).map_err(|e| e.to_string())?,
    )?;
    feedback.register_commands();
    drop(token);
    let mut cursors = Cursors::default();
    let mut progress = super::hook_dispatch::Progress::default();
    let mut checked = 0.0;
    #[cfg(windows)]
    let mut handoff: Option<std::path::PathBuf> = None;
    let result = (|| -> Result<()> {
        super::telegram_supervisor::record(
            &directory,
            &config,
            super::telegram_supervisor::Phase::Running,
        )?;
        writeln!(out,"{{\"service\":\"telegram\",\"state\":\"running\",\"transport_identity_verified\":true}}").map_err(|_|"Cannot write service state")?;
        out.flush().map_err(|_| "Cannot flush service state")?;
        while !STOP.load(Ordering::Acquire) {
            if super::telegram_supervisor::stop_requested(&directory)? {
                break;
            }
            let now = clock();
            if now - checked >= 1.0 {
                #[cfg(windows)]
                if let Some(executable) =
                    super::telegram_supervisor::replacement_for(&running_image)?
                {
                    handoff = Some(executable);
                    break;
                }
                let next = ServiceConfig::load(&path)?;
                registrations =
                    refresh_policy(&config, &next, &directory, &mut engine, &mut feeder)?;
                let updated_control = ControlConfig::parse(
                    &layouts::read_private(&directory.join("control.json"))?
                        .ok_or("Instance configuration is missing")?,
                )?;
                if updated_control.agent_name != operations.config.agent_name {
                    return Err(
                        "Instance identity changed; restart with the reviewed configuration".into(),
                    );
                }
                operations.config = updated_control;
                config = next;
                checked = now;
            }
            // Durable queued input is independent of Telegram connectivity.
            // Drain it before scheduling network work or processing new updates.
            cursors.feed(&mut engine, &mut feeder)?;
            poller.tick(&mut engine, now)?;
            engine.stage_controls(&mut dispatcher, &mut operations, &mut api, clock())?;
            engine.stage(&mut api, &mut feeder, &mut files, clock())?;
            if let Some(streams) = &mut engine.streams {
                streams.tick(
                    &mut engine.journal,
                    &engine.routes,
                    &config,
                    &registrations,
                    clock(),
                )?;
            }
            feedback.tick(&mut engine, &config, &registrations, clock());
            sender.tick(&mut engine, config.bot_id, clock())?;
            cursors.hooks(&mut engine, &config, &registrations, clock())?;
            progress.tick(
                &mut engine.journal,
                &engine.routes,
                &config,
                &registrations,
                clock(),
            )?;
            thread::sleep(std::time::Duration::from_millis(100));
        }
        Ok(())
    })();
    // Locks remain held until both network workers have stopped. Never overlap
    // a replacement poller merely because observing the old one timed out.
    let stopping_result = super::telegram_supervisor::record(
        &directory,
        &config,
        super::telegram_supervisor::Phase::Stopping,
    );
    feedback.shutdown();
    let send_result = sender.shutdown(&mut engine, clock);
    let poll_result = poller.shutdown();
    let stopped_result = super::telegram_supervisor::record(
        &directory,
        &config,
        super::telegram_supervisor::Phase::Stopped,
    );
    #[cfg_attr(not(windows), allow(unused_variables))]
    let restart = result.is_ok()
        && poll_result.is_ok()
        && send_result.is_ok()
        && stopping_result.is_ok()
        && stopped_result.is_ok();
    // Release the image and the owner lock before any replacement process starts.
    drop(owner);
    #[cfg(windows)]
    if restart {
        if let Some(executable) = handoff {
            super::telegram_supervisor::spawn_installed_service(&directory, &executable)?;
        }
    }
    result?;
    poll_result?;
    send_result?;
    stopping_result?;
    stopped_result?;
    writeln!(out, "{{\"service\":\"telegram\",\"state\":\"stopped\"}}")
        .map_err(|_| "Cannot write service state")?;
    Ok(())
}

#[cfg(windows)]
fn wide_path(path: &std::path::Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Path contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}
/// Owner mutex for one service lock file.
/// Share mode omits `FILE_SHARE_DELETE`: with delete sharing, a second process
/// can replace the directory entry and lock the new file while this handle
/// still "owns" the deleted one. The byte lock is one byte, not `u32::MAX` —
/// a 2^64-1 range overflows and Windows does not treat it as exclusive.
#[cfg(windows)]
fn open_service_lock(path: &std::path::Path, create: bool) -> std::io::Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
        },
    };
    if create && !path.exists() {
        // Seed the owner-only ACL, then reopen without delete sharing.
        drop(crate::platform::open_private(path, true, true)?);
    }
    let handle = unsafe {
        CreateFileW(
            wide_path(path)?.as_ptr(),
            GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let file = unsafe { std::fs::File::from_raw_handle(handle) };
    crate::platform::validate_private_file(&file)?;
    Ok(file)
}
#[cfg(windows)]
fn lock_owner_byte(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx},
        System::IO::OVERLAPPED,
    };
    unsafe {
        let mut overlap: OVERLAPPED = std::mem::zeroed();
        if LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlap,
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}
#[cfg(windows)]
fn unlock_owner_byte(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{Storage::FileSystem::UnlockFileEx, System::IO::OVERLAPPED};
    unsafe {
        let mut overlap: OVERLAPPED = std::mem::zeroed();
        if UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlap) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}
/// `Ok(true)` when another owner holds this lock. Does not create the file.
#[cfg(windows)]
pub(crate) fn probe_owner(path: &std::path::Path) -> std::io::Result<bool> {
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};
    let file = match open_service_lock(path, false) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => {
            return Ok(true);
        }
        Err(error) => return Err(error),
    };
    match lock_owner_byte(&file) {
        Ok(()) => {
            unlock_owner_byte(&file)?;
            Ok(false)
        }
        Err(error) if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) => Ok(true),
        Err(error) => Err(error),
    }
}
#[cfg(windows)]
impl Drop for OwnerLock {
    fn drop(&mut self) {
        // Release the byte even if a duplicated handle outlives this guard.
        // Never delete the lock file; a replacement inode admits a second owner.
        let _ = unlock_owner_byte(&self.0);
    }
}
#[cfg(windows)]
fn lock(path: &std::path::Path) -> Result<OwnerLock> {
    use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;
    crate::core::layouts::check_path(path)?;
    let parent = path.parent().ok_or("Missing service lock directory")?;
    crate::platform::ensure_private_dir(parent)
        .map_err(|_| "Cannot create private service lock directory")?;
    crate::platform::validate_private_dir(parent)
        .map_err(|_| "Service lock directory is not private and owned")?;
    let owned = |error: std::io::Error| -> String {
        if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
            || error.raw_os_error() == Some(33)
        {
            "Another service already owns this bot or instance".into()
        } else {
            "Cannot open service owner lock".into()
        }
    };
    let file = open_service_lock(path, true).map_err(owned)?;
    lock_owner_byte(&file).map_err(|_| "Another service already owns this bot or instance")?;
    Ok(OwnerLock(file))
}
#[cfg(unix)]
struct ServiceSignals {
    interrupt: libc::sighandler_t,
    terminate: libc::sighandler_t,
}
#[cfg(unix)]
impl ServiceSignals {
    fn install() -> Result<Self> {
        let interrupt =
            unsafe { libc::signal(libc::SIGINT, stop_signal as *const () as libc::sighandler_t) };
        if interrupt == libc::SIG_ERR {
            return Err("Cannot install service interrupt handler".into());
        }
        let terminate = unsafe {
            libc::signal(
                libc::SIGTERM,
                stop_signal as *const () as libc::sighandler_t,
            )
        };
        if terminate == libc::SIG_ERR {
            unsafe {
                libc::signal(libc::SIGINT, interrupt);
            }
            return Err("Cannot install service termination handler".into());
        }
        Ok(Self {
            interrupt,
            terminate,
        })
    }
}
#[cfg(unix)]
impl Drop for ServiceSignals {
    fn drop(&mut self) {
        unsafe {
            libc::signal(libc::SIGINT, self.interrupt);
            libc::signal(libc::SIGTERM, self.terminate);
        }
    }
}
#[cfg(windows)]
unsafe extern "system" fn console_stop(event: u32) -> windows_sys::core::BOOL {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };
    // Logoff and shutdown are how Windows updates ask a detached console
    // process to leave. Cooperative stop only; this handler never terminates.
    if matches!(
        event,
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
    ) {
        STOP.store(true, Ordering::Release);
        1
    } else {
        0
    }
}
#[cfg(windows)]
struct ServiceSignals(bool);
#[cfg(windows)]
impl ServiceSignals {
    fn install() -> Result<Self> {
        // A detached service has no console code page, but shutdown events are
        // still delivered when a handler is registered. Failure there is not
        // fatal: the private stop file remains the installer's signal.
        if unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(console_stop), 1)
        } == 0
        {
            if unsafe { windows_sys::Win32::System::Console::GetConsoleCP() } == 0 {
                return Ok(Self(false));
            }
            return Err("Cannot install service console handler".into());
        }
        Ok(Self(true))
    }
}
#[cfg(windows)]
impl Drop for ServiceSignals {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(console_stop), 0);
            }
        }
    }
}

pub(crate) fn global_directory() -> Result<std::path::PathBuf> {
    #[cfg(unix)]
    {
        Ok(std::path::PathBuf::from(format!(
            "/tmp/daedalus-service-{}",
            unsafe { libc::geteuid() }
        )))
    }
    #[cfg(windows)]
    {
        Ok(crate::platform::user_local_data_directory()
            .map_err(|_| "Cannot resolve authoritative user data directory")?
            .join("Daedalus")
            .join("service-owners"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{journal::Journal, routes::Registry};
    use serde_json::json;
    use std::time::{Duration, Instant};
    struct SendMock {
        started: SyncSender<()>,
        complete: Receiver<std::result::Result<Value, Error>>,
    }
    impl Api for SendMock {
        fn get_me(&mut self) -> std::result::Result<Value, Error> {
            Ok(json!({"id":123,"is_bot":true,"username":"example_bot"}))
        }
        fn updates(&mut self, _: i64, _: u64) -> Batch {
            unreachable!()
        }
        fn send_text(
            &mut self,
            _: i64,
            _: &str,
            _: Option<i64>,
        ) -> std::result::Result<Value, Error> {
            self.started.send(()).unwrap();
            self.complete
                .recv_timeout(Duration::from_secs(2))
                .unwrap_or(Err(Error::Uncertain))
        }
        fn download(&mut self, _: &str) -> std::result::Result<Vec<u8>, Error> {
            unreachable!()
        }
        fn send_document(
            &mut self,
            _: i64,
            _: &str,
            _: Vec<u8>,
            _: &str,
        ) -> std::result::Result<Value, Error> {
            unreachable!()
        }
    }
    #[test]
    fn reviewed_registration_reload_adds_and_removes_only_explicit_paths() {
        use super::super::{
            hook_runtime::{Options, Registration},
            hooks::{EventKind, Harness},
            telegram_config::ServiceConfig,
            telegram_feeder::NativeFeeder,
        };
        use crate::{
            adapters::terminals::Terminals,
            core::{
                goal_store,
                identity::{Backend, TerminalId},
                layouts,
            },
        };
        let directory = std::env::temp_dir().join(format!(
            "daedalus-reload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let registration = Registration {
            version: 1,
            id: "fixture".into(),
            directory: directory.clone(),
            scope: goal_store::Scope {
                instance: "example".into(),
                terminal: TerminalId {
                    backend: Backend::Tmux,
                    endpoint: crate::test_support::path("/tmp/mock.sock").into(),
                    pane: "%1".into(),
                    generation: "one".into(),
                },
                harness: Harness::Claude,
                conversation: "session".into(),
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
        let config = ServiceConfig {
            version: 1,
            instance: "example".into(),
            bot_id: 123,
            token_file: directory.join("token"),
            grants: vec![],
            listen_chats: Default::default(),
            registrations: vec![],
            hook_bindings: vec![],
        };
        let mut next = config.clone();
        next.registrations.push(registration.path());
        let mut engine = Engine::new(
            Journal::memory().unwrap(),
            Registry::memory().unwrap(),
            "example",
            123,
        )
        .unwrap();
        let mut feeder = NativeFeeder {
            instance: "example".into(),
            registrations: vec![],
            terminals: Terminals::default(),
        };
        assert_eq!(
            refresh_policy(&config, &next, &directory, &mut engine, &mut feeder)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(feeder.registrations, vec![registration.path()]);
        assert!(
            engine
                .routes
                .inspect(
                    &crate::core::routes::Scope {
                        instance: "example".into(),
                        bot: "123".into(),
                        chat: "42".into()
                    },
                    "42"
                )
                .is_err()
        );
        assert!(
            refresh_policy(&next, &config, &directory, &mut engine, &mut feeder)
                .unwrap()
                .is_empty()
        );
        assert!(feeder.registrations.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn reviewed_policy_reload_updates_grants_and_rejects_identity_changes() {
        use super::super::telegram_config::{Access, Grant, ServiceConfig};
        use crate::{
            adapters::terminals::Terminals,
            core::routes::{Permission, Scope},
        };
        let config = ServiceConfig {
            version: 1,
            instance: "example".into(),
            bot_id: 123,
            token_file: crate::test_support::path("/tmp/daedalus-fixture-token").into(),
            grants: vec![],
            listen_chats: Default::default(),
            registrations: vec![],
            hook_bindings: vec![],
        };
        let mut next = config.clone();
        next.grants.push(Grant {
            chat: 42,
            actor: 7,
            permissions: vec![Access::Read, Access::Send],
        });
        let mut engine = Engine::new(
            Journal::memory().unwrap(),
            Registry::memory().unwrap(),
            "example",
            123,
        )
        .unwrap();
        let mut feeder = super::super::telegram_feeder::NativeFeeder {
            instance: "example".into(),
            registrations: vec![],
            terminals: Terminals::default(),
        };
        let directory =
            std::path::Path::new(crate::test_support::path("/tmp/daedalus-fixture-instance"));
        let scope = Scope {
            instance: "example".into(),
            bot: "123".into(),
            chat: "42".into(),
        };
        refresh_policy(&config, &next, directory, &mut engine, &mut feeder).unwrap();
        assert!(
            engine
                .routes
                .authorize(&scope, "7", Permission::Send)
                .is_ok()
        );
        let mut foreign = next.clone();
        foreign.bot_id = 124;
        foreign.grants.clear();
        assert!(refresh_policy(&next, &foreign, directory, &mut engine, &mut feeder).is_err());
        assert!(
            engine
                .routes
                .authorize(&scope, "7", Permission::Send)
                .is_ok()
        );
        let mut invalid = next.clone();
        invalid.registrations.push(directory.join("missing.json"));
        invalid.grants.clear();
        assert!(refresh_policy(&next, &invalid, directory, &mut engine, &mut feeder).is_err());
        assert!(
            engine
                .routes
                .authorize(&scope, "7", Permission::Send)
                .is_ok()
        );
        refresh_policy(&next, &config, directory, &mut engine, &mut feeder).unwrap();
        assert!(
            engine
                .routes
                .authorize(&scope, "7", Permission::Send)
                .is_err()
        );
    }
    #[test]
    fn imported_instance_cannot_poll_before_cutover() {
        let root = std::env::temp_dir().join(format!(
            "daedalus-cutover-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        crate::core::layouts::write_private(
            &root.join("migration.json"),
            br#"{"version":1,"activation":"requires_cutover"}"#,
        )
        .unwrap();
        let mut output = Vec::new();
        let error = run(
            &[
                "serve".into(),
                "--config-dir".into(),
                root.to_string_lossy().into(),
            ],
            &mut output,
        )
        .unwrap_err();
        assert_eq!(error, "Cutover state missing");
        assert!(output.is_empty());
        assert!(!root.join(".telegram-service.lock").exists());
        assert!(!root.join("delivery.sqlite3").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn sender_keeps_owner_responsive_and_records_late_receipt() {
        let (started, observed) = mpsc::sync_channel(1);
        let (complete, response) = mpsc::sync_channel(1);
        let mut api = SendMock {
            started,
            complete: response,
        };
        let mut engine = Engine::new(
            Journal::memory().unwrap(),
            Registry::memory().unwrap(),
            "fixture",
            123,
        )
        .unwrap();
        engine.start(&mut api).unwrap();
        let mut sender = Sender::spawn(api).unwrap();
        engine
            .enqueue_text(42, "first", None, "first", 0.0)
            .unwrap();
        sender.tick(&mut engine, 123, 1.0).unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        // The worker is inside send_text; state admission remains available.
        engine
            .admit_updates(vec![json!({"update_id":7})], 2.0)
            .unwrap();
        sender.tick(&mut engine, 123, 2.0).unwrap();
        assert_eq!(engine.cursor().unwrap(), 8);
        assert_eq!(engine.journal.rows().unwrap()[0].state, "intent");
        complete
            .send(Ok(json!({"chat_id":42,"message_id":9})))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while sender.pending.is_some() {
            assert!(Instant::now() < deadline);
            sender.tick(&mut engine, 123, 100.0).unwrap();
            thread::yield_now();
        }
        assert_eq!(engine.journal.rows().unwrap()[0].state, "delivered");
        assert!(observed.try_recv().is_err());
        sender.shutdown(&mut engine, || 101.0).unwrap();
    }
    #[test]
    fn ownership_excludes_same_bot_across_instances_and_releases_partial_acquisition() {
        let root = std::env::temp_dir().join(format!(
            "daedalus-owner-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let global = root.join("global");
        let one = root.join("one");
        let two = root.join("two");
        let owner = Owner::acquire(&one, &global, 123).unwrap();
        assert!(Owner::acquire(&two, &global, 123).is_err());
        assert!(!two.exists());
        assert!(Owner::acquire(&one, &global, 456).is_err());
        let other = Owner::acquire(&two, &global, 456).unwrap();
        // Model descriptors retained briefly by a concurrent fork before exec.
        let inherited_bot = owner._bot.0.try_clone().unwrap();
        let inherited_instance = owner._instance.0.try_clone().unwrap();
        drop(owner);
        let replacement = Owner::acquire(&one, &global, 123).unwrap();
        drop(inherited_bot);
        drop(inherited_instance);
        drop(replacement);
        drop(other);
        assert!(one.join(".telegram-service.lock").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn grant_replacement_revokes_omitted_actor_without_affecting_other_bot() {
        use crate::core::routes::{Permission, Scope};
        let mut engine = Engine::new(
            Journal::memory().unwrap(),
            Registry::memory().unwrap(),
            "fixture",
            123,
        )
        .unwrap();
        let scope = Scope {
            instance: "fixture".into(),
            bot: "123".into(),
            chat: "42".into(),
        };
        let other = Scope {
            bot: "456".into(),
            ..scope.clone()
        };
        engine
            .routes
            .set_grant(&scope, "42", &[Permission::Read])
            .unwrap();
        engine
            .routes
            .set_grant(&other, "42", &[Permission::Read])
            .unwrap();
        engine.routes.replace_grants("fixture", "123", &[]).unwrap();
        assert!(
            engine
                .routes
                .authorize(&scope, "42", Permission::Read)
                .is_err()
        );
        assert!(
            engine
                .routes
                .authorize(&other, "42", Permission::Read)
                .is_ok()
        );
        let grants = vec![("42".into(), "42".into(), vec![Permission::Read]); 2];
        assert!(
            engine
                .routes
                .replace_grants("fixture", "456", &grants)
                .is_err()
        );
        assert!(
            engine
                .routes
                .authorize(&other, "42", Permission::Read)
                .is_ok()
        );
    }
    struct Mock {
        calls: SyncSender<i64>,
        replies: Receiver<Batch>,
    }
    impl Api for Mock {
        fn get_me(&mut self) -> std::result::Result<Value, Error> {
            Ok(json!({"id":123,"is_bot":true,"username":"example_bot"}))
        }
        fn updates(&mut self, offset: i64, _: u64) -> Batch {
            self.calls.send(offset).unwrap();
            self.replies
                .recv_timeout(Duration::from_secs(2))
                .unwrap_or(Err(Error::Uncertain))
        }
        fn send_text(
            &mut self,
            _: i64,
            _: &str,
            _: Option<i64>,
        ) -> std::result::Result<Value, Error> {
            unreachable!()
        }
        fn download(&mut self, _: &str) -> std::result::Result<Vec<u8>, Error> {
            unreachable!()
        }
        fn send_document(
            &mut self,
            _: i64,
            _: &str,
            _: Vec<u8>,
            _: &str,
        ) -> std::result::Result<Value, Error> {
            unreachable!()
        }
    }
    struct IdentityApi {
        replies: std::collections::VecDeque<std::result::Result<Value, Error>>,
        calls: usize,
    }
    impl Api for IdentityApi {
        fn get_me(&mut self) -> std::result::Result<Value, Error> {
            self.calls += 1;
            self.replies.pop_front().expect("unexpected identity retry")
        }
        fn updates(&mut self, _: i64, _: u64) -> Batch {
            panic!("Startup must not poll")
        }
        fn send_text(
            &mut self,
            _: i64,
            _: &str,
            _: Option<i64>,
        ) -> std::result::Result<Value, Error> {
            panic!("Startup must not send")
        }
        fn download(&mut self, _: &str) -> std::result::Result<Vec<u8>, Error> {
            panic!("Startup must not download")
        }
        fn send_document(
            &mut self,
            _: i64,
            _: &str,
            _: Vec<u8>,
            _: &str,
        ) -> std::result::Result<Value, Error> {
            panic!("Startup must not send")
        }
    }
    #[test]
    fn startup_retries_only_transient_identity_failures_and_remains_cancellable() {
        let new_engine = || {
            Engine::new(
                Journal::memory().unwrap(),
                Registry::memory().unwrap(),
                "fixture",
                123,
            )
            .unwrap()
        };
        let mut engine = new_engine();
        let identity = json!({"id":123,"is_bot":true,"username":"fixture_bot"});
        let mut api = IdentityApi {
            replies: [
                Err(Error::NotSent),
                Err(Error::Uncertain),
                Err(Error::Rejected(503)),
                Err(Error::RateLimited(17)),
                Ok(identity.clone()),
            ]
            .into(),
            calls: 0,
        };
        let mut waits = Vec::new();
        assert!(
            start_engine(&mut engine, &mut api, |duration| {
                waits.push(duration.as_secs());
                Ok(true)
            })
            .unwrap()
        );
        assert_eq!(waits, vec![0, 1, 2, 4, 17]);
        assert_eq!(api.calls, 5);
        assert_eq!(engine.cursor().unwrap(), 0);
        for response in [
            Err(Error::Rejected(401)),
            Err(Error::Invalid("invalid configuration")),
            Ok(json!({"id":999,"is_bot":true,"username":"wrong_bot"})),
        ] {
            let mut api = IdentityApi {
                replies: [response].into(),
                calls: 0,
            };
            assert!(start_engine(&mut new_engine(), &mut api, |_| Ok(true)).is_err());
            assert_eq!(api.calls, 1);
        }
        let mut api = IdentityApi {
            replies: [Err(Error::NotSent)].into(),
            calls: 0,
        };
        assert!(
            !start_engine(&mut new_engine(), &mut api, |duration| Ok(
                duration.is_zero()
            ))
            .unwrap()
        );
        assert_eq!(api.calls, 1);
    }
    #[test]
    fn polling_is_nonblocking_and_only_acknowledges_durable_admission() {
        let (calls, observed) = mpsc::sync_channel(1);
        let (replies, batches) = mpsc::sync_channel(1);
        let mut api = Mock {
            calls,
            replies: batches,
        };
        let mut engine = Engine::new(
            Journal::memory().unwrap(),
            Registry::memory().unwrap(),
            "fixture",
            123,
        )
        .unwrap();
        engine.start(&mut api).unwrap();
        let mut poller = Poller::spawn(api).unwrap();
        poller.tick(&mut engine, 1.0).unwrap();
        assert_eq!(observed.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        // HTTP remains blocked on the fixture; owner can still inspect state.
        assert_eq!(poller.tick(&mut engine, 1.0).unwrap(), 0);
        assert_eq!(engine.cursor().unwrap(), 0);
        replies.send(Ok(vec![json!({"update_id":7})])).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while engine.cursor().unwrap() != 8 {
            assert!(Instant::now() < deadline);
            poller.tick(&mut engine, 2.0).unwrap();
            thread::yield_now();
        }
        assert_eq!(observed.recv_timeout(Duration::from_secs(2)).unwrap(), 8);
        replies.send(Ok(vec![])).unwrap();
        poller.shutdown().unwrap();
    }
    #[test]
    fn rate_limit_does_not_acknowledge_or_spin_requests() {
        let (calls, observed) = mpsc::sync_channel(1);
        let (replies, batches) = mpsc::sync_channel(1);
        let mut api = Mock {
            calls,
            replies: batches,
        };
        let mut engine = Engine::new(
            Journal::memory().unwrap(),
            Registry::memory().unwrap(),
            "fixture",
            123,
        )
        .unwrap();
        engine.start(&mut api).unwrap();
        let mut poller = Poller::spawn(api).unwrap();
        poller.tick(&mut engine, 1.0).unwrap();
        assert_eq!(observed.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        replies.send(Err(Error::RateLimited(20))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while poller.pending {
            assert!(Instant::now() < deadline);
            poller.tick(&mut engine, 2.0).unwrap();
            thread::yield_now();
        }
        poller.tick(&mut engine, 21.0).unwrap();
        assert!(observed.try_recv().is_err());
        assert_eq!(engine.cursor().unwrap(), 0);
        poller.tick(&mut engine, 22.0).unwrap();
        assert_eq!(observed.recv_timeout(Duration::from_secs(2)).unwrap(), 0);
        replies.send(Ok(vec![])).unwrap();
        poller.shutdown().unwrap();
    }
}
