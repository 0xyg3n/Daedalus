//! Editable portable instance settings. Native credentials and grants are separate.
use super::config::ControlConfig;
use serde::{
    Serialize,
    de::{self, MapAccess, Visitor},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt, path::Path};
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    Directory,
    Toggle,
    Tools,
    Paths,
    Sessions,
    Bindings,
    Optional,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: Kind,
}
pub const FIELDS: &[Field] = &[
    Field {
        key: "agent_name",
        label: "Default instance identity",
        description: "Nonblank printable identity of 1 to 128 bytes, used by local operations. Changing it can stop a running service until its identity is reviewed and restarted. Existing routes, goals and registrations keep their saved identities.",
        kind: Kind::Text,
    },
    Field {
        key: "agent_home",
        label: "Default workspace",
        description: "Existing absolute directory used for future launches without another project directory. Existing terminals keep their own working directory.",
        kind: Kind::Directory,
    },
    Field {
        key: "subscriptions",
        label: "Preferred tools",
        description: "Saved tool selection only. Installation, sign-in, gateway enable state and paid account access require their own actions.",
        kind: Kind::Tools,
    },
    Field {
        key: "tmux_sockets",
        label: "Additional tmux sockets",
        description: "Exact absolute socket paths. Paths containing commas or spaces are preserved. Missing endpoints can remain configured.",
        kind: Kind::Paths,
    },
    Field {
        key: "discovery.automatic",
        label: "Automatic terminal discovery",
        description: "Discover native terminals and standard external endpoints on subsequent refreshes. Explicit socket paths and Herdr sessions are still queried when this is off.",
        kind: Kind::Toggle,
    },
    Field {
        key: "discovery.herdr_sessions",
        label: "Explicit Herdr sessions",
        description: "Exact session names to query, including when automatic discovery is off. Names use letters, digits, underscores, dots or dashes, up to 80 bytes.",
        kind: Kind::Sessions,
    },
    Field {
        key: "discovery.herdr_sockets",
        label: "Herdr metadata connections",
        description: "Explicit session-to-metadata-socket bindings for identity operations. Add the session to Explicit Herdr sessions when it must also be discovered. No endpoint is inferred.",
        kind: Kind::Bindings,
    },
    Field {
        key: "telegram.owner_id",
        label: "Portable Telegram owner hint",
        description: "Optional positive numeric owner hint for portable setup/import. This does not change a service's explicit grants or credentials. Clear the field to remove the hint.",
        kind: Kind::Optional,
    },
    Field {
        key: "telegram.bot_id",
        label: "Portable Telegram bot hint",
        description: "Optional positive numeric bot hint for portable setup/import. This does not change a service's bot identity or credentials. Clear the field to remove the hint.",
        kind: Kind::Optional,
    },
    Field {
        key: "telegram.username",
        label: "Portable Telegram username hint",
        description: "Optional bot username without @, using 5 to 64 ASCII letters, digits or underscores, for portable setup/import. This does not rename a bot or change the configured service. Clear the field to remove the hint.",
        kind: Kind::Optional,
    },
    Field {
        key: "discovery.native",
        label: "Native terminal discovery override",
        description: "Enter true to discover native terminals independently, false to disable native discovery, or inherit to follow Automatic terminal discovery. Other automatic backends and explicit endpoints keep their own policy.",
        kind: Kind::Optional,
    },
];
pub fn field(key: &str) -> Result<&'static Field> {
    FIELDS
        .iter()
        .find(|f| f.key == key)
        .ok_or_else(|| "Unknown instance setting".into())
}
pub fn value(config: &ControlConfig, key: &str) -> Result<Value> {
    field(key)?;
    if key == "discovery.herdr_sockets" {
        return Ok(json!(config.discovery.herdr_sockets));
    }
    let encoded = serde_json::to_value(config).map_err(|_| "Cannot encode instance settings")?;
    Ok(encoded
        .pointer(&format!("/{}", key.replace('.', "/")))
        .cloned()
        .unwrap_or(Value::Null))
}
pub fn set(config: &ControlConfig, key: &str, text: &str) -> Result<ControlConfig> {
    let field = field(key)?;
    if text.len() > super::config::MAX_CONFIG_BYTES {
        return Err("Instance setting exceeds the configuration size limit".into());
    }
    let mut updated = config.clone();
    match field.kind {
        Kind::Text => {
            if text.trim().is_empty() {
                return Err("Enter a nonempty instance identity".into());
            }
            updated.agent_name = text.into();
        }
        Kind::Directory => {
            let path = super::hooks::windows_native_path(Path::new(text));
            crate::core::layouts::check_path(&path).map_err(
                |_| "Use an absolute workspace path without symlinks or parent traversal",
            )?;
            if !path.is_dir() {
                return Err("Workspace directory does not exist".into());
            }
            updated.agent_home = path.to_str().ok_or("Workspace path must be UTF-8")?.into();
        }
        Kind::Toggle => {
            updated.discovery.automatic = match text {
                "true" => true,
                "false" => false,
                _ => return Err("Choose true or false".into()),
            }
        }
        Kind::Tools | Kind::Paths | Kind::Sessions => {
            let values: Vec<String> =
                serde_json::from_str(text).map_err(|_| "Use a JSON array of exact strings")?;
            match field.kind {
                Kind::Tools => updated.subscriptions = values,
                Kind::Paths => {
                    for path in &values {
                        crate::core::layouts::check_path(Path::new(path)).map_err(
                            |_| "Use absolute socket paths without symlinks or parent traversal",
                        )?;
                    }
                    updated.tmux_sockets = values;
                }
                _ => updated.discovery.herdr_sessions = values,
            }
        }
        Kind::Bindings => {
            let mut decoder = serde_json::Deserializer::from_str(text);
            let values = unique_strings(&mut decoder)
                .map_err(|_| "Use a JSON object with unique session names and socket paths")?;
            decoder
                .end()
                .map_err(|_| "Use one JSON object of metadata connections")?;
            for path in values.values() {
                crate::core::layouts::check_path(Path::new(path)).map_err(
                    |_| "Use absolute metadata socket paths without symlinks or parent traversal",
                )?;
            }
            updated.discovery.herdr_sockets = values;
        }
        Kind::Optional if key == "discovery.native" => {
            updated.discovery.native = match text {
                "true" => Some(true),
                "false" => Some(false),
                "inherit" | "null" | "" => None,
                _ => return Err("Choose true, false or inherit".into()),
            };
        }
        Kind::Optional => {
            let hint = if text.is_empty() || text == "null" {
                None
            } else {
                Some(text.into())
            };
            match key {
                "telegram.owner_id" => updated.telegram.owner_id = hint,
                "telegram.bot_id" => updated.telegram.bot_id = hint,
                _ => updated.telegram.username = hint,
            }
        }
    }
    updated.validate()?;
    updated.encode()?;
    Ok(updated)
}
/// Identity maps must never silently choose the last of repeated JSON keys.
pub(crate) fn unique_strings<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error> {
    struct Unique;
    impl<'de> Visitor<'de> for Unique {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a map with unique string keys and values")
        }
        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if values.insert(key, value).is_some() {
                    return Err(de::Error::custom("Repeated mapping key"));
                }
            }
            Ok(values)
        }
    }
    decoder.deserialize_map(Unique)
}

