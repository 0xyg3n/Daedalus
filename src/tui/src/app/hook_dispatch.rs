//! Expand progress effects into the ordinary durable Telegram outbox. An edit
//! references a confirmed message receipt; unknown sends never create another.
use super::{
    hook_effects::{self, Binding},
    hook_runtime::Registration,
    telegram_config::ServiceConfig,
};
use crate::core::{
    journal::{Event, Journal},
    routes::Registry,
};
use serde_json::json;
use sha2::{Digest, Sha256};
type Result<T> = std::result::Result<T, String>;
#[derive(Default)]
pub struct Progress {
    after: i64,
    next: f64,
}
impl Progress {
    pub fn tick(
        &mut self,
        journal: &mut Journal,
        routes: &Registry,
        config: &ServiceConfig,
        registrations: &[(std::path::PathBuf, Registration)],
        now: f64,
    ) -> Result<()> {
        if now < self.next {
            return Ok(());
        }
        self.next = now + 2.0;
        let events = journal
            .events_page(
                "hook-effects",
                &config.instance,
                &["admitted"],
                self.after,
                32,
            )
            .map_err(|e| e.to_string())?;
        if events.is_empty() {
            self.after = 0;
            return Ok(());
        }
        // Advance only past work that settled. An anchor still in intent/pending
        // must be retried: later admitted non-progress plans would otherwise
        // keep the cursor ahead of it for the life of the service.
        let page_last = events.last().map(|event| event.id).unwrap_or(self.after);
        let mut selected = std::collections::BTreeMap::new();
        for event in events {
            let operation = event.payload["operation"].as_str().unwrap_or_default();
            if !matches!(
                operation,
                "start_progress" | "update_progress" | "finish_progress"
            ) {
                continue;
            }
            let key = serde_json::to_string(&event.payload["binding"])
                .map_err(|_| "Invalid progress binding")?;
            if let Some(previous) = selected.insert(key, event) {
                journal
                    .event_state(
                        previous.id,
                        "handled",
                        Some("Superseded by a newer progress observation"),
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        let mut retry = None;
        for event in selected.into_values() {
            if let Err(error) = stage(journal, routes, config, registrations, &event, now) {
                journal
                    .event_state(event.id, "failed", Some(&error))
                    .map_err(|e| e.to_string())?;
                continue;
            }
            let pending = journal
                .events_page(
                    "hook-effects",
                    &config.instance,
                    &["admitted"],
                    event.id.saturating_sub(1),
                    1,
                )
                .map_err(|e| e.to_string())?;
            if pending.first().is_some_and(|row| row.id == event.id) {
                retry = Some(retry.map_or(event.id, |id: i64| id.min(event.id)));
            }
        }
        self.after = retry.map_or(page_last, |id| id.saturating_sub(1));
        Ok(())
    }
}
fn stage(
    journal: &mut Journal,
    routes: &Registry,
    config: &ServiceConfig,
    registrations: &[(std::path::PathBuf, Registration)],
    event: &Event,
    now: f64,
) -> Result<()> {
    let binding: Binding = serde_json::from_value(event.payload["binding"].clone())
        .map_err(|_| "Invalid progress binding")?;
    if !config
        .hook_bindings
        .iter()
        .any(|b| serde_json::to_value(b).ok() == Some(event.payload["binding"].clone()))
        || !registrations.iter().any(|(_, r)| {
            r.id == binding.registration_id
                && r.scope == binding.goal_scope
                && r.options.telegram_progress
        })
    {
        return Err("Progress binding is no longer enabled".into());
    }
    hook_effects::authorize(&binding, routes)?;
    let mut origin = event.payload["origin"].clone();
    if origin["kind"] != "verified_hook" || origin["binding"] != event.payload["binding"] {
        return Err("Invalid progress origin".into());
    }
    origin["effect"] = json!("progress");
    let chat = binding
        .route_scope
        .chat
        .parse::<i64>()
        .map_err(|_| "Invalid progress chat")?;
    let destination = format!("telegram:{}:{}", binding.route_scope.bot, chat);
    let anchor_key = format!(
        "progress:{:x}",
        Sha256::digest(serde_json::to_vec(&binding).map_err(|_| "Invalid progress binding")?)
    );
    if !journal
        .progress_head(&anchor_key, event.id)
        .map_err(|e| e.to_string())?
    {
        journal
            .event_state(
                event.id,
                "handled",
                Some("Superseded by a newer progress observation"),
            )
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let operation = event.payload["operation"]
        .as_str()
        .ok_or("Missing progress operation")?;
    let status = match operation {
        "start_progress" => "Agent started responding",
        "update_progress" => "Agent activity observed",
        "finish_progress" => "Agent turn finished",
        _ => return Err("Unknown progress operation".into()),
    };
    let text = format!("{status}\n{}", timestamp(event.created_at)?);
    let draft = journal
        .delivery_for_key("outbound", &anchor_key)
        .map_err(|e| e.to_string())?;
    match draft {
        None => {
            journal
                .enqueue(
                    "outbound",
                    &destination,
                    &json!({"chat":chat,"text":text,"reply":binding.reply_to}),
                    &anchor_key,
                    Some(&origin),
                    now,
                )
                .map_err(|e| e.to_string())?;
        }
        Some(row) if matches!(row.state.as_str(), "pending" | "retryable" | "intent") => {
            return Ok(());
        }
        Some(row) if row.state == "delivered" => {
            let receipt = row
                .receipt
                .as_ref()
                .ok_or("Progress message has no receipt")?;
            if receipt["chat_id"].as_i64() != Some(chat) {
                return Err("Progress receipt chat differs".into());
            }
            let message = receipt["message_id"]
                .as_i64()
                .filter(|id| *id > 0)
                .ok_or("Invalid progress receipt")?;
            let key = format!("progress-edit:{}:{}", config.instance, event.event_id);
            journal
                .enqueue(
                    "outbound",
                    &destination,
                    &json!({"chat":chat,"text":text,"edit_message_id":message}),
                    &key,
                    Some(&origin),
                    now,
                )
                .map_err(|e| e.to_string())?;
        }
        Some(_) => return Err("Progress send outcome requires review; no replacement sent".into()),
    }
    journal
        .event_state(event.id, "handled", None)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn timestamp(now: f64) -> Result<String> {
    if !now.is_finite() || !(0.0..253_402_300_800.0).contains(&now) {
        return Err("Invalid progress timestamp".into());
    }
    let seconds = now as libc::time_t;
    let mut time = std::mem::MaybeUninit::<libc::tm>::uninit();
    #[cfg(unix)]
    let valid = !unsafe { libc::gmtime_r(&seconds, time.as_mut_ptr()) }.is_null();
    #[cfg(windows)]
    let valid = unsafe { libc::gmtime_s(time.as_mut_ptr(), &seconds) } == 0;
    if !valid {
        return Err("Invalid progress timestamp".into());
    }
    let time = unsafe { time.assume_init() };
    Ok(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        time.tm_year + 1900,
        time.tm_mon + 1,
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{
            hook_runtime::Options,
            hooks::{EventKind, Harness},
            telegram_config::{Access, Grant},
        },
        core::{
            goal_store,
            identity::{Backend, TerminalId},
            routes::{Permission, Scope, Terminal},
        },
    };
    struct Fixture {
        journal: Journal,
        routes: Registry,
        config: ServiceConfig,
        registration: Registration,
        binding: Binding,
    }
    impl Fixture {
        fn new() -> Self {
            let terminal = TerminalId {
                backend: Backend::Tmux,
                endpoint: crate::test_support::path("/fixture.sock").into(),
                pane: "%1".into(),
                generation: "generation".into(),
            };
            let scope = goal_store::Scope {
                instance: "fixture".into(),
                terminal: terminal.clone(),
                conversation: "session".into(),
                harness: Harness::Claude,
            };
            let registration = Registration {
                version: 1,
                id: "hook".into(),
                directory: crate::test_support::path("/tmp/unused-progress-fixture").into(),
                scope: scope.clone(),
                goal_id: None,
                harness_process: None,
                server_emitter: None,
                transcript: None,
                events: vec![EventKind::UserPromptSubmit, EventKind::Stop],
                options: Options {
                    telegram_delivery: false,
                    telegram_progress: true,
                    notifications: false,
                    hindsight: false,
                },
                activity_lease_ms: 30000,
                pending_timeout_ms: 30000,
                hook_timeout_ms: 12000,
            };
            let mut routes = Registry::memory().unwrap();
            routes
                .inventory(
                    Backend::Tmux,
                    crate::test_support::path("/fixture.sock"),
                    &[Terminal {
                        identity: terminal.clone(),
                        label: "Agent".into(),
                        workspace: crate::test_support::path("/workspace").into(),
                        harness: "claude".into(),
                        conversation: Some("session".into()),
                        available: true,
                    }],
                    true,
                    true,
                )
                .unwrap();
            let route_scope = Scope {
                instance: "fixture".into(),
                bot: "123".into(),
                chat: "42".into(),
            };
            routes
                .set_grant(
                    &route_scope,
                    "7",
                    &[Permission::Read, Permission::Send, Permission::Manage],
                )
                .unwrap();
            routes
                .select(
                    &route_scope,
                    "7",
                    &terminal,
                    std::slice::from_ref(&terminal),
                    0,
                    true,
                    false,
                )
                .unwrap();
            let binding = Binding {
                registration_id: "hook".into(),
                goal_scope: scope,
                route_scope,
                actor: "7".into(),
                route_revision: 1,
                reply_to: None,
                hindsight_bank: None,
            };
            let config = ServiceConfig {
                version: 1,
                instance: "fixture".into(),
                bot_id: 123,
                token_file: crate::test_support::path("/tmp/unused-progress-token").into(),
                grants: vec![Grant {
                    chat: 42,
                    actor: 7,
                    permissions: vec![Access::Read, Access::Send, Access::Manage],
                }],
                listen_chats: Default::default(),
                registrations: vec![],
                hook_bindings: vec![binding.clone()],
            };
            Self {
                journal: Journal::memory().unwrap(),
                routes,
                config,
                registration,
                binding,
            }
        }
        fn admit(&mut self, id: &str, operation: &str, now: f64) -> Event {
            let origin = json!({"kind":"verified_hook","binding":self.binding,"event_id":id});
            self.journal
                .admit(
                    "hook-effects",
                    "fixture",
                    id,
                    &json!({"operation":operation,"binding":self.binding,"origin":origin}),
                    now,
                )
                .unwrap();
            self.journal
                .events_page("hook-effects", "fixture", &["admitted"], 0, 100)
                .unwrap()
                .into_iter()
                .find(|e| e.event_id == id)
                .unwrap()
        }
        fn stage(&mut self, event: &Event, now: f64) -> Result<()> {
            stage(
                &mut self.journal,
                &self.routes,
                &self.config,
                &[("/unused".into(), self.registration.clone())],
                event,
                now,
            )
        }
        fn complete_first(&mut self, now: f64) {
            let row = self
                .journal
                .begin("outbound", "telegram:123:42", "test", now, 90.0)
                .unwrap()
                .unwrap();
            self.journal
                .record_delivery_receipt(
                    &row.delivery_id,
                    row.lease_token.as_deref().unwrap(),
                    &json!({"chat_id":42,"message_id":9}),
                    now,
                )
                .unwrap();
        }
    }
    #[test]
    fn progress_waits_for_receipt_then_edits_exact_message_and_never_regresses() {
        let mut f = Fixture::new();
        let first = f.admit("start", "start_progress", 1.0);
        f.stage(&first, 1.0).unwrap();
        let older = f.admit("working", "update_progress", 2.0);
        f.stage(&older, 2.0).unwrap();
        assert_eq!(f.journal.rows().unwrap().len(), 1);
        f.complete_first(3.0);
        let newer = f.admit("done", "finish_progress", 4.0);
        f.stage(&newer, 4.0).unwrap();
        f.stage(&older, 5.0).unwrap();
        let rows = f.journal.rows().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].payload["edit_message_id"], 9);
        assert!(
            rows[1].payload["text"]
                .as_str()
                .unwrap()
                .starts_with("Agent turn finished")
        );
        f.stage(&newer, 6.0).unwrap();
        assert_eq!(f.journal.rows().unwrap().len(), 2);
    }
    #[test]
    fn uncertain_anchor_and_revoked_recipient_never_send_replacement() {
        let mut f = Fixture::new();
        let first = f.admit("start", "start_progress", 1.0);
        f.stage(&first, 1.0).unwrap();
        let row = f
            .journal
            .begin("outbound", "telegram:123:42", "test", 2.0, 90.0)
            .unwrap()
            .unwrap();
        f.journal
            .finish(
                &row.delivery_id,
                row.lease_token.as_deref().unwrap(),
                "unknown",
                None,
                Some("uncertain"),
                0.0,
                3.0,
            )
            .unwrap();
        let next = f.admit("next", "update_progress", 4.0);
        assert!(f.stage(&next, 4.0).is_err());
        assert_eq!(f.journal.rows().unwrap().len(), 1);
        f.routes
            .set_grant(&f.binding.route_scope, "7", &[])
            .unwrap();
        assert!(f.stage(&next, 5.0).is_err());
        assert_eq!(f.journal.rows().unwrap().len(), 1);
    }
    #[test]
    fn burst_coalesces_and_tick_is_bounded() {
        let mut f = Fixture::new();
        for i in 0..10 {
            f.admit(&i.to_string(), "update_progress", i as f64);
        }
        let mut progress = Progress::default();
        progress
            .tick(
                &mut f.journal,
                &f.routes,
                &f.config,
                &[("/unused".into(), f.registration.clone())],
                10.0,
            )
            .unwrap();
        assert_eq!(f.journal.rows().unwrap().len(), 1);
        assert!(
            f.journal
                .events_page("hook-effects", "fixture", &["admitted"], 0, 100)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn in_flight_anchor_is_retried_after_later_non_progress_plans() {
        let mut f = Fixture::new();
        let first = f.admit("start", "start_progress", 1.0);
        f.stage(&first, 1.0).unwrap();
        f.admit("working", "update_progress", 2.0);
        f.journal
            .admit(
                "hook-effects",
                "fixture",
                "memory",
                &json!({"operation":"persist_memory","binding":f.binding}),
                2.5,
            )
            .unwrap();
        let mut progress = Progress::default();
        progress
            .tick(
                &mut f.journal,
                &f.routes,
                &f.config,
                &[("/unused".into(), f.registration.clone())],
                3.0,
            )
            .unwrap();
        assert_eq!(f.journal.rows().unwrap().len(), 1);
        assert!(
            f.journal
                .events_page("hook-effects", "fixture", &["admitted"], 0, 100)
                .unwrap()
                .iter()
                .any(|event| event.event_id == "working")
        );
        f.complete_first(4.0);
        progress
            .tick(
                &mut f.journal,
                &f.routes,
                &f.config,
                &[("/unused".into(), f.registration.clone())],
                6.0,
            )
            .unwrap();
        let rows = f.journal.rows().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].payload["edit_message_id"], 9);
        assert!(
            rows[1].payload["text"]
                .as_str()
                .unwrap()
                .starts_with("Agent activity observed")
        );
    }
}
