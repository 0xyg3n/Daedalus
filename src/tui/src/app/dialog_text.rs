//! Human-readable review text. Keep the complete plan available for informed approval;
//! only presentation changes here, never the values used to approve or apply it.
use serde::Serialize;
use serde_json::Value;

pub fn format(value: &impl Serialize) -> Result<String, serde_json::Error> {
    serde_json::to_value(value).map(|value| review(&value))
}

pub fn review(value: &Value) -> String {
    let mut lines = Vec::new();
    render(value, 0, &mut lines);
    lines.join("\n")
}

fn label(key: &str) -> String {
    key.split('_')
        .map(|word| match word {
            "id" => "ID".into(),
            "pid" => "PID".into(),
            "cwd" => "Working directory".into(),
            "sha" => "SHA".into(),
            "sha256" => "SHA256".into(),
            "ms" => "Milliseconds".into(),
            "url" => "URL".into(),
            "api" => "API".into(),
            "telegram" => "Telegram".into(),
            "hindsight" => "Hindsight".into(),
            word => word.into(),
        })
        .collect::<Vec<String>>()
        .join(" ")
        .chars()
        .enumerate()
        .flat_map(|(i, c)| {
            if i == 0 {
                c.to_uppercase().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn scalar(value: &Value) -> Option<String> {
    Some(match value {
        Value::Null => "Not set".into(),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        Value::String(s) if s.is_empty() => "Not set".into(),
        Value::String(s) => s
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .collect(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) if a.is_empty() => "None".into(),
        Value::Object(o) if o.is_empty() => "None".into(),
        _ => return None,
    })
}

fn field(name: &str, value: &Value, depth: usize, lines: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    if let Some(mut text) = scalar(value) {
        if matches!(name, "State" | "Status" | "Readiness") {
            text = match text.as_str() {
                "not_running" => "Not running",
                "not_configured" => "Not configured",
                "unknown" => "Unknown",
                "pending" => "Pending",
                "queued" => "Queued",
                "submitted" => "Submitted",
                "completed" => "Completed",
                "failed" => "Failed",
                "ready" => "Ready",
                "running" => "Running",
                "stopped" => "Stopped",
                "saved" => "Saved",
                _ => &text,
            }
            .to_owned();
        }
        let mut parts = text.split('\n');
        let separator = if name.ends_with('.') { "" } else { ":" };
        lines.push(format!(
            "{indent}{name}{separator} {}",
            parts.next().unwrap_or("")
        ));
        lines.extend(parts.map(|line| format!("{indent}  {line}")));
    } else {
        lines.push(format!("{indent}{name}"));
        render(value, depth + 1, lines);
    }
}

fn render(value: &Value, depth: usize, lines: &mut Vec<String>) {
    if let Some(text) = scalar(value) {
        let indent = "  ".repeat(depth);
        lines.extend(text.split('\n').map(|line| format!("{indent}{line}")));
        return;
    }
    match value {
        Value::Object(fields) => {
            // Outcomes and actions lead; detailed identities remain visible below.
            const FIRST: &[&str] = &["operation", "status", "state", "message", "effect"];
            for key in FIRST {
                if let Some(value) = fields.get(*key) {
                    field(&label(key), value, depth, lines);
                }
            }
            for (key, value) in fields {
                if !FIRST.contains(&key.as_str()) {
                    field(&label(key), value, depth, lines);
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                field(&format!("{}.", index + 1), item, depth, lines);
            }
        }
        _ => unreachable!("scalar values handled above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn review_keeps_identity_and_uncertain_status_readable() {
        let text = review(&json!({
            "terminal": {"pane": "%12", "generation": "abc:123"},
            "submitted": true, "completed": false,
            "goal_id": null, "commands": ["provider --resume a-b"]
        }));
        assert!(text.contains("Terminal\n  Generation: abc:123\n  Pane: %12"));
        assert!(text.contains("Submitted: Yes"));
        assert!(text.contains("Completed: No"));
        assert!(text.contains("Goal ID: Not set"));
        assert!(text.contains("Commands\n  1. provider --resume a-b"));
        assert!(!text.contains('{'));
    }

    #[test]
    fn review_preserves_multiline_unicode_and_long_approval_values() {
        let approval = "abc123".repeat(100);
        let text = review(&json!({"approval": approval, "note": "Έλεγχος\nnext step"}));
        assert!(text.contains(&approval));
        assert!(text.contains("Note: Έλεγχος\n  next step"));
    }

    #[test]
    fn only_known_status_values_are_humanized() {
        let text = review(
            &json!({"status": "not_running", "pane": "not_running", "message": "submitted, awaiting readiness"}),
        );
        assert!(text.starts_with("Status: Not running\nMessage: submitted, awaiting readiness"));
        assert!(text.contains("Pane: not_running"));
        assert!(review(&json!({"status": "provider_specific"})).contains("provider_specific"));
    }

    #[test]
    fn review_displays_absent_values_and_strips_control_characters() {
        let text = review(&json!({"grants": [], "settings": {}, "note": "hello\u{7}world"}));
        assert!(text.contains("Grants: None"));
        assert!(text.contains("Settings: None"));
        assert!(text.contains("Note: helloworld"));
    }
}
