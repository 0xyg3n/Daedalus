//! Tab-scoped split panes. Every visible pane owns disjoint screen cells.
use crate::{
    model::{Layout, TileRow, Ui, View, WORKSPACES},
    theme::Tone,
    view::workspaces::preview_screen,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use daedalus_tui::core::map_layout::{Boundary, Card, Tiling};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    widgets::{Block, Borders, Paragraph},
};
#[derive(Default)]
pub struct State {
    pub split: String,
    pub primary: String,
    pub peer: String,
    pub ratio: u16,
    pub nav: crate::map_nav::State,
    pub fullscreen_hits: Vec<(Rect, String)>,
    pub toolbar_hits: Vec<(Rect, String, String)>,
    active: usize,
    panels: Vec<(Rect, String)>,
    comparison: Option<(Rect, u16, bool)>,
    cache: std::collections::HashMap<String, Cached>,
    pub cards: Vec<Card>,
    pub native_signatures: std::collections::BTreeMap<String, String>,
    pub area: Rect,
    pub drag: Option<Drag>,
    tree: Option<Tiling>,
    group: String,
    members: Vec<String>,
    boundaries: Vec<Boundary>,
    tab_hits: Vec<(Rect, isize)>,
}
#[derive(Default)]
struct Cached {
    tree: Option<Tiling>,
    members: Vec<String>,
    boundaries: Vec<Boundary>,
    area: Rect,
    tab_hits: Vec<(Rect, isize)>,
}
fn load(ui: &mut Ui, key: &str) {
    if ui.map.group == key {
        return;
    }
    let old = Cached {
        tree: ui.map.tree.take(),
        members: std::mem::take(&mut ui.map.members),
        boundaries: std::mem::take(&mut ui.map.boundaries),
        area: ui.map.area,
        tab_hits: std::mem::take(&mut ui.map.tab_hits),
    };
    if old.tree.is_some() {
        ui.map.cache.insert(ui.map.group.clone(), old);
    }
    let cached = ui.map.cache.remove(key).unwrap_or_default();
    ui.map.tree = cached.tree;
    ui.map.members = cached.members;
    ui.map.boundaries = cached.boundaries;
    ui.map.area = cached.area;
    ui.map.tab_hits = cached.tab_hits;
    ui.map.group = key.into();
}
impl State {
    pub fn restore(&mut self, cards: Vec<Card>) {
        if self.cards == cards {
            return;
        }
        self.cards = cards;
        self.tree = None;
        self.cache.clear();
        self.members.clear();
        self.group.clear();
        self.drag = None;
        self.boundaries.clear();
    }
}
pub enum Drag {
    Workspace {
        start: u16,
        area: Rect,
        horizontal: bool,
    },
    Resize {
        start: Tiling,
        boundary: Boundary,
        pointer: (u16, u16),
    },
    Move {
        start: Tiling,
        id: String,
        pointer: (u16, u16),
    },
}
fn group(ui: &Ui, row: &TileRow) -> String {
    row.source
        .and_then(|i| ui.snapshot.tree.get(i))
        .map(|n| n.parent.clone())
        .unwrap_or_default()
}
fn groups(ui: &Ui, tiles: &[TileRow]) -> Vec<Vec<usize>> {
    let mut keys = Vec::new();
    let mut groups = Vec::<Vec<usize>>::new();
    for (i, row) in tiles.iter().enumerate().filter(|(_, r)| !r.id.is_empty()) {
        let key = group(ui, row);
        let g = keys.iter().position(|k| k == &key).unwrap_or_else(|| {
            keys.push(key);
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }
    groups
}
pub fn can_compare(ui: &Ui) -> bool {
    let mut groups = std::collections::HashSet::new();
    ui.tiles()
        .iter()
        .filter_map(|r| r.source.and_then(|i| ui.snapshot.tree.get(i)))
        .any(|n| {
            groups.insert(n.parent.as_str());
            groups.len() > 1
        })
}
fn native_tree(layout: &Layout, ids: &[String]) -> Option<Tiling> {
    if layout.panes.len() != ids.len() || !layout.panes.iter().all(|p| ids.contains(&p.id)) {
        return None;
    }
    let panes = layout
        .panes
        .iter()
        .map(|p| (p.id.clone(), Rect::new(p.x, p.y, p.w, p.h)))
        .collect::<Vec<_>>();
    Tiling::from_rects(Rect::new(0, 0, layout.width, layout.height), &panes)
}
fn native_signature(layout: &Layout) -> String {
    use sha2::{Digest, Sha256};
    let mut canonical = layout.clone();
    for pane in &mut canonical.panes {
        pane.focused = false;
    }
    canonical.panes.sort_by(|a, b| a.id.cmp(&b.id));
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical).expect("layout serializes"))
    )
}
fn same_members(a: &[String], b: &[String]) -> bool {
    a == b || a.len() == b.len() && a.iter().all(|id| b.contains(id))
}
fn save(ui: &mut Ui) {
    let Some(tree) = ui.map.tree.as_ref() else {
        return;
    };
    let replacement = tree.cards();
    if replacement.is_empty() {
        return;
    }
    let mut cards = ui
        .map
        .cards
        .iter()
        .filter(|c| !ui.map.members.contains(&c.id))
        .cloned()
        .collect::<Vec<_>>();
    cards.extend(replacement);
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    if cards.len() > 512 {
        ui.status = "Tetris storage limit reached; layout remains visible".into();
        ui.status_error = true;
        return;
    }
    if ui.map.cards != cards {
        ui.map.cards = cards;
        ui.sizes_changed = Some(std::time::Instant::now());
    }
}
pub fn choose(ui: &mut Ui, id: &str) {
    let tiles = ui.tiles();
    let Some(index) = tiles.iter().position(|r| r.id == id) else {
        return;
    };
    let key = group(ui, &tiles[index]);
    if let Some(panel) = ui.map.panels.iter().position(|(_, target)| {
        tiles
            .iter()
            .find(|t| t.id == *target)
            .is_some_and(|t| group(ui, t) == key)
    }) {
        ui.map.active = panel;
    } else if ui.map.active == 1 && ui.map.split != "off" {
        ui.map.peer = id.into();
    } else {
        ui.map.primary = id.into();
    }
    ui.select_tile(index);
    load(ui, &key);
}
pub fn arrangement(ui: &mut Ui, mode: &str) {
    let tiles = ui.tiles();
    let Some(selected) = ui.selected_terminal() else {
        return;
    };
    if matches!(mode, "balance" | "grow" | "shrink") {
        let delta = if mode == "grow" { 10 } else { -10 } * if ui.map.active == 0 { 1 } else { -1 };
        ui.map.ratio = if mode == "balance" {
            50
        } else {
            (i32::from(ui.map.ratio) + delta).clamp(10, 90) as u16
        };
    } else if mode == "swap" {
        std::mem::swap(&mut ui.map.primary, &mut ui.map.peer);
        ui.map.active = 1 - ui.map.active;
    } else if mode == "rotate" {
        ui.map.split = if ui.map.split == "rows" {
            "columns"
        } else {
            "rows"
        }
        .into();
    } else {
        ui.map.split = mode.into();
        ui.map.primary = selected.clone();
        ui.map.active = 0;
        let key = tiles
            .iter()
            .find(|t| t.id == selected)
            .map(|r| group(ui, r))
            .unwrap_or_default();
        let ws = tiles
            .iter()
            .find(|t| t.id == selected)
            .map(|r| crate::view::workspaces::cube_workspace(ui, r).0)
            .unwrap_or_default();
        ui.map.peer = tiles
            .iter()
            .find(|r| group(ui, r) != key && crate::view::workspaces::cube_workspace(ui, r).0 != ws)
            .or_else(|| tiles.iter().find(|r| group(ui, r) != key))
            .map(|r| r.id.clone())
            .unwrap_or_default();
    }
    ui.sizes_changed = Some(std::time::Instant::now());
}
pub fn draw(f: &mut Frame, area: Rect, ui: &mut Ui) {
    ui.map.fullscreen_hits.clear();
    ui.map.toolbar_hits.clear();
    if ui.map.split.is_empty() {
        ui.map.split = "off".into();
    }
    if ui.map.ratio < 10 {
        ui.map.ratio = 50;
    }
    ui.map
        .cache
        .retain(|key, _| key.is_empty() || ui.snapshot.tree.iter().any(|n| n.id == *key));
    let body = crate::map_nav::draw(f, area, ui);
    let tiles = ui.tiles();
    let Some(selected) = ui.selected_terminal() else {
        draw_one(f, body, ui, ui.tile, true);
        return;
    };
    let Some(selected_index) = tiles.iter().position(|t| t.id == selected) else {
        return;
    };
    let selected_key = group(ui, &tiles[selected_index]);
    let find = |id: &str| tiles.iter().position(|t| t.id == id);
    if ui.map.split == "off" || find(&ui.map.primary).is_none() {
        ui.map.primary = selected.clone();
    }
    if ui.map.split != "off" {
        if find(&ui.map.primary).is_some_and(|i| group(ui, &tiles[i]) == selected_key) {
            ui.map.active = 0;
            ui.map.primary = selected.clone();
        } else if find(&ui.map.peer).is_some_and(|i| group(ui, &tiles[i]) == selected_key) {
            ui.map.active = 1;
            ui.map.peer = selected.clone();
        } else if ui.map.active == 1 {
            ui.map.peer = selected.clone();
        } else {
            ui.map.primary = selected.clone();
        }
    }
    ui.map.panels.clear();
    ui.map.comparison = None;
    let primary = find(&ui.map.primary).unwrap_or(selected_index);
    let peer = find(&ui.map.peer).filter(|i| group(ui, &tiles[*i]) != group(ui, &tiles[primary]));
    let split = ui.map.split != "off" && peer.is_some() && body.width >= 32 && body.height >= 12;
    let mut regions = vec![(body, primary)];
    if split {
        let horizontal = ui.map.split != "rows";
        let length = if horizontal { body.width } else { body.height };
        let minimum = (if horizontal { 20 } else { 6 }).min((length - 1) / 2);
        let cut = (((u32::from(length - 1) * u32::from(ui.map.ratio)) / 100) as u16)
            .clamp(minimum, length - 1 - minimum);
        let (a, b, pos) = if horizontal {
            (
                Rect::new(body.x, body.y, cut, body.height),
                Rect::new(body.x + cut + 1, body.y, body.width - cut - 1, body.height),
                body.x + cut,
            )
        } else {
            (
                Rect::new(body.x, body.y, body.width, cut),
                Rect::new(body.x, body.y + cut + 1, body.width, body.height - cut - 1),
                body.y + cut,
            )
        };
        regions = vec![(a, primary), (b, peer.unwrap_or(primary))];
        ui.map.comparison = Some((body, pos, horizontal));
        let line = if horizontal {
            Rect::new(pos, body.y, 1, body.height)
        } else {
            Rect::new(body.x, pos, body.width, 1)
        };
        for y in line.y..line.bottom() {
            for x in line.x..line.right() {
                f.buffer_mut().set_string(
                    x,
                    y,
                    ui.theme.glyph(
                        if horizontal { "│" } else { "─" },
                        if horizontal { "|" } else { "-" },
                    ),
                    ui.theme.fg(Tone::Accent),
                );
            }
        }
    }
    for (panel, (r, index)) in regions.into_iter().enumerate() {
        let key = group(ui, &tiles[index]);
        load(ui, &key);
        ui.map.panels.push((r, tiles[index].id.clone()));
        draw_one(f, r, ui, index, !split || ui.map.active == panel);
    }
    load(ui, &selected_key);
}
fn draw_one(f: &mut Frame, area: Rect, ui: &mut Ui, selected: usize, active: bool) {
    ui.map.tab_hits.clear();
    ui.map.boundaries.clear();
    let tiles = ui.tiles();
    let groups = groups(ui, &tiles);
    let index = groups
        .iter()
        .position(|g| g.contains(&selected))
        .unwrap_or(0);
    let Some(members) = groups.get(index) else {
        f.render_widget(Paragraph::new("Tetris · no matching terminals"), area);
        return;
    };
    if active && !members.contains(&selected) {
        ui.select_tile(members[0]);
    }
    let group = group(ui, &tiles[members[0]]);
    let ids = members
        .iter()
        .map(|i| tiles[*i].id.clone())
        .collect::<Vec<_>>();
    let label = ui
        .snapshot
        .tree
        .iter()
        .find(|n| n.id == group)
        .map(|n| n.label.as_str())
        .unwrap_or("Terminals");
    let heading = format!(
        "Tetris · tab {}/{} · {} panes · {} · PageUp/Down tabs · drag dividers to resize / titles to swap",
        index + 1,
        groups.len(),
        ids.len(),
        label
    );
    f.render_widget(
        Paragraph::new(heading),
        Rect::new(area.x, area.y, area.width, area.height.min(1)),
    );
    let heading_rows = if area.height >= 8 { 2 } else { 1 };
    if heading_rows == 2 {
        let buttons = [(" ‹ Previous tab ", -1), (" Next tab › ", 1)];
        let mut x = area.x;
        for (label, delta) in buttons {
            let width = (label.chars().count() as u16).min(area.right().saturating_sub(x));
            let r = Rect::new(x, area.y + 1, width, 1);
            f.render_widget(Paragraph::new(label).style(ui.theme.fg(Tone::Accent)), r);
            ui.map.tab_hits.push((r, delta));
            x = x.saturating_add(width);
        }
        for (label, action) in [
            ("[Tree] ", "tree"),
            ("[Actions] ", "actions"),
            ("[Shell+] ", "shell"),
            ("[Fit] ", "fit"),
        ] {
            let width = label.len() as u16;
            if x + width > area.right() {
                break;
            }
            let r = Rect::new(x, area.y + 1, width, 1);
            f.render_widget(Paragraph::new(label).style(ui.theme.fg(Tone::Accent)), r);
            ui.map
                .toolbar_hits
                .push((r, action.into(), tiles[selected].id.clone()));
            x += width;
        }
    }
    let screen = Rect::new(
        area.x,
        area.y.saturating_add(heading_rows),
        area.width,
        area.height.saturating_sub(heading_rows),
    );
    ui.map.area = screen;
    crate::view::quick_bar(f, screen, ui, &[("m", "More", "more")]);
    if screen.is_empty() {
        return;
    }
    ui.map.cards.retain(|c| {
        ui.snapshot
            .tree
            .iter()
            .any(|n| n.id == c.id && n.tile.is_some())
    });
    let native = ui.tab_layout(&tiles[selected].id).cloned();
    // Compare identity/geometry before rebuilding the split tree. Pane order and
    // focus flags are not geometry, so activity updates cannot reset local edits.
    let signature = native.as_ref().map(native_signature);
    let signature_changed = signature
        .as_ref()
        .is_some_and(|value| ui.map.native_signatures.get(&group) != Some(value));
    let members_changed = !same_members(&ui.map.members, &ids);
    if ui.map.tree.is_none() || members_changed || signature_changed {
        let backend = native.as_ref().and_then(|layout| native_tree(layout, &ids));
        let native_changed = signature_changed && backend.is_some();
        // Incomplete/unsupported geometry cannot displace an already fitted Map
        // or erase the last verified baseline. A later valid report is retried.
        if ui.map.tree.is_none() || members_changed || native_changed {
            let saved = ui
                .map
                .cards
                .iter()
                .filter(|c| ids.contains(&c.id))
                .cloned()
                .collect::<Vec<_>>();
            let restored = (saved.len() == ids.len())
                .then(|| Tiling::from_cards(&saved))
                .flatten();
            let observed = backend.is_some().then_some(signature).flatten();
            ui.map.tree = if native_changed || members_changed && !ui.map.members.is_empty() {
                backend.or(restored)
            } else {
                restored.or(backend)
            }
            .or_else(|| Tiling::balanced(&ids, Rect::new(0, 0, 1000, 1000)));
            ui.map.group = group.clone();
            ui.map.members = ids.clone();
            ui.map.drag = None;
            if let Some(signature) = observed
                && (ui.map.native_signatures.len() < 512
                    || ui.map.native_signatures.contains_key(&group))
                && ui
                    .map
                    .native_signatures
                    .insert(group, signature.clone())
                    .as_ref()
                    != Some(&signature)
            {
                ui.sizes_changed = Some(std::time::Instant::now());
            }
            save(ui);
        }
    }
    ui.map.members = ids.clone();
    let Some(tree) = ui.map.tree.as_ref() else {
        return;
    };
    let capacity = usize::from(screen.width) * usize::from(screen.height);
    let selected = members.iter().position(|i| *i == selected).unwrap_or(0);
    let page = selected / capacity.max(1);
    let visible = &ids[(page * capacity).min(ids.len())..((page + 1) * capacity).min(ids.len())];
    let projection = tree.project(screen, (4, 3));
    let fitted = projection.is_some();
    let projection =
        projection.or_else(|| Tiling::balanced(visible, screen)?.project(screen, (1, 1)));
    let Some(projection) = projection else { return };
    if !fitted {
        ui.status = format!(
            "Compact fit · {} of {} panes · arrows select · F3 expands",
            projection.panes.len(),
            ids.len()
        );
    }
    ui.tile_columns = 1;
    ui.tiles_per_page = members.len();
    let mut most = 1;
    for (id, rect) in projection.panes {
        let Some(tile) = members.iter().copied().find(|i| tiles[*i].id == id) else {
            continue;
        };
        most = most.max(draw_pane(
            f,
            rect,
            ui,
            &tiles[tile],
            active && tile == ui.tile,
        ));
        ui.tile_hits.push((rect, tile));
        if !ui.preview_targets.contains(&id) {
            ui.preview_targets.push(id);
        }
    }
    ui.preview_rows = most;
    if fitted {
        ui.map.boundaries = projection.boundaries;
    }
    // Only shared borders are draggable; outer bounds cannot cover another pane.
    for boundary in &ui.map.boundaries {
        if ui.pointer.is_some_and(|(x, y)| boundary.hit(x, y)) {
            let (x, y) = if boundary.horizontal {
                (
                    boundary.position.saturating_sub(1),
                    boundary.area.y + boundary.area.height / 2,
                )
            } else {
                (
                    boundary.area.x + boundary.area.width / 2,
                    boundary.position.saturating_sub(1),
                )
            };
            if screen.contains(Position::new(x, y)) {
                f.buffer_mut().set_string(
                    x,
                    y,
                    ui.theme.glyph(
                        if boundary.horizontal { "↔" } else { "↕" },
                        if boundary.horizontal { "|" } else { "-" },
                    ),
                    ui.theme.fg(Tone::Accent),
                );
            }
        }
    }
}
pub(crate) fn pane_identity(node: &crate::model::Node) -> String {
    if let Some((id, _)) = node.parts.iter().find(|(_, role)| role == "id") {
        return id.clone();
    }
    if let Ok(key) = serde_json::from_str::<Vec<serde_json::Value>>(&node.id)
        && key.len() >= 3
        && key[key.len() - 3] == "pane"
        && let Some(id) = key[key.len() - 2].as_str()
    {
        return id.into();
    }
    node.id.clone()
}
pub(crate) fn title_color(ui: &Ui, id: &str) -> ratatui::style::Color {
    const COLORS: [crate::theme::Rgb; 24] = [
        (118, 209, 243),
        (242, 194, 120),
        (173, 218, 139),
        (204, 173, 244),
        (240, 166, 191),
        (124, 220, 198),
        (166, 193, 251),
        (232, 195, 151),
        (188, 218, 112),
        (224, 172, 234),
        (246, 165, 150),
        (139, 214, 224),
        (146, 210, 166),
        (215, 194, 252),
        (239, 183, 133),
        (176, 209, 242),
        (227, 206, 130),
        (207, 182, 220),
        (147, 220, 180),
        (241, 177, 208),
        (188, 216, 233),
        (206, 215, 157),
        (211, 190, 250),
        (137, 218, 211),
    ];
    let Some(node) = ui.terminal_node(id) else {
        return ui.theme.rgb(COLORS[0]);
    };
    let rank = ui
        .snapshot
        .tree
        .iter()
        .filter(|n| n.tile.is_some() && n.parent == node.parent && n.id.as_str() < id)
        .count();
    {
        let color = COLORS[rank % COLORS.len()];
        let shade = ((rank / COLORS.len()).min(20) * 2) as u8;
        ui.theme.rgb((
            color.0.saturating_sub(shade),
            color.1.saturating_sub(shade),
            color.2.saturating_sub(shade),
        ))
    }
}
fn draw_pane(f: &mut Frame, r: Rect, ui: &mut Ui, row: &TileRow, selected: bool) -> u16 {
    let node = row.source.and_then(|i| ui.snapshot.tree.get(i));
    let title = node
        .map(|n| {
            n.tile
                .as_ref()
                .map(|t| t.title.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&n.label)
        })
        .unwrap_or("Terminal")
        .to_owned();
    let harness = node
        .and_then(|n| n.tile.as_ref())
        .map(|t| t.harness.as_str())
        .unwrap_or("")
        .to_owned();
    let id = node.map(pane_identity).unwrap_or_else(|| row.id.clone());
    let title = crate::harness_icons::label(ui, &harness, &title);
    let title = if title.starts_with(&id) {
        title
    } else {
        format!("{id} · {title}")
    };
    let color = if selected {
        ui.theme.focus_frame()
    } else {
        ui.theme.fg(Tone::Faint)
    };
    // Tiny native splits still own a visible, selectable cell. Never silently skip them.
    let screen = if r.width >= 4 && r.height >= 3 {
        let activity = node
            .map(|n| crate::map_nav::state(ui, n, crate::model::utc_seconds()))
            .unwrap_or(crate::model::AgentState::Unknown);
        let (glyph, tone) = crate::map_nav::symbol(ui, activity);
        let input = selected
            && ui.embed.as_ref().is_some_and(|e| {
                e.plan.node == row.id && e.mode == crate::embed::Mode::Control && e.live()
            });
        let mut status = if selected && r.width >= 28 {
            format!(" {} ", if input { "TYPING" } else { "FOCUSED" })
        } else if r.width >= 40 {
            format!(" {glyph} {} ", activity.label())
        } else if r.width >= 16 {
            format!(" {glyph} ")
        } else {
            String::new()
        };
        if r.width >= 20 {
            status.push_str("    ");
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(if selected {
                ui.theme.border_strong()
            } else {
                ui.theme.border()
            })
            .title(ratatui::text::Line::styled(
                title,
                ratatui::style::Style::default()
                    .fg(title_color(ui, &row.id))
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ))
            .title_top(
                ratatui::text::Line::styled(
                    status,
                    if selected {
                        ui.theme.focus_label()
                    } else {
                        ui.theme.fg(tone)
                    },
                )
                .right_aligned(),
            )
            .border_style(color)
            .style(ui.theme.surface());
        let inner = block.inner(r);
        f.render_widget(block, r);
        if selected {
            for x in r.x + 1..r.right().saturating_sub(1) {
                let cell = &mut f.buffer_mut()[(x, r.y)];
                if let Some(bg) = ui.theme.focus_label().bg {
                    cell.set_bg(bg);
                }
                if !ui.theme.color {
                    cell.modifier.insert(ratatui::style::Modifier::REVERSED);
                }
            }
        }
        if r.width >= 20 {
            let button = Rect::new(r.right() - 5, r.y, 3, 1);
            f.render_widget(
                Paragraph::new("[+]").style(
                    ui.theme
                        .fg(Tone::Accent)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                button,
            );
            ui.map.fullscreen_hits.push((button, row.id.clone()));
            if ui
                .pointer
                .is_some_and(|(x, y)| button.contains(Position::new(x, y)))
                && let Some(nav) = ui.map.nav.area
            {
                ui.tooltip = Some((
                    Rect::new(nav.x, nav.bottom() - 1, 0, 1),
                    "Full screen this pane · F11 returns to Tetris".into(),
                ));
            }
        }
        inner
    } else {
        f.render_widget(
            Paragraph::new(title).style(
                ratatui::style::Style::default()
                    .fg(title_color(ui, &row.id))
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            r,
        );
        return 0;
    };
    if selected
        && screen.width >= 20
        && screen.height >= 4
        && ui.embed.as_ref().is_some_and(|e| e.plan.node == row.id)
    {
        ui.inline_screen = Some(screen);
    }
    preview_screen(f, screen, ui, &row.id);
    screen.height
}
fn next_tab(ui: &mut Ui, delta: isize) {
    let tiles = ui.tiles();
    let groups = groups(ui, &tiles);
    if groups.is_empty() {
        return;
    }
    let index = groups
        .iter()
        .position(|g| g.contains(&ui.tile))
        .unwrap_or(0);
    let next = (index as isize + delta).rem_euclid(groups.len() as isize) as usize;
    let id = tiles[groups[next][0]].id.clone();
    choose(ui, &id);
    ui.map.drag = None;
}
pub fn key(ui: &mut Ui, key: KeyEvent) -> Option<crate::Step> {
    if ui.view != View::Map {
        return None;
    }
    if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
        || (key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Up | KeyCode::Down))
    {
        next_tab(
            ui,
            if matches!(key.code, KeyCode::PageUp | KeyCode::Up) {
                -1
            } else {
                1
            },
        );
        return Some(crate::Step::Continue);
    }
    let tiles = ui.tiles();
    let groups = groups(ui, &tiles);
    let members = groups.iter().find(|g| g.contains(&ui.tile))?;
    let current = members.iter().position(|i| *i == ui.tile).unwrap_or(0);
    let next = match key.code {
        KeyCode::Home => Some(members[0]),
        KeyCode::End => members.last().copied(),
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
            let here = ui
                .tile_hits
                .iter()
                .find(|(_, i)| *i == ui.tile)
                .map(|(r, _)| *r);
            here.and_then(|r| {
                ui.tile_hits
                    .iter()
                    .filter(|(_, i)| *i != ui.tile)
                    .filter(|(q, _)| match key.code {
                        KeyCode::Left => q.right() <= r.x,
                        KeyCode::Right => q.x >= r.right(),
                        KeyCode::Up => q.bottom() <= r.y,
                        _ => q.y >= r.bottom(),
                    })
                    .min_by_key(|(q, _)| {
                        let center = |r: Rect| {
                            (
                                i32::from(r.x) * 2 + i32::from(r.width),
                                i32::from(r.y) * 2 + i32::from(r.height),
                            )
                        };
                        let a = center(r);
                        let b = center(*q);
                        (a.0 - b.0).unsigned_abs() + (a.1 - b.1).unsigned_abs()
                    })
                    .map(|(_, i)| *i)
            })
            .or_else(|| {
                Some(
                    members[(current
                        + if matches!(key.code, KeyCode::Left | KeyCode::Up) {
                            members.len() - 1
                        } else {
                            1
                        })
                        % members.len()],
                )
            })
        }
        _ => return None,
    };
    if let Some(next) = next {
        choose(ui, &tiles[next].id);
    }
    Some(crate::Step::Continue)
}
pub fn cancel(ui: &mut Ui) -> bool {
    let Some(drag) = ui.map.drag.take() else {
        return false;
    };
    if let Drag::Workspace { start, .. } = drag {
        ui.map.ratio = start;
        return true;
    }
    ui.map.tree = Some(match drag {
        Drag::Resize { start, .. } | Drag::Move { start, .. } => start,
        Drag::Workspace { .. } => return true,
    });
    true
}
pub fn mouse(ui: &mut Ui, mouse: MouseEvent) -> bool {
    if ui.page != WORKSPACES || ui.view != View::Map {
        return false;
    }
    if crate::map_nav::mouse(ui, mouse) {
        return true;
    }
    if let Some(drag) = ui.map.drag.as_ref() {
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Drag::Workspace {
                    area, horizontal, ..
                } = drag
                {
                    let (at, origin, length) = if *horizontal {
                        (mouse.column, area.x, area.width)
                    } else {
                        (mouse.row, area.y, area.height)
                    };
                    ui.map.ratio = ((i32::from(at) - i32::from(origin)) * 100
                        / i32::from(length.max(1)))
                    .clamp(10, 90) as u16;
                }
                if let Drag::Resize {
                    start,
                    boundary,
                    pointer,
                } = drag
                {
                    let (delta, origin, length) = if boundary.horizontal {
                        (
                            i32::from(mouse.column) - i32::from(pointer.0),
                            boundary.area.x,
                            boundary.area.width,
                        )
                    } else {
                        (
                            i32::from(mouse.row) - i32::from(pointer.1),
                            boundary.area.y,
                            boundary.area.height,
                        )
                    };
                    let mut tree = start.clone();
                    tree.resize(
                        &boundary.path,
                        f64::from(i32::from(boundary.position) - i32::from(origin) + delta)
                            / f64::from(length.max(1)),
                    );
                    ui.map.tree = Some(tree);
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(Drag::Move { id, .. }) = ui.map.drag.take() {
                    let point = Position::new(mouse.column, mouse.row);
                    let tiles = ui.tiles();
                    let target = ui
                        .tile_hits
                        .iter()
                        .find(|(r, _)| r.contains(point))
                        .and_then(|(_, i)| tiles.get(*i))
                        .map(|t| t.id.clone());
                    if let Some(target) = target {
                        swap(ui, &id, &target);
                    }
                }
                ui.map.drag = None;
                ui.sizes_changed = Some(std::time::Instant::now());
                save(ui);
                true
            }
            _ => false,
        }
    } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        let point = Position::new(mouse.column, mouse.row);
        if let Some((area, pos, horizontal)) = ui.map.comparison
            && area.contains(point)
            && (if horizontal {
                mouse.column.abs_diff(pos) <= 1
            } else {
                mouse.row.abs_diff(pos) <= 1
            })
        {
            ui.map.drag = Some(Drag::Workspace {
                start: ui.map.ratio,
                area,
                horizontal,
            });
            return true;
        }
        let tiles = ui.tiles();
        let clicked = ui
            .tile_hits
            .iter()
            .find(|(r, _)| r.contains(point))
            .and_then(|(_, i)| tiles.get(*i))
            .map(|r| r.id.clone())
            .or_else(|| {
                ui.map
                    .panels
                    .iter()
                    .find(|(r, _)| r.contains(point))
                    .map(|(_, id)| id.clone())
            });
        if let Some(id) = clicked {
            choose(ui, &id);
        }
        if let Some((_, delta)) = ui
            .map
            .tab_hits
            .iter()
            .find(|(r, _)| r.contains(point))
            .copied()
        {
            next_tab(ui, delta);
            return true;
        }
        if let Some(boundary) = ui
            .map
            .boundaries
            .iter()
            .rev()
            .find(|b| b.hit(mouse.column, mouse.row))
            .cloned()
            && let Some(start) = ui.map.tree.clone()
        {
            ui.map.drag = Some(Drag::Resize {
                start,
                boundary,
                pointer: (mouse.column, mouse.row),
            });
            return true;
        }
        let Some((r, index)) = ui
            .tile_hits
            .iter()
            .find(|(r, _)| r.contains(point))
            .copied()
        else {
            return false;
        };
        let Some(id) = ui.tiles().get(index).map(|r| r.id.clone()) else {
            return false;
        };
        choose(ui, &id);
        if mouse.row == r.y
            && let Some(start) = ui.map.tree.clone()
        {
            ui.map.drag = Some(Drag::Move {
                start,
                id,
                pointer: (mouse.column, mouse.row),
            });
            return true;
        }
        false
    } else {
        false
    }
}
pub fn swap(ui: &mut Ui, target: &str, focused: &str) {
    if target == focused
        || !ui.map.members.iter().any(|id| id == target)
        || !ui.map.members.iter().any(|id| id == focused)
    {
        return;
    }
    if let Some(tree) = ui.map.tree.as_mut() {
        tree.swap(target, focused);
        save(ui);
    }
}
pub fn reset(ui: &mut Ui) {
    ui.map.cards.retain(|c| !ui.map.members.contains(&c.id));
    ui.map.tree = None;
    ui.map.drag = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LayoutPane, Node, Preview, Tile};
    fn fixture(count: usize) -> Ui {
        let mut ui = Ui {
            page: WORKSPACES,
            view: View::Map,
            ..Ui::default()
        };
        ui.snapshot.tree.push(Node {
            id: "tab".into(),
            kind: "tab".into(),
            label: "Fixture tab".into(),
            ..Node::default()
        });
        for i in 0..count {
            let id = format!("pane-{i}");
            ui.snapshot.tree.push(Node {
                id: id.clone(),
                parent: "tab".into(),
                label: id.clone(),
                kind: "pane".into(),
                tile: Some(Tile {
                    title: id.clone(),
                    ..Tile::default()
                }),
                ..Node::default()
            });
            ui.previews
                .insert(id, Preview::new(&format!("CONTENT-{i}"), ""));
        }
        ui
    }
    fn render(ui: &mut Ui, w: u16, h: u16) -> ratatui::buffer::Buffer {
        ui.tile_hits.clear();
        ui.preview_targets.clear();
        ui.screens.clear();
        ui.inline_screen = None;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, f.area(), ui)).unwrap();
        terminal.backend().buffer().clone()
    }
    fn fits(ui: &Ui, count: usize) {
        assert_eq!(ui.tile_hits.len(), count);
        let tiles = ui.tiles();
        let panes = ui
            .tile_hits
            .iter()
            .map(|(r, i)| (tiles[*i].id.clone(), *r))
            .collect::<Vec<_>>();
        assert!(Tiling::from_rects(ui.map.area, &panes).is_some());
    }
    fn native_fixture() -> Ui {
        let mut ui = fixture(2);
        ui.snapshot.tree[0].layout = Some(Layout {
            width: 100,
            height: 40,
            panes: vec![
                LayoutPane {
                    id: "pane-0".into(),
                    x: 0,
                    y: 0,
                    w: 40,
                    h: 40,
                    ..Default::default()
                },
                LayoutPane {
                    id: "pane-1".into(),
                    x: 40,
                    y: 0,
                    w: 60,
                    h: 40,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        ui
    }
    fn bounds(ui: &Ui, id: &str) -> Rect {
        ui.tile_hits
            .iter()
            .find(|(_, index)| ui.tiles()[*index].id == id)
            .unwrap()
            .0
    }
    fn external_swap(ui: &mut Ui) {
        let layout = ui
            .snapshot
            .tree
            .iter_mut()
            .find(|n| n.id == "tab")
            .unwrap()
            .layout
            .as_mut()
            .unwrap();
        layout.panes[0].x = 60;
        layout.panes[1].x = 0;
        layout.panes.reverse();
        // Herdr's reading order changes in the same snapshot as its geometry.
        ui.snapshot.tree.reverse();
        ui.reveal("pane-0");
    }
    #[test]
    fn restoring_saved_map_invalidates_cached_geometry_and_split_presets_preserve_all_panes() {
        let mut ui = native_fixture();
        render(&mut ui, 200, 64);
        let original = ui.map.cards.clone();
        swap(&mut ui, "pane-0", "pane-1");
        let changed = ui.map.cards.clone();
        assert_ne!(original, changed);
        ui.map.restore(original.clone());
        render(&mut ui, 200, 64);
        assert_eq!(ui.map.cards, original);
        assert_eq!(bounds(&ui, "pane-0").x, 0);
        ui.snapshot.tree.push(Node {
            id: "other".into(),
            kind: "tab".into(),
            ..Default::default()
        });
        ui.snapshot.tree.push(Node {
            id: "other-pane".into(),
            parent: "other".into(),
            kind: "pane".into(),
            tile: Some(Tile::default()),
            ..Default::default()
        });
        arrangement(&mut ui, "columns");
        ui.map.ratio = 60;
        render(&mut ui, 200, 64);
        choose(&mut ui, "other-pane");
        arrangement(&mut ui, "grow");
        assert_eq!(ui.map.ratio, 50);
        arrangement(&mut ui, "shrink");
        assert_eq!(ui.map.ratio, 60);
        arrangement(&mut ui, "balance");
        assert_eq!(ui.map.ratio, 50);
        for (width, height) in [(200, 64), (80, 24), (44, 18)] {
            render(&mut ui, width, height);
            let hits = &ui.map.toolbar_hits;
            for (i, (r, _, _)) in hits.iter().enumerate() {
                assert!(r.right() <= width && r.bottom() <= height);
                for (other, _, _) in &hits[i + 1..] {
                    assert!(r.intersection(*other).is_empty());
                }
            }
        }
    }
    #[test]
    fn herdr_swap_overrides_saved_map_even_when_reading_order_changes() {
        let mut ui = native_fixture();
        render(&mut ui, 200, 64);
        assert_eq!(bounds(&ui, "pane-0").x, 0);
        let old = ui.map.cards.clone();
        external_swap(&mut ui);
        render(&mut ui, 200, 64);
        fits(&ui, 2);
        assert_eq!(bounds(&ui, "pane-1").x, 0);
        assert_eq!(bounds(&ui, "pane-0").x, 120);
        assert_eq!(ui.selected_terminal().as_deref(), Some("pane-0"));
        assert_ne!(ui.map.cards, old);
    }
    #[test]
    fn baseline_survives_restart_and_distinguishes_local_edits_from_offline_native_changes() {
        let mut ui = native_fixture();
        render(&mut ui, 200, 64);
        let edge = ui.map.boundaries[0].clone();
        ui.map.tree.as_mut().unwrap().resize(&edge.path, 0.7);
        save(&mut ui);
        let local = ui.map.cards.clone();
        let baselines = ui.map.native_signatures.clone();
        let wire = crate::view_state(&ui);
        let mut stored = wire.clone();
        stored["selected"] = serde_json::Value::Null;
        let view: daedalus_tui::core::layouts::View = serde_json::from_value(stored).unwrap();
        view.validate().unwrap();
        let mut restarted = native_fixture();
        let mut snapshot = std::mem::take(&mut restarted.snapshot);
        snapshot.workspace.apply.map = Some(local.clone());
        snapshot
            .workspace
            .apply
            .switches
            .insert("map_native".into(), wire["map_native"].clone());
        restarted.update(snapshot);
        render(&mut restarted, 200, 64);
        assert_eq!(restarted.map.cards, local);
        assert_eq!(restarted.map.native_signatures, baselines);
        let mut changed = native_fixture();
        changed.map.cards = local.clone();
        changed.map.native_signatures = baselines;
        external_swap(&mut changed);
        render(&mut changed, 200, 64);
        assert_eq!(bounds(&changed, "pane-0").x, 120);
        assert_ne!(changed.map.cards, local);
        let mut legacy = native_fixture();
        legacy.map.cards = changed.map.cards.clone();
        render(&mut legacy, 200, 64);
        assert_eq!(bounds(&legacy, "pane-0").x, 0);
    }
    #[test]
    fn focus_activity_reordering_and_missing_geometry_preserve_local_edits_but_native_changes_cancel_old_drag()
     {
        let mut ui = native_fixture();
        render(&mut ui, 200, 64);
        let edge = ui.map.boundaries[0].clone();
        ui.map.tree.as_mut().unwrap().resize(&edge.path, 0.7);
        save(&mut ui);
        let local = ui.map.cards.clone();
        let baselines = ui.map.native_signatures.clone();
        ui.snapshot.tree[0].layout.as_mut().unwrap().panes[0].focused = true;
        ui.snapshot.tree[1..].reverse();
        ui.reveal("pane-0");
        render(&mut ui, 200, 64);
        assert_eq!(ui.map.cards, local);
        let native = ui.snapshot.tree[0].layout.take();
        render(&mut ui, 200, 64);
        assert_eq!(ui.map.cards, local);
        assert_eq!(ui.map.native_signatures, baselines);
        ui.snapshot.tree[0].layout = native;
        render(&mut ui, 200, 64);
        assert_eq!(ui.map.cards, local);
        ui.map.drag = Some(Drag::Move {
            start: ui.map.tree.clone().unwrap(),
            id: "pane-0".into(),
            pointer: (0, 0),
        });
        external_swap(&mut ui);
        render(&mut ui, 200, 64);
        assert!(ui.map.drag.is_none());
        assert_eq!(bounds(&ui, "pane-0").x, 120);
    }
    #[test]
    fn cached_workspace_and_comparison_half_adopt_native_swap_without_changing_split_or_selection()
    {
        let mut ui = native_fixture();
        ui.snapshot.tree.push(Node {
            id: "other".into(),
            kind: "tab".into(),
            ..Default::default()
        });
        ui.snapshot.tree.push(Node {
            id: "other-pane".into(),
            parent: "other".into(),
            kind: "pane".into(),
            tile: Some(Tile::default()),
            ..Default::default()
        });
        render(&mut ui, 200, 64);
        choose(&mut ui, "other-pane");
        render(&mut ui, 200, 64);
        external_swap(&mut ui);
        choose(&mut ui, "other-pane");
        arrangement(&mut ui, "columns");
        ui.map.ratio = 65;
        render(&mut ui, 200, 64);
        assert_eq!(ui.selected_terminal().as_deref(), Some("other-pane"));
        assert_eq!(ui.map.split, "columns");
        assert_eq!(ui.map.ratio, 65);
        assert!(bounds(&ui, "pane-1").x < bounds(&ui, "pane-0").x);
        choose(&mut ui, "pane-0");
        render(&mut ui, 200, 64);
        assert!(bounds(&ui, "pane-1").x < bounds(&ui, "pane-0").x);
    }
    #[test]
    fn focused_frame_and_label_are_distinct_in_each_theme_and_follow_selection() {
        for palette in crate::theme::Palette::ALL {
            let mut ui = fixture(2);
            ui.theme.palette = palette;
            let before = render(&mut ui, 160, 40);
            let selected = ui
                .tile_hits
                .iter()
                .find(|(_, index)| *index == ui.tile)
                .unwrap()
                .0;
            let other = ui
                .tile_hits
                .iter()
                .find(|(_, index)| *index != ui.tile)
                .unwrap()
                .0;
            assert_eq!(
                before[(selected.x, selected.y)].fg,
                ui.theme.focus_frame().fg.unwrap()
            );
            assert_ne!(
                before[(other.x, other.y)].fg,
                before[(selected.x, selected.y)].fg
            );
            let heading = (selected.x..selected.right())
                .map(|x| before[(x, selected.y)].symbol())
                .collect::<String>();
            assert!(heading.contains("FOCUSED"));
            let next = ui
                .tile_hits
                .iter()
                .find(|(_, index)| *index != ui.tile)
                .unwrap()
                .1;
            ui.select_tile(next);
            let after = render(&mut ui, 160, 40);
            assert_eq!(
                after[(other.x, other.y)].fg,
                ui.theme.focus_frame().fg.unwrap()
            );
            assert_ne!(
                after[(selected.x, selected.y)].fg,
                ui.theme.focus_frame().fg.unwrap()
            );
        }
    }
    #[test]
    fn more_than_nine_panes_and_legacy_overlaps_are_all_visible() {
        let mut ui = fixture(20);
        ui.map.cards = (0..20)
            .map(|i| Card {
                id: format!("pane-{i}"),
                x: (i % 3) * 300,
                y: (i / 3 % 3) * 300,
                width: 400,
                height: 400,
            })
            .collect();
        let buffer = render(&mut ui, 311, 74);
        fits(&ui, 20);
        let text = (0..74)
            .map(|y| {
                (0..311)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for i in 0..20 {
            assert!(
                text.contains(&format!("CONTENT-{i}")),
                "pane {i} content missing"
            );
        }
        assert_eq!(ui.preview_targets.len(), 20);
        assert!(Tiling::from_cards(&ui.map.cards).is_some());
    }
    #[test]
    fn native_layout_is_imported_and_adjacent_edges_share_the_exact_boundary() {
        let mut ui = fixture(3);
        ui.snapshot.tree[0].layout = Some(Layout {
            width: 100,
            height: 60,
            zoomed: false,
            panes: vec![
                LayoutPane {
                    id: "pane-0".into(),
                    x: 0,
                    y: 0,
                    w: 30,
                    h: 60,
                    focused: true,
                },
                LayoutPane {
                    id: "pane-1".into(),
                    x: 30,
                    y: 0,
                    w: 70,
                    h: 20,
                    focused: false,
                },
                LayoutPane {
                    id: "pane-2".into(),
                    x: 30,
                    y: 20,
                    w: 70,
                    h: 40,
                    focused: false,
                },
            ],
        });
        render(&mut ui, 200, 64);
        fits(&ui, 3);
        let rect = |id: &str| {
            ui.tile_hits
                .iter()
                .find(|(_, i)| ui.tiles()[*i].id == id)
                .unwrap()
                .0
        };
        assert_eq!(rect("pane-0"), Rect::new(0, 4, 60, 60));
        assert_eq!(rect("pane-1"), Rect::new(60, 4, 140, 20));
        assert_eq!(rect("pane-2"), Rect::new(60, 24, 140, 40));
        // A focus-only server update cannot erase a local divider adjustment.
        let edge = ui.map.boundaries[0].clone();
        ui.map.tree.as_mut().unwrap().resize(&edge.path, 0.5);
        save(&mut ui);
        ui.snapshot.tree[0].layout.as_mut().unwrap().panes[0].focused = false;
        render(&mut ui, 200, 64);
        fits(&ui, 3);
        assert_eq!(ui.tile_hits[0].0.width, 100);
    }
    #[test]
    fn divider_resize_cancel_and_title_drop_keep_a_complete_partition() {
        let mut ui = fixture(6);
        render(&mut ui, 180, 50);
        let edge = ui.map.boundaries[0].clone();
        let (x, y) = if edge.horizontal {
            (edge.position, edge.area.y + edge.area.height / 2)
        } else {
            (edge.area.x + edge.area.width / 2, edge.position)
        };
        let at = |kind, x, y| MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let original = ui.map.tree.clone();
        assert!(mouse(
            &mut ui,
            at(MouseEventKind::Down(MouseButton::Left), x, y)
        ));
        assert!(mouse(
            &mut ui,
            at(MouseEventKind::Drag(MouseButton::Left), x + 25, y + 5)
        ));
        render(&mut ui, 180, 50);
        fits(&ui, 6);
        assert!(cancel(&mut ui));
        assert_eq!(ui.map.tree, original);
        render(&mut ui, 180, 50);
        let tiles = ui.tiles();
        let (a, ai) = ui.tile_hits[0];
        let (b, bi) = *ui.tile_hits.last().unwrap();
        let from = tiles[ai].id.clone();
        let to = tiles[bi].id.clone();
        assert!(mouse(
            &mut ui,
            at(MouseEventKind::Down(MouseButton::Left), a.x + 1, a.y)
        ));
        assert!(mouse(
            &mut ui,
            at(MouseEventKind::Up(MouseButton::Left), b.x + 1, b.y)
        ));
        render(&mut ui, 180, 50);
        fits(&ui, 6);
        assert_eq!(
            ui.tile_hits
                .iter()
                .find(|(_, i)| ui.tiles()[*i].id == from)
                .unwrap()
                .0,
            b
        );
        assert_eq!(
            ui.tile_hits
                .iter()
                .find(|(_, i)| ui.tiles()[*i].id == to)
                .unwrap()
                .0,
            a
        );
    }
    #[test]
    fn tabs_stay_separate_and_every_tab_remains_reachable_with_mouse_and_keys() {
        let mut ui = fixture(3);
        ui.snapshot.tree.push(Node {
            id: "other".into(),
            kind: "tab".into(),
            label: "Other tab".into(),
            ..Node::default()
        });
        ui.snapshot.tree.push(Node {
            id: "other-pane".into(),
            parent: "other".into(),
            kind: "pane".into(),
            tile: Some(Tile::default()),
            ..Node::default()
        });
        render(&mut ui, 120, 36);
        fits(&ui, 3);
        key(
            &mut ui,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        );
        render(&mut ui, 120, 36);
        fits(&ui, 1);
        let (rect, _) = ui.map.tab_hits[0];
        assert!(mouse(
            &mut ui,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE
            }
        ));
        render(&mut ui, 120, 36);
        fits(&ui, 3);
        assert_eq!(ui.map.cards.len(), 4);
    }
    #[test]
    fn resizing_window_and_serializing_preserve_fitted_identities() {
        let mut ui = fixture(19);
        render(&mut ui, 311, 74);
        fits(&ui, 19);
        let before = ui.map.cards.clone();
        render(&mut ui, 70, 20);
        fits(&ui, 19);
        assert_eq!(ui.map.cards, before);
        let state = crate::view_state(&ui);
        let apply = serde_json::from_value::<crate::model::Apply>(state).unwrap();
        let mut restored = fixture(19);
        restored.snapshot.workspace.apply = apply;
        let snapshot = std::mem::take(&mut restored.snapshot);
        restored.update(snapshot);
        render(&mut restored, 311, 74);
        fits(&restored, 19);
        assert_eq!(restored.map.cards, before);
    }
    #[test]
    fn workspace_halves_rotate_resize_swap_and_keep_every_terminal_disjoint() {
        let mut ui = fixture(6);
        ui.snapshot.tree.push(Node {
            id: "other-tab".into(),
            kind: "tab".into(),
            ..Node::default()
        });
        ui.snapshot.tree.push(Node {
            id: "other-pane".into(),
            parent: "other-tab".into(),
            kind: "pane".into(),
            tile: Some(Tile::default()),
            ..Node::default()
        });
        render(&mut ui, 200, 64);
        arrangement(&mut ui, "columns");
        render(&mut ui, 200, 64);
        assert_eq!(ui.map.panels.len(), 2);
        assert_eq!(ui.tile_hits.len(), 7);
        let disjoint = |ui: &Ui| {
            for (i, (a, _)) in ui.tile_hits.iter().enumerate() {
                for (b, _) in &ui.tile_hits[i + 1..] {
                    assert!(a.intersection(*b).is_empty());
                }
            }
        };
        disjoint(&ui);
        choose(&mut ui, "other-pane");
        render(&mut ui, 200, 64);
        assert_eq!(ui.map.active, 1);
        assert_eq!(ui.selected_terminal().as_deref(), Some("other-pane"));
        arrangement(&mut ui, "rotate");
        render(&mut ui, 200, 64);
        assert_eq!(ui.map.split, "rows");
        disjoint(&ui);
        arrangement(&mut ui, "swap");
        render(&mut ui, 200, 64);
        assert_eq!(ui.map.panels[0].1, "other-pane");
        disjoint(&ui);
        ui.map.ratio = 10;
        render(&mut ui, 200, 24);
        assert_eq!(ui.tile_hits.len(), 7);
        disjoint(&ui);
        let view = crate::view_state(&ui);
        assert_eq!(view["map_split"], "rows");
        assert_eq!(view["map_ratio"], 10);
    }
    #[test]
    fn pane_titles_keep_exact_ids_and_each_sibling_has_a_distinct_readable_color() {
        let ui = fixture(48);
        let mut colors = std::collections::HashSet::new();
        for i in 0..48 {
            let id = format!("pane-{i}");
            assert!(colors.insert(title_color(&ui, &id)));
            assert_eq!(pane_identity(ui.terminal_node(&id).unwrap()), id);
        }
    }
}
