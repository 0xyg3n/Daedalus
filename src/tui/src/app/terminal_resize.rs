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
}
fn prepare(
    tree: &Tree,
    node: &str,
    request: &Request,
    settings: &super::runtime_settings::Settings,
) -> Result<Plan> {
    let policy = settings.resize_policy();
    if !policy.validate() {
        return Err("Invalid saved split resize settings".into());
    }
    if request.cells == 0
        || u64::from(request.cells.unsigned_abs()) > policy.max_cells
        || node == request.peer
        || request.layout.zoomed
        || request.layout.panes.len() as u64 > policy.pane_limit
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
    if !crate::core::geometry::unique_ids(current.panes.iter().map(|p| p.id.as_str())) {
        return Err("Ambiguous live pane layout".into());
    }
    if current.canonical() != request.layout.canonical() {
        return Err("Pane layout changed; drag the current divider again".into());
    }
    let a_index = current
        .panes
        .iter()
        .position(|p| p.id == node)
        .ok_or("Pane missing from layout")?;
    let b_index = current
        .panes
        .iter()
        .position(|p| p.id == request.peer)
        .ok_or("Neighbor missing from layout")?;
    let horizontal = request.axis == Axis::Horizontal;
    let a = &current.panes[a_index];
    let b = &current.panes[b_index];
    let geometry = crate::core::geometry::Geometry::new(
        current.width,
        current.height,
        current
            .panes
            .iter()
            .map(|p| crate::core::geometry::Rect {
                x: p.x,
                y: p.y,
                w: p.w,
                h: p.h,
            })
            .collect(),
    )
    .ok_or("Invalid live pane geometry")?;
    if !geometry.edges().iter().any(|edge| {
        edge.leading == a_index && edge.trailing == b_index && edge.horizontal == horizontal
    }) {
        return Err("Panes do not share an unobstructed divider".into());
    }
    let (size, other) = if horizontal {
        (a.w as i32, b.w as i32)
    } else {
        (a.h as i32, b.h as i32)
    };
    let pane = selected.pane.clone().ok_or("Select a live pane")?;
    let peer = peer.pane.clone().ok_or("Select a live neighbor")?;
    let id = terminals::identity(&pane)?;
    let peer_id = terminals::identity(&peer)?;
    if id.backend != peer_id.backend || id.endpoint != peer_id.endpoint || id == peer_id {
        return Err("Resize targets differ from the observed backend".into());
    }
    // Herdr redistributes ancestor descendants proportionally. Its native
    // topology planner checks every changed leaf rather than pair arithmetic.
    let minimum = policy.minimum(horizontal);
    if id.backend != Backend::Herdr
        && (size + request.cells < minimum || other - request.cells < minimum)
    {
        return Err("Resize exceeds the shared divider or minimum pane size".into());
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
    let tree = Tree::build(
        &snapshot,
        &json!({}),
        &ProjectionContext {
            runtime: config.runtime.clone(),
            checkpoint_matches: config.ui.checkpoint_matches,
            ..ProjectionContext::default()
        },
    );
    let plan = prepare(&tree, node, &request, &config.runtime)?;
    terminals.verify(&plan.pane)?;
    terminals.verify(&plan.peer)?;
    if plan.backend == Backend::Native {
        return Err(
            "Resize the native terminal view; its PTY follows the attached view size".into(),
        );
    }
    let success = if plan.backend == Backend::Herdr {
        let reports: Vec<_> = snapshot["layouts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|layout| {
                layout["session"].as_str() == Some(&plan.endpoint)
                    && ["workspace_id", "tab_id"].iter().all(|key| {
                        layout.get(*key).is_some() && layout.get(*key) == plan.pane.get(*key)
                    })
            })
            .collect();
        if reports.len() != 1 {
            return Err(
                "Native split metadata is missing or ambiguous; refresh before resizing".into(),
            );
        }
        let parent = tree.find(node).and_then(|node| node.parent);
        let mut panes = std::collections::HashMap::new();
        let mut expected = std::collections::HashMap::new();
        for row in &request.layout.panes {
            let node = tree.find(&row.id).ok_or("Resize terminal disappeared")?;
            if node.parent != parent {
                return Err("Resize terminal changed tabs".into());
            }
            let pane = node.pane.as_ref().ok_or("Resize terminal is unavailable")?;
            let identity = terminals::identity(pane)?;
            if identity.backend != Backend::Herdr || identity.endpoint != plan.endpoint {
                return Err("Resize terminal changed endpoint".into());
            }
            let id = pane["pane_id"]
                .as_str()
                .ok_or("Resize terminal is invalid")?;
            if panes.insert(id, pane).is_some() {
                return Err("Resize terminal is ambiguous".into());
            }
            expected.insert(
                id,
                crate::core::geometry::Rect {
                    x: row.x,
                    y: row.y,
                    w: row.w,
                    h: row.h,
                },
            );
        }
        let native = super::herdr_resize::prepare(
            reports[0],
            [request.layout.width, request.layout.height],
            &expected,
            &plan.target,
            request.axis == Axis::Horizontal,
            request.cells,
            &config.runtime,
        )?;
        for id in &native.affected {
            terminals.verify(panes[id])?;
        }
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
                &native.amount,
            ],
        );
        match reply {
            Ok(v)
                if v["type"] == "pane_resize"
                    && v["resize"]["pane_id"].as_str() == Some(&plan.target)
                    && v["resize"]["changed"] == true
                    && ["workspace_id", "tab_id"]
                        .iter()
                        .all(|key| v["resize"]["layout"].get(*key) == reports[0].get(*key))
                    && native.confirms(&v["resize"]["layout"]) =>
            {
                return Ok("Pane divider resized".into());
            }
            Ok(v)
                if v["type"] == "pane_resize"
                    && v["resize"]["pane_id"].as_str() == Some(&plan.target)
                    && v["resize"]["changed"] == false
                    && ["workspace_id", "tab_id"]
                        .iter()
                        .all(|key| v["resize"]["layout"].get(*key) == reports[0].get(*key))
                    && native.unchanged(&v["resize"]["layout"]) =>
            {
                return Ok("Pane divider is already at its size limit".into());
            }
            Ok(_) => false,
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
    #[cfg(unix)]
    #[test]
    fn herdr_effect_requires_exact_acknowledgement_and_all_affected_identities_without_replay() {
        use std::{
            fs,
            os::unix::fs::{DirBuilderExt, PermissionsExt},
        };
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let root = Fixture(
            std::env::temp_dir().join(format!("daedalus-herdr-resize-{}", std::process::id())),
        );
        fs::DirBuilder::new().mode(0o700).create(&root.0).unwrap();
        let r = |x, w| json!({"x":x,"y":0,"width":w,"height":40});
        let raw = json!({"session":"fixture","workspace_id":"work","tab_id":"tab","area":r(0,120),"zoomed":false,"panes":[{"pane_id":"one","rect":r(0,30)},{"pane_id":"two","rect":r(30,30)},{"pane_id":"three","rect":r(60,60)}],"splits":[{"id":"root","rect":r(0,120),"direction":"right","ratio":0.5},{"id":"inner","rect":r(0,60),"direction":"right","ratio":0.5}]});
        let panes:Vec<_>=["one","two","three"].into_iter().map(|id|json!({"backend":"herdr","session":"fixture","server":"fixture","workspace_id":"work","tab_id":"tab","pane_id":id,"terminal_id":format!("generation-{id}")})).collect();
        for pane in &panes {
            fs::write(
                root.0
                    .join(format!("{}.json", pane["pane_id"].as_str().unwrap())),
                serde_json::to_vec(&json!({"pane":pane})).unwrap(),
            )
            .unwrap();
        }
        let snapshot = json!({"panes":panes,"layouts":[raw.clone()]});
        fs::write(
            root.0.join("snapshot.json"),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        let script = root.0.join("herdr");
        fs::write(
            &script,
            r#"#!/usr/bin/python3
from pathlib import Path
import sys,json
root=Path(__file__).parent
a=sys.argv[3:]
if a==['api','snapshot']:
 print((root/'snapshot.json').read_text())
elif a[:2]==['pane','get']:
 v=json.loads((root/(a[2]+'.json')).read_text())
 with (root/'queries').open('a') as f:f.write(a[2]+'\n')
 if (root/'stale').exists() and a[2]=='one':v['pane']['terminal_id']='reused-generation'
 print(json.dumps(v))
elif a[:2]==['pane','process-info']:
 print('{"process_info":{}}')
elif a[:2]==['pane','resize']:
 with (root/'effects').open('a') as f:f.write(json.dumps(a)+'\n')
 print((root/'reply.json').read_text())
else:sys.exit(2)
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = ControlConfig::fresh(&root.0).unwrap();
        config.discovery.automatic = false;
        config.discovery.herdr_sessions = vec!["fixture".into()];
        let terminals = Terminals {
            herdr: script,
            ..Default::default()
        };
        let tree = Tree::build(&snapshot, &json!({}), &ProjectionContext::default());
        let node = tree
            .nodes
            .iter()
            .find(|n| n.pane.as_ref().is_some_and(|p| p["pane_id"] == "two"))
            .unwrap();
        let peer = tree
            .nodes
            .iter()
            .find(|n| n.pane.as_ref().is_some_and(|p| p["pane_id"] == "three"))
            .unwrap();
        let request = Request {
            peer: peer.id(),
            axis: Axis::Horizontal,
            cells: 5,
            layout: serde_json::from_value(
                tree.nodes[node.parent.unwrap()].attributes["layout"].clone(),
            )
            .unwrap(),
        };
        let mut after = raw.clone();
        after["panes"][0]["rect"] = r(0, 33);
        after["panes"][1]["rect"] = r(33, 32);
        after["panes"][2]["rect"] = r(65, 55);
        for mode in 0..6 {
            let mut reply = json!({"type":"pane_resize","resize":{"pane_id":"two","changed":true,"layout":after}});
            match mode {
                1 => reply = json!({}),
                2 => reply["resize"]["pane_id"] = json!("different-target"),
                3 => {
                    reply["resize"]["changed"] = json!(false);
                    reply["resize"]["layout"] = raw.clone();
                }
                4 => reply["resize"]["layout"]["panes"][0]["rect"]["width"] = json!(32),
                5 => reply = json!({"resize":{"changed":false}}),
                _ => (),
            }
            fs::write(
                root.0.join("reply.json"),
                serde_json::to_vec(&reply).unwrap(),
            )
            .unwrap();
            fs::write(root.0.join("effects"), "").unwrap();
            fs::write(root.0.join("queries"), "").unwrap();
            let result = apply(&config, &terminals, &node.id(), request.clone());
            if mode == 0 {
                assert_eq!(result.unwrap(), "Pane divider resized");
            } else if mode == 3 {
                assert!(result.unwrap().contains("size limit"));
            } else {
                assert!(result.unwrap_err().contains("unconfirmed"));
            }
            assert_eq!(
                fs::read_to_string(root.0.join("effects"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
            assert!(
                fs::read_to_string(root.0.join("queries"))
                    .unwrap()
                    .lines()
                    .any(|id| id == "one")
            );
        }
        fs::write(root.0.join("stale"), "").unwrap();
        fs::write(root.0.join("effects"), "").unwrap();
        assert!(apply(&config, &terminals, &node.id(), request).is_err());
        assert_eq!(fs::read_to_string(root.0.join("effects")).unwrap(), "");
    }
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
    fn saved_resize_movement_and_minimum_dimensions_reach_the_real_planner() {
        let (tree, node, mut request) = fixture();
        let mut settings = super::super::runtime_settings::Settings {
            terminal_resize_max_cells: 4,
            ..Default::default()
        };
        assert!(prepare(&tree, &node, &request, &settings).is_err());
        settings.terminal_resize_max_cells = 5;
        assert!(prepare(&tree, &node, &request, &settings).is_ok());
        settings.terminal_resize_min_cols = 46;
        assert!(prepare(&tree, &node, &request, &settings).is_err());
        settings.terminal_resize_min_cols = 45;
        assert!(prepare(&tree, &node, &request, &settings).is_ok());
        request.cells = -5;
        settings.terminal_resize_min_cols = 45;
        assert!(prepare(&tree, &node, &request, &settings).is_err());
        settings.terminal_resize_min_cols = 44;
        assert!(prepare(&tree, &node, &request, &settings).is_ok());

        let (mut tree, node, mut request) = fixture();
        let parent = tree.find(&node).unwrap().parent.unwrap();
        std::mem::swap(&mut request.layout.width, &mut request.layout.height);
        for pane in &mut request.layout.panes {
            std::mem::swap(&mut pane.x, &mut pane.y);
            std::mem::swap(&mut pane.w, &mut pane.h);
        }
        tree.nodes[parent].attributes["layout"] = serde_json::to_value(&request.layout).unwrap();
        request.axis = Axis::Vertical;
        settings.terminal_resize_min_rows = 46;
        assert!(prepare(&tree, &node, &request, &settings).is_err());
        settings.terminal_resize_min_rows = 45;
        assert_eq!(
            prepare(&tree, &node, &request, &settings)
                .unwrap()
                .direction,
            "down"
        );
    }
    #[test]
    fn resize_never_skips_an_intervening_one_cell_terminal() {
        let panes:Vec<_>=[("%1",0,49),("%2",50,50),("%3",49,1)].into_iter().map(|(id,x,w)|json!({"backend":"tmux","socket":crate::test_support::path("/tmp/divider-gap.sock"),"session":"test","session_id":"$1","window_id":"@1","pane_id":id,"terminal_id":id,"left":x,"top":0,"width":w,"height":40})).collect();
        let tree = Tree::build(
            &json!({"panes":panes}),
            &json!({}),
            &ProjectionContext::default(),
        );
        let a = tree
            .nodes
            .iter()
            .find(|n| n.kind == "pane" && n.pane.as_ref().unwrap()["pane_id"] == "%1")
            .unwrap();
        let b = tree
            .nodes
            .iter()
            .find(|n| n.kind == "pane" && n.pane.as_ref().unwrap()["pane_id"] == "%2")
            .unwrap();
        let layout: Layout =
            serde_json::from_value(tree.nodes[a.parent.unwrap()].attributes["layout"].clone())
                .unwrap();
        let request = Request {
            peer: b.id(),
            axis: Axis::Horizontal,
            cells: 5,
            layout,
        };
        let mut settings = super::super::runtime_settings::Settings {
            terminal_resize_pane_limit: 2,
            ..Default::default()
        };
        assert!(prepare(&tree, &a.id(), &request, &settings).is_err());
        settings.terminal_resize_pane_limit = 3;
        assert!(
            prepare(&tree, &a.id(), &request, &settings)
                .err()
                .unwrap()
                .contains("unobstructed")
        );
    }
    #[test]
    fn exact_shared_divider_grows_the_correct_side() {
        let (t, n, mut r) = fixture();
        let p = prepare(
            &t,
            &n,
            &r,
            &super::super::runtime_settings::Settings::default(),
        )
        .unwrap();
        assert_eq!(p.target, "%1");
        assert_eq!(p.direction, "right");
        r.cells = -5;
        let p = prepare(
            &t,
            &n,
            &r,
            &super::super::runtime_settings::Settings::default(),
        )
        .unwrap();
        assert_eq!(p.target, "%2");
        assert_eq!(p.direction, "left");
    }
    #[test]
    fn stale_geometry_non_neighbors_and_tiny_panes_are_rejected() {
        let (t, n, mut r) = fixture();
        r.layout.panes[0].w -= 1;
        assert!(
            prepare(
                &t,
                &n,
                &r,
                &super::super::runtime_settings::Settings::default()
            )
            .is_err()
        );
        let (t, n, mut r) = fixture();
        r.axis = Axis::Vertical;
        assert!(
            prepare(
                &t,
                &n,
                &r,
                &super::super::runtime_settings::Settings::default()
            )
            .is_err()
        );
        r.axis = Axis::Horizontal;
        r.cells = 49;
        assert!(
            prepare(
                &t,
                &n,
                &r,
                &super::super::runtime_settings::Settings::default()
            )
            .is_err()
        );
    }
    #[test]
    fn focus_changes_do_not_invalidate_geometry() {
        let (t, n, mut r) = fixture();
        for p in &mut r.layout.panes {
            p.focused = !p.focused;
        }
        assert!(
            prepare(
                &t,
                &n,
                &r,
                &super::super::runtime_settings::Settings::default()
            )
            .is_ok()
        );
    }
}
