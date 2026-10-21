//! Workspace layouts, terminal previews and native split views.
use super::{
    glow, hovered, keys, node_icon, node_tone, page_hue, panel, quick_bar, search, since, split,
    switcher, tone_of, tree_node_spans,
};
use crate::{
    embed::{self, Mode, Phase},
    model::{Handle, Kind, Node, Order, Sizes, Split, TileRow, Ui, View},
    neon,
    theme::{self, Theme, Tone},
    ui_text::{clean, fit, width, wrap},
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
pub(super) const TILE_WIDTH: u16 = 44;
pub(super) const TILE_HEIGHT: u16 = 10;

/// Grid of live terminal previews; only on-screen tiles are ever read.
/// An unfolded cube: readable front face with four neighbouring terminals.
pub(super) fn cube_panel(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let block = panel(&ui.theme, Line::from(""));
    let inner = block.inner(area);
    f.render_widget(block, area);
    switcher(f, area.x + 2, area.y, area.width.saturating_sub(4), ui);
    if inner.width < 20 || inner.height < 6 {
        return;
    }
    let tiles = ui.tiles();
    ui.tile = ui.tile.min(tiles.len().saturating_sub(1));
    ui.tile_columns = 2;
    ui.tiles_per_page = 5;
    let grid = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(3),
    );
    let groups = cube_groups(ui, &tiles);
    let group_index = groups
        .iter()
        .position(|g| g.contains(&ui.tile))
        .unwrap_or(0);
    let members = groups.get(group_index).cloned().unwrap_or_default();
    let workspace = tiles.get(ui.tile).map(|row| cube_workspace(ui, row));
    let heading = workspace
        .as_ref()
        .map(|(_, number, name)| {
            format!(
                " Cube {} of {} · WS {} · {} · {} terminals · Alt+Up/Down cubes · drag ↔ ↕ to resize",
                group_index + 1,
                groups.len(),
                number,
                name,
                members.len()
            )
        })
        .unwrap_or_else(|| "Cube · no terminals".into());
    let color = cube_color(
        &ui.theme,
        workspace.as_ref().map(|w| w.0.as_str()).unwrap_or(""),
    );
    f.render_widget(
        Paragraph::new(heading).style(ui.theme.surface().fg(color).add_modifier(Modifier::BOLD)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    quick_bar(
        f,
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        ui,
        &[("m", "More", "more")],
    );
    if tiles.is_empty() {
        no_tiles(f, grid, ui);
        return;
    }
    let faces = cube_faces(grid, ui.sizes);
    let movement = since(ui.moved_at, &ui.theme, 0.6);
    let burst = neon::burst(&ui.theme, 5.0, 0.3);
    let effect = if movement < 1.0 {
        movement
    } else if burst.is_some() {
        0.4
    } else {
        1.0
    };
    let anchor = ui
        .cube_anchor
        .as_ref()
        .and_then(|id| members.iter().position(|i| &tiles[*i].id == id))
        .unwrap_or_else(|| members.iter().position(|i| *i == ui.tile).unwrap_or(0));
    let mut seen = std::collections::HashSet::new();
    let mut most = 1;
    for (offset, mut rect) in faces {
        if offset == 0 && !ui.theme.reduced_motion && ui.embed.is_none() {
            let elapsed = ui
                .moved_at
                .map(|at| at.elapsed().as_millis())
                .unwrap_or(180);
            rect = cube_slide(rect, elapsed, ui.cube_motion);
        }
        let index = if offset == 0 {
            ui.tile
        } else {
            members[(anchor as isize + offset).rem_euclid(members.len() as isize) as usize]
        };
        if rect.width < 3 || rect.height < 3 {
            continue;
        }
        if offset != 0 && (!seen.insert(index) || (index == ui.tile && ui.cube_anchor.is_none())) {
            continue;
        }
        let row = &tiles[index];
        most = most.max(draw_tile(f, rect, ui, row, offset == 0));
        let workspace = cube_workspace(ui, row);
        let color = cube_color(&ui.theme, &workspace.0);
        cube_decoration(
            f,
            rect,
            ui,
            color,
            &format!("WS {} · {}", workspace.1, workspace.2),
            offset == 0,
        );
        ui.tile_hits.push((rect, index));
        if !row.id.is_empty() && !ui.preview_targets.contains(&row.id) {
            ui.preview_targets.push(row.id.clone());
        }
    }
    let protected: Vec<_> = ui.tile_hits.iter().map(|(r, _)| *r).collect();
    neon::cube_field(
        f.buffer_mut(),
        grid,
        &protected,
        &ui.theme,
        ui.theme.clock(),
        group_index as u64,
    );
    neon::fragments(
        f.buffer_mut(),
        grid,
        &protected,
        &ui.theme,
        effect,
        burst.unwrap_or(group_index as u64),
    );
    if let Some((front, _)) = ui
        .tile_hits
        .first()
        .copied()
        .filter(|_| grid.width >= 80 && grid.height >= 24)
    {
        for (split, area, origin, length, symbol) in [
            (
                Split::CubeWidth,
                Rect::new(
                    front.right() - 1,
                    front.y + 1,
                    1,
                    front.height.saturating_sub(2),
                ),
                grid.x,
                grid.width,
                ui.theme.glyph("↔", ">"),
            ),
            (
                Split::CubeHeight,
                Rect::new(
                    front.x + 1,
                    front.bottom() - 1,
                    front.width.saturating_sub(2),
                    1,
                ),
                grid.y,
                grid.height,
                ui.theme.glyph("↕", "v"),
            ),
        ] {
            ui.handles.push(Handle {
                area,
                split,
                origin,
                length,
            });
            let x = area.x + area.width / 2;
            let y = area.y + area.height / 2;
            f.render_widget(
                Paragraph::new(symbol).style(ui.theme.fg(Tone::Accent)),
                Rect::new(x, y, 1, 1),
            );
        }
    }
    ui.preview_rows = most;
}

/// One cube per workspace, retaining the current terminal order within each cube.
pub(crate) fn cube_groups(ui: &Ui, tiles: &[TileRow]) -> Vec<Vec<usize>> {
    let mut identities = Vec::<String>::new();
    let mut groups = Vec::<Vec<usize>>::new();
    for (index, row) in tiles.iter().enumerate() {
        let key = cube_workspace(ui, row).0;
        let group = identities
            .iter()
            .position(|id| id == &key)
            .unwrap_or_else(|| {
                identities.push(key);
                groups.push(Vec::new());
                groups.len() - 1
            });
        groups[group].push(index);
    }
    let mut ordered: Vec<_> = identities.into_iter().zip(groups).collect();
    ordered.sort_by_key(|(id, _)| {
        ui.snapshot
            .tree
            .iter()
            .position(|n| &n.id == id)
            .unwrap_or(usize::MAX)
    });
    ordered.into_iter().map(|(_, members)| members).collect()
}

/// Derive workspace membership from ancestry, not names or terminal order.
pub(crate) fn cube_workspace(ui: &Ui, row: &TileRow) -> (String, usize, String) {
    let mut current = row.source.and_then(|i| ui.snapshot.tree.get(i));
    for _ in 0..ui.snapshot.tree.len() {
        let Some(node) = current else {
            break;
        };
        if matches!(node.kind.as_str(), "workspace" | "tmux session") {
            let number = ui
                .snapshot
                .tree
                .iter()
                .filter(|n| matches!(n.kind.as_str(), "workspace" | "tmux session"))
                .position(|n| n.id == node.id)
                .unwrap_or(0)
                + 1;
            return (node.id.clone(), number, node.label.clone());
        }
        current = ui.snapshot.tree.iter().find(|n| n.id == node.parent);
    }
    (String::new(), 0, "Unassigned".into())
}

pub(super) fn cube_color(theme: &Theme, identity: &str) -> Color {
    // Stable across terminal ordering and process restarts.
    let hash = identity.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    let palette = [
        theme::CYAN,
        theme::VIOLET,
        theme::GREEN,
        theme::AMBER,
        theme::ROSE,
    ];
    theme.rgb(palette[hash as usize % palette.len()])
}

fn cube_decoration(f: &mut Frame, rect: Rect, ui: &Ui, color: Color, badge: &str, selected: bool) {
    let theme = ui.theme;
    // Workspace colour belongs to the frame; output keeps the harness's own colours.
    for y in rect.y + 1..rect.bottom().saturating_sub(1) {
        f.buffer_mut()[(rect.x, y)].fg = color;
        f.buffer_mut()[(rect.right() - 1, y)].fg = color;
    }
    let elapsed = ui
        .moved_at
        .map(|at| at.elapsed().as_millis())
        .unwrap_or(600);
    if theme.animated() && elapsed < 480 {
        let phase = (elapsed / 40) as u16;
        let scan = rect.y + 1 + phase % rect.height.saturating_sub(2).max(1);
        // A chromatic scan sweeps over existing glyphs, without shuffling terminal text.
        for x in rect.x + 1..rect.right().saturating_sub(1) {
            if (x + phase) % 7 < 3 {
                f.buffer_mut()[(x, scan)].fg = color;
            }
        }
        let glyph = theme.glyph("▰", "=");
        for n in 0..3u16 {
            let x = rect.x + 1 + (phase * 3 + n * 5) % rect.width.saturating_sub(2).max(1);
            f.buffer_mut()[(x, rect.bottom() - 1)]
                .set_symbol(glyph)
                .set_fg(color);
        }
    }
    neon::edge_glitch(
        f.buffer_mut(),
        rect,
        &theme,
        since(ui.moved_at, &theme, 0.6),
        u64::from(rect.x) ^ (u64::from(rect.y) << 16),
    );
    let label = if selected {
        format!(" ◇ {badge} · FRONT ")
    } else {
        format!(" {badge} ")
    };
    let label = if theme.ascii {
        label.replace('◇', "*").replace('·', "/")
    } else {
        label
    };
    let room = rect.width.saturating_sub(4);
    if room > 0 {
        f.render_widget(
            Paragraph::new(Span::styled(
                fit(&label, room as usize, &theme),
                theme.surface().fg(color).add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            )),
            Rect::new(rect.x + 2, rect.bottom() - 1, room, 1),
        );
    }
}

/// A short face-unfold transition; only drawing changes, never the underlying PTY.
pub(super) fn cube_turn(mut area: Rect, elapsed_ms: u128) -> Rect {
    let remaining = 180u128.saturating_sub(elapsed_ms);
    let inset = (u128::from(area.width / 6) * remaining / 180) as u16;
    area.x += inset;
    area.width = area.width.saturating_sub(inset * 2);
    area
}

/// Directional easing stays inside the face and settles in 180 ms.
pub(super) fn cube_slide(area: Rect, elapsed_ms: u128, direction: (i16, i16)) -> Rect {
    let mut face = cube_turn(area, elapsed_ms);
    let inset = (area.width - face.width) / 2;
    if direction.0 != 0 {
        face.x = area.x + if direction.0 > 0 { inset * 2 } else { 0 };
    }
    if direction.1 != 0 {
        let remaining = 180u128.saturating_sub(elapsed_ms);
        let shift = (u128::from(area.height / 8) * remaining / 180) as u16;
        face.height = face.height.saturating_sub(shift);
        if direction.1 > 0 {
            face.y += shift;
        }
    }
    face
}

pub(super) fn cube_faces(area: Rect, sizes: Sizes) -> Vec<(isize, Rect)> {
    if area.width < 80 || area.height < 24 {
        return vec![(0, area)];
    }
    // Terminal cells are tall: the broad centre remains readable, edges form a cross.
    let sizes = sizes.clamped();
    let wanted_width = (u32::from(area.width) * u32::from(sizes.cube_width) / 100) as u16;
    let wanted_height = (u32::from(area.height) * u32::from(sizes.cube_height) / 100) as u16;
    let side = ((area.width - wanted_width) / 2).max(12);
    let cap = ((area.height - wanted_height) / 2).max(4);
    let centre = Rect::new(
        area.x + side,
        area.y + cap,
        area.width - side * 2,
        area.height - cap * 2,
    );
    vec![
        (0, centre),
        (-1, Rect::new(area.x, centre.y, side, centre.height)),
        (1, Rect::new(centre.right(), centre.y, side, centre.height)),
        (-2, Rect::new(centre.x, area.y, centre.width, cap)),
        (2, Rect::new(centre.x, centre.bottom(), centre.width, cap)),
    ]
}

/// A workspace desk: one readable main screen with a strip of companions.
pub(super) fn tiles_panel(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    let tiles = ui.tiles();
    let block = panel(&theme, Line::from(""));
    let inner = block.inner(area);
    f.render_widget(block, area);
    switcher(f, area.x + 2, area.y, area.width.saturating_sub(4), ui);
    if inner.height < 5 || inner.width < 20 {
        return;
    }
    ui.tile = ui.tile.min(tiles.len().saturating_sub(1));
    if ui.searching || !ui.filter.is_empty() {
        search(
            f,
            Rect::new(inner.x, inner.y, inner.width, 1),
            ui,
            "Terminals",
        );
    } else {
        workspace_strip(f, Rect::new(inner.x, inner.y, inner.width, 1), ui, &tiles);
    }
    let grid = Rect::new(
        inner.x,
        inner.y + 2,
        inner.width,
        inner.height.saturating_sub(2),
    );
    quick_bar(f, inner, ui, &[("m", "More", "more")]);
    if tiles.is_empty() {
        no_tiles(f, grid, ui);
        return;
    }
    let members = cube_groups(ui, &tiles)
        .into_iter()
        .find(|g| g.contains(&ui.tile))
        .unwrap_or_default();
    let scale = |cells: u16| (u32::from(cells) * u32::from(ui.sizes.tiles) / 100).max(1) as u16;
    let companions = if grid.height >= TILE_HEIGHT * 2 && members.len() > 1 {
        (grid.width / scale(TILE_WIDTH)).clamp(1, 4) as usize
    } else {
        0
    };
    let capacity = companions + 1;
    ui.tiles_per_page = capacity;
    ui.tile_columns = 1;
    let position = members.iter().position(|i| *i == ui.tile).unwrap_or(0);
    let page = position / capacity;
    let mut shown = vec![ui.tile];
    shown.extend(
        members
            .iter()
            .copied()
            .skip(page * capacity)
            .take(capacity)
            .filter(|i| *i != ui.tile),
    );
    let main_height = if shown.len() > 1 {
        (grid.height * 2 / 3).min(grid.height - TILE_HEIGHT)
    } else {
        grid.height
    };
    let workspace = cube_workspace(ui, &tiles[ui.tile]);
    let heading = format!(
        " WS {} · {} · terminal {}/{} · page {}/{} · Alt+Up/Down workspace",
        workspace.1,
        workspace.2,
        position + 1,
        members.len(),
        page + 1,
        members.len().div_ceil(capacity)
    );
    f.render_widget(
        Paragraph::new(heading).style(theme.fg(Tone::Muted)),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );
    let mut most = 1;
    for (n, index) in shown.iter().copied().enumerate() {
        let rect = if n == 0 {
            Rect::new(grid.x, grid.y, grid.width, main_height)
        } else {
            let count = (shown.len() - 1) as u16;
            let left = grid.x + grid.width * (n as u16 - 1) / count;
            let right = grid.x + grid.width * n as u16 / count;
            Rect::new(
                left,
                grid.y + main_height,
                right - left,
                grid.height - main_height,
            )
        };
        most = most.max(draw_tile(f, rect, ui, &tiles[index], n == 0));
        ui.tile_hits.push((rect, index));
        if !tiles[index].id.is_empty() {
            ui.preview_targets.push(tiles[index].id.clone());
        }
    }
    ui.preview_rows = most;
}

/// Compact workspace tabs share exact selection identities across preview layouts.
fn workspace_strip(f: &mut Frame, area: Rect, ui: &mut Ui, tiles: &[TileRow]) {
    let groups = cube_groups(ui, tiles);
    let selected = groups
        .iter()
        .position(|g| g.contains(&ui.tile))
        .unwrap_or(0);
    let tab_width = 26u16.min(area.width).max(1);
    let capacity = (area.width / tab_width).max(1) as usize;
    let first = selected / capacity * capacity;
    for (n, members) in groups.iter().enumerate().skip(first).take(capacity) {
        let workspace = cube_workspace(ui, &tiles[members[0]]);
        let rect = Rect::new(
            area.x + ((n - first) as u16 * tab_width),
            area.y,
            tab_width,
            1,
        );
        let label = format!(
            " {} WS {} {} ({})",
            if n == selected {
                ui.theme.glyph("▸", ">")
            } else {
                " "
            },
            workspace.1,
            workspace.2,
            members.len()
        );
        let style = if n == selected {
            ui.theme.selected(1.0)
        } else {
            ui.theme.surface()
        }
        .fg(cube_color(&ui.theme, &workspace.0));
        f.render_widget(
            Paragraph::new(fit(&label, rect.width as usize, &ui.theme)).style(style),
            rect,
        );
        ui.quick_hits
            .push((rect, format!("workspace:{}", members[0])));
    }
}

/// The count, live terminals and order on a grid panel's top border.
fn grid_summary(f: &mut Frame, area: Rect, ui: &Ui, mut summary: String) {
    let live = ui
        .tiles()
        .iter()
        .filter_map(|t| t.source.and_then(|i| ui.snapshot.tree.get(i)))
        .filter(|n| n.live())
        .count();
    if live > 0 {
        summary += &format!(" · {live} live");
    }
    if ui.pinned_only {
        summary += " · pinned only";
    }
    summary += match ui.order {
        Order::Activity => " · latest first ",
        Order::Layout => " · layout order ",
    };
    let w = width(&summary) as u16;
    if w + 30 < area.width {
        f.render_widget(
            Paragraph::new(Span::styled(summary, ui.theme.fg(Tone::Faint))),
            Rect::new(area.right() - w - 1, area.y, w, 1),
        );
    }
}

fn no_tiles(f: &mut Frame, grid: Rect, ui: &Ui) {
    let text = if ui.working_only {
        "No agents are reporting working or blocked. Select All to see every terminal."
    } else if ui.pinned_only {
        "No pinned terminals. P shows every terminal; p pins the selected one."
    } else if !ui.filter.is_empty() {
        "No matches. Esc clears the search."
    } else if ui.busy {
        "Discovering…"
    } else {
        "No terminals discovered."
    };
    f.render_widget(
        Paragraph::new(Span::styled(format!(" {text}"), ui.theme.fg(Tone::Muted))),
        grid,
    );
}

/// Smallest framed screen on the Wall: a border around two rows.
pub(super) const WALL_CELL: (u16, u16) = (18, 4);
/// One terminal per line when even those do not fit.
const WALL_CHIP: u16 = 26;

/// Every terminal at once, never paged: framed live screens while they fit,
/// then one line each. Only what is drawn is previewed.
fn wall_scope(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let all = ui.snapshot.tree.iter().filter(|n| n.tile.is_some()).count();
    let working = ui
        .snapshot
        .tree
        .iter()
        .filter(|n| n.tile.is_some() && n.live())
        .count();
    let mut x = area.x;
    for (id, label, on) in [
        (
            "wall.all",
            format!(" All {all} "),
            !ui.working_only && !ui.pinned_only,
        ),
        (
            "wall.working",
            format!(" Working {working} "),
            ui.working_only,
        ),
        (
            "wall.pinned",
            format!(" Pinned {} ", ui.snapshot.workspace.pins.len()),
            ui.pinned_only,
        ),
    ] {
        let w = (width(&label) as u16).min(area.right().saturating_sub(x));
        if w == 0 {
            break;
        }
        let hit = Rect::new(x, area.y, w, 1);
        let style = if on {
            ui.theme.selected(1.0)
        } else {
            ui.theme.fg(Tone::Muted)
        };
        f.render_widget(Paragraph::new(label).style(style), hit);
        ui.quick_hits.push((hit, id.into()));
        x += w + 1;
    }
}

pub(super) fn wall_panel(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    let tiles = ui.tiles();
    let block = panel(&theme, Line::from(""));
    let inner = block.inner(area);
    f.render_widget(block, area);
    glow(f.buffer_mut(), area, &theme, page_hue(ui), 0.8);
    switcher(f, area.x + 2, area.y, area.width.saturating_sub(4), ui);
    if inner.height < 5 || inner.width < 20 {
        return;
    }
    if ui.searching || !ui.filter.is_empty() {
        search(
            f,
            Rect::new(inner.x, inner.y, inner.width, 1),
            ui,
            "Terminals",
        );
    } else {
        workspace_strip(f, Rect::new(inner.x, inner.y, inner.width, 1), ui, &tiles);
    }
    wall_scope(f, Rect::new(inner.x, inner.y + 1, inner.width, 1), ui);
    let full_grid = Rect::new(
        inner.x,
        inner.y + 2,
        inner.width,
        inner.height.saturating_sub(3),
    );
    quick_bar(
        f,
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        ui,
        &[("m", "More", "more")],
    );
    let count = tiles.len();
    ui.tile = ui.tile.min(count.saturating_sub(1));
    grid_summary(f, area, ui, format!(" {count} terminals · overview"));
    // Never shrink a fleet into unreadable postage stamps. Keep all names
    // visible and give the selected terminal most of the width instead.
    // Account for the inspector before choosing compact cards. A fleet can
    // technically fit in tiny boxes while leaving no useful terminal text.
    let readable = usize::from((full_grid.width / 2 / 50).max(1))
        * usize::from((full_grid.height / 10).max(1));
    let dense = count > readable;
    let (grid, inspect) = if ui.embed.is_some() && full_grid.width < 84 && full_grid.height < 20 {
        (Rect::default(), Some(full_grid))
    } else if count > 1 && full_grid.width >= 84 {
        let parts = split(
            full_grid,
            Direction::Horizontal,
            vec![
                Constraint::Length((full_grid.width / 3).clamp(32, 64)),
                Constraint::Min(45),
            ],
        );
        (parts[0], Some(parts[1]))
    } else if (dense && full_grid.height >= 20) || (count > 1 && full_grid.height >= 24) {
        let parts = split(
            full_grid,
            Direction::Vertical,
            vec![Constraint::Percentage(45), Constraint::Percentage(55)],
        );
        (parts[0], Some(parts[1]))
    } else {
        (full_grid, None)
    };
    let mut selected_rows = 0;
    if let Some(inspect) = inspect {
        f.render_widget(
            Paragraph::new(" SELECTED TERMINAL ").style(theme.fg(Tone::Accent)),
            Rect::new(inspect.x, inspect.y, inspect.width, 1),
        );
        if let Some(row) = tiles.get(ui.tile) {
            selected_rows = draw_tile(
                f,
                Rect::new(
                    inspect.x,
                    inspect.y + 1,
                    inspect.width,
                    inspect.height.saturating_sub(1),
                ),
                ui,
                row,
                true,
            );
        }
    }
    if tiles.is_empty() {
        no_tiles(f, grid, ui);
        return;
    }
    if dense {
        wall_roster(f, grid, ui, &tiles);
        if selected_rows > 0 && !tiles[ui.tile].id.is_empty() {
            ui.preview_targets.push(tiles[ui.tile].id.clone());
            ui.preview_rows = selected_rows;
        }
        return;
    }
    let fit_columns = (grid.width / WALL_CELL.0).max(1) as usize;
    let fit_rows = (grid.height / WALL_CELL.1).max(1) as usize;
    if count > fit_columns * fit_rows {
        wall_chips(f, grid, ui, &tiles);
        if selected_rows > 0 {
            ui.preview_targets.push(tiles[ui.tile].id.clone());
            ui.preview_rows = selected_rows;
        }
        return;
    }
    let (columns, used_rows) = grid_shape(grid, count, fit_columns, fit_rows);
    ui.tile_columns = columns;
    ui.tiles_per_page = count;
    let cell_w = grid.width / columns as u16;
    let cell_h = if count == 1 {
        grid.height
    } else {
        (grid.height / used_rows as u16).min(8)
    };
    let mut most = selected_rows;
    for (n, row) in tiles.iter().enumerate() {
        let (c, r) = ((n % columns) as u16, (n / columns) as u16);
        let x = grid.x + c * cell_w;
        let rect = Rect::new(
            x,
            grid.y + r * cell_h,
            if c as usize + 1 == columns {
                grid.right() - x
            } else {
                cell_w
            },
            cell_h,
        );
        most = most.max(if n == ui.tile && inspect.is_none() {
            draw_tile(f, rect, ui, row, true)
        } else {
            wall_cell(f, rect, ui, row, n == ui.tile)
        });
        ui.tile_hits.push((rect, n));
        if !row.id.is_empty() {
            ui.preview_targets.push(row.id.clone());
        }
    }
    ui.preview_rows = most.max(1);
}

/// How warm a terminal's edge is: live ones in their state's color, the
/// last five minutes violet, the last hour blue, the rest plain. Cyan is
/// kept for the selection.
pub(super) fn terminal_status(node: &Node) -> (&'static str, Tone) {
    let states = [
        node.state.as_str(),
        node.activity.as_str(),
        node.tile.as_ref().map_or("", |t| t.state.as_str()),
    ];
    if states
        .iter()
        .any(|s| matches!(*s, "failed" | "error" | "exited"))
    {
        ("error", Tone::Rose)
    } else if states.contains(&"blocked") {
        ("blocked", Tone::Warn)
    } else if states.iter().any(|s| matches!(*s, "working" | "busy")) {
        ("working", Tone::Ok)
    } else if states
        .iter()
        .any(|s| matches!(*s, "idle" | "ready" | "done"))
    {
        ("idle", Tone::Muted)
    } else {
        ("unknown", Tone::Faint)
    }
}
fn heat(ui: &Ui, node: &Node) -> Option<Tone> {
    let (status, tone) = terminal_status(node);
    if matches!(status, "working" | "blocked" | "error") {
        return Some(tone);
    }
    if node.active_at <= 0.0 || ui.snapshot.observed_at <= 0.0 {
        return None;
    }
    let age = ui.snapshot.observed_at - node.active_at;
    if age < 300.0 {
        Some(Tone::Violet)
    } else if age < 3600.0 {
        Some(Tone::Info)
    } else {
        None
    }
}

/// One framed screen on the Wall. Returns how many preview rows it shows.
fn wall_cell(f: &mut Frame, rect: Rect, ui: &Ui, row: &TileRow, selected: bool) -> u16 {
    let theme = ui.theme;
    let room = rect.width.saturating_sub(4) as usize;
    let Some(node) = row.source.and_then(|i| ui.snapshot.tree.get(i)) else {
        let label = row
            .pin
            .and_then(|p| ui.snapshot.workspace.pins.get(p))
            .map(|p| p.label.clone())
            .unwrap_or_default();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme.border())
            .border_style(theme.fg(if selected { Tone::Accent } else { Tone::Warn }))
            .title(Line::from(vec![
                Span::styled(format!("{} ", theme.icon("missing")), theme.fg(Tone::Warn)),
                Span::styled(
                    fit(&label, room.saturating_sub(2), &theme),
                    theme.fg(Tone::Muted),
                ),
            ]))
            .style(theme.surface());
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        f.render_widget(
            Paragraph::new(Span::styled(" Pinned, gone", theme.fg(Tone::Faint))),
            inner,
        );
        return 0;
    };
    let tile = node.tile.clone().unwrap_or_default();
    let tone = terminal_status(node).1;
    let edge = if selected {
        Tone::Accent
    } else {
        heat(ui, node).unwrap_or(Tone::Faint)
    };
    let mut right = Vec::new();
    if tile.pinned {
        right.push(Span::styled(theme.icon("pin"), theme.fg(Tone::Warn)));
    }
    if tile.split > 1 {
        right.push(Span::styled(
            format!(" {}{}", theme.icon("wall"), tile.split),
            theme.fg(Tone::Info),
        ));
    }
    let age = ui.ago(node);
    if !age.is_empty() {
        right.push(Span::styled(
            format!(" {age}"),
            theme.fg(heat(ui, node).unwrap_or(Tone::Faint)),
        ));
    }
    let used = right.iter().map(|s| width(&s.content)).sum::<usize>() + 1;
    let title_style = if selected {
        theme.selected(1.0)
    } else {
        theme.fg(Tone::Text).add_modifier(Modifier::BOLD)
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_set(if selected {
            theme.border_strong()
        } else {
            theme.border()
        })
        .border_style(theme.fg(edge))
        .title(Line::from(vec![
            Span::styled(
                format!("{} ", node_icon(&theme, "pane", &tile.state)),
                theme.fg(tone),
            ),
            Span::styled(
                fit(&tile.title, room.saturating_sub(used + 2), &theme),
                title_style,
            ),
        ]))
        .title_top(Line::from(right).right_aligned())
        .style(theme.surface());
    if rect.width >= 30 {
        let place = fit(&tile.place, rect.width.saturating_sub(6) as usize, &theme);
        block = block.title_bottom(
            Line::from(Span::styled(format!(" {place} "), theme.fg(Tone::Faint))).right_aligned(),
        );
    }
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    if inner.height == 0 || inner.width == 0 {
        return 0;
    }
    preview_screen(f, inner, ui, &node.id);
    inner.height
}

/// A readable fleet list when simultaneous previews would be too small.
pub(super) fn wall_roster(f: &mut Frame, area: Rect, ui: &mut Ui, tiles: &[TileRow]) {
    let theme = ui.theme;
    ui.wall_roster = true;
    let block = panel(&theme, Line::from(" TERMINALS "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let height = if tiles.len().saturating_mul(2) <= usize::from(inner.height) {
        2
    } else {
        1
    };
    let capacity = (usize::from(inner.height) / height).max(1);
    let first = ui.tile / capacity * capacity;
    ui.tile_columns = 1;
    ui.tiles_per_page = capacity;
    for (index, row) in tiles.iter().enumerate().skip(first).take(capacity) {
        let rect = Rect::new(
            inner.x,
            inner.y + ((index - first) * height) as u16,
            inner.width,
            height as u16,
        );
        let selected = index == ui.tile;
        let node = row.source.and_then(|i| ui.snapshot.tree.get(i));
        let (title, place, icon, status, tone) = match node {
            Some(node) => {
                let tile = node.tile.as_ref().unwrap();
                let (status, tone) = terminal_status(node);
                (
                    tile.title.as_str(),
                    tile.place.as_str(),
                    theme.icon(&tile.harness),
                    status,
                    tone,
                )
            }
            None => {
                let pin = row.pin.and_then(|i| ui.snapshot.workspace.pins.get(i));
                (
                    pin.map_or("Missing terminal", |p| p.label.as_str()),
                    pin.map_or("", |p| p.place.as_str()),
                    theme.icon("missing"),
                    "missing",
                    Tone::Warn,
                )
            }
        };
        let base = if selected {
            theme.selected(1.0)
        } else {
            theme.surface()
        };
        let workspace = cube_workspace(ui, row);
        let badge = format!(
            "{} W{} ",
            if selected {
                theme.glyph("▸", ">")
            } else {
                " "
            },
            workspace.1
        );
        let prefix = format!("{badge}{icon} ");
        let suffix = format!(" {status} ");
        let room = usize::from(inner.width).saturating_sub(width(&prefix) + width(&suffix));
        let title = fit(&clean(title), room, &theme);
        let pad = room.saturating_sub(width(&title));
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, base.fg(cube_color(&theme, &workspace.0))),
                Span::styled(title, base),
                Span::styled(" ".repeat(pad), base),
                Span::styled(suffix, base.fg(theme.rgb(tone.rgb()))),
            ]))
            .style(base),
            Rect { height: 1, ..rect },
        );
        if height == 2 {
            f.render_widget(
                Paragraph::new(format!(
                    "   {}",
                    fit(
                        &clean(place),
                        usize::from(inner.width).saturating_sub(3),
                        &theme
                    )
                ))
                .style(base.fg(theme.rgb(Tone::Muted.rgb()))),
                Rect {
                    y: rect.y + 1,
                    height: 1,
                    ..rect
                },
            );
        }
        ui.tile_hits.push((rect, index));
    }
    let end = (first + capacity).min(tiles.len());
    let label = format!(" {}–{end}/{} · click selects ", first + 1, tiles.len());
    f.render_widget(
        Paragraph::new(fit(
            &label,
            usize::from(area.width.saturating_sub(2)),
            &theme,
        ))
        .style(theme.fg(Tone::Faint)),
        Rect::new(
            area.x + 1,
            area.bottom() - 1,
            area.width.saturating_sub(2),
            1,
        ),
    );
}

