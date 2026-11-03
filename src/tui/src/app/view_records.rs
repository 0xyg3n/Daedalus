//! Exact viewer relationships, with conservative reads of the legacy wire format.
use crate::core::{
    identity::{Backend, TerminalId},
    layouts::TerminalRef,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub const WARNING: &str = "Saved viewer records contain invalid, ambiguous or unsupported identities; unresolved links were omitted. Review saved viewer records before relying on view counts.";

fn field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}
fn structured(value: &Value) -> Option<TerminalId> {
    let fields = value.as_object()?;
    if fields.len() != 4
        || !["backend", "endpoint", "pane", "generation"]
            .iter()
            .all(|key| fields.contains_key(*key))
    {
        return None;
    }
    if ["endpoint", "pane", "generation"].iter().any(|key| {
        fields
            .get(*key)
            .and_then(Value::as_str)
            .is_none_or(|value| value.is_empty() || value.len() > 4096)
    }) {
        return None;
    }
    let id: TerminalId = serde_json::from_value(value.clone()).ok()?;
    id.validate().ok()?;
    Some(id)
}
fn legacy(backend: &str, server: &str, pane: &str, generation: &str) -> Option<TerminalId> {
    let backend = match backend {
        "tmux" => Backend::Tmux,
        "herdr" => Backend::Herdr,
        "native" => Backend::Native,
        _ => return None,
    };
    if pane.len() > 4096 || generation.len() > 4096 || server.len() > 5 + 3 * 4096 {
        return None;
    }
    TerminalRef {
        backend,
        server: server.into(),
        pane_id: pane.into(),
        terminal_id: generation.into(),
        generation: String::new(),
        label: String::new(),
        place: String::new(),
    }
    .identity()
    .ok()
}
pub fn source(row: &Value) -> Option<TerminalId> {
    let value = row.get("source")?;
    if !value.is_string() {
        return structured(value);
    }
    // Extra separators could belong to either pane or generation. Never infer
    // the original identity from whichever candidate happens to be live today.
    let mut parts = value.as_str()?.split('/');
    let (backend, server, pane, generation) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    legacy(backend, server, pane, generation)
}
pub fn viewer(row: &Value) -> Option<TerminalId> {
    if let Some(value) = row.get("viewer") {
        return structured(value);
    }
    let generation = if row.get("viewer_terminal").is_some() {
        field(row, "viewer_terminal")
    } else {
        field(row, "terminal_id")
    };
    legacy(
        field(row, "backend"),
        field(row, "server"),
        field(row, "pane_id"),
        generation,
    )
}
pub struct Record<'a> {
    pub row: &'a Value,
    pub source: TerminalId,
    pub viewer: TerminalId,
}
#[derive(Default)]
pub struct Resolved<'a> {
    pub records: Vec<Record<'a>>,
    pub unresolved: usize,
}
pub fn resolve(rows: &[Value], mut proceed: impl FnMut() -> bool) -> Resolved<'_> {
    let mut result = Resolved::default();
    let mut seen = HashMap::new();
    let mut conflicts = HashSet::new();
    let mut invalid_viewers = HashSet::new();
    for row in rows {
        if !proceed() {
            result.unresolved += 1;
            result.records.clear();
            return result;
        }
        let decoded = (|| {
            if row.get("version").is_some_and(|value| value != 2) {
                return None;
            }
            let source = source(row)?;
            let viewer = viewer(row)?;
            if source == viewer {
                return None;
            }
            Some(Record {
                row,
                source,
                viewer,
            })
        })();
        let Some(record) = decoded else {
            result.unresolved += 1;
            if let Some(viewer) = viewer(row) {
                invalid_viewers.insert(viewer);
            }
            continue;
        };
        if let Some(index) = seen.get(&record.viewer).copied() {
            let previous: &Record<'_> = &result.records[index];
            if previous.source != record.source {
                conflicts.insert(record.viewer);
            }
        } else {
            seen.insert(record.viewer.clone(), result.records.len());
            result.records.push(record);
        }
    }
    result.unresolved += conflicts.len();
    // Unseen late rows might contradict an earlier association. Publish no
    // relationship until the complete identity pass finishes within its budget.
    if !proceed() {
        result.records.clear();
        result.unresolved += 1;
        return result;
    }
    result.records.retain(|record| {
        !conflicts.contains(&record.viewer) && !invalid_viewers.contains(&record.viewer)
    });
    result
}

#[derive(Default)]
pub struct Observation {
    pub rows: Vec<Value>,
    pub unresolved: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn id(pane: &str, generation: &str) -> Value {
        json!({"backend":"herdr","endpoint":"fixture","pane":pane,"generation":generation})
    }
    #[test]
    fn structured_fields_keep_delimiters_distinct_and_legacy_ambiguity_is_refused() {
        let a = json!({"source":id("a/b","c"),"viewer":id("v1","generation")});
        let b = json!({"source":id("a","b/c"),"viewer":id("v2","generation")});
        assert_ne!(source(&a), source(&b));
        assert!(source(&json!({"source":"herdr/fixture/a/b/c"})).is_none());
        assert_eq!(
            source(&json!({"source":"herdr/fixture/pane/generation"})),
            Some(serde_json::from_value(id("pane", "generation")).unwrap())
        );
        let rows = [a, b];
        assert_eq!(resolve(&rows, || true).records.len(), 2);
    }
    #[test]
    fn invalid_unknown_self_and_conflicting_viewer_records_are_never_arbitrarily_selected() {
        let a = json!({"source":id("a","generation"),"viewer":id("viewer","generation")});
        let mut b = a.clone();
        b["source"] = id("b", "generation");
        let rows = [a.clone(), a.clone()];
        assert_eq!(resolve(&rows, || true).records.len(), 1);
        let rows = [a.clone(), b];
        let resolved = resolve(&rows, || true);
        assert!(resolved.records.is_empty());
        assert_eq!(resolved.unresolved, 1);
        let mut bad = a.clone();
        bad["source"]["generation"] = json!("");
        assert!(source(&bad).is_none());
        let mut bad = a.clone();
        bad["viewer"]["future_field"] = json!(true);
        assert!(viewer(&bad).is_none());
        let mut bad = a.clone();
        bad["source"] = bad["viewer"].clone();
        assert!(resolve(&[bad], || true).records.is_empty());
        let mut bad = a;
        bad["version"] = json!(3);
        assert!(resolve(&[bad], || true).records.is_empty());
        assert_eq!(resolve(&rows, || false).unresolved, 1);
    }
    #[test]
    fn partial_or_malformed_duplicate_records_cannot_establish_unique_affinity() {
        let good = json!({"source":id("source","generation"),"viewer":id("viewer","generation")});
        let mut bad = good.clone();
        bad["source"] = json!("herdr/fixture/a/b/c");
        assert!(resolve(&[good.clone(), bad], || true).records.is_empty());
        let mut calls = 0;
        assert!(
            resolve(&[good.clone(), good.clone()], || {
                calls += 1;
                calls < 2
            })
            .records
            .is_empty()
        );
        calls = 0;
        assert!(
            resolve(&[good], || {
                calls += 1;
                calls < 2
            })
            .records
            .is_empty()
        );
    }
}