/// Check before the legacy importer materializes a Value and discards unknown
/// fields; otherwise repeated authority keys would already have been collapsed.
pub(crate) fn check_unique_json(bytes: &[u8]) -> Result<()> {
    use serde::Deserialize;
    if bytes.len() > super::config::MAX_CONFIG_BYTES {
        return Err("Import JSON exceeds the configuration size limit".into());
    }
    struct Tree;
    struct Walk;
    impl<'de> Deserialize<'de> for Tree {
        fn deserialize<D: serde::Deserializer<'de>>(
            decoder: D,
        ) -> std::result::Result<Self, D::Error> {
            decoder.deserialize_any(Walk)
        }
    }
    impl<'de> Visitor<'de> for Walk {
        type Value = Tree;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("JSON with unique object keys")
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<Tree, E> {
            Ok(Tree)
        }
        fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<Tree, E> {
            Ok(Tree)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<Tree, E> {
            Ok(Tree)
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<Tree, E> {
            Ok(Tree)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Tree, E> {
            Ok(Tree)
        }
        fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<Tree, E> {
            Ok(Tree)
        }
        fn visit_seq<A: de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> std::result::Result<Tree, A::Error> {
            while sequence.next_element::<Tree>()?.is_some() {}
            Ok(Tree)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Tree, A::Error> {
            let mut keys = std::collections::BTreeSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !keys.insert(key) {
                    return Err(de::Error::custom("Repeated mapping key"));
                }
                map.next_value::<Tree>()?;
            }
            Ok(Tree)
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    Tree::deserialize(&mut decoder).map_err(|_| "Invalid import JSON or repeated field")?;
    decoder
        .end()
        .map_err(|_| "Invalid import JSON or repeated field".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_portable_instance_fields_have_strict_editors_and_metadata() {
        let root = std::env::temp_dir();
        let config = ControlConfig::fresh(&root).unwrap();
        let path = root.join("exact, endpoint.sock");
        let socket = path.to_str().unwrap();
        let bindings = std::collections::BTreeMap::from([("example", socket)]);
        let values = [
            ("agent_name", "cancel".to_owned()),
            ("agent_home", root.to_str().unwrap().to_owned()),
            ("subscriptions", json!(["codex", "gateway"]).to_string()),
            ("tmux_sockets", json!([socket]).to_string()),
            ("discovery.automatic", "false".into()),
            ("discovery.native", "true".into()),
            ("discovery.herdr_sessions", json!(["example"]).to_string()),
            ("discovery.herdr_sockets", json!(bindings).to_string()),
            ("telegram.owner_id", "7".into()),
            ("telegram.bot_id", "123".into()),
            ("telegram.username", "unset".into()),
        ];
        let keys = FIELDS
            .iter()
            .map(|f| f.key)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(keys, values.iter().map(|(k, _)| *k).collect());
        assert_eq!(keys.len(), FIELDS.len());
        for (key, text) in values {
            let updated = config.with_setting(key, &text).unwrap();
            assert_ne!(updated.setting_value(key).unwrap(), Value::Null);
            assert_eq!(
                ControlConfig::parse(&updated.encode().unwrap()).unwrap(),
                updated
            );
            assert_eq!(updated.profiles, config.profiles);
        }
        let selected = config.with_setting("telegram.username", "unset").unwrap();
        assert_eq!(selected.telegram.username.as_deref(), Some("unset"));
        assert!(
            selected
                .with_setting("telegram.username", "null")
                .unwrap()
                .telegram
                .username
                .is_none()
        );
        assert!(
            selected
                .with_setting("telegram.username", "")
                .unwrap()
                .telegram
                .username
                .is_none()
        );
        for (key, text) in [
            ("subscriptions", r#"["codex","codex"]"#),
            ("subscriptions", r#"["unknown"]"#),
            ("tmux_sockets", r#"["relative"]"#),
            ("discovery.herdr_sessions", r#"["../example"]"#),
            (
                "discovery.herdr_sockets",
                r#"{"example":"/first","example":"/second"}"#,
            ),
            ("discovery.automatic", "yes"),
            ("telegram.owner_id", "PRIVATE_INVALID_ID"),
            ("agent_name", "  "),
            ("version", "2"),
        ] {
            let error = config.with_setting(key, text).unwrap_err();
            assert!(!error.contains("PRIVATE_INVALID_ID"));
        }
        check_unique_json(br#"{"future":[null,true,-1,1,1.5,"text",{"one":1}]}"#).unwrap();
        assert!(
            check_unique_json(br#"{"future":[{"key":"PRIVATE_VALUE","key":false}]}"#)
                .unwrap_err()
                .contains("repeated")
        );
    }
}