/// Too many for screens: one line per terminal, in columns, with the
/// selected one always in sight.
fn wall_chips(f: &mut Frame, grid: Rect, ui: &mut Ui, tiles: &[TileRow]) {
    let theme = ui.theme;
    let columns = (grid.width / WALL_CHIP).max(1) as usize;
    let capacity = columns * grid.height.max(1) as usize;
    ui.tile_columns = columns;
    ui.tiles_per_page = capacity;
    let first = ui.tile / capacity * capacity;
    let chip_w = grid.width / columns as u16;
    for (n, row) in tiles.iter().enumerate().skip(first).take(capacity) {
        let slot = n - first;
        let rect = Rect::new(
            grid.x + (slot % columns) as u16 * chip_w,
            grid.y + (slot / columns) as u16,
            chip_w.saturating_sub(1),
            1,
        );
        let selected = n == ui.tile;
        let node = row.source.and_then(|i| ui.snapshot.tree.get(i));
        let (icon, tone, title, age) = match node {
            Some(node) => {
                let tile = node.tile.clone().unwrap_or_default();
                (
                    node_icon(&theme, "pane", &tile.state),
                    heat(ui, node).unwrap_or(node_tone(&tile.state, "pane")),
                    tile.title,
                    ui.ago(node),
                )
            }
            None => (
                theme.icon("missing"),
                Tone::Warn,
                row.pin
                    .and_then(|p| ui.snapshot.workspace.pins.get(p))
                    .map(|p| p.label.clone())
                    .unwrap_or_default(),
                String::new(),
            ),
        };
        let room = (rect.width as usize).saturating_sub(width(&age) + 4);
        let title = fit(&title, room, &theme);
        let pad = (rect.width as usize).saturating_sub(width(&title) + width(&age) + 3);
        let style = if selected {
            theme.selected(1.0)
        } else {
            theme.fg(Tone::Text)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{icon} "), theme.fg(tone)),
                Span::styled(title, style),
                Span::raw(" ".repeat(pad)),
                Span::styled(format!(" {age}"), theme.fg(Tone::Faint)),
            ])),
            rect,
        );
        ui.tile_hits.push((rect, n));
    }
}

