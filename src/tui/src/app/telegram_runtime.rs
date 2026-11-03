//! Saved Telegram transport and service behavior. Identity and retry safety remain enforced.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub api_origin: String,
    pub feedback_typing_enabled: bool,
    pub feedback_ack_enabled: bool,
    pub feedback_commands_enabled: bool,
    pub feedback_queue_limit: u64,
    pub feedback_typing_interval_ms: u64,
    pub feedback_typing_ttl_ms: u64,
    pub feedback_ack_ttl_ms: u64,
    pub feedback_commands_ttl_ms: u64,
    pub progress_interval_ms: u64,
    pub progress_batch_limit: u64,
    pub stream_event_batch_limit: u64,
    pub stream_input_retention: u64,
    pub stream_subscription_limit: u64,
    pub stream_text_limit_bytes: u64,
    pub stream_chunk_utf16_limit: u64,

    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub feedback_connect_timeout_ms: u64,
    pub feedback_timeout_ms: u64,
    pub pairing_connect_timeout_ms: u64,
    pub pairing_timeout_ms: u64,
    pub poll_timeout_seconds: u64,
    pub poll_timeout_slack_ms: u64,
    pub poll_request_min_ms: u64,
    pub poll_retry_ms: u64,
    pub startup_retry_initial_ms: u64,
    pub startup_retry_max_ms: u64,
    pub loop_interval_ms: u64,
    pub stop_poll_ms: u64,
    pub policy_reload_ms: u64,
    pub outbound_batch_limit: u64,
    pub inbound_batch_limit: u64,
    pub hook_batch_limit: u64,
    pub updates_batch_limit: u64,
    pub json_limit_bytes: u64,
    pub file_limit_bytes: u64,
    pub stream_message_limit_per_event: u64,
    pub outbound_retry_ms: u64,
    pub outbound_lease_ms: u64,
    pub inbound_lease_ms: u64,
    pub feeder_retry_ms: u64,
    pub stage_batch_limit: u64,
    pub control_batch_limit: u64,
    pub update_limit_bytes: u64,
    pub input_text_limit_bytes: u64,
    pub callback_ttl_ms: u64,
    pub callback_busy_timeout_ms: u64,
    pub feeder_registration_limit: u64,
    pub feeder_prompt_limit_bytes: u64,
    pub feedback_ack_buffer_limit: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            api_origin: "https://api.telegram.org".into(),
            feedback_typing_enabled: true,
            feedback_ack_enabled: true,
            feedback_commands_enabled: true,
            feedback_queue_limit: 64,
            feedback_typing_interval_ms: 4000,
            feedback_typing_ttl_ms: 1000,
            feedback_ack_ttl_ms: 10000,
            feedback_commands_ttl_ms: 10000,
            progress_interval_ms: 2000,
            progress_batch_limit: 32,
            stream_event_batch_limit: 64,
            stream_input_retention: 10000,
            stream_subscription_limit: 1024,
            stream_text_limit_bytes: 64000,
            stream_chunk_utf16_limit: 4000,

            connect_timeout_ms: 10000,
            request_timeout_ms: 60000,
            feedback_connect_timeout_ms: 2000,
            feedback_timeout_ms: 3000,
            pairing_connect_timeout_ms: 3000,
            pairing_timeout_ms: 8000,
            poll_timeout_seconds: 50,
            poll_timeout_slack_ms: 20000,
            poll_request_min_ms: 30000,
            poll_retry_ms: 5000,
            startup_retry_initial_ms: 1000,
            startup_retry_max_ms: 60000,
            loop_interval_ms: 100,
            stop_poll_ms: 100,
            policy_reload_ms: 1000,
            outbound_batch_limit: 32,
            inbound_batch_limit: 8,
            hook_batch_limit: 32,
            updates_batch_limit: 100,
            json_limit_bytes: 4194304,
            file_limit_bytes: 20971520,
            stream_message_limit_per_event: 16,
            outbound_retry_ms: 5000,
            outbound_lease_ms: 90000,
            inbound_lease_ms: 90000,
            feeder_retry_ms: 2000,
            stage_batch_limit: 100,
            control_batch_limit: 100,
            update_limit_bytes: 1048576,
            input_text_limit_bytes: 60000,
            callback_ttl_ms: 300000,
            callback_busy_timeout_ms: 5000,
            feeder_registration_limit: 1024,
            feeder_prompt_limit_bytes: 64000,
            feedback_ack_buffer_limit: 100,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
