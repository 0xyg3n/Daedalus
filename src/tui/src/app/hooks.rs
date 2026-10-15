//! Bounded lifecycle normalization and goal response formatting.
//!
//! This module neither authenticates a process nor sends Telegram/memory requests.
//! Callers authorize the emitter, persist returned plans and enforce tool policy.
use crate::core::goals::{Binding, Decision, Goal};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Read;

pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_ID_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    Claude,
    Codex,
    Grok,
    Cursor,
    Hermes,
    Pi,
    Opencode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
    PreCompact,
    SessionEnd,
    SubagentStop,
    Notification,
    AssistantResponse,
    Activity,
    Settled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InputTooLarge,
    InputIo,
    InvalidJson,
    InvalidObject,
    MissingField(&'static str),
    InvalidField(&'static str),
    UnsupportedEvent,
    IdentityMismatch,
    WrongGoalEvent,
    UnsupportedGoalHarness,
    Goal(crate::core::goals::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub harness: Harness,
    pub kind: EventKind,
    pub binding: Binding,
    /// Untrusted path hint: an adapter must verify its scope and session metadata.
    pub transcript_path: Option<String>,
    pub tool_name: Option<String>,
    pub stop_hook_active: bool,
    /// Grok continuation is meaningful only at a native turn-end gate.
    pub grok_turn_gate: bool,
}

/// Reads at most limit+1 bytes. Never read the whole stream then apply its limit.
pub fn read_payload(reader: impl Read) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_PAYLOAD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::InputIo)?;
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(Error::InputTooLarge);
    }
    Ok(bytes)
}

fn string_field<'a>(value: &'a Value, key: &'static str) -> Result<Option<&'a str>, Error> {
    match value.get(key) {
        None => Ok(None),
        Some(Value::String(s)) if !s.contains('\0') => Ok(Some(s)),
        _ => Err(Error::InvalidField(key)),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && !id
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '/' || c == '\\')
        && id != "."
        && id != ".."
}

/// Normalizes each supported native harness lifecycle envelope.
/// `expected` comes from authenticated registration, never from this payload.
/// Notification-only `agent-turn-complete` envelopes are not treated as Stop hooks.
pub fn parse(harness: Harness, bytes: &[u8], expected: &Binding) -> Result<Event, Error> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(Error::InputTooLarge);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Error::InvalidJson)?;
    if !value.is_object() {
        return Err(Error::InvalidObject);
    }
    let (session_key, event_key, tool_key) = if harness == Harness::Grok {
        ("sessionId", "hookEventName", "toolName")
    } else if harness == Harness::Cursor {
        ("conversation_id", "hook_event_name", "tool_name")
    } else {
        ("session_id", "hook_event_name", "tool_name")
    };
    let sid = string_field(&value, session_key)?.ok_or(Error::MissingField(session_key))?;
    if !valid_id(sid) {
        return Err(Error::InvalidField("session_id"));
    }
    if expected.terminal.trim().is_empty() || sid != expected.conversation {
        return Err(Error::IdentityMismatch);
    }
    let kind = native_kind(harness, &value)?;
    if harness == Harness::Grok
        && !matches!(kind, EventKind::SubagentStop | EventKind::Notification)
        && value.get("subagentType").is_some_and(|v| !v.is_null())
    {
        return Err(Error::IdentityMismatch);
    }
    if matches!(harness, Harness::Pi | Harness::Opencode)
        && (value.get("bridge_version").and_then(Value::as_str)
            != Some(if harness == Harness::Pi {
                "daedalus.pi.v1"
            } else {
                "daedalus.opencode.v2"
            })
            || !string_field(&value, "event_id")?.is_some_and(valid_id))
    {
        return Err(Error::InvalidField("bridge_version"));
    }
    let _ = event_key;
    if harness == Harness::Cursor
        && matches!(
            kind,
            EventKind::Stop | EventKind::UserPromptSubmit | EventKind::AssistantResponse
        )
        && !string_field(&value, "generation_id")?.is_some_and(valid_id)
    {
        return Err(Error::InvalidField("generation_id"));
    }
    let stop_hook_active = match value.get(if harness == Harness::Grok {
        "stopHookActive"
    } else {
        "stop_hook_active"
    }) {
        None => false,
        Some(Value::Bool(active)) => *active,
        _ => return Err(Error::InvalidField("stop_hook_active")),
    };
    Ok(Event {
        harness,
        kind,
        binding: expected.clone(),
        transcript_path: if value.get("transcript_path").is_some_and(Value::is_null) {
            None
        } else {
            string_field(&value, "transcript_path")?.map(str::to_owned)
        },
        tool_name: if value.get(tool_key).is_some_and(Value::is_null) {
            None
        } else {
            string_field(&value, tool_key)?.map(str::to_owned)
        },
        stop_hook_active,
        grok_turn_gate: harness == Harness::Grok
            && kind == EventKind::Stop
            && value.get("reason").and_then(Value::as_str) == Some("end_turn")
            && value
                .get("promptId")
                .and_then(Value::as_str)
                .is_some_and(valid_id)
            && value.get("stopHookActive").is_some_and(Value::is_boolean)
            && value.get("subagentType").is_none_or(Value::is_null),
    })
}