/// Columns and rows for `count` tiles: closest to a terminal's usual shape
/// (about 3 cells wide per row) without going below the minimum tile size.
pub(super) fn grid_shape(
    grid: Rect,
    count: usize,
    max_columns: usize,
    max_rows: usize,
) -> (usize, usize) {
    if count >= max_columns * max_rows {
        return (max_columns, max_rows);
    }
    let count = count.max(1);
    (1..=max_columns.min(count))
        .map(|columns| (columns, count.div_ceil(columns)))
        .filter(|&(_, rows)| rows <= max_rows)
        .min_by(|a, b| {
            let score = |(c, r): (usize, usize)| {
                let w = f32::from(grid.width) / c as f32;
                let h = f32::from(grid.height) / r as f32;
                ((w / h.max(1.0)) / 3.0).ln().abs()
            };
            score(*a).total_cmp(&score(*b))
        })
        .unwrap_or((1, 1))
}

/// One tile. Returns how many preview rows it can show.
fn draw_tile(f: &mut Frame, rect: Rect, ui: &mut Ui, row: &TileRow, selected: bool) -> u16 {
    let theme = ui.theme;
    let title_style = if selected {
        theme.selected(1.0)
    } else {
        theme.fg(Tone::Text).add_modifier(Modifier::BOLD)
    };
    let room = rect.width.saturating_sub(10) as usize;
    let Some(node) = row.source.and_then(|i| ui.snapshot.tree.get(i)) else {
        // A pinned terminal that is gone: shown, never rebound to another one.
        let Some(pin) = row.pin.and_then(|p| ui.snapshot.workspace.pins.get(p)) else {
            return 0;
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(theme.border())
            .border_style(theme.fg(if selected { Tone::Warn } else { Tone::Faint }))
            .title(Line::from(vec![
                Span::styled(format!(" {} ", theme.icon("missing")), theme.fg(Tone::Warn)),
                Span::styled(fit(&pin.label, room, &theme), title_style),
                Span::raw(" "),
            ]))
            .style(theme.surface());
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let w = inner.width.saturating_sub(1) as usize;
        let mut lines = vec![Line::from(vec![
            Span::styled(
                format!(" {} ", pin.status.to_uppercase()),
                theme.fg(Tone::Warn).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                fit(&pin.place, w.saturating_sub(10), &theme),
                theme.fg(Tone::Muted),
            ),
        ])];
        for line in wrap(&pin.reason, w) {
            lines.push(Line::from(Span::styled(
                format!(" {line}"),
                theme.fg(Tone::Text),
            )));
        }
        lines.push(Line::from(Span::styled(
            " Pinned, never rebound. u forgets it.",
            theme.fg(Tone::Faint),
        )));
        f.render_widget(Paragraph::new(lines), inner);
        return 0;
    };
    let tile = node.tile.clone().unwrap_or_default();
    let tone = terminal_status(node).1;
    let warm = heat(ui, node);
    let edge = if selected {
        Tone::Accent
    } else {
        warm.unwrap_or(Tone::Faint)
    };
    let mut right = Vec::new();
    if tile.pinned {
        right.push(Span::styled(
            format!("{} ", theme.icon("pin")),
            theme.fg(Tone::Warn),
        ));
    }
    if tile.views > 1 {
        right.push(Span::styled(
            format!("{} views ", tile.views),
            theme.fg(Tone::Info),
        ));
    }
    if tile.split > 1 {
        right.push(Span::styled(
            format!("{}{} ", theme.icon("wall"), tile.split),
            theme.fg(Tone::Info),
        ));
    }
    let age = ui.ago(node);
    if !age.is_empty() {
        right.push(Span::styled(
            format!("{age} "),
            theme.fg(warm.unwrap_or(Tone::Faint)),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(if selected {
            theme.border_strong()
        } else {
            theme.border()
        })
        .border_style(theme.fg(edge))
        .title(Line::from(vec![
            Span::styled(
                format!(" {} ", node_icon(&theme, "pane", &tile.state)),
                theme.fg(tone),
            ),
            Span::styled(fit(&tile.title, room, &theme), title_style),
            Span::raw(" "),
        ]))
        .title_top(Line::from(right).right_aligned())
        .style(theme.surface());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    if inner.height == 0 {
        return 0;
    }
    let w = inner.width as usize;
    let mut facts = vec![tile.place.clone()];
    if !tile.harness.is_empty() {
        facts.push(format!("{} {}", theme.icon(&tile.harness), tile.harness));
    }
    if tile.viewer {
        facts.push("Daedalus view".into());
    }
    let state = fit(terminal_status(node).0, 16, &theme);
    let left = fit(
        &facts.join(" · "),
        w.saturating_sub(width(&state) + 3),
        &theme,
    );
    let pad = w.saturating_sub(width(&left) + width(&state) + 2);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {left}"), theme.fg(Tone::Muted)),
            Span::raw(" ".repeat(pad)),
            Span::styled(state, theme.fg(tone)),
        ])),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let screen = Rect::new(
        inner.x + 1,
        inner.y + 1,
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(1),
    );
    if screen.height == 0 || screen.width == 0 {
        return 0;
    }
    if selected && screen.width >= 20 && screen.height >= 4 {
        ui.pane_hits.push((screen, row.id.clone()));
    }
    // Only the selected face may accept input. Keep neighbors as passive previews.
    if selected
        && screen.width >= 20
        && screen.height >= 4
        && ui.embed.as_ref().is_some_and(|e| e.plan.node == row.id)
    {
        ui.inline_screen = Some(screen);
    }
    // The preview reads as a small screen inset in the tile.
    preview_screen(f, screen, ui, &node.id);
    screen.height
}

/// A terminal's last read screen, drawn as a dark inset; read-only.
pub(super) fn preview_screen(f: &mut Frame, screen: Rect, ui: &Ui, id: &str) {
    let theme = ui.theme;
    f.render_widget(Block::default().style(theme.screen()), screen);
    match ui.previews.get(id) {
        Some(preview) if preview.error.is_empty() => {
            if ui.view == View::Wall && ui.selected_terminal().as_deref() == Some(id) {
                // Read-only snapshots cannot resize the source terminal. Wrap
                // the selected output instead of hiding its right-hand columns.
                let rows = preview.wrapped_rows(screen.width, screen.height);
                let y = screen.bottom() - rows.len() as u16;
                for (i, (row, left, width)) in rows.into_iter().enumerate() {
                    embed::draw_screen(
                        preview.screen.screen(),
                        Rect::new(screen.x, y + i as u16, width, 1),
                        f.buffer_mut(),
                        &theme,
                        (row, left),
                    );
                }
                return;
            }
            // Bottom-aligned: the latest lines matter most.
            let top = preview.lines.saturating_sub(screen.height);
            let shown = Rect {
                height: screen.height.min(preview.lines),
                ..screen
            };
            let shown = Rect {
                y: screen.bottom() - shown.height,
                ..shown
            };
            embed::draw_screen(
                preview.screen.screen(),
                shown,
                f.buffer_mut(),
                &theme,
                (top, 0),
            );
        }
        Some(preview) => {
            f.render_widget(
                Paragraph::new(Span::styled(
                    fit(&preview.error, screen.width as usize, &theme),
                    theme.fg(Tone::Faint),
                )),
                screen,
            );
        }
        None => {
            f.render_widget(
                Paragraph::new(Span::styled("Reading screen…", theme.fg(Tone::Faint))),
                screen,
            );
        }
    }
}

pub(super) fn detail_tip(rect: Rect, text: String) -> (Rect, String) {
    (
        Rect::new(rect.right().saturating_add(1), rect.y, 0, 1),
        text,
    )
}
/// Keep the human name separate from harness/profile/session identity.
pub(super) fn session_name(node: &Node) -> String {
    let names: Vec<_> = node
        .parts
        .iter()
        .filter(|(_, role)| role == "title")
        .map(|(name, _)| clean(name))
        .filter(|s| !s.trim().is_empty())
        .collect();
    if names.is_empty() {
        clean(
            node.tile
                .as_ref()
                .filter(|t| !t.title.trim().is_empty())
                .map_or(node.label.as_str(), |t| t.title.as_str()),
        )
    } else {
        names.join(" · ")
    }
}
pub(super) fn session_field<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    node.fields
        .iter()
        .find(|f| f.key == key)
        .map(|f| f.value.trim())
        .filter(|v| !v.is_empty() && !v.starts_with("not reported"))
}
pub(super) fn session_meta(node: &Node) -> String {
    let mut facts = Vec::new();
    if let Some(tile) = &node.tile
        && !tile.harness.is_empty()
    {
        facts.push(clean(&tile.harness));
    }
    let pane = node
        .parts
        .iter()
        .find(|(_, role)| role == "id")
        .map(|(value, _)| value.as_str())
        .or_else(|| {
            session_field(node, "Tab · pane")
                .or_else(|| session_field(node, "Window · pane"))
                .and_then(|value| value.split(" · ").nth(1))
        });
    if let Some(pane) = pane {
        facts.push(clean(pane));
    }
    if let Some(sid) = session_field(node, "Agent session") {
        facts.push(format!("#{}", sid.chars().take(8).collect::<String>()));
    }
    facts.extend(
        node.parts
            .iter()
            .filter(|(_, role)| role == "model")
            .map(|(text, _)| clean(text)),
    );
    if facts.is_empty() {
        facts.push(if node.backend.is_empty() {
            "terminal".into()
        } else {
            clean(&node.backend)
        });
    }
    facts.join(" · ")
}

