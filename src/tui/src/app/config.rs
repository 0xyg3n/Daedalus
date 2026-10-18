//! Portable private configuration. Native account credentials stay with each harness.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const HARNESSES: [&str; 7] = [
    "claude", "codex", "grok", "hermes", "cursor", "pi", "opencode",
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryConfig {
    #[serde(default = "automatic_discovery")]
    pub automatic: bool,
    #[serde(default)]
    pub herdr_sessions: Vec<String>,
    /// Explicit session-to-metadata-socket bindings; never infer private endpoints.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub herdr_sockets: BTreeMap<String, String>,
}
fn automatic_discovery() -> bool {
    true
}
impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            automatic: true,
            herdr_sessions: Vec::new(),
            herdr_sockets: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlConfig {
    pub version: u32,
    pub agent_name: String,
    pub agent_home: String,
    #[serde(default)]
    pub subscriptions: Vec<String>,
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub profiles: BTreeMap<String, String>,
    #[serde(default)]
    pub tmux_sockets: Vec<String>,
    #[serde(default)]
    pub discovery: DiscoveryConfig,
}

impl ControlConfig {
    pub fn fresh(agent_home: &Path) -> Result<Self, String> {
        let home = agent_home.to_str().ok_or("Workspace path must be UTF-8")?;
        let config = Self {
            version: 1,
            agent_name: "agent".into(),
            agent_home: home.into(),
            subscriptions: vec![],
            telegram: TelegramConfig::default(),
            profiles: BTreeMap::new(),
            tmux_sockets: Vec::new(),
            discovery: DiscoveryConfig::default(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        // Do not include parser errors: malformed input may contain credentials.
        let config: Self =
            serde_json::from_slice(bytes).map_err(|_| "Invalid configuration structure")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("Unsupported configuration version".into());
        }
        if self.agent_name.is_empty()
            || self.agent_name.len() > 128
            || self.agent_name.chars().any(char::is_control)
        {
            return Err("Invalid agent name".into());
        }
        validate_path(&self.agent_home)?;
        let mut seen = BTreeSet::new();
        for subscription in &self.subscriptions {
            if (!HARNESSES.contains(&subscription.as_str()) && subscription != "gateway")
                || !seen.insert(subscription)
            {
                return Err("Unknown or duplicate harness selection".into());
            }
        }
        for (harness, path) in &self.profiles {
            if !HARNESSES.contains(&harness.as_str()) {
                return Err("Unknown harness profile".into());
            }
            validate_path(path)?;
        }
        let mut sockets = BTreeSet::new();
        for socket in &self.tmux_sockets {
            validate_path(socket)?;
            if !sockets.insert(socket) {
                return Err("Duplicate tmux socket".into());
            }
        }
        let mut sessions = BTreeSet::new();
        for session in &self.discovery.herdr_sessions {
            if session.is_empty()
                || session.len() > 80
                || matches!(session.as_str(), "." | "..")
                || !session
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                || !sessions.insert(session)
            {
                return Err("Invalid or duplicate Herdr session".into());
            }
        }
        for (session, socket) in &self.discovery.herdr_sockets {
            if session.is_empty()
                || session.len() > 80
                || matches!(session.as_str(), "." | "..")
                || !session
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
            {
                return Err("Invalid Herdr metadata session".into());
            }
            validate_path(socket)?;
            if socket.len() >= 108 {
                return Err("Herdr metadata socket path exceeds Unix socket limit".into());
            }
        }
        for id in [&self.telegram.owner_id, &self.telegram.bot_id]
            .into_iter()
            .flatten()
        {
            if id.is_empty()
                || !id.bytes().all(|b| b.is_ascii_digit())
                || id.parse::<u64>().ok().filter(|id| *id > 0).is_none()
            {
                return Err("Telegram IDs must be positive integers".into());
            }
        }
        if let Some(username) = &self.telegram.username
            && (!(5..=64).contains(&username.len())
                || !username
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_'))
        {
            return Err("Invalid Telegram username".into());
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(|_| "Cannot serialize configuration")?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

fn validate_path(value: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_control) || !Path::new(value).is_absolute() {
        return Err(
            "Configuration paths must be absolute and contain no control characters".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fresh_round_trip() {
        let config =
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/work space"))).unwrap();
        assert_eq!(
            config,
            ControlConfig::parse(&config.encode().unwrap()).unwrap()
        );
    }
    #[test]
    fn discovery_endpoints_are_preserved_and_validated() {
        let mut config =
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/work"))).unwrap();
        assert!(config.discovery.automatic);
        config.discovery.automatic = false;
        config.discovery.herdr_sessions = vec!["work-one".into(), "work_two".into()];
        config.tmux_sockets = vec![crate::test_support::path("/tmp/tmux-example/work").into()];
        config.discovery.herdr_sockets.insert(
            "work-one".into(),
            crate::test_support::path("/tmp/example/herdr.sock").into(),
        );
        assert_eq!(
            config,
            ControlConfig::parse(&config.encode().unwrap()).unwrap()
        );
        config.discovery.herdr_sessions.push("../escape".into());
        assert!(config.validate().is_err());
    }
    #[test]
    fn metadata_socket_mappings_reject_relative_paths_and_ambiguous_sessions() {
        let mut config =
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/work"))).unwrap();
        config
            .discovery
            .herdr_sockets
            .insert("work".into(), "relative".into());
        assert!(config.validate().is_err());
        config.discovery.herdr_sockets.clear();
        config.discovery.herdr_sockets.insert(
            "../other".into(),
            crate::test_support::path("/tmp/socket").into(),
        );
        assert!(config.validate().is_err());
        config.discovery.herdr_sockets.clear();
        config
            .discovery
            .herdr_sockets
            .insert("work".into(), format!("/{}", "x".repeat(108)));
        assert!(config.validate().is_err());
    }
    #[test]
    fn rejects_unsafe_or_unknown_configuration() {
        assert!(ControlConfig::fresh(Path::new("relative")).is_err());
        let mut config =
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/workspace"))).unwrap();
        config.subscriptions = vec!["claude".into(), "claude".into()];
        assert!(config.validate().is_err());
        config.subscriptions.clear();
        config.telegram.owner_id = Some("0".into());
        assert!(config.validate().is_err());
        assert!(
            !ControlConfig::parse(b"{\"token\":\"secret-token\"}")
                .unwrap_err()
                .contains("secret-token")
        );
    }
}