pub enum Kind {
    Origin,
    Boolean,
    Number { min: u64, max: u64 },
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub description: &'static str,
    pub kind: Kind,
}
pub const FIELDS: &[Field] = &[
    Field {
        key: "api_origin",
        label: "API origin",
        group: "Network",
        description: "HTTPS server receiving the bot credential. No URL credentials, path, query or fragment. Applies on restart.",
        kind: Kind::Origin,
    },
    Field {
        key: "connect_timeout_ms",
        label: "Connection timeout (ms)",
        group: "Network",
        description: "Maximum connection wait for normal API requests.",
        kind: Kind::Number {
            min: 100,
            max: 30000,
        },
    },
    Field {
        key: "request_timeout_ms",
        label: "Request timeout (ms)",
        group: "Network",
        description: "Deadline for normal API requests and attachments.",
        kind: Kind::Number {
            min: 100,
            max: 120000,
        },
    },
    Field {
        key: "feedback_connect_timeout_ms",
        label: "Feedback connection timeout (ms)",
        group: "Network",
        description: "Connection wait for typing, reactions and command feedback.",
        kind: Kind::Number {
            min: 100,
            max: 30000,
        },
    },
    Field {
        key: "feedback_timeout_ms",
        label: "Feedback request timeout (ms)",
        group: "Network",
        description: "Deadline for the feedback transport.",
        kind: Kind::Number {
            min: 100,
            max: 120000,
        },
    },
    Field {
        key: "pairing_connect_timeout_ms",
        label: "Discovery connection timeout (ms)",
        group: "Network",
        description: "Connection wait when discovering recent chats.",
        kind: Kind::Number {
            min: 100,
            max: 30000,
        },
    },
    Field {
        key: "pairing_timeout_ms",
        label: "Discovery request timeout (ms)",
        group: "Network",
        description: "Deadline for read-only chat discovery.",
        kind: Kind::Number {
            min: 100,
            max: 120000,
        },
    },
    Field {
        key: "poll_timeout_seconds",
        label: "Long poll (seconds)",
        group: "Polling",
        description: "Server wait for updates; the HTTP deadline includes separate connection slack.",
        kind: Kind::Number { min: 1, max: 50 },
    },
    Field {
        key: "poll_timeout_slack_ms",
        label: "Polling deadline slack (ms)",
        group: "Polling",
        description: "HTTP deadline beyond the server wait; must cover the connection timeout.",
        kind: Kind::Number {
            min: 100,
            max: 60000,
        },
    },
    Field {
        key: "poll_request_min_ms",
        label: "Minimum poll deadline (ms)",
        group: "Polling",
        description: "Minimum HTTP deadline for update requests.",
        kind: Kind::Number {
            min: 100,
            max: 120000,
        },
    },
    Field {
        key: "poll_retry_ms",
        label: "Polling retry delay (ms)",
        group: "Polling",
        description: "Wait after an unavailable update request. Server retry-after still takes priority.",
        kind: Kind::Number {
            min: 100,
            max: 60000,
        },
    },
    Field {
        key: "startup_retry_initial_ms",
        label: "Initial startup retry (ms)",
        group: "Service",
        description: "Initial retry delay for read-only bot identity verification.",
        kind: Kind::Number {
            min: 100,
            max: 60000,
        },
    },
    Field {
        key: "startup_retry_max_ms",
        label: "Maximum startup retry (ms)",
        group: "Service",
        description: "Maximum exponential startup backoff. Server retry-after still takes priority.",
        kind: Kind::Number {
            min: 100,
            max: 3600000,
        },
    },
    Field {
        key: "loop_interval_ms",
        label: "Service cycle interval (ms)",
        group: "Service",
        description: "Delay between service work cycles.",
        kind: Kind::Number {
            min: 10,
            max: 10000,
        },
    },
    Field {
        key: "stop_poll_ms",
        label: "Startup stop polling (ms)",
        group: "Service",
        description: "How often startup backoff checks for a stop request.",
        kind: Kind::Number { min: 10, max: 1000 },
    },
    Field {
        key: "policy_reload_ms",
        label: "Policy refresh interval (ms)",
        group: "Service",
        description: "How often running service checks local grants, routes and configuration.",
        kind: Kind::Number {
            min: 100,
            max: 60000,
        },
    },
    Field {
        key: "outbound_batch_limit",
        label: "Outbound destinations per cycle",
        group: "Batches",
        description: "Maximum destination candidates examined for the next outbound send.",
        kind: Kind::Number {
            min: 1,
            max: crate::core::journal::MAX_DESTINATION_PAGE as u64,
        },
    },
    Field {
        key: "inbound_batch_limit",
        label: "Inbound destinations per cycle",
        group: "Batches",
        description: "Maximum queued input destinations examined per service cycle.",
        kind: Kind::Number {
            min: 1,
            max: crate::core::journal::MAX_DESTINATION_PAGE as u64,
        },
    },
    Field {
        key: "hook_batch_limit",
        label: "Hook events per cycle",
        group: "Batches",
        description: "Maximum admitted hook events expanded per service cycle.",
        kind: Kind::Number { min: 1, max: 1024 },
    },
    Field {
        key: "updates_batch_limit",
        label: "Updates per poll",
        group: "Batches",
        description: "Maximum updates requested in a Telegram poll.",
        kind: Kind::Number { min: 1, max: 100 },
    },
    Field {
        key: "json_limit_bytes",
        label: "Response size limit (bytes)",
        group: "Network",
        description: "Maximum JSON response size, up to the built-in 4 MiB ceiling.",
        kind: Kind::Number {
            min: 1024,
            max: 4194304,
        },
    },
    Field {
        key: "file_limit_bytes",
        label: "Attachment size limit (bytes)",
        group: "Network",
        description: "Maximum downloaded, uploaded or cutover-relocated pending attachment size, up to 20 MiB.",
        kind: Kind::Number {
            min: 1,
            max: 20971520,
        },
    },
    Field {
        key: "feedback_typing_enabled",
        label: "Typing indicator",
        group: "Feedback",
        description: "Send best-effort typing indicators only for verified busy conversations.",
        kind: Kind::Boolean,
    },
    Field {
        key: "feedback_ack_enabled",
        label: "Queue reaction",
        group: "Feedback",
        description: "React to authorized queued messages; a reaction is not agent completion.",
        kind: Kind::Boolean,
    },
    Field {
        key: "feedback_commands_enabled",
        label: "Register bot commands",
        group: "Feedback",
        description: "Offer the public command menu when the service starts; disabling does not remove an existing menu.",
        kind: Kind::Boolean,
    },
    Field {
        key: "feedback_queue_limit",
        label: "Feedback queue capacity",
        group: "Feedback",
        description: "Maximum queued cosmetic actions; excess actions are dropped.",
        kind: Kind::Number { min: 1, max: 4096 },
    },
    Field {
        key: "feedback_typing_interval_ms",
        label: "Typing refresh interval (ms)",
        group: "Feedback",
        description: "Elapsed wait between verified busy typing observations.",
        kind: Kind::Number {
            min: 100,
            max: 60000,
        },
    },
    Field {
        key: "feedback_typing_ttl_ms",
        label: "Typing expiry (ms)",
        group: "Feedback",
        description: "Discard queued typing actions after this elapsed age.",
        kind: Kind::Number { min: 1, max: 60000 },
    },
    Field {
        key: "feedback_ack_ttl_ms",
        label: "Queue reaction expiry (ms)",
        group: "Feedback",
        description: "Discard queued reactions after this elapsed age; no replay.",
        kind: Kind::Number { min: 1, max: 60000 },
    },
    Field {
        key: "feedback_commands_ttl_ms",
        label: "Command menu expiry (ms)",
        group: "Feedback",
        description: "Discard queued command registration after this elapsed age.",
        kind: Kind::Number {
            min: 1,
            max: 120000,
        },
    },
    Field {
        key: "progress_interval_ms",
        label: "Progress refresh interval (ms)",
        group: "Progress",
        description: "Elapsed wait between durable progress work cycles.",
        kind: Kind::Number {
            min: 100,
            max: 60000,
        },
    },
    Field {
        key: "progress_batch_limit",
        label: "Progress events per cycle",
        group: "Progress",
        description: "Maximum admitted progress candidates examined in a cycle.",
        kind: Kind::Number { min: 1, max: 1024 },
    },
    Field {
        key: "stream_event_batch_limit",
        label: "Stream events per subscription",
        group: "Streaming",
        description: "Maximum verified hook events examined per subscription in a cycle.",
        kind: Kind::Number { min: 1, max: 1024 },
    },
    Field {
        key: "stream_input_retention",
        label: "Accepted stream inputs retained",
        group: "Streaming",
        description: "Recent accepted-prompt provenance records retained. Pruning can suppress output; it never authorizes a fallback.",
        kind: Kind::Number {
            min: 1,
            max: 1000000,
        },
    },
    Field {
        key: "stream_subscription_limit",
        label: "Active stream limit",
        group: "Streaming",
        description: "Maximum enabled private-chat streams; lower limits require surplus streams to be stopped.",
        kind: Kind::Number { min: 1, max: 65536 },
    },
    Field {
        key: "stream_text_limit_bytes",
        label: "Stream event size (bytes)",
        group: "Streaming",
        description: "Maximum visible text event accepted before Unicode chunking, up to the existing 64000-byte ceiling.",
        kind: Kind::Number { min: 1, max: 64000 },
    },
    Field {
        key: "stream_chunk_utf16_limit",
        label: "Stream chunk size (UTF-16 units)",
        group: "Streaming",
        description: "Maximum text units in each plain-text stream message, up to the existing 4000-unit ceiling; whole Unicode characters remain intact.",
        kind: Kind::Number { min: 2, max: 4000 },
    },
    Field {
        key: "stream_message_limit_per_event",
        label: "Stream messages per event",
        group: "Streaming",
        description: "Refuse the whole stream event before queueing when Unicode chunking would exceed this message count.",
        kind: Kind::Number { min: 1, max: 1024 },
    },
    Field {
        key: "outbound_retry_ms",
        label: "Known-not-sent retry (ms)",
        group: "Delivery",
        description: "Elapsed retry wait after a connection failed before sending. Complete server retry-after still takes priority.",
        kind: Kind::Number {
            min: 100,
            max: 3600000,
        },
    },
    Field {
        key: "outbound_lease_ms",
        label: "Outbound attempt lease (ms)",
        group: "Delivery",
        description: "Durable ownership lease for an outbound attempt. Expiry remains uncertain; exact correlated late receipts remain evidence.",
        kind: Kind::Number {
            min: 1000,
            max: 3600000,
        },
    },
    Field {
        key: "inbound_lease_ms",
        label: "Feeder attempt lease (ms)",
        group: "Delivery",
        description: "Durable ownership lease for a terminal submission; expiry never authorizes automatic replay.",
        kind: Kind::Number {
            min: 1000,
            max: 3600000,
        },
    },
    Field {
        key: "feeder_retry_ms",
        label: "Feeder readiness retry (ms)",
        group: "Delivery",
        description: "Durable retry wait only for input known not submitted because discovery/readiness is unavailable.",
        kind: Kind::Number {
            min: 100,
            max: 60000,
        },
    },
    Field {
        key: "stage_batch_limit",
        label: "Inputs staged per cycle",
        group: "Batches",
        description: "Maximum admitted inputs examined for feeder staging per service cycle.",
        kind: Kind::Number { min: 1, max: 1024 },
    },
    Field {
        key: "control_batch_limit",
        label: "Controls staged per cycle",
        group: "Batches",
        description: "Maximum admitted control candidates examined per cycle; permissions and one-use approvals remain required.",
        kind: Kind::Number { min: 1, max: 1024 },
    },
    Field {
        key: "update_limit_bytes",
        label: "Individual update size (bytes)",
        group: "Delivery",
        description: "Maximum serialized individual update before durable admission, up to the existing 1 MiB ceiling.",
        kind: Kind::Number {
            min: 1024,
            max: 1048576,
        },
    },
    Field {
        key: "input_text_limit_bytes",
        label: "Input text size (bytes)",
        group: "Delivery",
        description: "Maximum message/caption text before queueing. Encoded prompt size has its own separate ceiling.",
        kind: Kind::Number { min: 1, max: 60000 },
    },
    Field {
        key: "callback_ttl_ms",
        label: "Confirmation lifetime (ms)",
        group: "Controls",
        description: "Absolute UTC expiry for newly issued persisted one-use confirmations; existing claims retain their saved expiry.",
        kind: Kind::Number {
            min: 1000,
            max: 86400000,
        },
    },
    Field {
        key: "callback_busy_timeout_ms",
        label: "Confirmation storage wait (ms)",
        group: "Controls",
        description: "SQLite busy timeout for the confirmation store.",
        kind: Kind::Number { min: 1, max: 60000 },
    },
    Field {
        key: "feeder_registration_limit",
        label: "Feeder registrations",
        group: "Delivery",
        description: "Maximum selected hook registrations, up to the service schema1024 ceiling. Reduce registrations before lowering this value.",
        kind: Kind::Number { min: 1, max: 1024 },
    },
    Field {
        key: "feeder_prompt_limit_bytes",
        label: "Encoded feeder prompt (bytes)",
        group: "Delivery",
        description: "Maximum complete escaped transport envelope sent to a terminal, up to the existing 64000-byte ceiling.",
        kind: Kind::Number {
            min: 1024,
            max: 64000,
        },
    },
    Field {
        key: "feedback_ack_buffer_limit",
        label: "Pending queue reactions",
        group: "Feedback",
        description: "Maximum authorized queue-reaction observations retained before the cosmetic worker takes them; excess observations are dropped.",
        kind: Kind::Number { min: 1, max: 4096 },
    },
];
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        self.validate_transport(false)
    }
    pub(crate) fn validate_transport(&self, local_test: bool) -> Result<(), String> {
        let values =
            serde_json::to_value(self).map_err(|_| "Cannot inspect Telegram runtime settings")?;
        for field in FIELDS {
            if let Kind::Number { min, max } = field.kind
                && !values[field.key]
                    .as_u64()
                    .is_some_and(|n| (min..=max).contains(&n))
            {
                return Err(format!("Invalid Telegram runtime setting: {}", field.key));
            }
        }
        let origin = &self.api_origin;
        let url = reqwest::Url::parse(origin).map_err(|_| "Invalid Telegram API origin")?;
        let loopback = local_test && url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
        if origin.trim() != origin
            || origin.chars().any(char::is_control)
            || (url.scheme() != "https" && !loopback)
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err("Telegram API requires an HTTPS origin without credentials, path, query or fragment".into());
        }
        for (connect, request) in [
            (self.connect_timeout_ms, self.request_timeout_ms),
            (self.feedback_connect_timeout_ms, self.feedback_timeout_ms),
            (self.pairing_connect_timeout_ms, self.pairing_timeout_ms),
        ] {
            if connect > request {
                return Err("Connection timeout must not exceed its request timeout".into());
            }
        }
        if self.poll_timeout_slack_ms < self.connect_timeout_ms {
            return Err("Polling slack must cover the connection timeout".into());
        }
        if self.startup_retry_initial_ms > self.startup_retry_max_ms {
            return Err("Initial startup retry must not exceed maximum backoff".into());
        }
        Ok(())
    }
    pub fn poll_deadline(&self, timeout: u64) -> Duration {
        Duration::from_millis(
            self.poll_request_min_ms.max(
                timeout
                    .saturating_mul(1000)
                    .saturating_add(self.poll_timeout_slack_ms),
            ),
        )
    }
    pub fn value(&self, key: &str) -> Result<Value, String> {
        let key = key.strip_prefix("runtime.").unwrap_or(key);
        if !FIELDS.iter().any(|f| f.key == key) {
            return Err("Unknown Telegram runtime setting".into());
        }
        serde_json::to_value(self)
            .map(|v| v[key].clone())
            .map_err(|_| "Cannot inspect Telegram runtime setting".into())
    }
    pub fn display(&self, key: &str) -> Result<String, String> {
        Ok(match self.value(key)? {
            Value::String(s) => s,
            v => v.to_string(),
        })
    }
    pub fn set(&self, key: &str, text: &str) -> Result<Self, String> {
        let key = key.strip_prefix("runtime.").unwrap_or(key);
        let field = FIELDS
            .iter()
            .find(|f| f.key == key)
            .ok_or("Unknown Telegram runtime setting")?;
        let mut value =
            serde_json::to_value(self).map_err(|_| "Cannot inspect Telegram runtime settings")?;
        value[key] = match field.kind {
            Kind::Origin => Value::String(text.trim().into()),
            Kind::Boolean => Value::Bool(match text.trim() {
                "true" => true,
                "false" => false,
                _ => return Err("Enter true or false".into()),
            }),
            Kind::Number { min, max } => {
                let number = text
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| "Enter a whole nonnegative number")?;
                if !(min..=max).contains(&number) {
                    return Err(format!("Enter a number from {min} to {max}"));
                }
                Value::from(number)
            }
        };
        let updated: Self =
            serde_json::from_value(value).map_err(|_| "Invalid Telegram runtime settings")?;
        updated.validate()?;
        Ok(updated)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_metadata_covers_every_saved_setting_and_defaults_preserve_existing_budgets() {
        let defaults = Settings::default();
        defaults.validate().unwrap();
        let saved = serde_json::to_value(&defaults).unwrap();
        let keys: std::collections::BTreeSet<_> = FIELDS.iter().map(|f| f.key).collect();
        assert_eq!(keys.len(), FIELDS.len());
        assert_eq!(
            keys,
            saved
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect()
        );
        assert_eq!(defaults.poll_deadline(50), Duration::from_secs(70));
        assert_eq!(defaults.poll_deadline(0), Duration::from_secs(30));
        let example: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/telegram-service.example.json"
        )))
        .unwrap();
        let example: Settings = serde_json::from_value(example["runtime"].clone()).unwrap();
        assert_eq!(example, defaults);
    }
    #[test]
    fn unsafe_origins_bounds_and_inconsistent_budgets_fail_without_echoing_input() {
        let initial = Settings::default();
        for (key, value) in [
            ("api_origin", "http://example.invalid"),
            ("api_origin", "https://user:secret-value@example.invalid"),
            ("api_origin", "https://example.invalid/path"),
            ("api_origin", "https://example.invalid?token=secret-value"),
            ("request_timeout_ms", "0"),
            ("connect_timeout_ms", "30000"),
            ("poll_timeout_seconds", "51"),
            ("inbound_batch_limit", "1001"),
            ("outbound_batch_limit", "1001"),
            ("startup_retry_max_ms", "100"),
            ("poll_timeout_slack_ms", "100"),
            ("json_limit_bytes", "4194305"),
        ] {
            assert!(
                !initial
                    .set(key, value)
                    .unwrap_err()
                    .contains("secret-value")
            );
        }
        assert_eq!(initial, Settings::default());
        assert_eq!(
            initial
                .set("runtime.loop_interval_ms", "250")
                .unwrap()
                .loop_interval_ms,
            250
        );
        assert!(serde_json::from_str::<Settings>(r#"{"unknown":1}"#).is_err());
    }
}