/// A persistent outline beside Focus; it never captures the terminal's keys.
pub(super) fn focus_structure(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border())
        .border_style(theme.fg(Tone::Faint))
        .style(theme.surface())
        .title(" Workspaces ");
    let inner = block.inner(area);
    let count = ui.snapshot.tree.iter().filter(|n| n.tile.is_some()).count();
    let block = block.title_bottom(Line::from(Span::styled(
        format!(" {count} terminals · click to watch "),
        theme.fg(Tone::Faint),
    )));
    f.render_widget(block, area);
    ui.focus_tree_area = Some(area);
    // Use the exact same visible nodes, hierarchy, order and collapse state
    // as the main Tree. Focus changes presentation, not which nodes exist.
    let rows: Vec<_> = ui
        .rows()
        .into_iter()
        .filter(|r| r.kind == Kind::Node)
        .collect();
    let detailed = inner.height >= 12 && inner.width >= 26;
    let heights: Vec<usize> = rows
        .iter()
        .map(|row| {
            let node = &ui.snapshot.tree[row.source];
            if detailed && node.tile.is_some() {
                if row.id == ui.focus_target
                    && node
                        .parts
                        .iter()
                        .any(|(s, role)| role == "topic" && !s.is_empty())
                {
                    3
                } else {
                    2
                }
            } else {
                1
            }
        })
        .collect();
    let visible = usize::from(inner.height);
    ui.focus_tree_offset = ui.focus_tree_offset.min(rows.len().saturating_sub(1));
    if ui.focus_tree_reveal {
        if let Some(index) = rows.iter().position(|r| r.id == ui.focus_target) {
            if index < ui.focus_tree_offset {
                ui.focus_tree_offset = index;
            }
            let mut used = heights[ui.focus_tree_offset..=index].iter().sum::<usize>();
            while ui.focus_tree_offset < index && used > visible {
                used = used.saturating_sub(heights[ui.focus_tree_offset]);
                ui.focus_tree_offset += 1;
            }
        }
        ui.focus_tree_reveal = false;
    }
    let mut y = inner.y;
    let mut end = ui.focus_tree_offset;
    for (index, row) in rows.iter().enumerate().skip(ui.focus_tree_offset) {
        let height = heights[index] as u16;
        if y.saturating_add(height) > inner.bottom() {
            break;
        }
        let node = &ui.snapshot.tree[row.source];
        let active = ui
            .terminal_node(&ui.focus_target)
            .is_some_and(|n| n.id == row.id);
        let style = if active {
            theme.selected(1.0)
        } else {
            theme.surface()
        };
        let hit = Rect::new(inner.x, y, inner.width, height);
        let branch_text = theme.branch(&row.branch.chars().skip(3).collect::<String>());
        if row.children {
            let x = inner.x.saturating_add(1 + width(&branch_text) as u16);
            if x < inner.right() {
                ui.focus_tree_toggles
                    .push((Rect::new(x, y, 1, 1), row.id.clone()));
            }
        }
        f.render_widget(Block::default().style(style), hit);
        f.render_widget(
            Paragraph::new(Line::from(tree_node_spans(
                &theme,
                row,
                Some(node),
                style,
                active,
                inner.width as usize,
                tone_of(ui),
            ))),
            Rect { height: 1, ..hit },
        );
        let branch = node.tile.is_none() && row.children;
        let name = &row.title;
        let metadata = session_meta(node);
        let raw_branch: String = row.branch.chars().skip(3).collect();
        let continued = raw_branch
            .strip_suffix("├─ ")
            .map(|head| format!("{head}│  "))
            .or_else(|| {
                raw_branch
                    .strip_suffix("└─ ")
                    .map(|head| format!("{head}   "))
            })
            .unwrap_or(raw_branch);
        let continuation = format!("{}    ", theme.branch(&continued));
        let indent = width(&continuation);
        if height > 1 {
            let detail = format!(
                "{}{}",
                continuation,
                fit(
                    &metadata,
                    usize::from(inner.width).saturating_sub(indent),
                    &theme
                )
            );
            f.render_widget(
                Paragraph::new(detail).style(style.fg(theme.rgb(Tone::Muted.rgb()))),
                Rect {
                    y: y + 1,
                    height: 1,
                    ..hit
                },
            );
        }
        if height > 2
            && let Some((topic, _)) = node.parts.iter().find(|(_, role)| role == "topic")
        {
            f.render_widget(
                Paragraph::new(format!(
                    "{}{}",
                    continuation,
                    fit(
                        &clean(topic),
                        usize::from(inner.width).saturating_sub(indent),
                        &theme
                    )
                ))
                .style(style.fg(theme.rgb(Tone::Info.rgb()))),
                Rect {
                    y: y + 2,
                    height: 1,
                    ..hit
                },
            );
        }
        if hovered(ui, hit) {
            ui.tooltip = Some(detail_tip(hit, format!("{name} · {metadata}")));
        }
        ui.focus_tree_hits.push((hit, row.id.clone(), branch));
        y += height;
        end = index + 1;
    }
    if (ui.focus_tree_offset > 0 || end < rows.len()) && end > ui.focus_tree_offset {
        let label = format!(
            " {}{}{end}/{} rows · scroll ",
            ui.focus_tree_offset + 1,
            theme.glyph("–", "-"),
            rows.len()
        );
        f.render_widget(
            Paragraph::new(fit(
                &label,
                usize::from(area.width.saturating_sub(2)),
                &theme,
            ))
            .style(theme.fg(Tone::Faint)),
            Rect::new(
                area.x + 1,
                area.bottom() - 1,
                area.width.saturating_sub(2),
                1,
            ),
        );
    }
}

