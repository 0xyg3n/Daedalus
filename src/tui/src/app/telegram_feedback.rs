//! Best-effort Telegram UI feedback. No feeder or delivery receipt is inferred
//! from a reaction or typing action; expired feedback is discarded, never replayed.
use super::{hook_effects, hook_runtime, telegram::Engine, telegram_config::ServiceConfig};
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
struct Action {
    chat: i64,
    message: Option<i64>,
    expires: Instant,
}
pub struct Feedback {
    requests: Option<SyncSender<Action>>,
    worker: Option<JoinHandle<()>>,
    next: f64,
}
impl Feedback {
    pub fn spawn(mut api: impl Api + Send + 'static) -> Result<Self, String> {
        let (requests, receiver) = mpsc::sync_channel::<Action>(64);
        let worker = thread::Builder::new()
            .name("telegram-feedback".into())
            .spawn(move || {
                let mut retry_at = Instant::now();
                while let Ok(action) = receiver.recv() {
                    if Instant::now() >= action.expires || Instant::now() < retry_at {
                        continue;
                    }
                    let result = match action.message {
                        Some(message) => api.acknowledge(action.chat, message),
                        None => api.typing(action.chat),
                    };
                    // Cosmetic failures never fail delivery or trigger retries.
                    if let Err(Error::RateLimited(seconds)) = result {
                        retry_at = Instant::now() + Duration::from_secs(seconds.clamp(1, 86400));
                    }
                }
            })
            .map_err(|_| "Cannot start Telegram feedback worker")?;
        Ok(Self {
            requests: Some(requests),
            worker: Some(worker),
            next: 0.0,
        })
    }
    fn offer(&self, chat: i64, message: Option<i64>) {
        if let Some(requests) = &self.requests {
            let _ = requests.try_send(Action {
                chat,
                message,
                expires: Instant::now()
                    + Duration::from_secs(if message.is_some() { 10 } else { 1 }),
            });
        }
    }
    pub fn tick(
        &mut self,
        engine: &mut Engine,
        config: &ServiceConfig,
        registrations: &[(PathBuf, hook_runtime::Registration)],
        now: f64,
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
                self.offer(chat, Some(frame.message_id));
            }
        }
        if !now.is_finite() || now < self.next {
            return;
        }
        self.next = now + 4.0;
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
            if hook_runtime::ready_snapshot(path, Path::new("/proc")).is_ok_and(|r| {
                r.scope == binding.goal_scope && r.state == hook_runtime::Readiness::Busy
            }) && let Ok(chat) = binding.route_scope.chat.parse::<i64>()
            {
                chats.insert(chat);
            }
        }
        for chat in chats {
            self.offer(chat, None);
        }
    }
    pub fn shutdown(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
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
        rate_limited: bool,
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
            Err(if self.rate_limited {
                Error::RateLimited(30)
            } else {
                Error::Rejected(400)
            })
        }
        fn typing(&mut self, _: i64) -> crate::adapters::telegram::Result<()> {
            self.calls.lock().unwrap().push("typing");
            Ok(())
        }
    }
    #[test]
    fn failed_reactions_do_not_block_typing_and_rate_limits_drop_feedback() {
        for rate_limited in [false, true] {
            let calls = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
            let mut worker = Feedback::spawn(Fake {
                calls: calls.clone(),
                rate_limited,
            })
            .unwrap();
            // Expired actions must never reach transport.
            worker
                .requests
                .as_ref()
                .unwrap()
                .send(Action {
                    chat: 42,
                    message: None,
                    expires: Instant::now() - Duration::from_secs(1),
                })
                .unwrap();
            worker.offer(42, Some(7));
            worker.offer(42, None);
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
