//! Goal continuation policy. Transport and storage must persist a transition before
//! acknowledging its event; this module performs no I/O or background execution.
use serde::{Deserialize, Serialize};

pub const COOLDOWN_MS: u64 = 8_000;
pub const MAX_DIRECTIVE_BYTES: usize = 64 * 1024;

/// A stable terminal identity includes its backend/server/process generation.
/// Conversation identity is separate; labels and pane titles are not identities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub terminal: String,
    pub conversation: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    Armed,
    Pending {
        event_sequence: u64,
        requested_at: u64,
    },
    Running {
        lease_until: u64,
    },
    Blocked {
        reason: String,
    },
    Paused,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub directive: String,
    pub binding: Binding,
    pub status: Status,
    pub last_sequence: u64,
    pub last_continuation: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    EmptyDirective,
    DirectiveTooLarge,
    InvalidBinding,
    WrongBinding,
    InvalidSequence,
    InvalidLease,
    NotPending,
    NotResumable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Disabled/paused/completed: no continuation and no implicit reactivation.
    AllowStop,
    /// The event was already applied. The transport uses its durable receipt.
    Duplicate,
    /// Keep the hook pending; revalidate release/binding after this deadline.
    WaitUntil(u64),
    Continue {
        directive: String,
        event_sequence: u64,
    },
    /// Never invent a continuation when execution outcome is unknown.
    Blocked(String),
}

impl Goal {
    pub fn arm(directive: &str, binding: Binding) -> Result<Self, Error> {
        if binding.terminal.trim().is_empty() || binding.conversation.trim().is_empty() {
            return Err(Error::InvalidBinding);
        }
        let directive = legacy_directive(directive).ok_or(Error::EmptyDirective)?;
        if directive.len() > MAX_DIRECTIVE_BYTES {
            return Err(Error::DirectiveTooLarge);
        }
        Ok(Self {
            directive: directive.to_owned(),
            binding,
            status: Status::Armed,
            last_sequence: 0,
            last_continuation: None,
        })
    }

    /// Called with a durable, monotonically increasing per-goal event sequence.
    /// A cooldown response does not consume the event: retry it at the deadline.
    /// The caller must separately authorize the emitter and verify binding liveness.
    pub fn on_stop(
        &mut self,
        binding: &Binding,
        sequence: u64,
        now: u64,
    ) -> Result<Decision, Error> {
        if binding != &self.binding {
            return Err(Error::WrongBinding);
        }
        if sequence == 0 {
            return Err(Error::InvalidSequence);
        }
        match &self.status {
            Status::Paused | Status::Completed => return Ok(Decision::AllowStop),
            Status::Blocked { reason } => return Ok(Decision::Blocked(reason.clone())),
            _ => {}
        }
        if sequence <= self.last_sequence {
            return Ok(
                if sequence == self.last_sequence
                    && matches!(self.status, Status::Pending { .. } | Status::Running { .. })
                {
                    Decision::Duplicate
                } else {
                    Decision::Blocked("Stale continuation event must not be replayed".into())
                },
            );
        }
        if let Status::Pending { .. } = &self.status {
            return Ok(Decision::Blocked(
                "Continuation acknowledgement is pending".into(),
            ));
        }
        if let Some(last) = self.last_continuation {
            let deadline = last.saturating_add(COOLDOWN_MS);
            if now < deadline {
                return Ok(Decision::WaitUntil(deadline));
            }
        }
        self.last_sequence = sequence;
        self.last_continuation = Some(now);
        self.status = Status::Pending {
            event_sequence: sequence,
            requested_at: now,
        };
        Ok(Decision::Continue {
            directive: self.directive.clone(),
            event_sequence: sequence,
        })
    }

    /// Only authenticated activity correlated to this continuation proves it runs.
    /// Receipt of the hook response alone is not evidence of execution.
    pub fn acknowledge_activity(
        &mut self,
        binding: &Binding,
        sequence: u64,
        now: u64,
        lease_ms: u64,
    ) -> Result<(), Error> {
        if binding != &self.binding {
            return Err(Error::WrongBinding);
        }
        if lease_ms == 0 || now.checked_add(lease_ms).is_none() {
            return Err(Error::InvalidLease);
        }
        match self.status {
            Status::Pending { event_sequence, .. } if event_sequence == sequence => {}
            Status::Running { .. } if self.last_sequence == sequence => {}
            _ => return Err(Error::NotPending),
        }
        self.status = Status::Running {
            lease_until: now + lease_ms,
        };
        Ok(())
    }

