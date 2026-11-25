//! Compact, clickable workspace and pane activity bands above Tetris.
use crate::{
    model::{AgentState, Node, Ui},
    theme::Tone,
    ui_scroll::Window,
};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};
#[derive(Default)]
pub struct State {
    pub area: Option<Rect>,
    hits: Vec<(Rect, String)>,
    rows: Vec<(Rect, Vec<String>, usize)>,
    remembered: std::collections::HashMap<String, String>,
    hovered: Option<String>,
}
impl State {
    pub fn clear_hits(&mut self) {
        self.hits.clear();
        self.rows.clear();
        self.hovered = None;
        self.area = None;
    }
}
#[derive(Clone)]
pub struct Entry {
    pub key: String,
    pub target: String,
    pub label: String,
    pub number: usize,
    pub prefix: &'static str,
    pub state: AgentState,
}
pub fn state(ui: &Ui, node: &Node, now: f64) -> AgentState {
    if !ui.settings.agent_activity {
        return AgentState::Unknown;
    }
    let state = node.agent_state(now);
    if ui.snapshot.activity_stale(now) && matches!(state, AgentState::Working | AgentState::Idle) {
        AgentState::Stale
    } else {
        state
    }
}
pub fn symbol(ui: &Ui, state: AgentState) -> (&'static str, Tone) {
    match state {
        AgentState::Working => (ui.theme.glyph("●", "*"), Tone::Warn),
        AgentState::Waiting => (ui.theme.glyph("◉", "!"), Tone::Danger),
        AgentState::Idle => (ui.theme.glyph("○", "o"), Tone::Ok),
        AgentState::Error => (ui.theme.glyph("×", "x"), Tone::Danger),
        AgentState::Stale => (ui.theme.glyph("◌", "~"), Tone::Faint),
        AgentState::Unknown => ("?", Tone::Muted),
    }
}
fn priority(state: AgentState) -> u8 {
    match state {
        AgentState::Error => 6,
        AgentState::Waiting => 5,
        AgentState::Working => 4,
        AgentState::Stale => 3,
        AgentState::Unknown => 2,
        AgentState::Idle => 1,
    }
}
pub fn workspaces(ui: &Ui) -> Vec<Entry> {
    let tiles = ui.tiles();
    let mut entries = Vec::<Entry>::new();
    let now = crate::model::utc_seconds();
    for row in &tiles {
        let Some(node) = row.source.and_then(|i| ui.snapshot.tree.get(i)) else {
            continue;
        };
        let (mut key, mut number, mut label) = crate::view::workspaces::cube_workspace(ui, row);
        if key.is_empty() {
            key = node.parent.clone();
            label = "Terminals".into();
        }
        if let Some(workspace) = ui.snapshot.tree.iter().find(|n| n.id == key)
            && matches!(workspace.backend.as_str(), "native" | "tmux")
        {
            number = ui
                .snapshot
                .tree
                .iter()
                .filter(|n| n.kind == workspace.kind && n.parent == workspace.parent)
                .position(|n| n.id == key)
                .unwrap_or(0)
                + 1;
        }
        let activity = state(ui, node, now);
        if let Some(entry) = entries.iter_mut().find(|e| e.key == key) {
            if priority(activity) > priority(entry.state) {
                entry.state = activity;
            }
        } else {
            entries.push(Entry {
                key: key.clone(),
                target: row.id.clone(),
                label,
                number,
                prefix: match ui
                    .snapshot
                    .tree
                    .iter()
                    .find(|n| n.id == key)
                    .map(|n| n.backend.as_str())
                {
                    Some("tmux") => "T",
                    Some("native") => "N",
                    _ => "",
                },
                state: activity,
            });
        }
    }
    entries.sort_by_key(|e| (e.prefix, e.number));
    for (i, entry) in entries.iter_mut().enumerate() {
        if entry.number == 0 {
            entry.number = i + 1;
        }
    }
    entries
}
fn panes(ui: &Ui) -> Vec<Entry> {
    let Some(selected) = ui.selected_terminal().and_then(|id| ui.terminal_node(&id)) else {
        return Vec::new();
    };
    let now = crate::model::utc_seconds();
    let mut entries = ui
        .snapshot
        .tree
        .iter()
        .filter(|n| n.tile.is_some() && n.parent == selected.parent)
        .enumerate()
        .map(|(i, node)| Entry {
            key: node.id.clone(),
            target: node.id.clone(),
            label: node
                .tile
                .as_ref()
                .map(|t| t.title.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| node.label.clone()),
            number: i + 1,
            prefix: "",
            state: state(ui, node, now),
        })
        .collect::<Vec<_>>();
    if let Some(layout) = ui.tab_layout(&selected.id) {
        entries.sort_by_key(|e| {
            layout
                .panes
                .iter()
                .position(|p| p.id == e.key)
                .unwrap_or(usize::MAX)
        });
    }
    for (i, e) in entries.iter_mut().enumerate() {
        e.number = i + 1;
    }
    entries
}
fn badge_width(entries: &[Entry]) -> u16 {
    entries
        .iter()
        .map(|e| e.number.to_string().len() + e.prefix.len() + 4)
        .max()
        .unwrap_or(5) as u16
}
fn band(f: &mut Frame, area: Rect, ui: &mut Ui, label: &str, entries: &[Entry], selected: usize) {
    if area.height == 0 || entries.is_empty() {
        return;
    }
    let width = badge_width(entries);
    if area.width < width {
        return;
    }
    let desired_label = label.len() as u16 + 1;
    let label_width = if area.width >= desired_label + width + 4 {
        desired_label
    } else {
        0
    };
    if label_width > 0 {
        f.render_widget(
            Paragraph::new(label).style(ui.theme.fg(Tone::Muted)),
            Rect::new(area.x, area.y, label_width, 1),
        );
    }
    let room = area.width - label_width;
    let overflow = entries.len() * usize::from(width + 1) > usize::from(room + 1);
    let controls = if overflow && room >= width + 4 {
        2
    } else if overflow && room >= width + 2 {
        1
    } else {
        0
    };
    let capacity = ((room - controls * 2 + 1) / (width + 1)).max(1) as usize;
    let window = Window::following(entries.len(), capacity, selected);
    let mut x = area.x + label_width;
    if controls > 0 {
        let r = Rect::new(x, area.y, controls, 1);
        f.render_widget(
            Paragraph::new("‹").style(ui.theme.fg(if selected > 0 {
                Tone::Accent
            } else {
                Tone::Faint
            })),
            r,
        );
        if selected > 0 {
            ui.map
                .nav
                .hits
                .push((r, entries[selected - 1].target.clone()));
        }
        x += controls;
    }
    for (i, e) in entries
        .iter()
        .enumerate()
        .take(window.end())
        .skip(window.offset)
    {
        let (glyph, tone) = symbol(ui, e.state);
        let style = if i == selected && label == "Panes" {
            ui.theme.focus_label()
        } else if i == selected {
            ui.theme.selected(1.0)
        } else {
            ui.theme.surface()
        };
        let spans = vec![
            Span::styled(format!("[{}{} ", e.prefix, e.number), style),
            Span::styled(
                glyph,
                style
                    .fg(ui.theme.fg(tone).fg.unwrap_or_default())
                    .add_modifier(if e.state == AgentState::Working {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled("]", style),
        ];
        let r = Rect::new(x, area.y, width, 1);
        f.render_widget(Paragraph::new(Line::from(spans)), r);
        ui.map.nav.hits.push((r, e.target.clone()));
        if ui
            .pointer
            .is_some_and(|(x, y)| r.contains(Position::new(x, y)))
        {
            ui.map.nav.hovered = Some(format!(
                "{label} {}{} · {} · {}",
                e.prefix,
                e.number,
                e.label,
                e.state.label()
            ));
        }
        x += width + 1;
    }
    if controls > 0 {
        let r = Rect::new(area.right() - controls, area.y, controls, 1);
        f.render_widget(
            Paragraph::new("›").style(ui.theme.fg(if selected + 1 < entries.len() {
                Tone::Accent
            } else {
                Tone::Faint
            })),
            r,
        );
        if let Some(e) = entries.get(selected + 1) {
            ui.map.nav.hits.push((r, e.target.clone()));
        }
    }
    ui.map.nav.rows.push((
        area,
        entries.iter().map(|e| e.target.clone()).collect(),
        selected,
    ));
}
pub fn draw(f: &mut Frame, area: Rect, ui: &mut Ui) -> Rect {
    ui.map.nav.area = None;
    ui.map.nav.hits.clear();
    ui.map.nav.rows.clear();
    ui.map.nav.hovered = None;
    if area.height < 8 {
        return area;
    }
    let selected = ui.selected_terminal().unwrap_or_default();
    let tiles = ui.tiles();
    let workspace = tiles
        .iter()
        .find(|t| t.id == selected)
        .map(|r| crate::view::workspaces::cube_workspace(ui, r).0)
        .unwrap_or_default();
    let mut spaces = workspaces(ui);
    ui.map
        .nav
        .remembered
        .retain(|key, _| spaces.iter().any(|e| e.key == *key));
    ui.map
        .nav
        .remembered
        .insert(workspace.clone(), selected.clone());
    for e in &mut spaces {
        if let Some(id) = ui
            .map
            .nav
            .remembered
            .get(&e.key)
            .filter(|id| tiles.iter().any(|t| t.id == **id))
        {
            e.target = id.clone();
        }
    }
    let terminals = panes(ui);
    let ws = spaces.iter().position(|e| e.key == workspace).unwrap_or(0);
    let pane = terminals
        .iter()
        .position(|e| e.key == selected)
        .unwrap_or(0);
    let desired = 11 + spaces.len() as u16 * (badge_width(&spaces) + 1);
    let pane_min = 6 + (badge_width(&terminals) + 1) * 2 + 4;
    let stacked = area.width < desired.saturating_add(pane_min).saturating_add(2);
    let split = desired.min(area.width);
    let (workspace_area, pane_area, summary_y) = if stacked {
        (
            Rect::new(area.x, area.y, area.width, 1),
            Rect::new(area.x, area.y + 1, area.width, 1),
            area.y + 2,
        )
    } else {
        (
            Rect::new(area.x, area.y, split, 1),
            Rect::new(area.x + split + 2, area.y, area.width - split - 2, 1),
            area.y + 1,
        )
    };
    band(f, workspace_area, ui, "Workspaces", &spaces, ws);
    band(f, pane_area, ui, "Panes", &terminals, pane);
    let working = terminals
        .iter()
        .filter(|e| e.state == AgentState::Working)
        .count();
    let waiting = terminals
        .iter()
        .filter(|e| e.state == AgentState::Waiting)
        .count();
    let focused = terminals
        .get(pane)
        .map(|e| format!("{} · {}", e.label, e.state.label()))
        .unwrap_or_default();
    let summary = format!(
        "{} panes · {} working · {} waiting   {}",
        terminals.len(),
        working,
        waiting,
        focused
    );
    f.render_widget(
        Paragraph::new(ui.map.nav.hovered.clone().unwrap_or(summary)).style(
            ui.theme
                .fg(if waiting > 0 { Tone::Warn } else { Tone::Muted }),
        ),
        Rect::new(area.x, summary_y, area.width, 1),
    );
    let used = summary_y - area.y + 1;
    ui.map.nav.area = Some(Rect::new(area.x, area.y, area.width, used));
    Rect::new(area.x, area.y + used, area.width, area.height - used)
}
pub fn context_target(ui: &Ui, point: Position) -> Option<String> {
    let (_, target) = ui.map.nav.hits.iter().find(|(r, _)| r.contains(point))?;
    if ui
        .map
        .nav
        .rows
        .first()
        .is_some_and(|(r, _, _)| r.contains(point))
    {
        let rows = ui.tiles();
        let row = rows.iter().find(|r| r.id == *target)?;
        let (scope, _, _) = crate::view::workspaces::cube_workspace(ui, row);
        if !scope.is_empty() {
            return Some(scope);
        }
    }
    Some(target.clone())
}
pub fn mouse(ui: &mut Ui, mouse: MouseEvent) -> bool {
    let p = Position::new(mouse.column, mouse.row);
    let target = match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => ui
            .map
            .nav
            .hits
            .iter()
            .find(|(r, _)| r.contains(p))
            .map(|(_, id)| id.clone()),
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => ui
            .map
            .nav
            .rows
            .iter()
            .find(|(r, _, _)| r.contains(p))
            .and_then(|(_, ids, i)| {
                ids.get(if mouse.kind == MouseEventKind::ScrollDown {
                    (i + 1).min(ids.len().saturating_sub(1))
                } else {
                    i.saturating_sub(1)
                })
            })
            .cloned(),
        _ => None,
    };
    if let Some(id) = target {
        crate::map::choose(ui, &id);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Tile, View, WORKSPACES};
    pub(super) fn fixture() -> Ui {
        let mut ui = Ui {
            page: WORKSPACES,
            view: View::Map,
            ..Ui::default()
        };
        ui.snapshot.demo = true;
        for (workspace, tab, id, activity) in [
            ("ws-a", "tab-a", "pane-a", "working"),
            ("ws-a", "tab-a", "pane-b", "needs_input"),
            ("ws-b", "tab-b", "pane-c", "idle"),
        ] {
            if !ui.snapshot.tree.iter().any(|n| n.id == workspace) {
                ui.snapshot.tree.push(Node {
                    id: workspace.into(),
                    label: "Same label".into(),
                    kind: "workspace".into(),
                    ..Node::default()
                });
                ui.snapshot.tree.push(Node {
                    id: tab.into(),
                    parent: workspace.into(),
                    label: tab.into(),
                    kind: "tab".into(),
                    ..Node::default()
                });
            }
            ui.snapshot.tree.push(Node {
                id: id.into(),
                parent: tab.into(),
                label: id.into(),
                kind: "pane".into(),
                state: activity.into(),
                tile: Some(Tile {
                    title: id.into(),
                    ..Tile::default()
                }),
                ..Node::default()
            });
        }
        ui
    }
    fn many_workspaces() -> Ui {
        let mut ui = Ui {
            page: WORKSPACES,
            view: View::Map,
            ..Ui::default()
        };
        ui.snapshot.demo = true;
        // Native terminals must not shift Herdr's own workspace numbers.
        for i in 0..13 {
            let native = i == 0;
            let workspace = format!("workspace-{i}");
            let tab = format!("tab-{i}");
            ui.snapshot.tree.push(Node {
                id: workspace.clone(),
                parent: if native { "native" } else { "herdr" }.into(),
                label: format!("Workspace {i} with a long name"),
                kind: "workspace".into(),
                backend: if native { "native" } else { "herdr" }.into(),
                workspace_number: i,
                ..Node::default()
            });
            ui.snapshot.tree.push(Node {
                id: tab.clone(),
                parent: workspace,
                kind: "tab".into(),
                ..Node::default()
            });
            ui.snapshot.tree.push(Node {
                id: format!("pane-{i}"),
                parent: tab,
                kind: "pane".into(),
                tile: Some(Tile::default()),
                ..Node::default()
            });
        }
        crate::map::choose(&mut ui, "pane-1");
        ui
    }
    fn render(ui: &mut Ui, width: u16) -> ratatui::buffer::Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|f| {
                draw(f, f.area(), ui);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }
    #[test]
    fn every_workspace_remains_reachable_with_disjoint_buttons_and_uncovered_hover_labels() {
        for width in [24, 32, 40, 64, 80, 120, 240] {
            let mut ui = many_workspaces();
            let entries = workspaces(&ui);
            assert_eq!(entries[5].number, 6);
            assert_eq!(entries[5].target, "pane-6");
            assert_eq!(entries[12].prefix, "N");
            for number in 1..=12 {
                render(&mut ui, width);
                for (i, (r, _)) in ui.map.nav.hits.iter().enumerate() {
                    assert!(r.right() <= width);
                    for (other, _) in &ui.map.nav.hits[i + 1..] {
                        assert!(
                            r.intersection(*other).is_empty(),
                            "overlapping targets at width {width}"
                        );
                    }
                }
                let target = format!("pane-{number}");
                let r = ui
                    .map
                    .nav
                    .hits
                    .iter()
                    .find(|(_, id)| id == &target)
                    .unwrap_or_else(|| panic!("workspace {number} unreachable at {width}"))
                    .0;
                assert!(mouse(
                    &mut ui,
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: r.x,
                        row: r.y,
                        modifiers: crossterm::event::KeyModifiers::NONE
                    }
                ));
                assert_eq!(ui.selected_terminal().as_deref(), Some(target.as_str()));
                let before = render(&mut ui, width);
                let r = ui
                    .map
                    .nav
                    .hits
                    .iter()
                    .find(|(_, id)| id == &target)
                    .unwrap()
                    .0;
                assert_eq!(before[(r.x, r.y)].symbol(), "[");
                ui.pointer = Some((r.x, r.y));
                let after = render(&mut ui, width);
                assert!(
                    ui.tooltip.is_none(),
                    "hover must not cover neighboring navigation"
                );
                for x in 0..width {
                    assert_eq!(before[(x, r.y)], after[(x, r.y)]);
                }
                assert!(
                    ui.map
                        .nav
                        .hovered
                        .as_ref()
                        .unwrap()
                        .contains(&format!("Workspaces {number} ·"))
                );
                ui.pointer = None;
            }
        }
    }
    #[test]
    fn circles_aggregate_activity_and_click_routes_by_identity_despite_duplicate_names() {
        let mut ui = fixture();
        let entries = workspaces(&ui);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].state, AgentState::Waiting);
        assert_eq!(entries[1].state, AgentState::Idle);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|f| {
                draw(f, f.area(), &mut ui);
            })
            .unwrap();
        let r = ui
            .map
            .nav
            .hits
            .iter()
            .find(|(_, id)| id == "pane-c")
            .unwrap()
            .0;
        assert!(mouse(
            &mut ui,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: r.x,
                row: r.y,
                modifiers: crossterm::event::KeyModifiers::NONE
            }
        ));
        assert_eq!(ui.selected_terminal().as_deref(), Some("pane-c"));
    }
    #[test]
    fn stale_snapshot_never_claims_a_terminal_is_working() {
        let mut ui = fixture();
        ui.snapshot.demo = false;
        ui.snapshot.freshness = "STALE".into();
        assert_eq!(
            state(&ui, &ui.snapshot.tree[2], crate::model::utc_seconds()),
            AgentState::Stale
        );
    }
}
