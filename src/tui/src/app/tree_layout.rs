//! Complete, exact and bounded native layout observations. No terminal effects.
use super::{
    discovery_budget::json_size,
    runtime_settings::Settings,
    tree::{Node, Tree},
};
use crate::core::geometry::{Geometry, Rect};
use serde::{Serialize, Serializer, ser::SerializeStruct};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    time::{Duration, Instant},
};

const INVALID: &str = "Layout geometry is incomplete: invalid, overlapping or unsupported coordinates; refresh before moving dividers.";
const AMBIGUOUS: &str = "Layout geometry is incomplete: missing or ambiguous pane identity; refresh before moving dividers.";
const DUPLICATE: &str = "Layout geometry is incomplete: duplicate or conflicting layout rows; refresh before moving dividers.";
const PANES: &str = "Layout geometry is incomplete: pane limit reached; review Layout panes per group in Terminals and discovery.";
const BYTES: &str = "Layout geometry is incomplete: metadata budget reached; review Layout metadata budget in Terminals and discovery.";
const TIME: &str = "Layout geometry is incomplete: deadline reached; review Layout geometry deadline in Terminals and discovery.";
const CONFLICT: &str = "Layout geometry is incomplete: conflicting terminal observations; refresh before moving dividers.";

pub struct Projected {
    pub parent: usize,
    pub value: Value,
    pub order: Vec<usize>,
}
#[derive(Default)]
pub struct Projection {
    pub groups: Vec<Projected>,
    pub warnings: BTreeSet<&'static str>,
}
struct Budget {
    remaining: usize,
    deadline: Instant,
    warnings: BTreeSet<&'static str>,
}
impl Budget {
    fn left(&mut self) -> bool {
        if Instant::now() >= self.deadline {
            self.warnings.insert(TIME);
            false
        } else {
            true
        }
    }
    fn admit(&mut self, value: &impl Serialize) -> bool {
        if !self.left() {
            return false;
        }
        let Ok(bytes) = json_size(value, self.remaining) else {
            self.warnings.insert(BYTES);
            return false;
        };
        if !self.left() {
            return false;
        }
        self.remaining -= bytes;
        true
    }
}
fn rows<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn field<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn cells<const N: usize>(value: &Value, keys: [&str; N]) -> Option<[u16; N]> {
    let mut result = [0; N];
    for (i, key) in keys.iter().enumerate() {
        let n = value.get(*key)?;
        result[i] = u16::try_from(
            n.as_u64()
                .or_else(|| n.as_str().and_then(|n| n.parse().ok()))?,
        )
        .ok()?;
    }
    Some(result)
}
#[derive(Default)]
struct Group {
    size: Option<[u16; 2]>,
    zoomed: bool,
    rects: Vec<(usize, Rect, bool)>,
    seen: HashSet<usize>,
    report: Option<usize>,
    invalid: bool,
}
struct WirePane<'a> {
    node: &'a Node,
    rect: Rect,
    focused: bool,
}
impl Serialize for WirePane<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut row = serializer.serialize_struct("LayoutPane", 6)?;
        row.serialize_field("id", &self.node.id())?;
        row.serialize_field("x", &self.rect.x)?;
        row.serialize_field("y", &self.rect.y)?;
        row.serialize_field("w", &self.rect.w)?;
        row.serialize_field("h", &self.rect.h)?;
        row.serialize_field("focused", &self.focused)?;
        row.end()
    }
}
#[derive(Serialize)]
struct WireLayout<'a> {
    width: u16,
    height: u16,
    zoomed: bool,
    panes: Vec<WirePane<'a>>,
}

