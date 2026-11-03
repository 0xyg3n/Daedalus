//! Portable private configuration. Native account credentials stay with each harness.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;

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
    /// None preserves the older automatic-discovery policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<bool>,
    #[serde(default)]
    pub herdr_sessions: Vec<String>,
    /// Explicit session-to-metadata-socket bindings; never infer private endpoints.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "super::control_settings::unique_strings"
    )]
    pub herdr_sockets: BTreeMap<String, String>,
}
fn automatic_discovery() -> bool {
    true
}
impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            automatic: true,
            native: None,
            herdr_sessions: Vec::new(),
            herdr_sockets: BTreeMap::new(),
        }
    }
}
impl DiscoveryConfig {
    pub(crate) fn native_enabled(&self) -> bool {
        self.native.unwrap_or(self.automatic)
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
    #[serde(default, deserialize_with = "super::control_settings::unique_strings")]
    pub profiles: BTreeMap<String, String>,
    #[serde(default)]
    pub tmux_sockets: Vec<String>,
    #[serde(default)]
    pub discovery: DiscoveryConfig,
    #[serde(default)]
    pub ui: super::interface_settings::InterfaceSettings,
    #[serde(default)]
    pub runtime: super::runtime_settings::Settings,
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
            ui: super::interface_settings::InterfaceSettings::default(),
            runtime: super::runtime_settings::Settings::default(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err("Configuration exceeds the 1 MiB size limit".into());
        }
        // Do not include parser errors: malformed input may contain credentials.
        let config: Self =
            serde_json::from_slice(bytes).map_err(|_| "Invalid configuration structure")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.ui.validate()?;
        self.runtime.validate()?;
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
            #[cfg(unix)]
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
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err("Configuration exceeds the 1 MiB size limit".into());
        }
        Ok(bytes)
    }

    pub fn setting_value(&self, key: &str) -> Result<serde_json::Value, String> {
        if super::control_settings::field(key).is_ok() {
            super::control_settings::value(self, key)
        } else if key.starts_with("ui.") {
            self.ui.value(key)
        } else if key.starts_with("runtime.") {
            self.runtime.value(key)
        } else if let Some(tool) = key.strip_prefix("profiles.") {
            if !HARNESSES.contains(&tool) {
                return Err("Unknown native profile tool".into());
            }
            Ok(self
                .profiles
                .get(tool)
                .map_or(serde_json::Value::Null, |value| {
                    serde_json::Value::String(value.clone())
                }))
        } else {
            Err("Use a listed instance, ui., runtime. or profiles. setting; config show lists editable settings".into())
        }
    }

    pub fn with_setting(&self, key: &str, value: &str) -> Result<Self, String> {
        self.setting_value(key)?;
        if super::control_settings::field(key).is_ok() {
            return super::control_settings::set(self, key, value);
        }
        let mut updated = self.clone();
        if key.starts_with("ui.") {
            updated.ui = self.ui.set(key, value)?;
        } else if key.starts_with("runtime.") {
            updated.runtime = self.runtime.set(key, value)?;
        } else {
            let tool = key
                .strip_prefix("profiles.")
                .ok_or("Unknown native profile setting")?;
            if value == "inherit" {
                updated.profiles.remove(tool);
            } else {
                let path = super::hooks::windows_native_path(Path::new(value));
                crate::core::layouts::check_path(&path)
                    .map_err(|_| "Profile directory must be absolute and symlink-free")?;
                if !path.is_dir() {
                    return Err("Profile directory does not exist".into());
                }
                updated.profiles.insert(
                    tool.into(),
                    path.to_str()
                        .ok_or("Profile directory must be UTF-8")?
                        .into(),
                );
            }
        }
        updated.validate()?;
        Ok(updated)
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
    fn audit_configuration_duplicate_profile_or_endpoint_is_not_ambiguous() {
        let original =
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/project"))).unwrap();
        let value = serde_json::to_string(crate::test_support::path("/tmp/profile")).unwrap();
        let encoded = serde_json::to_string(&original).unwrap();
        let profiles = encoded.replace(
            r#""profiles":{}"#,
            &format!(r#""profiles":{{"codex":{value},"codex":{value}}}"#),
        );
        assert_ne!(profiles, encoded);
        assert!(ControlConfig::parse(profiles.as_bytes()).is_err());
        let endpoints = encoded.replace(
            r#""herdr_sessions":[]"#,
            &format!(
                r#""herdr_sessions":[],"herdr_sockets":{{"example":{value},"example":{value}}}"#
            ),
        );
        assert_ne!(endpoints, encoded);
        assert!(ControlConfig::parse(endpoints.as_bytes()).is_err());
    }
    #[test]
    fn legacy_runtime_defaults_partial_updates_and_size_limits_are_consistent() {
        let original =
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/project"))).unwrap();
        let mut value = serde_json::to_value(&original).unwrap();
        value.as_object_mut().unwrap().remove("runtime");
        assert_eq!(
            ControlConfig::parse(&serde_json::to_vec(&value).unwrap()).unwrap(),
            original
        );
        value["runtime"] = serde_json::json!({"native_launch_cols":90});
        let parsed = ControlConfig::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(parsed.runtime.native_launch_cols, 90);
        assert_eq!(parsed.runtime.native_launch_rows, 32);
        value["runtime"]["native_launch_rows"] = serde_json::json!(0);
        assert!(ControlConfig::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut oversized = original;
        oversized.profiles.insert(
            "codex".into(),
            format!(
                "{}{}",
                crate::test_support::path("/tmp/"),
                "x".repeat(MAX_CONFIG_BYTES)
            ),
        );
        oversized.validate().unwrap();
        assert!(oversized.encode().unwrap_err().contains("size limit"));
        assert!(
            ControlConfig::parse(&serde_json::to_vec(&oversized).unwrap())
                .unwrap_err()
                .contains("size limit")
        );
    }
    #[test]
    fn configurations_without_interface_settings_keep_existing_defaults() {
        let original =
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/project"))).unwrap();
        let mut legacy = serde_json::to_value(&original).unwrap();
        legacy.as_object_mut().unwrap().remove("ui");
        assert_eq!(
            ControlConfig::parse(&serde_json::to_vec(&legacy).unwrap()).unwrap(),
            original
        );
        legacy["ui"] = serde_json::json!({"splash":false});
        let loaded = ControlConfig::parse(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert!(!loaded.ui.splash);
        assert_eq!(
            loaded.ui.preview_interval_ms,
            original.ui.preview_interval_ms
        );
        legacy["ui"]["input_batch_limit"] = serde_json::json!(0);
        assert!(ControlConfig::parse(&serde_json::to_vec(&legacy).unwrap()).is_err());
    }
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
    fn native_discovery_override_preserves_legacy_inheritance_and_explicit_choices() {
        for automatic in [false, true] {
            let legacy: DiscoveryConfig =
                serde_json::from_value(serde_json::json!({"automatic":automatic})).unwrap();
            assert_eq!(legacy.native, None);
            assert_eq!(legacy.native_enabled(), automatic);
            let mut config = ControlConfig::fresh(&std::env::temp_dir()).unwrap();
            config.discovery = legacy;
            for (value, expected) in [
                ("true", Some(true)),
                ("false", Some(false)),
                ("inherit", None),
            ] {
                config = config.with_setting("discovery.native", value).unwrap();
                assert_eq!(config.discovery.native, expected);
                assert_eq!(
                    config.discovery.native_enabled(),
                    expected.unwrap_or(automatic)
                );
                assert_eq!(
                    ControlConfig::parse(&config.encode().unwrap()).unwrap(),
                    config
                );
            }
            assert!(config.with_setting("discovery.native", "yes").is_err());
        }
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
    fn metadata_socket_length_uses_the_native_transport() {
        let mut config =
            ControlConfig::fresh(Path::new(crate::test_support::path("/tmp/work"))).unwrap();
        config.discovery.herdr_sockets.insert(
            "work".into(),
            format!(
                "{}{}.sock",
                crate::test_support::path("/tmp/"),
                "x".repeat(120)
            ),
        );
        #[cfg(unix)]
        assert!(config.validate().is_err());
        #[cfg(windows)]
        assert!(config.validate().is_ok());
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
