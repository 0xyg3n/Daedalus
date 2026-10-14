//! Expand verified hook inbox events into durable delivery/effect plans.
//! This module never calls Telegram or Hindsight and never marks delivery done.
use super::{hook_runtime::Registration, hooks::EventKind};
use crate::core::{
    goal_store,
    journal::{Delivery, Event, Journal},
    routes::{self, Permission, Registry},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub registration_id: String,
    pub goal_scope: goal_store::Scope,
    pub route_scope: routes::Scope,
    /// Local operator-authorized recipient actor, never inferred from hook input.
    pub actor: String,
    pub route_revision: i64,
    pub reply_to: Option<i64>,
    /// Explicit configured memory bank. No user-supplied endpoint is executed.
    pub hindsight_bank: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expanded {
    pub deliveries: Vec<String>,
    pub effect_plans: usize,
    pub deferred: bool,
}
pub(crate) fn authorize(binding: &Binding, registry: &Registry) -> Result<()> {
    binding.goal_scope.validate().map_err(|e| e.to_string())?;
    binding.route_scope.validate().map_err(|e| e.to_string())?;
    if binding.goal_scope.instance != binding.route_scope.instance
        || binding.registration_id.is_empty()
        || binding.route_revision <= 0
    {
        return Err("Invalid hook recipient binding".into());
    }
    let bot = binding
        .route_scope
        .bot
        .parse::<i64>()
        .map_err(|_| "Invalid bot ID")?;
    let chat = binding
        .route_scope
        .chat
        .parse::<i64>()
        .map_err(|_| "Invalid chat ID")?;
    if bot <= 0 || chat == 0 || binding.reply_to.is_some_and(|id| id <= 0) {
        return Err("Invalid Telegram recipient".into());
    }
    registry
        .authorize(&binding.route_scope, &binding.actor, Permission::Send)
        .map_err(|_| "Hook recipient permission is absent")?;
    let route = registry
        .inspect(&binding.route_scope, &binding.actor)
        .map_err(|_| "Hook recipient permission is absent")?
        .ok_or("Hook route is missing")?;
    if route.terminal != binding.goal_scope.terminal || route.revision != binding.route_revision {
        return Err("Hook route changed; review recipient binding".into());
    }
    let terminal = registry
        .terminal(&route.terminal)
        .map_err(|e| e.to_string())?
        .ok_or("Hook terminal is missing")?;
    if terminal.conversation.as_deref() != Some(binding.goal_scope.conversation.as_str()) {
        return Err("Hook conversation changed".into());
    }
    Ok(())
}
/// Recheck local recipient consent immediately before transport dispatch. A
/// formerly authorized queued item is not authority after route/grant changes.
pub fn authorize_delivery(row: &Delivery, registry: &Registry) -> Result<()> {
    if row.origin.get("kind").and_then(Value::as_str) != Some("verified_hook") {
        return Err("Not a verified hook delivery".into());
    }
    let binding: Binding = serde_json::from_value(row.origin["binding"].clone())
        .map_err(|_| "Invalid hook delivery provenance")?;
    authorize(&binding, registry)?;
    if row.destination
        != format!(
            "telegram:{}:{}",
            binding.route_scope.bot, binding.route_scope.chat
        )
        || row.payload["chat"].as_i64() != binding.route_scope.chat.parse::<i64>().ok()
    {
        return Err("Hook delivery destination mismatch".into());
    }
    Ok(())
}
fn chunks(text: &str) -> Result<Vec<String>> {
    if text.contains('\0') || text.len() > 256 * 1024 {
        return Err("Hook final message exceeds supported text bounds".into());
    }
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut length = 0;
    for c in text.chars() {
        let size = c.len_utf16();
        if length + size > 4096 {
            parts.push(std::mem::take(&mut part));
            length = 0;
        }
        part.push(c);
        length += size;
    }
    if !part.is_empty() {
        parts.push(part);
    }
    if parts.len() > 64 {
        return Err("Too many Telegram text parts".into());
    }
    Ok(parts)
}
/// Event provenance is produced by the private native hook runtime. Calling this
/// with arbitrary user JSON is not authentication; the service owns the journal.
fn expand_one(
    journal: &mut Journal,
    registry: &Registry,
    registration: &Registration,
    binding: &Binding,
    event: &Event,
    now: f64,
) -> Result<Expanded> {
    let persisted = journal
        .events_page(
            "hooks",
            &registration.scope.instance,
            &["admitted"],
            event.id.saturating_sub(1),
            1,
        )
        .map_err(|e| e.to_string())?;
    if persisted.first() != Some(event) {
        return Err("Hook event is not the current admitted journal record".into());
    }
    if binding
        .hindsight_bank
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.len() > 128 || s.chars().any(char::is_control))
    {
        return Err("Invalid configured memory bank".into());
    }
    registration.validate().map_err(|e| e.to_string())?;
    authorize(binding, registry)?;
    if event.channel != "hooks"
        || event.bot != registration.scope.instance
        || event.state != "admitted"
        || binding.registration_id != registration.id
        || binding.goal_scope != registration.scope
    {
        return Err("Hook event does not match registration/recipient".into());
    }
    let scope: goal_store::Scope = serde_json::from_value(event.payload["scope"].clone())
        .map_err(|_| "Invalid hook event scope")?;
    if scope != registration.scope
        || event.payload["registration_id"].as_str() != Some(registration.id.as_str())
        || event.payload["transcript"] != json!(registration.transcript)
    {
        return Err("Hook event binding changed".into());
    }
    let kind: EventKind =
        serde_json::from_value(event.payload["event"].clone()).map_err(|_| "Unknown hook event")?;
    if !registration.events.contains(&kind) {
        return Err("Hook event is not enabled".into());
    }
    let names = event.payload["effects"]
        .as_array()
        .ok_or("Invalid hook effects")?;
    let enabled = |name: &str| names.iter().any(|v| v.as_str() == Some(name));
    let source_hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&event.payload).map_err(|_| "Invalid hook event")?)
    );
    let key = recipient_key(registration, binding, event)?;
    let origin = json!({"kind":"verified_hook","binding":binding,"event_id":event.event_id,"source_sha256":source_hash,"source":"native_hook_payload"});
    let mut output = Expanded::default();
    let final_text = event.payload["final_message"].as_str();
    if ((kind == EventKind::Stop && enabled("queue_final_reply"))
        || (kind == EventKind::AssistantResponse && enabled("queue_assistant_reply")))
        && registration.options.telegram_delivery
    {
        let Some(text) = final_text else {
            output.deferred = true;
            return Ok(output);
        };
        for (index, text) in chunks(text)?.iter().enumerate() {
            let dedup = format!("{key}:text:{index}");
            if journal
                .delivery_for_key("outbound", &dedup)
                .map_err(|e| e.to_string())?
                .is_some_and(|row| row.origin != origin)
            {
                return Err("Hook delivery provenance conflicts with existing queue item".into());
            }
            let id=journal.enqueue("outbound",&format!("telegram:{}:{}",binding.route_scope.bot,binding.route_scope.chat),&json!({"chat":binding.route_scope.chat.parse::<i64>().map_err(|_|"Invalid chat")?,"text":text,"reply":binding.reply_to}),&dedup,Some(&origin),now).map_err(|e|e.to_string())?;
            output.deliveries.push(id);
        }
    }
    for name in [
        "start_progress",
        "update_progress",
        "finish_progress",
        "notify",
        "recall_memory",
        "persist_memory",
    ] {
        if !enabled(name) {
            continue;
        }
        let memory = matches!(name, "recall_memory" | "persist_memory");
        if memory && (!registration.options.hindsight || binding.hindsight_bank.is_none()) {
            continue;
        }
        if name == "notify" && !registration.options.notifications {
            continue;
        }
        if name.ends_with("progress") && !registration.options.telegram_progress {
            continue;
        }
        let content = if name == "persist_memory" {
            let Some(text) = final_text else {
                output.deferred = true;
                return Ok(output);
            };
            Some(text)
        } else {
            None
        };
        let plan = json!({"operation":name,"binding":binding,"origin":origin,"content":content,"bank":if memory {binding.hindsight_bank.as_deref()}else{None},"state":"planned_not_delivered"});
        if journal
            .admit(
                "hook-effects",
                &registration.scope.instance,
                &format!("{key}:{name}"),
                &plan,
                now,
            )
            .map_err(|e| e.to_string())?
        {
            output.effect_plans += 1;
        }
    }
    Ok(output)
}

