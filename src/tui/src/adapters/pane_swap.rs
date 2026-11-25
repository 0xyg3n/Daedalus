//! Explicit same-tab Herdr swap. Revalidate both terminal generations before submission.
use super::terminals::{self, Terminals};
use crate::core::identity::Backend;
use serde_json::Value;
pub fn apply(adapter: &Terminals, source: &Value, target: &Value) -> Result<String, String> {
    if [source, target]
        .iter()
        .any(|p| super::pane_observations::conflicted(p))
    {
        return Err(super::pane_observations::WARNING.into());
    }
    validate(source, target)?;
    let source = adapter.verify(source)?;
    let target = adapter.verify(target)?;
    let a = terminals::identity(&source)?;
    let b = terminals::identity(&target)?;
    validate(&source, &target)?;
    let before = adapter.herdr_result(&a.endpoint, &["pane", "layout", "--pane", &a.pane])?;
    let layout = &before["layout"];
    let panes = layout["panes"]
        .as_array()
        .ok_or("Herdr layout is unavailable")?;
    if !panes.iter().any(|p| p["pane_id"] == a.pane)
        || !panes.iter().any(|p| p["pane_id"] == b.pane)
    {
        return Err("Swap target left the tab; refresh before swapping".into());
    }
    adapter.verify(&source)?;
    adapter.verify(&target)?;
    let result = adapter
        .herdr_result(
            &a.endpoint,
            &[
                "pane",
                "swap",
                "--source-pane",
                &a.pane,
                "--target-pane",
                &b.pane,
            ],
        )
        .map_err(|_| "Pane swap outcome is unconfirmed; inspect the layout before retrying")?;
    if !confirms(layout, &result["swap"], &a.pane, &b.pane) {
        return Err(
            "Herdr did not confirm the pane swap; inspect the layout before retrying".into(),
        );
    }
    Ok("Herdr confirmed the pane swap".into())
}
fn confirms(before: &Value, swap: &Value, source: &str, target: &str) -> bool {
    if swap["changed"] != true
        || swap["source_pane_id"] != source
        || swap["target_pane_id"] != target
    {
        return false;
    }
    let after = &swap["layout"];
    let (Some(old), Some(new)) = (before["panes"].as_array(), after["panes"].as_array()) else {
        return false;
    };
    if old.len() != new.len()
        || before["area"] != after["area"]
        || before["tab_id"] != after["tab_id"]
        || before["workspace_id"] != after["workspace_id"]
    {
        return false;
    }
    old.iter().all(|p| {
        let id = p["pane_id"].as_str().unwrap_or_default();
        let peer = if id == source {
            target
        } else if id == target {
            source
        } else {
            id
        };
        new.iter()
            .filter(|n| n["pane_id"] == peer && n["rect"] == p["rect"])
            .count()
            == 1
    })
}

fn validate(source: &Value, target: &Value) -> Result<(), String> {
    let a = terminals::identity(source)?;
    let b = terminals::identity(target)?;
    if a.backend != Backend::Herdr
        || b.backend != Backend::Herdr
        || a.endpoint != b.endpoint
        || a.pane == b.pane
    {
        return Err("Select two distinct Herdr panes on the same server".into());
    }
    for key in ["workspace_id", "tab_id"] {
        let left = source[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or("Missing Herdr tab identity")?;
        if target[key].as_str() != Some(left) {
            return Err("Herdr pane swaps require the same tab".into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn reject_cross_server_cross_tab_and_same_pane() {
        let a = json!({"backend":"herdr","session":"work","pane_id":"a","terminal_id":"t-a","workspace_id":"w","tab_id":"t"});
        let mut b = a.clone();
        b["pane_id"] = json!("b");
        b["terminal_id"] = json!("t-b");
        assert!(validate(&a, &b).is_ok());
        for (key, value) in [
            ("session", "other"),
            ("tab_id", "other"),
            ("workspace_id", "other"),
            ("pane_id", "a"),
        ] {
            let mut invalid = b.clone();
            invalid[key] = json!(value);
            assert!(validate(&a, &invalid).is_err());
        }
    }
    #[test]
    fn acknowledgement_must_include_the_exact_swapped_geometry() {
        let before = json!({"area":{"width":80,"height":24},"tab_id":"t","workspace_id":"w","panes":[{"pane_id":"a","rect":{"x":0,"width":40}},{"pane_id":"b","rect":{"x":40,"width":40}}]});
        let mut after = before.clone();
        after["panes"][0]["pane_id"] = json!("b");
        after["panes"][1]["pane_id"] = json!("a");
        let mut swap =
            json!({"changed":true,"source_pane_id":"a","target_pane_id":"b","layout":after});
        assert!(confirms(&before, &swap, "a", "b"));
        swap["layout"] = before.clone();
        assert!(!confirms(&before, &swap, "a", "b"));
        swap["changed"] = json!(false);
        assert!(!confirms(&before, &swap, "a", "b"));
    }
}