/// Provider event names are normalized independently of CLI wrapper names.
pub fn native_kind(harness: Harness, value: &Value) -> Result<EventKind, Error> {
    let key = if harness == Harness::Grok {
        "hookEventName"
    } else {
        "hook_event_name"
    };
    let name = string_field(value, key)?.ok_or(Error::MissingField(key))?;
    if harness == Harness::Grok {
        return match name {
            "session_start" | "SessionStart" => Ok(EventKind::SessionStart),
            "session_end" | "SessionEnd" => Ok(EventKind::SessionEnd),
            "user_prompt_submit" | "UserPromptSubmit" => Ok(EventKind::UserPromptSubmit),
            "pre_tool_use" | "PreToolUse" => Ok(EventKind::PreToolUse),
            "post_tool_use" | "PostToolUse" => Ok(EventKind::PostToolUse),
            "stop" | "Stop" => Ok(EventKind::Stop),
            "pre_compact" | "PreCompact" => Ok(EventKind::PreCompact),
            "subagent_stop" | "SubagentStop" => Ok(EventKind::SubagentStop),
            "notification" | "Notification"
                if value.get("notificationType").and_then(Value::as_str) == Some("idle_prompt") =>
            {
                Ok(EventKind::Settled)
            }
            "notification" | "Notification" => Ok(EventKind::Notification),
            _ => Err(Error::UnsupportedEvent),
        };
    }
    if harness == Harness::Hermes {
        return match name {
            "on_session_start" => Ok(EventKind::SessionStart),
            "on_session_finalize" => Ok(EventKind::SessionEnd),
            "pre_llm_call" => Ok(EventKind::UserPromptSubmit),
            "post_llm_call" => Ok(EventKind::Stop),
            "pre_tool_call" => Ok(EventKind::PreToolUse),
            "post_tool_call" => Ok(EventKind::PostToolUse),
            "subagent_stop" => Ok(EventKind::SubagentStop),
            _ => Err(Error::UnsupportedEvent),
        };
    }
    if harness == Harness::Cursor {
        return match name {
            "sessionStart" => Ok(EventKind::SessionStart),
            "sessionEnd" => Ok(EventKind::SessionEnd),
            "beforeSubmitPrompt" => Ok(EventKind::UserPromptSubmit),
            "preToolUse" => Ok(EventKind::PreToolUse),
            "postToolUse" => Ok(EventKind::PostToolUse),
            "stop" => Ok(EventKind::Stop),
            "preCompact" => Ok(EventKind::PreCompact),
            "subagentStop" => Ok(EventKind::SubagentStop),
            "afterAgentResponse" => Ok(EventKind::AssistantResponse),
            _ => Err(Error::UnsupportedEvent),
        };
    }
    serde_json::from_value(serde_json::json!(name)).map_err(|_| Error::UnsupportedEvent)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    pub telegram_delivery: bool,
    pub telegram_progress: bool,
    pub notifications: bool,
    pub hindsight: bool,
}

/// Requests for the application layer, not claims that these operations happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    RegisterIdentity,
    RecordActivity,
    CheckToolAuthorization,
    StartProgress,
    UpdateProgress,
    FinishProgress,
    QueueFinalReply,
    QueueAssistantReply,
    Notify,
    RecallMemory,
    PersistMemory,
}