fn recipient_key(registration: &Registration, binding: &Binding, event: &Event) -> Result<String> {
    let recipient =
        serde_json::to_vec(&binding.route_scope).map_err(|_| "Invalid recipient scope")?;
    Ok(format!(
        "hook:{}:{}:{:x}",
        registration.scope.instance,
        event.event_id,
        Sha256::digest(recipient)
    ))
}
pub fn expand(
    journal: &mut Journal,
    registry: &Registry,
    registration: &Registration,
    binding: &Binding,
    event: &Event,
    now: f64,
) -> Result<Expanded> {
    expand_many(
        journal,
        registry,
        registration,
        std::slice::from_ref(binding),
        event,
        now,
    )
}
/// Fan out one persisted hook event before marking it handled. Revoked recipients
/// fail preflight before queue mutation; deferred content keeps the event admitted.
/// A crash after any enqueue reuses recipient-specific deterministic keys.
pub fn expand_many(
    journal: &mut Journal,
    registry: &Registry,
    registration: &Registration,
    bindings: &[Binding],
    event: &Event,
    now: f64,
) -> Result<Expanded> {
    if bindings.is_empty() || bindings.len() > 256 {
        return Err("Supply 1..256 authorized hook recipients".into());
    }
    let mut recipients = std::collections::BTreeSet::new();
    for binding in bindings {
        authorize(binding, registry)?;
        if binding.registration_id != registration.id || binding.goal_scope != registration.scope {
            return Err("Hook recipient does not match registration".into());
        }
        let scope =
            serde_json::to_string(&binding.route_scope).map_err(|_| "Invalid recipient scope")?;
        if !recipients.insert(scope) {
            return Err("Duplicate hook recipient".into());
        }
    }
    let mut result = Expanded::default();
    for binding in bindings {
        let next = expand_one(journal, registry, registration, binding, event, now)?;
        result.deliveries.extend(next.deliveries);
        result.effect_plans += next.effect_plans;
        result.deferred |= next.deferred;
    }
    if !result.deferred {
        journal
            .event_state(event.id, "handled", None)
            .map_err(|e| e.to_string())?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{hook_runtime::Options, hooks::Harness},
        core::identity::{Backend, TerminalId},
    };
    struct Fixture {
        registration: Registration,
        binding: Binding,
        registry: Registry,
        journal: Journal,
    }
    impl Fixture {
        fn new() -> Self {
            let terminal = TerminalId {
                backend: Backend::Herdr,
                endpoint: "example".into(),
                pane: "pane".into(),
                generation: "terminal".into(),
            };
            let scope = goal_store::Scope {
                instance: "example".into(),
                terminal: terminal.clone(),
                conversation: "session".into(),
                harness: Harness::Claude,
            };
            let route_scope = routes::Scope {
                instance: "example".into(),
                bot: "1".into(),
                chat: "2".into(),
            };
            let registration = Registration {
                version: 1,
                id: "hook".into(),
                directory: "/tmp/unused-hook-effect-fixture".into(),
                scope: scope.clone(),
                goal_id: None,
                harness_process: None,
                server_emitter: None,
                transcript: Some("/tmp/unused-hook-transcript.jsonl".into()),
                events: vec![EventKind::Stop, EventKind::UserPromptSubmit],
                options: Options {
                    telegram_delivery: true,
                    telegram_progress: true,
                    notifications: false,
                    hindsight: true,
                },
                activity_lease_ms: 30000,
                pending_timeout_ms: 30000,
                hook_timeout_ms: 12000,
            };
            let mut registry = Registry::memory().unwrap();
            registry
                .inventory(
                    Backend::Herdr,
                    "example",
                    &[routes::Terminal {
                        identity: terminal.clone(),
                        label: "Agent".into(),
                        workspace: "project".into(),
                        harness: "claude".into(),
                        conversation: Some("session".into()),
                        available: true,
                    }],
                    true,
                    true,
                )
                .unwrap();
            registry
                .set_grant(
                    &route_scope,
                    "3",
                    &[Permission::Read, Permission::Send, Permission::Manage],
                )
                .unwrap();
            registry
                .select(
                    &route_scope,
                    "3",
                    &terminal,
                    std::slice::from_ref(&terminal),
                    0,
                    true,
                    false,
                )
                .unwrap();
            Self {
                registration,
                binding: Binding {
                    registration_id: "hook".into(),
                    goal_scope: scope,
                    route_scope,
                    actor: "3".into(),
                    route_revision: 1,
                    reply_to: Some(4),
                    hindsight_bank: Some("memory".into()),
                },
                registry,
                journal: Journal::memory().unwrap(),
            }
        }
        fn event(&mut self, final_text: Option<&str>) -> Event {
            let payload = json!({"registration_id":"hook","scope":self.registration.scope,"event":"Stop","transcript":self.registration.transcript,"effects":["queue_final_reply","finish_progress"],"final_message":final_text,"disposition":"planned_not_delivered"});
            self.journal
                .admit("hooks", "example", "event-1", &payload, 1.0)
                .unwrap();
            self.journal
                .events("hooks", "example", &["admitted"])
                .unwrap()
                .remove(0)
        }
    }
    #[test]
    fn captured_final_text_splits_unicode_and_creates_plans_without_delivery() {
        let mut f = Fixture::new();
        let message = "😀".repeat(3000);
        let event = f.event(Some(&message));
        let result = expand(
            &mut f.journal,
            &f.registry,
            &f.registration,
            &f.binding,
            &event,
            2.0,
        )
        .unwrap();
        assert_eq!(result.deliveries.len(), 2);
        assert_eq!(result.effect_plans, 1);
        let row = f
            .journal
            .begin("outbound", "telegram:1:2", "test", 3.0, 30.0)
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "intent");
        assert!(row.payload["text"].as_str().unwrap().encode_utf16().count() <= 4096);
        authorize_delivery(&row, &f.registry).unwrap();
        assert!(
            f.journal
                .events("hooks", "example", &["admitted"])
                .unwrap()
                .is_empty()
        );
        let plans = f
            .journal
            .events("hook-effects", "example", &["admitted"])
            .unwrap();
        assert_eq!(plans.len(), 1);
        assert!(
            plans
                .iter()
                .all(|e| e.payload["state"] == "planned_not_delivered")
        );
        f.registry
            .set_grant(&f.binding.route_scope, "3", &[])
            .unwrap();
        assert!(authorize_delivery(&row, &f.registry).is_err());
    }
    #[test]
    fn wrong_actor_scope_route_or_forged_event_never_queues_output() {
        let mut f = Fixture::new();
        let event = f.event(Some("answer"));
        let mut binding = f.binding.clone();
        binding.actor = "intruder".into();
        assert!(
            expand(
                &mut f.journal,
                &f.registry,
                &f.registration,
                &binding,
                &event,
                2.0
            )
            .is_err()
        );
        binding = f.binding.clone();
        binding.route_revision = 2;
        assert!(
            expand(
                &mut f.journal,
                &f.registry,
                &f.registration,
                &binding,
                &event,
                2.0
            )
            .is_err()
        );
        let mut forged = event.clone();
        forged.payload["final_message"] = "injected".into();
        assert!(
            expand(
                &mut f.journal,
                &f.registry,
                &f.registration,
                &f.binding,
                &forged,
                2.0
            )
            .is_err()
        );
        assert!(
            f.journal
                .begin("outbound", "telegram:1:2", "test", 3.0, 30.0)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn absent_captured_final_defers_instead_of_reading_later_transcript() {
        let mut f = Fixture::new();
        let event = f.event(None);
        let result = expand(
            &mut f.journal,
            &f.registry,
            &f.registration,
            &f.binding,
            &event,
            2.0,
        )
        .unwrap();
        assert!(result.deferred);
        assert!(result.deliveries.is_empty());
        assert_eq!(
            f.journal
                .events("hooks", "example", &["admitted"])
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn fanout_retries_partial_work_and_handles_only_after_all_recipients() {
        let mut f = Fixture::new();
        let event = f.event(Some("final answer"));
        let mut second = f.binding.clone();
        second.route_scope.chat = "9".into();
        f.registry
            .set_grant(
                &second.route_scope,
                &second.actor,
                &[Permission::Read, Permission::Send, Permission::Manage],
            )
            .unwrap();
        f.registry
            .select(
                &second.route_scope,
                &second.actor,
                &second.goal_scope.terminal,
                std::slice::from_ref(&second.goal_scope.terminal),
                0,
                true,
                false,
            )
            .unwrap();
        let before = expand_one(
            &mut f.journal,
            &f.registry,
            &f.registration,
            &f.binding,
            &event,
            2.0,
        )
        .unwrap();
        assert_eq!(
            f.journal
                .events("hooks", "example", &["admitted"])
                .unwrap()
                .len(),
            1
        );
        let result = expand_many(
            &mut f.journal,
            &f.registry,
            &f.registration,
            &[f.binding.clone(), second],
            &event,
            3.0,
        )
        .unwrap();
        assert_eq!(result.deliveries.len(), 2);
        assert_eq!(before.deliveries[0], result.deliveries[0]);
        assert_ne!(result.deliveries[0], result.deliveries[1]);
        assert!(
            f.journal
                .events("hooks", "example", &["admitted"])
                .unwrap()
                .is_empty()
        );
        assert!(
            f.journal
                .begin("outbound", "telegram:1:2", "test", 4.0, 30.0)
                .unwrap()
                .is_some()
        );
        assert!(
            f.journal
                .begin("outbound", "telegram:1:9", "test", 4.0, 30.0)
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn revoked_fanout_recipient_prevents_any_enqueue_and_preserves_pending() {
        let mut f = Fixture::new();
        let event = f.event(Some("final answer"));
        let mut revoked = f.binding.clone();
        revoked.route_scope.chat = "9".into();
        assert!(
            expand_many(
                &mut f.journal,
                &f.registry,
                &f.registration,
                &[f.binding.clone(), revoked],
                &event,
                2.0
            )
            .is_err()
        );
        assert!(
            f.journal
                .begin("outbound", "telegram:1:2", "test", 3.0, 30.0)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            f.journal
                .events("hooks", "example", &["admitted"])
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn retry_after_partial_enqueue_uses_identical_delivery_key() {
        let mut f = Fixture::new();
        let event = f.event(Some("final text"));
        let key = format!(
            "{}:text:0",
            recipient_key(&f.registration, &f.binding, &event).unwrap()
        );
        let origin = json!({"kind":"verified_hook","binding":f.binding,"event_id":event.event_id,"source_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&event.payload).unwrap())),"source":"native_hook_payload"});
        let existing = f
            .journal
            .enqueue(
                "outbound",
                "telegram:1:2",
                &json!({"chat":2,"text":"final text","reply":4}),
                &key,
                Some(&origin),
                1.0,
            )
            .unwrap();
        let result = expand(
            &mut f.journal,
            &f.registry,
            &f.registration,
            &f.binding,
            &event,
            2.0,
        )
        .unwrap();
        assert_eq!(result.deliveries, vec![existing]);
    }
}