    /// Lack of activity is uncertainty, not completion or permission to replay.
    pub fn expire(&mut self, now: u64, pending_timeout_ms: u64) {
        let reason = match self.status {
            Status::Running { lease_until } if now >= lease_until => {
                Some("Execution activity expired; inspect the session before resuming")
            }
            Status::Pending { requested_at, .. }
                if now >= requested_at.saturating_add(pending_timeout_ms) =>
            {
                Some("Continuation was requested but execution was not confirmed")
            }
            _ => None,
        };
        if let Some(reason) = reason {
            self.status = Status::Blocked {
                reason: reason.into(),
            };
        }
    }

    /// On service restart do not claim persisted execution is still alive.
    /// An adapter may reconcile against the exact bound session before resuming.
    pub fn recover(&mut self) {
        if matches!(self.status, Status::Running { .. } | Status::Pending { .. }) {
            self.status = Status::Blocked {
                reason: "Service restarted; continuation requires session reconciliation".into(),
            };
        }
    }

    pub fn pause(&mut self) {
        if self.status != Status::Completed {
            self.status = Status::Paused;
        }
    }

    pub fn complete(&mut self) {
        self.status = Status::Completed;
    }

    /// Explicit operator resume or verified adapter reconciliation; never timer-driven.
    pub fn resume(&mut self, binding: &Binding) -> Result<(), Error> {
        if binding != &self.binding {
            return Err(Error::WrongBinding);
        }
        if !matches!(self.status, Status::Paused | Status::Blocked { .. }) {
            return Err(Error::NotResumable);
        }
        self.status = Status::Armed;
        Ok(())
    }
}

/// Legacy release markers use their first whitespace-delimited token.
pub fn legacy_directive(text: &str) -> Option<&str> {
    let text = text.trim();
    let first = text.split_whitespace().next()?;
    if ["done", "clear", "off", "stop", "release", "none"]
        .iter()
        .any(|word| first.eq_ignore_ascii_case(word))
    {
        None
    } else {
        Some(text)
    }
}

