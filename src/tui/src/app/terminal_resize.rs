//! Resize an observed split only while both exact terminals and its geometry match.
use super::{
    config::ControlConfig,
    inventory,
    tree::{ProjectionContext, Tree},
};
use crate::adapters::terminals::{self, Terminals};
use crate::core::identity::Backend;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub peer: String,
    pub axis: Axis,
    pub cells: i32,
    pub layout: Layout,
}
#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    Horizontal,
    Vertical,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    width: u16,
    height: u16,
    zoomed: bool,
    panes: Vec<Rect>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
struct Rect {
    id: String,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    #[serde(default)]
    focused: bool,
}
impl Layout {
    fn canonical(&self) -> Self {
        let mut l = self.clone();
        for p in &mut l.panes {
            p.focused = false;
        }
        l.panes.sort_by(|a, b| a.id.cmp(&b.id));
        l
    }
}
struct Plan {
    pane: Value,
    peer: Value,
    backend: Backend,
    endpoint: String,
    target: String,
    direction: &'static str,
    count: u32,
    fraction: f64,
}
fn prepare(tree: &Tree, node: &str, request: &Request) -> Result<Plan> {
    if request.cells == 0
        || request.cells.unsigned_abs() > 4096
        || node == request.peer
        || request.layout.zoomed
        || request.layout.panes.len() > 128
    {
        return Err("Invalid resize; select an unzoomed split and move its divider".into());
    }
    let selected = tree
        .find(node)
        .ok_or("Pane disappeared; refresh before resizing")?;
    let peer = tree
        .find(&request.peer)
        .ok_or("Neighbor disappeared; refresh before resizing")?;
    if selected.parent != peer.parent || selected.parent.is_none() {
        return Err("Panes no longer share a tab".into());
    }
    let parent = &tree.nodes[selected.parent.unwrap()];
    let current: Layout = serde_json::from_value(
        parent
            .attributes
            .get("layout")
            .cloned()
            .ok_or("Pane layout unavailable")?,
    )
    .map_err(|_| "Invalid live pane layout")?;
    if current.canonical() != request.layout.canonical() {
        return Err("Pane layout changed; drag the current divider again".into());
    }
    let a = current
        .panes
        .iter()
        .find(|p| p.id == node)
        .ok_or("Pane missing from layout")?;
    let b = current
        .panes
        .iter()
        .find(|p| p.id == request.peer)
        .ok_or("Neighbor missing from layout")?;
    let horizontal = request.axis == Axis::Horizontal;
    let (start, size, next, other, overlap) = if horizontal {
        (
            a.x as i32,
            a.w as i32,
            b.x as i32,
            b.w as i32,
            (a.y as u32) < (b.y as u32 + b.h as u32) && (b.y as u32) < (a.y as u32 + a.h as u32),
        )
    } else {
        (
            a.y as i32,
            a.h as i32,
            b.y as i32,
            b.h as i32,
            (a.x as u32) < (b.x as u32 + b.w as u32) && (b.x as u32) < (a.x as u32 + a.w as u32),
        )
    };
    if !overlap
        || !(0..=1).contains(&(next - start - size))
        || size + request.cells < 3
        || other - request.cells < 3
    {
        return Err("Resize exceeds the shared divider or minimum pane size".into());
    }
    let pane = selected.pane.clone().ok_or("Select a live pane")?;
    let peer = peer.pane.clone().ok_or("Select a live neighbor")?;
    let id = terminals::identity(&pane)?;
    let peer_id = terminals::identity(&peer)?;
    if id.backend != peer_id.backend || id.endpoint != peer_id.endpoint || id == peer_id {
        return Err("Resize targets differ from the observed backend".into());
    }
    // Grow the pane on the side toward which the shared divider moves. This
    // targets the same edge when nested panes also have another nearby split.
    let target = if request.cells > 0 {
        id.pane.clone()
    } else {
        peer_id.pane
    };
    let direction = match (horizontal, request.cells > 0) {
        (true, true) => "right",
        (true, false) => "left",
        (false, true) => "down",
        (false, false) => "up",
    };
    let count = request.cells.unsigned_abs();
    Ok(Plan {
        pane,
        peer,
        backend: id.backend,
        endpoint: id.endpoint,
        target,
        direction,
        count,
        fraction: count as f64 / (next - start + other) as f64,
    })
}
pub fn apply(
    config: &ControlConfig,
    terminals: &Terminals,
    node: &str,
    request: Request,
) -> Result<String> {
    let mut roots = vec![PathBuf::from("/tmp")];
    if let Some(p) = std::env::var_os("TMUX_TMPDIR") {
        roots.push(p.into());
    }
    let snapshot = inventory::collect(config, terminals, &roots);
    let tree = Tree::build(&snapshot, &json!({}), &ProjectionContext::default());
    let plan = prepare(&tree, node, &request)?;
    terminals.verify(&plan.pane)?;
    terminals.verify(&plan.peer)?;
    if plan.backend == Backend::Native {
        return Err(
            "Resize the native terminal view; its PTY follows the attached view size".into(),
        );
    }
    let success = if plan.backend == Backend::Herdr {
        let amount = format!("{:.8}", plan.fraction.min(0.5));
        let reply = terminals.herdr_result(
            &plan.endpoint,
            &[
                "pane",
                "resize",
                "--pane",
                &plan.target,
                "--direction",
                plan.direction,
                "--amount",
                &amount,
            ],
        );
        match reply {
            Ok(v) => {
                return Ok(if v["resize"]["changed"] == true {
                    "Pane divider resized"
                } else {
                    "Pane divider is already at its size limit"
                }
                .into());
            }
            Err(_) => false,
        }
    } else {
        let direction = match plan.direction {
            "left" => "-L",
            "right" => "-R",
            "up" => "-U",
            _ => "-D",
        };
        terminals
            .query(
                &terminals.tmux,
                &[
                    "-S",
                    &plan.endpoint,
                    "resize-pane",
                    "-t",
                    &plan.target,
                    direction,
                    &plan.count.to_string(),
                ],
            )
            .is_ok()
    };
    if success {
        Ok("Pane divider resized".into())
    } else {
        Err("Resize outcome unconfirmed; inspect the layout before trying again".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Tree, String, Request) {
        let snapshot = json!({"panes":[{"backend":"tmux","socket":crate::test_support::path("/tmp/example"),"session":"test","session_id":"$1","window":"test","window_id":"@1","pane_id":"%1","pid":"42","terminal_id":"one","left":"0","top":"0","width":"49","height":"40"},{"backend":"tmux","socket":crate::test_support::path("/tmp/example"),"session":"test","session_id":"$1","window":"test","window_id":"@1","pane_id":"%2","pid":"43","terminal_id":"two","left":"50","top":"0","width":"50","height":"40"}]});
        let tree = Tree::build(&snapshot, &json!({}), &ProjectionContext::default());
        let nodes: Vec<_> = tree.nodes.iter().filter(|n| n.pane.is_some()).collect();
        let node = nodes[0].id();
        let peer = nodes[1].id();
        let parent = nodes[0].parent.unwrap();
        let layout: Layout =
            serde_json::from_value(tree.nodes[parent].attributes["layout"].clone()).unwrap();
        let request = Request {
            peer,
            axis: Axis::Horizontal,
            cells: 5,
            layout,
        };
        (tree, node, request)
    }
    #[test]
    fn exact_shared_divider_grows_the_correct_side() {
        let (t, n, mut r) = fixture();
        let p = prepare(&t, &n, &r).unwrap();
        assert_eq!(p.target, "%1");
        assert_eq!(p.direction, "right");
        r.cells = -5;
        let p = prepare(&t, &n, &r).unwrap();
        assert_eq!(p.target, "%2");
        assert_eq!(p.direction, "left");
    }
    #[test]
    fn stale_geometry_non_neighbors_and_tiny_panes_are_rejected() {
        let (t, n, mut r) = fixture();
        r.layout.panes[0].w -= 1;
        assert!(prepare(&t, &n, &r).is_err());
        let (t, n, mut r) = fixture();
        r.axis = Axis::Vertical;
        assert!(prepare(&t, &n, &r).is_err());
        r.axis = Axis::Horizontal;
        r.cells = 49;
        assert!(prepare(&t, &n, &r).is_err());
    }
    #[test]
    fn focus_changes_do_not_invalidate_geometry() {
        let (t, n, mut r) = fixture();
        for p in &mut r.layout.panes {
            p.focused = !p.focused;
        }
        assert!(prepare(&t, &n, &r).is_ok());
    }
}