/// The focused terminal: a live embedded view, or how to open one.
pub(super) fn focus_panel(f: &mut Frame, area: Rect, ui: &mut Ui) -> Option<(u16, u16)> {
    let theme = ui.theme;
    let node = ui.terminal_node(&ui.focus_target).cloned();
    let (chip, tone) = match ui.embed.as_ref().map(|e| (&e.phase, e.mode)) {
        None => ("NOT CONNECTED", Tone::Faint),
        Some((Phase::Connecting, _)) => ("CONNECTING", Tone::Info),
        Some((Phase::Ended(_), _)) => ("ENDED", Tone::Danger),
        Some((Phase::Live, Mode::Control)) => ("CONTROL", Tone::Warn),
        Some((Phase::Live, Mode::Observe)) => ("OBSERVE", Tone::Accent),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border())
        .border_style(theme.fg(if chip == "CONTROL" {
            Tone::Warn
        } else {
            Tone::Faint
        }))
        .style(theme.surface());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if chip != "CONTROL" {
        // Control keeps its amber edge: input goes to the terminal.
        glow(f.buffer_mut(), area, &theme, page_hue(ui), 0.6);
    }
    switcher(f, area.x + 2, area.y, area.width.saturating_sub(4), ui);
    let (views, input) = match (&ui.embed, &node) {
        (Some(e), _) => (e.plan.views, e.plan.input.clone()),
        (None, Some(n)) => (n.tile.as_ref().map(|t| t.views).unwrap_or(0), String::new()),
        _ => (0, String::new()),
    };
    let split = node
        .as_ref()
        .and_then(|n| ui.tab_layout(&n.id))
        .map_or(0, |l| l.panes.len());
    let mut parts = Vec::new();
    if split > 1 {
        parts.push(format!(
            "{split} terminals in this tab{}",
            if ui.zoomed { " · zoomed" } else { "" }
        ));
    }
    if split > 1 && !ui.zoomed {
        parts.push("drag borders to resize".into());
    }
    if views > 0 {
        parts.push(format!(
            "{views} {}",
            if views == 1 { "view" } else { "views" }
        ));
    }
    let facts = if parts.is_empty() {
        String::new()
    } else {
        format!(" {} ", parts.join(" · "))
    };
    let w = width(&facts) as u16;
    if w > 0 && w + 32 < area.width {
        f.render_widget(
            Paragraph::new(Span::styled(facts, theme.fg(Tone::Info))),
            Rect::new(area.right() - w - 1, area.y, w, 1),
        );
    }
    if inner.height < 3 || inner.width < 20 {
        return None;
    }
    let title = match (&ui.embed, &node) {
        (Some(e), _) if !e.plan.title.is_empty() => e.plan.title.clone(),
        (_, Some(n)) => n.label.clone(),
        _ => "No terminal selected".into(),
    };
    let notice = ui
        .embed
        .as_ref()
        .map(|e| e.notice.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or(input);
    let cap = format!(" {chip} ");
    let chip_style = if theme.color {
        Style::default()
            .bg(theme.rgb(tone.rgb()))
            .fg(theme.rgb(theme::BG))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    };
    let room = (inner.width as usize).saturating_sub(width(&cap) + 2);
    let title = fit(
        &title,
        (room / 2).max(room.saturating_sub(width(&notice) + 3)),
        &theme,
    );
    let notice = fit(&notice, room.saturating_sub(width(&title) + 3), &theme);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(cap, chip_style),
            Span::styled(
                format!(" {title}"),
                theme.fg(Tone::Text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {notice}"), theme.fg(Tone::Muted)),
        ])),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let context = u16::from(inner.height >= 12 && node.is_some());
    if context > 0
        && let Some(node) = &node
    {
        let folder = session_field(node, "Folder")
            .unwrap_or_else(|| node.tile.as_ref().map_or("", |t| t.place.as_str()));
        let context = format!(" {} · {}", session_meta(node), clean(folder));
        f.render_widget(
            Paragraph::new(fit(&context, usize::from(inner.width), &theme))
                .style(theme.fg(Tone::Muted)),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
    let screen = Rect::new(
        inner.x,
        inner.y + 1 + context,
        inner.width,
        inner.height.saturating_sub(2 + context),
    );
    let hints = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    // A terminal in a split tab is shown with its neighbors, where the
    // backend places them; z zooms it alone.
    let current = node.as_ref().map(|n| n.id.clone()).unwrap_or_default();
    let layout = if ui.zoomed {
        None
    } else {
        ui.tab_layout(&current).cloned()
    };
    let cursor = match layout {
        Some(layout) => split_panes(f, screen, ui, &layout, &current),
        None => focus_screen(f, screen, ui, node.is_some()),
    };
    let control = ui
        .embed
        .as_ref()
        .is_some_and(|e| e.mode == Mode::Control && e.live());
    if ui.embed.as_ref().is_some_and(|e| e.prefix) {
        let pairs = [
            ("D", "Detach"),
            ("K", "Switch"),
            ("W", "Wall"),
            ("B", "Working"),
            ("U", "Updates"),
            ("O", "Observe"),
            ("A", "Attach"),
            ("F", "Fit size"),
            ("Alt+arrows", "Resize pane"),
            ("R", "Redraw"),
            ("Z", "Zoom"),
            (theme.glyph("←→", "Arrows"), "Pane"),
            ("M", "More"),
            ("T", "Take over (Herdr)"),
            ("^]", "Send ^]"),
        ];
        let mut line = keys(&theme, &pairs, inner.width as usize - 12);
        line.spans.insert(
            0,
            Span::styled(
                " Ctrl+] then: ",
                theme.fg(Tone::Warn).add_modifier(Modifier::BOLD),
            ),
        );
        f.render_widget(Paragraph::new(line), hints);
    } else if control {
        // The way out is always on screen while keys go to the terminal.
        let escape = format!(
            " Ctrl+] then D detaches {} it keeps running ",
            theme.glyph("·", "-")
        );
        let w = width(&escape) as u16;
        f.render_widget(
            Paragraph::new(Span::styled(escape, theme.keycap())),
            Rect::new(hints.x, hints.y, w.min(hints.width), 1),
        );
        ui.quick_hits.push((
            Rect::new(hints.x, hints.y, w.min(hints.width), 1),
            "detach".into(),
        ));
        let rest = format!(
            "  Drag borders to resize {} ^] then Alt+arrows",
            theme.glyph("·", "-")
        );
        if w + width(&rest) as u16 <= hints.width {
            f.render_widget(
                Paragraph::new(Span::styled(rest, theme.fg(Tone::Muted))),
                Rect::new(hints.x + w, hints.y, hints.width - w, 1),
            );
        }
    } else {
        quick_bar(
            f,
            hints,
            ui,
            &[
                ("Esc", "Back", "detach"),
                ("m", "More", "more"),
                ("Alt+arrows", "Resize pane", "focus.resize.hint"),
            ],
        );
    }
    cursor
}

/// The focused terminal's live screen, or why there is none.
pub(super) fn focus_screen(
    f: &mut Frame,
    screen: Rect,
    ui: &mut Ui,
    known: bool,
) -> Option<(u16, u16)> {
    let theme = ui.theme;
    let Some(embed) = ui.embed.as_mut() else {
        let lines: &[&str] = if known {
            &[
                "Not connected. Nothing is read from or sent to this terminal yet.",
                "Enter or w watches it read-only. a attaches to type into it.",
                "Esc returns. Detaching never stops the terminal.",
            ]
        } else {
            &[
                "This terminal is no longer in the inventory.",
                "Esc returns.",
            ]
        };
        notice_box(f, screen, &theme, "Focus", lines);
        return None;
    };
    if screen.width < 20 || screen.height < 4 {
        ui.inline_screen = None;
        f.render_widget(
            Paragraph::new("Enlarge this pane to type").style(theme.fg(Tone::Warn)),
            screen,
        );
        return None;
    }
    ui.inline_screen = Some(screen);
    embed.resize(screen.height, screen.width);
    let cursor = embed.render(screen, f.buffer_mut(), &theme);
    if let Phase::Ended(reason) = &embed.phase {
        let reason = reason.clone();
        notice_box(
            f,
            screen,
            &theme,
            "Stream ended",
            &[
                reason.as_str(),
                "w watches it again; a attaches; Esc detaches.",
            ],
        );
    }
    cursor
}

/// A whole tab as its backend lays it out, scaled to `screen`: the focused
/// terminal live in its place, the others as read-only previews. Each is
/// framed and named; a click or an arrow moves Focus to another.
fn split_panes(
    f: &mut Frame,
    screen: Rect,
    ui: &mut Ui,
    layout: &crate::model::Layout,
    current: &str,
) -> Option<(u16, u16)> {
    let theme = ui.theme;
    let control = ui
        .embed
        .as_ref()
        .is_some_and(|e| e.mode == Mode::Control && e.live());
    // Edges scale exactly, so neighbors share their borders without gaps.
    let scale = |at: u16, size: u16, origin: u16, room: u16| {
        origin + (u32::from(at.min(size)) * u32::from(room) / u32::from(size.max(1))) as u16
    };
    let mut cursor = None;
    let mut most = 0;
    for pane in &layout.panes {
        let x0 = scale(pane.x, layout.width, screen.x, screen.width);
        let x1 = scale(pane.x + pane.w, layout.width, screen.x, screen.width);
        let y0 = scale(pane.y, layout.height, screen.y, screen.height);
        let y1 = scale(pane.y + pane.h, layout.height, screen.y, screen.height);
        let rect = Rect::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0));
        if rect.width < 4 || rect.height < 3 {
            continue;
        }
        let here = pane.id == current;
        let (title, state, age, edge) = match ui.snapshot.tree.iter().find(|n| n.id == pane.id) {
            Some(node) => {
                let tile = node.tile.clone().unwrap_or_default();
                let edge = if here {
                    if control { Tone::Warn } else { Tone::Accent }
                } else {
                    heat(ui, node).unwrap_or(Tone::Faint)
                };
                (tile.title, tile.state, ui.ago(node), edge)
            }
            None => ("Terminal".into(), String::new(), String::new(), Tone::Faint),
        };
        let room = rect.width.saturating_sub(4 + width(&age) as u16) as usize;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(if here {
                theme.border_strong()
            } else {
                theme.border()
            })
            .border_style(theme.fg(edge))
            .title(Line::from(vec![
                Span::styled(
                    format!("{} ", node_icon(&theme, "pane", &state)),
                    theme.fg(node_tone(&state, "pane")),
                ),
                Span::styled(
                    fit(&title, room.saturating_sub(2), &theme),
                    if here {
                        theme.selected(1.0)
                    } else {
                        theme.fg(Tone::Muted)
                    },
                ),
            ]))
            .title_top(
                Line::from(Span::styled(format!(" {age}"), theme.fg(Tone::Faint))).right_aligned(),
            )
            .style(theme.surface());
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        ui.pane_hits.push((rect, pane.id.clone()));
        if here {
            cursor = focus_screen(f, inner, ui, true);
        } else {
            preview_screen(f, inner, ui, &pane.id);
            ui.preview_targets.push(pane.id.clone());
            most = most.max(inner.height);
        }
    }
    if most > 0 {
        ui.preview_rows = most;
    }
    ui.pane_dividers = layout.dividers(screen);
    for divider in &ui.pane_dividers {
        let hovered = ui
            .pointer
            .is_some_and(|(x, y)| divider.area.contains(ratatui::layout::Position::new(x, y)));
        let x = divider.area.x;
        let y = divider.area.y;
        let (x, y) = if divider.horizontal {
            (x, y + divider.area.height / 2)
        } else {
            (x + divider.area.width / 2, y)
        };
        f.buffer_mut().set_string(
            x,
            y,
            theme.glyph(
                if divider.horizontal { "↔" } else { "↕" },
                if divider.horizontal { "|" } else { "-" },
            ),
            theme.fg(if hovered { Tone::Accent } else { Tone::Muted }),
        );
    }
    if let Some(drag) = &ui.pane_resize {
        let divider = &drag.divider;
        let offset =
            drag.cells * i32::from(divider.screen_length) / i32::from(divider.native_length.max(1));
        if divider.horizontal {
            let x = (i32::from(divider.area.x) + offset).clamp(
                i32::from(screen.x),
                i32::from(screen.right().saturating_sub(1)),
            ) as u16;
            for y in divider.area.y..divider.area.bottom() {
                f.buffer_mut()
                    .set_string(x, y, theme.glyph("┃", "|"), theme.fg(Tone::Accent));
            }
        } else {
            let y = (i32::from(divider.area.y) + offset).clamp(
                i32::from(screen.y),
                i32::from(screen.bottom().saturating_sub(1)),
            ) as u16;
            for x in divider.area.x..divider.area.right() {
                f.buffer_mut()
                    .set_string(x, y, theme.glyph("━", "-"), theme.fg(Tone::Accent));
            }
        }
    }
    cursor
}

fn notice_box(f: &mut Frame, area: Rect, theme: &Theme, title: &str, lines: &[&str]) {
    let width_ = area.width.saturating_sub(4).min(76);
    let body: Vec<String> = lines
        .iter()
        .flat_map(|l| wrap(l, width_.saturating_sub(4) as usize))
        .collect();
    let height = (body.len() as u16 + 2).min(area.height);
    if width_ < 10 || height < 3 {
        return;
    }
    let rect = Rect::new(
        area.x + (area.width - width_) / 2,
        area.y + (area.height - height) / 2,
        width_,
        height,
    );
    f.render_widget(Clear, rect);
    let block = panel(
        theme,
        Line::from(Span::styled(
            format!(" {title} "),
            theme.fg(Tone::Accent).add_modifier(Modifier::BOLD),
        )),
    );
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(
        Paragraph::new(
            body.into_iter()
                .map(|l| Line::from(Span::styled(format!(" {l}"), theme.fg(Tone::Text))))
                .collect::<Vec<_>>(),
        ),
        inner,
    );
}