/// An existing empty/released window flag overrides an active global flag.
/// Import resolves the window to a stable identity before arming the result.
pub fn legacy_effective<'a>(window: Option<&'a str>, global: Option<&'a str>) -> Option<&'a str> {
    window.or(global).and_then(legacy_directive)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding {
        Binding {
            terminal: "tmux:test:%1:generation-1".into(),
            conversation: "session-a".into(),
        }
    }
    fn goal() -> Goal {
        Goal::arm("Finish verified work", binding()).unwrap()
    }

    #[test]
    fn armed_is_not_running_and_stop_needs_activity_acknowledgement() {
        let mut g = goal();
        assert_eq!(g.status, Status::Armed);
        assert!(matches!(
            g.on_stop(&binding(), 1, 0).unwrap(),
            Decision::Continue { .. }
        ));
        assert!(matches!(g.status, Status::Pending { .. }));
        g.acknowledge_activity(&binding(), 1, 1, 30_000).unwrap();
        assert_eq!(
            g.status,
            Status::Running {
                lease_until: 30_001
            }
        );
    }

    #[test]
    fn duplicate_event_survives_serialization_without_reexecution() {
        let mut g = goal();
        g.on_stop(&binding(), 4, 0).unwrap();
        let mut restored: Goal = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(
            restored.on_stop(&binding(), 4, 20_000).unwrap(),
            Decision::Duplicate
        );
        restored.recover();
        assert!(matches!(
            restored.on_stop(&binding(), 5, 20_000).unwrap(),
            Decision::Blocked(_)
        ));
    }

    #[test]
    fn cooldown_can_be_released_before_retry() {
        let mut g = goal();
        g.on_stop(&binding(), 1, 0).unwrap();
        g.acknowledge_activity(&binding(), 1, 0, 30_000).unwrap();
        assert_eq!(
            g.on_stop(&binding(), 2, 1).unwrap(),
            Decision::WaitUntil(COOLDOWN_MS)
        );
        g.complete();
        assert_eq!(
            g.on_stop(&binding(), 2, COOLDOWN_MS).unwrap(),
            Decision::AllowStop
        );
        assert_eq!(g.resume(&binding()), Err(Error::NotResumable));
    }

    #[test]
    fn cooldown_is_per_goal_and_does_not_consume_event() {
        let mut g = goal();
        g.on_stop(&binding(), 1, 0).unwrap();
        g.acknowledge_activity(&binding(), 1, 0, 30_000).unwrap();
        g.on_stop(&binding(), 2, 1).unwrap();
        assert_eq!(g.last_sequence, 1);
        assert!(matches!(
            goal().on_stop(&binding(), 1, 1).unwrap(),
            Decision::Continue { .. }
        ));
        assert!(matches!(
            g.on_stop(&binding(), 2, COOLDOWN_MS).unwrap(),
            Decision::Continue { .. }
        ));
    }

    #[test]
    fn wrong_conversation_and_reused_pane_are_rejected_without_mutation() {
        let mut g = goal();
        let original = g.clone();
        for b in [
            Binding {
                conversation: "other".into(),
                ..binding()
            },
            Binding {
                terminal: "tmux:test:%1:generation-2".into(),
                ..binding()
            },
        ] {
            assert_eq!(g.on_stop(&b, 1, 0), Err(Error::WrongBinding));
            assert_eq!(g, original);
        }
    }

    #[test]
    fn stale_acknowledgement_cannot_reactivate_paused_goal() {
        let mut g = goal();
        g.on_stop(&binding(), 1, 0).unwrap();
        g.pause();
        assert_eq!(
            g.acknowledge_activity(&binding(), 1, 1, 30),
            Err(Error::NotPending)
        );
        assert_eq!(g.status, Status::Paused);
    }

    #[test]
    fn missing_activity_and_restart_do_not_claim_running() {
        let mut g = goal();
        g.on_stop(&binding(), 1, 0).unwrap();
        g.expire(1_000, 1_000);
        assert!(matches!(g.status, Status::Blocked { .. }));
        g.resume(&binding()).unwrap();
        g.on_stop(&binding(), 2, COOLDOWN_MS).unwrap();
        g.acknowledge_activity(&binding(), 2, COOLDOWN_MS, 30)
            .unwrap();
        g.expire(COOLDOWN_MS + 30, 1_000);
        assert!(matches!(g.status, Status::Blocked { .. }));
        let mut waiting = goal();
        waiting.recover();
        assert_eq!(waiting.status, Status::Armed);
    }

    #[test]
    fn release_overrides_global_and_preserves_multiline_directive() {
        assert_eq!(
            legacy_effective(Some("DONE\nold text"), Some("continue")),
            None
        );
        assert_eq!(legacy_effective(Some(""), Some("continue")), None);
        assert_eq!(legacy_effective(None, Some("one\ntwo")), Some("one\ntwo"));
        for value in ["done", "clear", "off", "stop", "release", "none", "  "] {
            assert_eq!(legacy_directive(value), None);
        }
    }

    #[test]
    fn release_pause_and_recovery_override_duplicate_receipts() {
        let mut original = goal();
        original.on_stop(&binding(), 1, 0).unwrap();
        let mut paused = original.clone();
        paused.pause();
        assert_eq!(
            paused.on_stop(&binding(), 1, 10).unwrap(),
            Decision::AllowStop
        );
        paused.resume(&binding()).unwrap();
        assert!(matches!(
            paused.on_stop(&binding(), 1, 10).unwrap(),
            Decision::Blocked(_)
        ));
        let mut completed = original.clone();
        completed.complete();
        assert_eq!(
            completed.on_stop(&binding(), 1, 10).unwrap(),
            Decision::AllowStop
        );
        original.recover();
        assert!(matches!(
            original.on_stop(&binding(), 1, 10).unwrap(),
            Decision::Blocked(_)
        ));
    }
}