pub fn dispatch(event: &Event, features: Features) -> Vec<Effect> {
    let mut effects = Vec::new();
    match event.kind {
        EventKind::SessionStart => {
            effects.push(Effect::RegisterIdentity);
            if features.hindsight {
                effects.push(Effect::RecallMemory);
            }
        }
        EventKind::UserPromptSubmit => {
            effects.push(Effect::RecordActivity);
            if features.telegram_progress {
                effects.push(Effect::StartProgress);
            }
        }
        EventKind::PreToolUse | EventKind::PostToolUse | EventKind::Activity => {
            if event.kind == EventKind::PreToolUse {
                effects.push(Effect::CheckToolAuthorization);
            }
            effects.push(Effect::RecordActivity);
            if features.telegram_progress {
                effects.push(Effect::UpdateProgress);
            }
        }
        EventKind::Stop => {
            if features.telegram_delivery
                && !matches!(
                    event.harness,
                    Harness::Cursor | Harness::Pi | Harness::Opencode
                )
            {
                effects.push(Effect::QueueFinalReply);
            }
            if features.telegram_progress {
                effects.push(Effect::FinishProgress);
            }
        }
        EventKind::PreCompact | EventKind::SessionEnd => {
            if features.hindsight {
                effects.push(Effect::PersistMemory);
            }
        }
        EventKind::Settled => {
            if features.telegram_progress {
                effects.push(Effect::FinishProgress);
            }
        }
        EventKind::AssistantResponse => {
            if features.telegram_delivery {
                effects.push(Effect::QueueAssistantReply);
            }
        }
        EventKind::SubagentStop | EventKind::Notification => {
            if features.notifications {
                effects.push(Effect::Notify);
            }
        }
    }
    effects
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoalOutput {
    /// Write no JSON to hook stdout.
    Empty,
    /// Persist the continuation intent before writing this JSON to hook stdout.
    Json(Value),
    /// Do not return success/allow: hold the hook and recheck at this deadline.
    WaitUntil(u64),
    /// Recover the existing event's durable wire response; do not invoke again.
    ReplayReceipt,
    /// Surface this to diagnostics; the adapter must resolve its execution policy.
    Blocked(String),
}

/// Claude's existing goal hook uses `decision:block` with a continuation reason.
/// Claude, Codex and verified Grok turn-end gates use decision:block.
/// Grok teardown or unverified Stop payloads cannot continue a goal.
/// Do not emit Claude response JSON to other harnesses merely because inputs match.
pub fn goal_response(
    event: &Event,
    goal: Option<&mut Goal>,
    sequence: u64,
    now: u64,
) -> Result<GoalOutput, Error> {
    if event.kind != EventKind::Stop {
        return Err(Error::WrongGoalEvent);
    }
    let Some(goal) = goal else {
        return Ok(GoalOutput::Empty);
    };
    if !matches!(
        event.harness,
        Harness::Claude | Harness::Codex | Harness::Cursor | Harness::Pi | Harness::Grok
    ) || (event.harness == Harness::Grok && !event.grok_turn_gate)
    {
        return Err(Error::UnsupportedGoalHarness);
    }
    match goal
        .on_stop(&event.binding, sequence, now)
        .map_err(Error::Goal)?
    {
        Decision::AllowStop => Ok(GoalOutput::Empty),
        Decision::Duplicate => Ok(GoalOutput::ReplayReceipt),
        Decision::WaitUntil(deadline) => Ok(GoalOutput::WaitUntil(deadline)),
        Decision::Blocked(reason) => Ok(GoalOutput::Blocked(reason)),
        Decision::Continue { directive, .. } => {
            let reason = format!(
                "[GOAL LOOP] Standing goal: {directive}\nTake the next concrete step toward the goal. Keep the operator informed of real progress; never fabricate it. Release the goal only when it is achieved or the operator asks you to stop. Normal tool permissions and approval requirements remain in force."
            );
            Ok(GoalOutput::Json(if event.harness == Harness::Pi {
                json!({"continue":true,"entries":[{"type":"custom_message","customType":"daedalus-goal","content":reason,"display":false}]})
            } else if event.harness == Harness::Cursor {
                json!({"followup_message":reason})
            } else {
                json!({"decision":"block","reason":reason})
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grok_native_snake_case_lifecycle_is_normalized() {
        for (native, expected) in [
            ("session_start", EventKind::SessionStart),
            ("session_end", EventKind::SessionEnd),
            ("user_prompt_submit", EventKind::UserPromptSubmit),
            ("post_tool_use", EventKind::PostToolUse),
            ("stop", EventKind::Stop),
        ] {
            assert_eq!(event(Harness::Grok, native).kind, expected);
        }
    }
    fn binding() -> Binding {
        Binding {
            terminal: "tmux:server:%2:generation-1".into(),
            conversation: "session-a".into(),
        }
    }
    fn event(harness: Harness, name: &str) -> Event {
        parse(
            harness,
            &serde_json::to_vec(&if harness == Harness::Grok {
                json!({"sessionId":"session-a", "hookEventName":name})
            } else {
                json!({"session_id":"session-a", "hook_event_name":name})
            })
            .unwrap(),
            &binding(),
        )
        .unwrap()
    }

    #[test]
    fn normalizes_observed_shared_envelopes_without_inventing_aliases() {
        for harness in [Harness::Claude, Harness::Codex, Harness::Grok] {
            assert_eq!(event(harness, "SessionStart").kind, EventKind::SessionStart);
            assert_eq!(event(harness, "SessionEnd").kind, EventKind::SessionEnd);
        }
        let data = br#"{"type":"agent-turn-complete","thread-id":"session-a"}"#;
        assert_eq!(
            parse(Harness::Codex, data, &binding()),
            Err(Error::MissingField("session_id"))
        );
    }
    #[test]
    fn malformed_and_oversized_inputs_fail_without_raw_payload_errors() {
        assert_eq!(
            parse(Harness::Claude, b"[]", &binding()),
            Err(Error::InvalidObject)
        );
        assert_eq!(
            parse(Harness::Claude, b"secret is not json", &binding()),
            Err(Error::InvalidJson)
        );
        assert_eq!(
            read_payload(std::io::repeat(b' ')),
            Err(Error::InputTooLarge)
        );
        assert_eq!(
            parse(
                Harness::Claude,
                &vec![b' '; MAX_PAYLOAD_BYTES + 1],
                &binding()
            ),
            Err(Error::InputTooLarge)
        );
    }
    #[test]
    fn identity_mismatch_and_traversal_are_rejected() {
        for (sid, expected_error) in [
            ("other", Error::IdentityMismatch),
            ("../escape", Error::InvalidField("session_id")),
            ("", Error::InvalidField("session_id")),
        ] {
            let bytes =
                serde_json::to_vec(&json!({"session_id":sid,"hook_event_name":"Stop"})).unwrap();
            assert_eq!(
                parse(Harness::Claude, &bytes, &binding()),
                Err(expected_error)
            );
        }
    }
    #[test]
    fn unsupported_event_and_wrong_types_are_explicit() {
        let unknown = br#"{"session_id":"session-a","hook_event_name":"Other"}"#;
        assert_eq!(
            parse(Harness::Claude, unknown, &binding()),
            Err(Error::UnsupportedEvent)
        );
        let invalid =
            br#"{"session_id":"session-a","hook_event_name":"Stop","stop_hook_active":"true"}"#;
        assert_eq!(
            parse(Harness::Claude, invalid, &binding()),
            Err(Error::InvalidField("stop_hook_active"))
        );
    }
    #[test]
    fn optional_network_effects_are_off_and_security_check_is_first() {
        assert!(dispatch(&event(Harness::Claude, "Stop"), Features::default()).is_empty());
        assert_eq!(
            dispatch(&event(Harness::Claude, "PreToolUse"), Features::default()),
            vec![Effect::CheckToolAuthorization, Effect::RecordActivity]
        );
        assert_eq!(
            dispatch(
                &event(Harness::Grok, "PreCompact"),
                Features {
                    hindsight: true,
                    ..Features::default()
                }
            ),
            vec![Effect::PersistMemory]
        );
    }
    #[test]
    fn goal_response_preserves_claude_contract_and_requires_receipt_on_duplicate() {
        let e = event(Harness::Claude, "Stop");
        let mut goal = Goal::arm("finish tests", binding()).unwrap();
        let GoalOutput::Json(value) = goal_response(&e, Some(&mut goal), 1, 0).unwrap() else {
            panic!("missing continuation")
        };
        assert_eq!(value["decision"], "block");
        assert!(value["reason"].as_str().unwrap().contains("finish tests"));
        assert_eq!(
            goal_response(&e, Some(&mut goal), 1, 0),
            Ok(GoalOutput::ReplayReceipt)
        );
    }
    #[test]
    fn unverified_harness_goal_output_does_not_mutate_goal() {
        let mut goal = Goal::arm("finish tests", binding()).unwrap();
        let original = goal.clone();
        {
            let harness = Harness::Grok;
            assert_eq!(
                goal_response(&event(harness, "Stop"), Some(&mut goal), 1, 0),
                Err(Error::UnsupportedGoalHarness)
            );
            assert_eq!(goal, original);
        }
    }
    #[test]
    fn cooldown_is_not_silently_translated_to_allow_stop() {
        let e = event(Harness::Claude, "Stop");
        let mut goal = Goal::arm("finish tests", binding()).unwrap();
        goal_response(&e, Some(&mut goal), 1, 0).unwrap();
        goal.acknowledge_activity(&binding(), 1, 1, 30_000).unwrap();
        assert_eq!(
            goal_response(&e, Some(&mut goal), 2, 2),
            Ok(GoalOutput::WaitUntil(8_000))
        );
        goal.complete();
        assert_eq!(
            goal_response(&e, Some(&mut goal), 2, 8_000),
            Ok(GoalOutput::Empty)
        );
    }
}
