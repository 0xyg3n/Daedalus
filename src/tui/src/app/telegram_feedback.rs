//! Best-effort Telegram UI feedback. No feeder or delivery receipt is inferred
//! from a reaction or typing action; expired feedback is discarded, never replayed.
use super::{
    hook_effects, hook_runtime, telegram::Engine, telegram_config::ServiceConfig,
    telegram_runtime::Settings, timing::Delay,
};
use crate::adapters::telegram::{Api, Error};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::mpsc::{self, SyncSender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(crate) struct Receipt {
    pub scope: crate::core::routes::Scope,
    pub actor: String,
    pub message_id: i64,
    pub terminal: crate::core::identity::TerminalId,
    pub route_revision: i64,
    pub conversation: Option<String>,
}
impl From<super::telegram::Frame> for Receipt {
    fn from(frame: super::telegram::Frame) -> Self {
        Self {
            scope: frame.scope,
            actor: frame.actor,
            message_id: frame.message_id,
            terminal: frame.terminal,
            route_revision: frame.route_revision,
            conversation: frame.conversation,
        }
    }
}
enum Operation {
    Typing(i64),
    Acknowledge(i64, i64),
    Commands,
}
struct Action {
    operation: Operation,
    expiry: Delay,
}
pub struct Feedback {
    requests: Option<SyncSender<Action>>,
    worker: Option<JoinHandle<()>>,
    next: Option<Delay>,
    settings: Settings,
}
impl Feedback {
    pub fn spawn(api: impl Api + Send + 'static) -> Result<Self, String> {
        Self::spawn_configured(api, &Default::default())
    }
    pub fn spawn_configured(
        mut api: impl Api + Send + 'static,
        settings: &Settings,
    ) -> Result<Self, String> {
        settings.validate()?;
        let (requests, receiver) =
            mpsc::sync_channel::<Action>(settings.feedback_queue_limit as usize);
        let worker = thread::Builder::new()
            .name("telegram-feedback".into())
            .spawn(move || {
                let mut retry = None;
                while let Ok(action) = receiver.recv() {
                    dispatch(&mut api, action, &mut retry, Instant::now);
                }
            })
            .map_err(|_| "Cannot start Telegram feedback worker")?;
        Ok(Self {
            requests: Some(requests),
            worker: Some(worker),
            next: None,
            settings: settings.clone(),
        })
    }
    fn offer(&self, chat: i64, message: Option<i64>, elapsed: Instant) {
        let (operation, ttl) = match message {
            Some(message) if self.settings.feedback_ack_enabled => (
                Operation::Acknowledge(chat, message),
                self.settings.feedback_ack_ttl_ms,
            ),
            None if self.settings.feedback_typing_enabled => (
                Operation::Typing(chat),
                self.settings.feedback_typing_ttl_ms,
            ),
            _ => return,
        };
        if let Some(requests) = &self.requests {
            let _ = requests.try_send(Action {
                operation,
                expiry: Delay::new(elapsed, Duration::from_millis(ttl)),
            });
        }
    }
    pub fn register_commands(&self) {
        if self.settings.feedback_commands_enabled
            && let Some(requests) = &self.requests
        {
            let _ = requests.try_send(Action {
                operation: Operation::Commands,
                expiry: Delay::new(
                    Instant::now(),
                    Duration::from_millis(self.settings.feedback_commands_ttl_ms),
                ),
            });
        }
    }
    pub fn tick(
        &mut self,
        engine: &mut Engine,
        config: &ServiceConfig,
        registrations: &[(PathBuf, hook_runtime::Registration)],
    ) {
        self.tick_with(
            engine,
            config,
            registrations,
            Instant::now(),
            |path, scope| {
                hook_runtime::ready_snapshot(path, Path::new("/proc"))
                    .is_ok_and(|r| r.scope == *scope && r.state == hook_runtime::Readiness::Busy)
            },
        );
    }
    fn tick_with(
        &mut self,
        engine: &mut Engine,
        config: &ServiceConfig,
        registrations: &[(PathBuf, hook_runtime::Registration)],
        elapsed: Instant,
        mut busy: impl FnMut(&Path, &crate::core::goal_store::Scope) -> bool,
    ) {
        for frame in std::mem::take(&mut engine.feedback_acks) {
            let valid = engine
                .routes
                .inspect(&frame.scope, &frame.actor)
                .ok()
                .flatten()
                .is_some_and(|route| {
                    route.revision == frame.route_revision
                        && route.terminal == frame.terminal
                        && engine
                            .routes
                            .authorize(
                                &frame.scope,
                                &frame.actor,
                                crate::core::routes::Permission::Send,
                            )
                            .is_ok()
                        && engine
                            .routes
                            .terminal(&route.terminal)
                            .ok()
                            .flatten()
                            .is_some_and(|t| t.conversation == frame.conversation)
                });
            if valid
                && frame.scope.bot == config.bot_id.to_string()
                && frame.scope.instance == config.instance
                && let Ok(chat) = frame.scope.chat.parse::<i64>()
            {
                self.offer(chat, Some(frame.message_id), elapsed);
            }
        }
        if !self.settings.feedback_typing_enabled
            || self.next.is_some_and(|delay| !delay.ready(elapsed))
        {
            return;
        }
        self.next = Some(Delay::new(
            elapsed,
            Duration::from_millis(self.settings.feedback_typing_interval_ms),
        ));
        let mut chats = BTreeSet::new();
        for binding in &config.hook_bindings {
            if binding.route_scope.bot != config.bot_id.to_string()
                || binding.goal_scope.instance != config.instance
                || hook_effects::authorize(binding, &engine.routes).is_err()
            {
                continue;
            }
            let Some((path, _)) = registrations.iter().find(|(_, r)| {
                r.id == binding.registration_id
                    && r.scope == binding.goal_scope
                    && (r.options.telegram_delivery || r.options.telegram_progress)
            }) else {
                continue;
            };
            if busy(path, &binding.goal_scope)
                && let Ok(chat) = binding.route_scope.chat.parse::<i64>()
            {
                chats.insert(chat);
            }
        }
        for chat in chats {
            self.offer(chat, None, elapsed);
        }
    }
    pub fn shutdown(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn dispatch(
    api: &mut impl Api,
    action: Action,
    retry: &mut Option<Delay>,
    clock: impl Fn() -> Instant,
) {
    let now = clock();
    if action.expiry.ready(now) || retry.is_some_and(|delay| !delay.ready(now)) {
        return;
    }
    let result = match action.operation {
        Operation::Acknowledge(chat, message) => api.acknowledge(chat, message),
        Operation::Typing(chat) => api.typing(chat),
        Operation::Commands => api.register_commands(serde_json::json!(super::telegram_commands::BOT_COMMANDS.iter().map(|(command, description)|serde_json::json!({"command":command,"description":description})).collect::<Vec<_>>())),
    };
    // Cosmetic failures never fail delivery or replay an action. Honor the
    // complete server delay, even when it cannot be added to an Instant.
    if let Err(Error::RateLimited(seconds)) = result {
        *retry = Some(Delay::new(clock(), Duration::from_secs(seconds.max(1))));
    }
}

impl Drop for Feedback {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    struct Fake {
        calls: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>,
        rate_limit: Option<u64>,
    }
    impl Api for Fake {
        fn get_me(&mut self) -> crate::adapters::telegram::Result<Value> {
            unreachable!()
        }
        fn updates(&mut self, _: i64, _: u64) -> crate::adapters::telegram::Result<Vec<Value>> {
            unreachable!()
        }
        fn send_text(
            &mut self,
            _: i64,
            _: &str,
            _: Option<i64>,
        ) -> crate::adapters::telegram::Result<Value> {
            unreachable!()
        }
        fn download(&mut self, _: &str) -> crate::adapters::telegram::Result<Vec<u8>> {
            unreachable!()
        }
        fn send_document(
            &mut self,
            _: i64,
            _: &str,
            _: Vec<u8>,
            _: &str,
        ) -> crate::adapters::telegram::Result<Value> {
            unreachable!()
        }
        fn acknowledge(&mut self, _: i64, _: i64) -> crate::adapters::telegram::Result<()> {
            self.calls.lock().unwrap().push("ack");
            Err(self
                .rate_limit
                .map_or(Error::Rejected(400), Error::RateLimited))
        }
        fn typing(&mut self, _: i64) -> crate::adapters::telegram::Result<()> {
            self.calls.lock().unwrap().push("typing");
            Ok(())
        }
        fn register_commands(&mut self, _: Value) -> crate::adapters::telegram::Result<()> {
            self.calls.lock().unwrap().push("commands");
            Ok(())
        }
    }
    fn queued(settings: Settings) -> (Feedback, mpsc::Receiver<Action>) {
        let (sender, receiver) = mpsc::sync_channel(settings.feedback_queue_limit as usize);
        (
            Feedback {
                requests: Some(sender),
                worker: None,
                next: None,
                settings,
            },
            receiver,
        )
    }
    #[test]
    fn configured_queue_expiry_switches_and_server_delay_control_real_dispatch() {
        let settings = Settings {
            feedback_queue_limit: 1,
            feedback_typing_ttl_ms: 250,
            feedback_ack_ttl_ms: 750,
            feedback_commands_ttl_ms: 1250,
            ..Default::default()
        };
        let (mut feedback, receiver) = queued(settings);
        let calls = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let mut api = Fake {
            calls: calls.clone(),
            rate_limit: None,
        };
        let start = Instant::now();
        let mut retry = None;
        feedback.offer(42, None, start);
        feedback.offer(42, Some(7), start); // Full queue drops this action.
        dispatch(&mut api, receiver.try_recv().unwrap(), &mut retry, || {
            start + Duration::from_millis(250)
        });
        assert!(receiver.try_recv().is_err());
        assert!(calls.lock().unwrap().is_empty());
        feedback.offer(42, None, start);
        dispatch(&mut api, receiver.try_recv().unwrap(), &mut retry, || {
            start + Duration::from_millis(249)
        });
        feedback.offer(42, Some(7), start);
        dispatch(&mut api, receiver.try_recv().unwrap(), &mut retry, || {
            start + Duration::from_millis(749)
        });
        feedback.offer(42, Some(8), start);
        dispatch(&mut api, receiver.try_recv().unwrap(), &mut retry, || {
            start + Duration::from_millis(750)
        });
        feedback.register_commands();
        let commands = receiver.try_recv().unwrap();
        let after_queued = Instant::now();
        dispatch(&mut api, commands, &mut retry, || {
            after_queued + Duration::from_millis(1250)
        });
        assert_eq!(*calls.lock().unwrap(), vec!["typing", "ack"]);
        feedback.settings.feedback_ack_enabled = false;
        feedback.settings.feedback_typing_enabled = false;
        feedback.settings.feedback_commands_enabled = false;
        feedback.offer(42, None, start);
        feedback.offer(42, Some(7), start);
        feedback.register_commands();
        assert!(receiver.try_recv().is_err());
        api.rate_limit = Some(u64::MAX);
        dispatch(
            &mut api,
            Action {
                operation: Operation::Acknowledge(42, 7),
                expiry: Delay::new(start, Duration::from_secs(1)),
            },
            &mut retry,
            || start,
        );
        let next_day = start + Duration::from_secs(86401);
        dispatch(
            &mut api,
            Action {
                operation: Operation::Typing(42),
                expiry: Delay::new(next_day, Duration::from_secs(1)),
            },
            &mut retry,
            || next_day,
        );
        assert_eq!(*calls.lock().unwrap(), vec!["typing", "ack", "ack"]);
    }
    #[test]
    fn typing_uses_configured_elapsed_cadence_and_rechecks_recipient_authorization() {
        use crate::core::{
            goal_store,
            identity::{Backend, TerminalId},
            journal::Journal,
            routes::{Permission, Registry, Scope, Terminal},
        };
        let terminal = TerminalId {
            backend: Backend::Herdr,
            endpoint: "fixture".into(),
            pane: "pane".into(),
            generation: "generation".into(),
        };
        let scope = Scope {
            instance: "fixture".into(),
            bot: "1".into(),
            chat: "2".into(),
        };
        let goal = goal_store::Scope {
            instance: "fixture".into(),
            terminal: terminal.clone(),
            conversation: "conversation".into(),
            harness: super::super::hooks::Harness::Pi,
        };
        let mut routes = Registry::memory().unwrap();
        routes
            .inventory(
                Backend::Herdr,
                "fixture",
                &[Terminal {
                    identity: terminal.clone(),
                    label: "fixture".into(),
                    workspace: crate::test_support::path("/tmp").into(),
                    harness: "pi".into(),
                    conversation: Some("conversation".into()),
                    available: true,
                }],
                true,
                true,
            )
            .unwrap();
        routes
            .set_grant(
                &scope,
                "2",
                &[Permission::Read, Permission::Send, Permission::Manage],
            )
            .unwrap();
        routes
            .select(
                &scope,
                "2",
                &terminal,
                std::slice::from_ref(&terminal),
                0,
                true,
                false,
            )
            .unwrap();
        let binding = hook_effects::Binding {
            registration_id: "hook".into(),
            goal_scope: goal.clone(),
            route_scope: scope.clone(),
            actor: "2".into(),
            route_revision: 1,
            reply_to: None,
            hindsight_bank: None,
        };
        let config: ServiceConfig = serde_json::from_value(serde_json::json!({"version":1,"instance":"fixture","bot_id":1,"token_file":crate::test_support::path("/tmp/unused-feedback-key"),"hook_bindings":[binding]})).unwrap();
        let registration = hook_runtime::Registration {
            version: 1,
            id: "hook".into(),
            directory: crate::test_support::path("/tmp/unused-feedback-state").into(),
            scope: goal,
            goal_id: None,
            harness_process: None,
            server_emitter: None,
            transcript: None,
            events: vec![],
            options: hook_runtime::Options::guided(),
            activity_lease_ms: 30000,
            pending_timeout_ms: 30000,
            hook_timeout_ms: 12000,
        };
        let registrations = [(
            crate::test_support::path("/tmp/unused-feedback-registration").into(),
            registration,
        )];
        let mut engine = Engine::new(Journal::memory().unwrap(), routes, "fixture", 1).unwrap();
        let (mut feedback, receiver) = queued(Settings {
            feedback_typing_interval_ms: 500,
            ..Default::default()
        });
        let start = Instant::now();
        feedback.tick_with(&mut engine, &config, &registrations, start, |_, _| true);
        assert!(matches!(
            receiver.try_recv().unwrap().operation,
            Operation::Typing(2)
        ));
        feedback.tick_with(
            &mut engine,
            &config,
            &registrations,
            start + Duration::from_millis(499),
            |_, _| panic!("Typing cadence was bypassed"),
        );
        assert!(receiver.try_recv().is_err());
        feedback.tick_with(
            &mut engine,
            &config,
            &registrations,
            start + Duration::from_millis(500),
            |_, _| true,
        );
        assert!(matches!(
            receiver.try_recv().unwrap().operation,
            Operation::Typing(2)
        ));
        engine.routes.set_grant(&scope, "2", &[]).unwrap();
        feedback.tick_with(
            &mut engine,
            &config,
            &registrations,
            start + Duration::from_millis(1000),
            |_, _| panic!("Revoked recipient reached readiness"),
        );
        assert!(receiver.try_recv().is_err());
    }
    #[test]
    fn failed_reactions_do_not_block_typing_and_rate_limits_drop_feedback() {
        for rate_limited in [false, true] {
            let calls = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
            let mut worker = Feedback::spawn(Fake {
                calls: calls.clone(),
                rate_limit: rate_limited.then_some(30),
            })
            .unwrap();
            // Expired actions must never reach transport.
            worker
                .requests
                .as_ref()
                .unwrap()
                .send(Action {
                    operation: Operation::Typing(42),
                    expiry: Delay::new(
                        Instant::now() - Duration::from_secs(1),
                        Duration::from_millis(1),
                    ),
                })
                .unwrap();
            worker.offer(42, Some(7), Instant::now());
            worker.offer(42, None, Instant::now());
            worker.shutdown();
            assert_eq!(
                *calls.lock().unwrap(),
                if rate_limited {
                    vec!["ack"]
                } else {
                    vec!["ack", "typing"]
                }
            );
        }
    }
}