pub fn project(tree: &Tree, snapshot: &Value, settings: &Settings) -> Projection {
    let now = Instant::now();
    let mut budget = Budget {
        remaining: settings.discovery_layout_limit_bytes as usize,
        deadline: now
            .checked_add(Duration::from_millis(settings.discovery_layout_timeout_ms))
            .unwrap_or(now),
        warnings: BTreeSet::new(),
    };
    if settings.validate().is_err() {
        budget
            .warnings
            .insert("Layout geometry is incomplete: invalid saved runtime settings.");
        return Projection {
            groups: Vec::new(),
            warnings: budget.warnings,
        };
    }
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut coordinates: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (index, node) in tree.nodes.iter().enumerate() {
        if !budget.left() {
            return Projection {
                groups: Vec::new(),
                warnings: budget.warnings,
            };
        }
        if node.kind != "pane" {
            continue;
        }
        let Some(pane) = &node.pane else {
            continue;
        };
        let Some(parent) = node.parent else {
            continue;
        };
        members.entry(parent).or_default().push(index);
        if field(pane, "backend") == "herdr" {
            coordinates
                .entry((field(pane, "session"), field(pane, "pane_id")))
                .or_default()
                .push(index);
        }
    }
    let mut groups: BTreeMap<usize, Group> = BTreeMap::new();
    for (parent, nodes) in &members {
        let pane = tree.nodes[nodes[0]].pane.as_ref().unwrap();
        if field(pane, "backend") != "tmux" {
            continue;
        }
        let reported = nodes.iter().any(|n| {
            ["left", "top", "width", "height"]
                .iter()
                .any(|k| tree.nodes[*n].pane.as_ref().unwrap().get(*k).is_some())
        });
        if !reported {
            continue;
        }
        if nodes.len() > settings.discovery_layout_pane_limit as usize {
            budget.warnings.insert(PANES);
            continue;
        }
        let group = groups.entry(*parent).or_default();
        for index in nodes {
            if !budget.left() {
                group.invalid = true;
                break;
            }
            let pane = tree.nodes[*index].pane.as_ref().unwrap();
            let Some([x, y, w, h]) = cells(pane, ["left", "top", "width", "height"]) else {
                group.invalid = true;
                budget.warnings.insert(INVALID);
                continue;
            };
            group
                .rects
                .push((*index, Rect { x, y, w, h }, pane["active"] == "1"));
        }
    }
    let mut bad_sessions = HashSet::new();
    for (report, layout) in rows(snapshot, "layouts").iter().enumerate() {
        if !budget.left() {
            break;
        }
        let session = field(layout, "session");
        let Some([width, height]) = cells(&layout["area"], ["width", "height"]) else {
            budget.warnings.insert(INVALID);
            bad_sessions.insert(session);
            continue;
        };
        if width == 0 || height == 0 {
            budget.warnings.insert(INVALID);
            bad_sessions.insert(session);
            continue;
        }
        for item in rows(layout, "panes") {
            if !budget.left() {
                bad_sessions.insert(session);
                break;
            }
            let candidates = coordinates
                .get(&(session, field(item, "pane_id")))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let matching: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|index| {
                    let pane = tree.nodes[*index].pane.as_ref().unwrap();
                    ["workspace_id", "tab_id", "terminal_id"].iter().all(|key| {
                        let expected = if *key == "terminal_id" {
                            item.get(*key)
                        } else {
                            layout.get(*key)
                        };
                        expected
                            .filter(|v| !v.is_null())
                            .is_none_or(|v| pane.get(*key) == Some(v))
                    })
                })
                .collect();
            if matching.len() != 1 {
                budget.warnings.insert(AMBIGUOUS);
                bad_sessions.insert(session);
                continue;
            }
            let index = matching[0];
            let parent = tree.nodes[index].parent.unwrap();
            let group = groups.entry(parent).or_default();
            if members[&parent].len() > settings.discovery_layout_pane_limit as usize {
                budget.warnings.insert(PANES);
                group.invalid = true;
                continue;
            }
            if group.report.is_some_and(|previous| previous != report) || !group.seen.insert(index)
            {
                budget.warnings.insert(DUPLICATE);
                group.invalid = true;
                continue;
            }
            group.report = Some(report);
            group.size = Some([width, height]);
            group.zoomed = layout["zoomed"] == true;
            let Some([x, y, w, h]) = cells(&item["rect"], ["x", "y", "width", "height"]) else {
                group.invalid = true;
                budget.warnings.insert(INVALID);
                continue;
            };
            group
                .rects
                .push((index, Rect { x, y, w, h }, item["focused"] == true));
        }
    }
    let mut projection = Projection::default();
    for (parent, mut group) in groups {
        if !budget.left() {
            break;
        }
        let pane = tree.nodes[members[&parent][0]].pane.as_ref().unwrap();
        if members[&parent].iter().any(|index| {
            tree.nodes[*index]
                .pane
                .as_ref()
                .is_some_and(crate::adapters::pane_observations::conflicted)
        }) {
            budget.warnings.insert(CONFLICT);
            continue;
        }
        if field(pane, "backend") == "herdr" && bad_sessions.contains(field(pane, "session")) {
            continue;
        }
        if group.invalid {
            continue;
        }
        if group.rects.is_empty() || (!group.zoomed && group.rects.len() != members[&parent].len())
        {
            budget.warnings.insert(INVALID);
            continue;
        }
        let width = group
            .rects
            .iter()
            .map(|(_, r, _)| r.right())
            .max()
            .unwrap_or_default();
        let height = group
            .rects
            .iter()
            .map(|(_, r, _)| r.bottom())
            .max()
            .unwrap_or_default();
        let Some([width, height]) = group
            .size
            .or_else(|| Some([u16::try_from(width).ok()?, u16::try_from(height).ok()?]))
        else {
            budget.warnings.insert(INVALID);
            continue;
        };
        if Geometry::new(
            width,
            height,
            group.rects.iter().map(|(_, r, _)| *r).collect(),
        )
        .is_none()
        {
            budget.warnings.insert(INVALID);
            continue;
        }
        group.rects.sort_by_key(|(_, r, _)| (r.y, r.x));
        let wire = WireLayout {
            width,
            height,
            zoomed: group.zoomed,
            panes: group
                .rects
                .iter()
                .map(|(index, r, focused)| WirePane {
                    node: &tree.nodes[*index],
                    rect: *r,
                    focused: *focused,
                })
                .collect(),
        };
        if !budget.admit(&wire) {
            continue;
        }
        let Ok(value) = serde_json::to_value(&wire) else {
            budget.warnings.insert(INVALID);
            continue;
        };
        if !budget.left() {
            continue;
        }
        projection.groups.push(Projected {
            parent,
            value,
            order: group.rects.iter().map(|(index, _, _)| *index).collect(),
        });
    }
    projection.warnings = budget.warnings;
    projection
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tree::ProjectionContext;
    use serde_json::json;
    #[test]
    fn group_pane_and_total_byte_limits_refuse_only_complete_layouts() {
        let panes:Vec<_>=(0..2).flat_map(|view|(0..5).map(move |n|json!({"backend":"tmux","socket":crate::test_support::path("/tmp/layout-budget.sock"),"session":format!("v{view}"),"session_id":format!("${view}"),"window_id":"@1","pane_id":format!("%{n}"),"terminal_id":"generation".repeat(30),"left":n*10,"top":0,"width":9,"height":24}))).collect();
        let snapshot = json!({"panes":panes});
        let tree = Tree::build(&snapshot, &json!({}), &ProjectionContext::default());
        let mut settings = Settings {
            discovery_layout_pane_limit: 4,
            ..Settings::default()
        };
        let partial = project(&tree, &snapshot, &settings);
        assert!(partial.groups.is_empty() && partial.warnings.contains(PANES));
        settings.discovery_layout_pane_limit = 5;
        let complete = project(&tree, &snapshot, &settings);
        assert_eq!(complete.groups.len(), 2);
        assert!(complete.warnings.is_empty());
        let sizes: Vec<_> = complete
            .groups
            .iter()
            .map(|group| json_size(&group.value, usize::MAX).unwrap())
            .collect();
        assert!(sizes[0] >= 1024);
        settings.discovery_layout_limit_bytes = (sizes[0] + sizes[1] - 1) as u64;
        let partial = project(&tree, &snapshot, &settings);
        assert_eq!(partial.groups.len(), 1);
        assert!(partial.warnings.contains(BYTES));
        settings.discovery_layout_limit_bytes = 1024;
        let partial = project(&tree, &snapshot, &settings);
        assert!(partial.groups.is_empty() && partial.warnings.contains(BYTES));
    }
    #[test]
    fn late_serialization_is_refused_without_consuming_capacity() {
        struct Slow;
        impl Serialize for Slow {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                std::thread::sleep(Duration::from_millis(5));
                serializer.serialize_str("late")
            }
        }
        let mut budget = Budget {
            remaining: 1024,
            deadline: Instant::now() + Duration::from_millis(1),
            warnings: BTreeSet::new(),
        };
        assert!(!budget.admit(&Slow));
        assert_eq!(budget.remaining, 1024);
        assert!(budget.warnings.contains(TIME));
    }
    #[test]
    fn herdr_layout_rows_cannot_restore_geometry_for_a_conflicted_terminal() {
        let first = json!({"backend":"herdr","session":"fixture","server":"fixture","workspace_id":"work","tab_id":"tab","pane_id":"p0","terminal_id":"g0","agent_session":{"kind":"id","value":"one"}});
        let mut duplicate = first.clone();
        duplicate["agent_session"]["value"] = json!("two");
        let second = json!({"backend":"herdr","session":"fixture","server":"fixture","workspace_id":"work","tab_id":"tab","pane_id":"p1","terminal_id":"g1"});
        let snapshot = json!({"panes":[first,duplicate,second],"layouts":[{"session":"fixture","workspace_id":"work","tab_id":"tab","area":{"width":100,"height":24},"panes":[{"pane_id":"p0","rect":{"x":0,"y":0,"width":49,"height":24}},{"pane_id":"p1","rect":{"x":50,"y":0,"width":50,"height":24}}]}]});
        let tree = Tree::build(&snapshot, &json!({}), &ProjectionContext::default());
        assert!(
            tree.nodes
                .iter()
                .filter(|node| node.kind == "tab")
                .all(|node| node.attributes.get("layout").is_none())
        );
        assert!(tree.layout_warnings.contains(&CONFLICT));
    }
    #[test]
    fn zoomed_subset_is_valid_but_uncontained_or_conflicting_rows_are_not() {
        let panes:Vec<_>=(0..2).map(|n|json!({"backend":"herdr","session":"fixture","server":"fixture","workspace_id":"work","tab_id":"tab","pane_id":format!("p{n}"),"terminal_id":format!("g{n}")})).collect();
        let mut snapshot = json!({"panes":panes,"layouts":[{"session":"fixture","workspace_id":"work","tab_id":"tab","area":{"width":100,"height":24},"zoomed":true,"panes":[{"pane_id":"p0","rect":{"x":0,"y":0,"width":100,"height":24}}]}]});
        let context = ProjectionContext::default();
        let tree = Tree::build(&snapshot, &json!({}), &context);
        let good = project(&tree, &snapshot, &context.runtime);
        assert_eq!(good.groups.len(), 1);
        assert!(good.warnings.is_empty());
        snapshot["layouts"][0]["panes"][0]["rect"]["x"] = json!(1);
        let bad = project(&tree, &snapshot, &context.runtime);
        assert!(bad.groups.is_empty() && bad.warnings.contains(INVALID));
        snapshot["layouts"][0]["panes"][0]["rect"]["x"] = json!(0);
        let repeated = snapshot["layouts"][0].clone();
        snapshot["layouts"].as_array_mut().unwrap().push(repeated);
        let bad = project(&tree, &snapshot, &context.runtime);
        assert!(bad.groups.is_empty() && bad.warnings.contains(DUPLICATE));
    }
}
