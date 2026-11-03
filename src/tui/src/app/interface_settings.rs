//! Persistent interface behavior shared by setup, the CLI and TUI settings.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_CLIPBOARD_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckpointMatches {
    #[default]
    All,
    Source,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InterfaceSettings {
    pub colors: bool,
    pub icons: String,
    pub reduced_motion: bool,
    pub backdrop: bool,
    pub splash: bool,
    pub graphics: String,
    pub screensaver_seconds: u64,
    pub update_check: bool,
    pub refresh_interval_ms: u64,
    pub preview_interval_ms: u64,
    pub preview_settle_ms: u64,
    pub resize_debounce_ms: u64,
    pub quit_timeout_ms: u64,
    pub graphics_probe_timeout_ms: u64,
    pub live_frame_ms: u64,
    pub animation_frame_ms: u64,
    pub saver_frame_ms: u64,
    pub idle_frame_ms: u64,
    pub reduced_motion_frame_ms: u64,
    pub still_saver_frame_ms: u64,
    pub input_batch_ms: u64,
    pub input_batch_limit: u64,
    pub suspended_poll_ms: u64,
    pub quit_poll_ms: u64,
    pub animation_window_ms: u64,
    pub double_click_ms: u64,
    pub clipboard_read_limit_bytes: u64,
    pub clipboard_write_limit_bytes: u64,
    pub checkpoint_matches: CheckpointMatches,
    pub terminal_scroll_rows: u64,
    pub list_page_rows: u64,
    pub details_page_rows: u64,
    pub options_page_items: u64,
    pub mouse_scroll_rows: u64,
    pub choice_wheel_items: u64,
    pub search_query_limit_chars: u64,
    pub shortcut_query_limit_chars: u64,
    pub agent_session_ids: bool,
}
impl Default for InterfaceSettings {
    fn default() -> Self {
        Self {
            colors: true,
            icons: "auto".into(),
            reduced_motion: false,
            backdrop: true,
            splash: true,
            graphics: "auto".into(),
            screensaver_seconds: 15,
            update_check: true,
            refresh_interval_ms: 5000,
            preview_interval_ms: 2000,
            preview_settle_ms: 250,
            resize_debounce_ms: 600,
            quit_timeout_ms: 1500,
            graphics_probe_timeout_ms: 1000,
            live_frame_ms: 16,
            animation_frame_ms: 33,
            saver_frame_ms: 50,
            idle_frame_ms: 100,
            reduced_motion_frame_ms: 250,
            still_saver_frame_ms: 1000,
            input_batch_ms: 4,
            input_batch_limit: 64,
            suspended_poll_ms: 40,
            quit_poll_ms: 15,
            animation_window_ms: 400,
            double_click_ms: 500,
            clipboard_read_limit_bytes: 65536,
            clipboard_write_limit_bytes: 524290,
            checkpoint_matches: CheckpointMatches::All,
            terminal_scroll_rows: 3,
            list_page_rows: 10,
            details_page_rows: 8,
            options_page_items: 5,
            mouse_scroll_rows: 3,
            choice_wheel_items: 1,
            search_query_limit_chars: 1024,
            shortcut_query_limit_chars: 160,
            agent_session_ids: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub enum Kind {
    Toggle,
    Choice(&'static [&'static str]),
    Number { min: u64, max: u64 },
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub description: &'static str,
    pub kind: Kind,
}
macro_rules! setting {
    ($key:ident, $label:literal, $group:literal, $description:literal, $kind:expr) => {
        Setting {
            key: stringify!($key),
            label: $label,
            group: $group,
            description: $description,
            kind: $kind,
        }
    };
}
pub const SETTINGS: &[Setting] = &[
    setting!(
        colors,
        "Colors",
        "Appearance",
        "Use color when the terminal supports it. NO_COLOR still disables color.",
        Kind::Toggle
    ),
    setting!(
        icons,
        "Icons",
        "Appearance",
        "Detect Unicode support or choose ASCII or Unicode explicitly.",
        Kind::Choice(&["auto", "ascii", "unicode"])
    ),
    setting!(
        reduced_motion,
        "Reduced motion",
        "Appearance",
        "Keep the interface still, including transition effects.",
        Kind::Toggle
    ),
    setting!(
        backdrop,
        "Artwork and background",
        "Appearance",
        "Enable artwork when true color and Unicode are available.",
        Kind::Toggle
    ),
    setting!(
        splash,
        "Startup splash",
        "Appearance",
        "Show the splash during startup. Applies on the next launch.",
        Kind::Toggle
    ),
    setting!(
        graphics,
        "Artwork rendering",
        "Appearance",
        "Use automatic detection, text cells, or the Kitty graphics protocol. Applies on the next launch.",
        Kind::Choice(&["auto", "cells", "kitty"])
    ),
    setting!(
        screensaver_seconds,
        "Screensaver delay (seconds)",
        "Appearance",
        "Idle time before artwork appears; zero disables it, otherwise use at least three seconds. Requires artwork support.",
        Kind::Number { min: 0, max: 86400 }
    ),
    setting!(
        update_check,
        "Automatic update checks",
        "Application",
        "Check for releases automatically. Manual update checks remain available.",
        Kind::Toggle
    ),
    setting!(
        refresh_interval_ms,
        "Inventory refresh (ms)",
        "Refresh",
        "Time between automatic inventory refreshes while the backend is idle.",
        Kind::Number {
            min: 100,
            max: 3600000
        }
    ),
    setting!(
        preview_interval_ms,
        "Preview refresh (ms)",
        "Refresh",
        "Time between refreshing visible terminal previews.",
        Kind::Number {
            min: 100,
            max: 3600000
        }
    ),
    setting!(
        preview_settle_ms,
        "Preview selection delay (ms)",
        "Refresh",
        "Wait after a previous preview before reading a newly selected terminal.",
        Kind::Number { min: 0, max: 60000 }
    ),
    setting!(
        resize_debounce_ms,
        "Panel save delay (ms)",
        "Refresh",
        "Wait for panel resizing to settle before saving its dimensions.",
        Kind::Number { min: 0, max: 60000 }
    ),
    setting!(
        quit_timeout_ms,
        "Save-on-exit timeout (ms)",
        "Application",
        "Maximum wait for the backend to save the current view before exit.",
        Kind::Number {
            min: 100,
            max: 60000
        }
    ),
    setting!(
        graphics_probe_timeout_ms,
        "Graphics detection timeout (ms)",
        "Application",
        "Maximum wait for the terminal to answer the startup graphics probe.",
        Kind::Number { min: 0, max: 10000 }
    ),
    setting!(
        live_frame_ms,
        "Live terminal frame interval (ms)",
        "Rendering",
        "Rendering interval while a terminal stream is open.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        animation_frame_ms,
        "Animation frame interval (ms)",
        "Rendering",
        "Rendering interval during startup and active transitions.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        saver_frame_ms,
        "Animated screensaver interval (ms)",
        "Rendering",
        "Rendering interval for moving screensaver artwork.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        idle_frame_ms,
        "Idle frame interval (ms)",
        "Rendering",
        "Rendering interval while the interface is idle.",
        Kind::Number { min: 1, max: 10000 }
    ),
    setting!(
        reduced_motion_frame_ms,
        "Still interface interval (ms)",
        "Rendering",
        "Rendering interval when motion is disabled.",
        Kind::Number { min: 1, max: 10000 }
    ),
    setting!(
        still_saver_frame_ms,
        "Still screensaver interval (ms)",
        "Rendering",
        "Rendering interval for static screensaver artwork.",
        Kind::Number { min: 1, max: 60000 }
    ),
    setting!(
        input_batch_ms,
        "Input batch budget (ms)",
        "Input",
        "Maximum time to drain ready input before drawing again.",
        Kind::Number { min: 1, max: 100 }
    ),
    setting!(
        input_batch_limit,
        "Input events per batch",
        "Input",
        "Maximum queued input events handled before drawing again.",
        Kind::Number { min: 1, max: 4096 }
    ),
    setting!(
        suspended_poll_ms,
        "External program polling (ms)",
        "Application",
        "Polling interval while another program owns the terminal.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        quit_poll_ms,
        "Exit polling interval (ms)",
        "Application",
        "Polling interval while waiting for the backend to finish saving.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        animation_window_ms,
        "Transition duration (ms)",
        "Rendering",
        "How long recent movement keeps the interface at its animation frame rate.",
        Kind::Number { min: 0, max: 10000 }
    ),
    setting!(
        double_click_ms,
        "Double-click interval (ms)",
        "Input",
        "Maximum gap between clicks that open an entry or reset a panel size.",
        Kind::Number {
            min: 100,
            max: 2000
        }
    ),
    setting!(
        clipboard_read_limit_bytes,
        "Clipboard paste limit (bytes)",
        "Clipboard",
        "Maximum Windows clipboard buffer read by explicit paste shortcuts. Includes UTF-16 text and its terminator. Other platforms use their terminal's paste handling.",
        Kind::Number {
            min: 2,
            max: MAX_CLIPBOARD_BYTES
        }
    ),
    setting!(
        clipboard_write_limit_bytes,
        "Clipboard copy limit (bytes)",
        "Clipboard",
        "Reject a larger selection before copying. Windows counts UTF-16 text plus its terminator; other platforms count UTF-8 text before OSC 52 encoding. Text is never truncated.",
        Kind::Number {
            min: 2,
            max: MAX_CLIPBOARD_BYTES
        }
    ),
    setting!(
        checkpoint_matches,
        "Checkpoint tree links",
        "Workspace display",
        "Show all native-ID matches, only checkpoints saved from this exact terminal source, or hide tree links. Saved checkpoints and reviewed resume/fork remain available. A matching native ID does not verify the current profile.",
        Kind::Choice(&["all", "source", "off"])
    ),
    setting!(
        terminal_scroll_rows,
        "Terminal wheel scroll rows",
        "Input",
        "Rows requested per wheel step in native terminal history and controlled Herdr streams. Native observers can scroll independently. Programs owning tmux mouse input receive their original mouse events.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        list_page_rows,
        "List and dialog page rows",
        "Navigation",
        "PageUp/Down step for lists, searches, shortcut results and dialog text/details. Workspace grids outside search still page by their visible layout. Chooser page keys scroll details without changing the choice.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        details_page_rows,
        "Details panel page rows",
        "Navigation",
        "Rows scrolled by Alt+PageUp/Down in the details panel without moving the selected item.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        options_page_items,
        "Options page items",
        "Navigation",
        "Items moved by PageUp/Down in Options. Movement selects an item without opening or changing it.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        mouse_scroll_rows,
        "Local wheel scroll rows",
        "Navigation",
        "Rows per wheel step in lists, shortcut results, details, the Focus outline and output dialogs. Workspace grids retain their layout movement; terminal history uses its separate wheel setting.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        choice_wheel_items,
        "Choice wheel items",
        "Navigation",
        "Items per wheel step in Options and chooser dialogs. Wheel movement never confirms a choice or changes a setting.",
        Kind::Number { min: 1, max: 1000 }
    ),
    setting!(
        search_query_limit_chars,
        "List search character limit",
        "Input",
        "Maximum Unicode characters for typed or pasted list, command and terminal-switcher queries. Controls are removed. Lowering the limit stops growth without truncating an existing query.",
        Kind::Number { min: 1, max: 16384 }
    ),
    setting!(
        shortcut_query_limit_chars,
        "Shortcut search character limit",
        "Input",
        "Maximum Unicode characters for typed or pasted shortcut queries. Controls are removed. Lowering the limit stops growth without truncating an existing query.",
        Kind::Number { min: 1, max: 16384 }
    ),
    setting!(
        agent_session_ids,
        "Agent session IDs in views",
        "Workspace display",
        "Show labeled conversation session IDs on workspace cards, outlines and selected-terminal status. Missing reported IDs are explicit. Full observed values remain available in Details; terminal IDs are never substituted.",
        Kind::Toggle
    ),
];

impl InterfaceSettings {
    pub fn validate(&self) -> Result<(), String> {
        if matches!(self.screensaver_seconds, 1 | 2) {
            return Err("Screensaver delay must be zero or at least three seconds".into());
        }
        let values = serde_json::to_value(self).map_err(|_| "Cannot inspect interface settings")?;
        for setting in SETTINGS {
            let value = &values[setting.key];
            let valid = match setting.kind {
                Kind::Toggle => value.is_boolean(),
                Kind::Choice(choices) => value.as_str().is_some_and(|v| choices.contains(&v)),
                Kind::Number { min, max } => {
                    value.as_u64().is_some_and(|v| (min..=max).contains(&v))
                }
            };
            if !valid {
                return Err(format!("Invalid interface setting: {}", setting.key));
            }
        }
        Ok(())
    }
    pub fn value(&self, key: &str) -> Result<Value, String> {
        let key = key.strip_prefix("ui.").unwrap_or(key);
        if !SETTINGS.iter().any(|s| s.key == key) {
            return Err("Unknown interface setting".into());
        }
        serde_json::to_value(self)
            .map(|v| v[key].clone())
            .map_err(|_| "Cannot inspect interface setting".into())
    }
    pub fn display(&self, key: &str) -> Result<String, String> {
        Ok(match self.value(key)? {
            Value::String(v) => v,
            Value::Bool(true) => "On".into(),
            Value::Bool(false) => "Off".into(),
            v => v.to_string(),
        })
    }
    pub fn set(&self, key: &str, text: &str) -> Result<Self, String> {
        let key = key.strip_prefix("ui.").unwrap_or(key);
        let setting = SETTINGS
            .iter()
            .find(|s| s.key == key)
            .ok_or("Unknown interface setting")?;
        let text = text.trim();
        let value = match setting.kind {
            Kind::Toggle => match text.to_ascii_lowercase().as_str() {
                "on" | "true" => Value::Bool(true),
                "off" | "false" => Value::Bool(false),
                _ => return Err("Choose on or off".into()),
            },
            Kind::Choice(choices) if choices.contains(&text) => Value::String(text.into()),
            Kind::Choice(_) => return Err("Choose one of the listed values".into()),
            Kind::Number { min, max } => {
                let n = text
                    .parse::<u64>()
                    .map_err(|_| "Enter a whole nonnegative number")?;
                if !(min..=max).contains(&n) {
                    return Err(format!("Enter a number from {min} to {max}"));
                }
                Value::from(n)
            }
        };
        let mut updated =
            serde_json::to_value(self).map_err(|_| "Cannot inspect interface settings")?;
        updated[key] = value;
        let updated: Self =
            serde_json::from_value(updated).map_err(|_| "Invalid interface settings")?;
        updated.validate()?;
        Ok(updated)
    }
    /// Environment variables take precedence over the corresponding saved value.
    pub fn environment_default(&self, name: &str) -> Option<String> {
        match name {
            "DAEDALUS_ICONS" if self.icons != "auto" => Some(self.icons.clone()),
            "DAEDALUS_REDUCED_MOTION" => Some(if self.reduced_motion { "1" } else { "0" }.into()),
            "DAEDALUS_BACKDROP" => Some(if self.backdrop { "1" } else { "0" }.into()),
            "DAEDALUS_SPLASH" => Some(if self.splash { "1" } else { "0" }.into()),
            "DAEDALUS_SCREENSAVER" => Some(self.screensaver_seconds.to_string()),
            "DAEDALUS_GRAPHICS" if self.graphics != "auto" => Some(
                if self.graphics == "cells" {
                    "0"
                } else {
                    "kitty"
                }
                .into(),
            ),
            "DAEDALUS_UPDATE_CHECK" => Some(if self.update_check { "1" } else { "0" }.into()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_navigation_settings_preserve_legacy_defaults_and_validate_both_loading_and_edits() {
        let defaults = InterfaceSettings::default();
        let mut legacy = serde_json::to_value(&defaults).unwrap();
        for (key, default, max) in [
            ("list_page_rows", 10, 1000),
            ("details_page_rows", 8, 1000),
            ("options_page_items", 5, 1000),
            ("mouse_scroll_rows", 3, 1000),
            ("choice_wheel_items", 1, 1000),
            ("search_query_limit_chars", 1024, 16384),
            ("shortcut_query_limit_chars", 160, 16384),
        ] {
            assert_eq!(
                legacy.as_object_mut().unwrap().remove(key),
                Some(Value::from(default))
            );
            for value in [1, max] {
                let changed = defaults.set(key, &value.to_string()).unwrap();
                assert_eq!(changed.value(key).unwrap(), value);
                assert_eq!(
                    serde_json::from_value::<InterfaceSettings>(
                        serde_json::to_value(&changed).unwrap()
                    )
                    .unwrap(),
                    changed
                );
            }
            for value in [0, max + 1] {
                assert!(defaults.set(key, &value.to_string()).is_err());
                let mut invalid = serde_json::to_value(&defaults).unwrap();
                invalid[key] = Value::from(value);
                assert!(
                    serde_json::from_value::<InterfaceSettings>(invalid)
                        .unwrap()
                        .validate()
                        .is_err()
                );
            }
        }
        assert_eq!(
            serde_json::from_value::<InterfaceSettings>(legacy).unwrap(),
            defaults
        );
    }
    #[test]
    fn clipboard_defaults_remain_compatible_and_have_bounded_editors() {
        let defaults = InterfaceSettings::default();
        let mut old = serde_json::to_value(&defaults).unwrap();
        old.as_object_mut()
            .unwrap()
            .remove("clipboard_read_limit_bytes");
        old.as_object_mut()
            .unwrap()
            .remove("clipboard_write_limit_bytes");
        assert_eq!(
            serde_json::from_value::<InterfaceSettings>(old).unwrap(),
            defaults
        );
        let example: serde_json::Value =
            serde_json::from_str(include_str!("../../../../config/control.example.json")).unwrap();
        assert_eq!(example["ui"], serde_json::to_value(&defaults).unwrap());
        for key in ["clipboard_read_limit_bytes", "clipboard_write_limit_bytes"] {
            assert!(defaults.set(key, "1").is_err());
            assert!(
                defaults
                    .set(key, &(MAX_CLIPBOARD_BYTES + 1).to_string())
                    .is_err()
            );
            defaults.set(key, "2").unwrap().validate().unwrap();
            defaults
                .set(key, &MAX_CLIPBOARD_BYTES.to_string())
                .unwrap()
                .validate()
                .unwrap();
        }
    }
    #[test]
    fn every_serialized_setting_has_exactly_one_editor_and_default() {
        let defaults = InterfaceSettings::default();
        let mut legacy = serde_json::to_value(&defaults).unwrap();
        legacy.as_object_mut().unwrap().remove("agent_session_ids");
        assert!(
            serde_json::from_value::<InterfaceSettings>(legacy)
                .unwrap()
                .agent_session_ids
        );
        defaults.validate().unwrap();
        let json = serde_json::to_value(&defaults).unwrap();
        let names: std::collections::BTreeSet<_> = SETTINGS.iter().map(|s| s.key).collect();
        assert_eq!(names.len(), SETTINGS.len());
        assert_eq!(
            names,
            json.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect()
        );
        for field in SETTINGS {
            assert!(!field.label.is_empty() && !field.description.is_empty());
        }
    }
    #[test]
    fn invalid_edits_are_rejected_without_mutating_current_values_or_echoing_them() {
        let old = InterfaceSettings::default();
        for (key, input) in [
            ("preview_interval_ms", "0"),
            ("input_batch_limit", "-1"),
            ("screensaver_seconds", "1"),
            ("screensaver_seconds", "2"),
            ("icons", "secret-value"),
            ("colors", "secret-value"),
            ("unknown", "secret-value"),
        ] {
            assert!(!old.set(key, input).unwrap_err().contains("secret-value"));
        }
        assert_eq!(old, InterfaceSettings::default());
        let new = old.set("ui.preview_interval_ms", "700").unwrap();
        assert_eq!(new.preview_interval_ms, 700);
        assert_eq!(old.preview_interval_ms, 2000);
        assert!(serde_json::from_str::<InterfaceSettings>(r#"{"unknown":true}"#).is_err());
    }
}
