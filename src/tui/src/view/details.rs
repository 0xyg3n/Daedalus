//! Selected-record facts and native split diagrams.
use super::workspaces::preview_screen;
use super::{glow, harness_tone, node_icon, node_tone, page_hue, page_meta, panel, tone_of};
use crate::{
    model::{Field, Handle, Kind, Row, Split, Ui, WORKSPACES},
    theme::{self, Theme, Tone},
    ui_text::{fit, width, wrap},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
/// Badges as colored chips, as many to a line as fit in `room`.
pub(super) fn badge_chips<'a>(
    theme: &Theme,
    badges: &[(String, String)],
    room: usize,
) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0;
    let mut seen = std::collections::HashSet::new();
    for (label, tone) in badges {
        if label.trim().is_empty() || !seen.insert(label.trim().to_lowercase()) {
            continue;
        }
        let label = fit(label, room.saturating_sub(3), theme);
        let chip = 2 + width(&label);
        if used > 0 && used + 3 + chip > room {
            lines.push(Line::from(std::mem::take(&mut spans)));
            used = 0;
        }
        let gap = if used == 0 { 1 } else { 3 };
        spans.push(Span::raw(" ".repeat(gap)));
        let style = theme.fg(Tone::named(tone));
        spans.push(Span::styled(format!("{} ", theme.glyph("●", "*")), style));
        spans.push(Span::styled(label, style));
        used += chip + gap;
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

/// An inspector value in the color its tone names.
fn field_style(theme: &Theme, field: &Field) -> Style {
    match field.tone.as_str() {
        "" | "text" => theme.fg(Tone::Text),
        "state" => theme
            .fg(node_tone(&field.value, "pane"))
            .add_modifier(Modifier::BOLD),
        "harness" => theme.fg(harness_tone(&field.value)),
        "herdr" => theme.fg(Tone::Violet),
        "tmux" => theme.fg(Tone::Accent),
        "id" => Style::default().fg(theme.mix(theme::FAINT, theme::CYAN, 0.55)),
        "muted" => theme.fg(Tone::Muted),
        "faint" => theme.fg(Tone::Faint),
        "rose" => theme.fg(Tone::Rose),
        tone => theme.fg(Tone::named(tone)),
    }
}

pub(super) fn inspector(f: &mut Frame, area: Rect, ui: &mut Ui, row: Option<&Row>) {
    let theme = ui.theme;
    let block = panel(
        &theme,
        Line::from(Span::styled(" Details ", theme.fg(Tone::Muted))),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    glow(f.buffer_mut(), area, &theme, page_hue(ui), 0.4);
    // A terminal, or an agent session running in one, shows its screen
    // between the title and the facts when there is room.
    let preview = row
        .filter(|r| r.kind == Kind::Node && ui.page == WORKSPACES)
        .and_then(|r| ui.snapshot.tree.get(r.source))
        .and_then(|n| ui.terminal_node(&n.id))
        .map(|n| n.id.clone())
        .filter(|_| ui.prefs.screen && inner.height >= 20 && inner.width >= 24);
    let mut head = 0;
    let w = inner.width.saturating_sub(2) as usize;
    let tone = tone_of(ui);
    let mut lines: Vec<Line> = Vec::new();
    let para = |lines: &mut Vec<Line>, text: &str, t: Tone| {
        for l in wrap(text, w) {
            lines.push(Line::from(Span::styled(format!(" {l}"), theme.fg(t))));
        }
    };
    let title = |lines: &mut Vec<Line>, icon: &str, text: &str, t: Tone| {
        let mut first = true;
        for l in wrap(text, w.saturating_sub(3)) {
            lines.push(Line::from(vec![
                Span::styled(
                    if first {
                        format!(" {} ", icon)
                    } else {
                        "   ".into()
                    },
                    theme.fg(t),
                ),
                Span::styled(l, theme.fg(Tone::Text).add_modifier(Modifier::BOLD)),
            ]));
            first = false;
        }
    };
    let Some(row) = row else {
        para(
            &mut lines,
            "Select an item to see what it does.",
            Tone::Muted,
        );
        f.render_widget(Paragraph::new(lines), inner);
        return;
    };
    let key = (ui.page, row.id.clone());
    if ui.detail_view.key != key {
        ui.detail_view.key = key;
        ui.detail_view.scroll = 0;
    }
    let section = page_meta(ui, ui.page).0;
    let mut demo_guard = false;
    let enter: &str;
    // Shown whole or not at all, after the facts.
    let mut map: Vec<Line> = Vec::new();
    match row.kind {
        Kind::Node => {
            let Some(node) = ui.snapshot.tree.get(row.source) else {
                return;
            };
            let chip = format!(
                "{} · {}",
                node.kind.to_uppercase(),
                node.backend.to_uppercase()
            );
            lines.push(Line::from(Span::styled(
                format!(" {chip}"),
                theme
                    .fg(if node.backend == "herdr" {
                        Tone::Violet
                    } else {
                        Tone::Accent
                    })
                    .add_modifier(Modifier::BOLD),
            )));
            title(
                &mut lines,
                node_icon(&theme, &node.kind, &node.state),
                &node.label,
                node_tone(&node.state, &node.kind),
            );
            lines.push(Line::from(""));
            head = lines.len();
            let key_width = node
                .fields
                .iter()
                .map(|f| width(&f.key))
                .max()
                .unwrap_or(0)
                .min(14);
            let stacked = w < key_width + 16;
            for field in &node.fields {
                let (key, style) = (&field.key, field_style(&theme, field));
                if stacked {
                    lines.push(Line::from(Span::styled(
                        format!(" {key}"),
                        theme.fg(Tone::Muted),
                    )));
                    for l in wrap(&field.value, w) {
                        lines.push(Line::from(Span::styled(format!(" {l}"), style)));
                    }
                    continue;
                }
                let mut first = true;
                for part in wrap(&field.value, w.saturating_sub(key_width + 2)) {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(
                                " {:<width$} ",
                                if first {
                                    fit(key, key_width, &theme)
                                } else {
                                    String::new()
                                },
                                width = key_width
                            ),
                            theme.fg(Tone::Muted),
                        ),
                        Span::styled(part, style),
                    ]));
                    first = false;
                }
            }
            // A tab, or a terminal in a split tab, shows where each of the
            // tab's terminals sits.
            let (layout, current) = match node.layout.as_ref().filter(|l| l.panes.len() > 1) {
                Some(layout) => (Some(layout), String::new()),
                None => (
                    ui.tab_layout(&node.id),
                    ui.terminal_node(&node.id)
                        .map(|n| n.id.clone())
                        .unwrap_or_default(),
                ),
            };
            if let Some(layout) = layout {
                let drawn = layout_map(ui, layout, &current, inner.width.saturating_sub(2));
                if !drawn.is_empty() {
                    map.push(Line::from(""));
                    map.push(Line::from(Span::styled(
                        format!(
                            " Layout · {} terminals{}",
                            layout.panes.len(),
                            if layout.zoomed { " · zoomed" } else { "" }
                        ),
                        theme.fg(Tone::Muted),
                    )));
                    map.extend(drawn);
                }
            }
            enter = if node.actionable {
                if node.kind == "checkpoint" {
                    "Open this checkpoint"
                } else {
                    "Actions for this exact terminal"
                }
            } else if node.children {
                "Fold or unfold · ← → also work"
            } else {
                "Show this entry"
            };
        }
        Kind::Action => {
            let Some(item) = ui.section().and_then(|s| s.items.get(row.source)) else {
                return;
            };
            title(&mut lines, theme.icon(&item.icon), &item.title, tone);
            if !item.status.0.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(" ", theme.fg(Tone::Muted)),
                    Span::styled(
                        format!(" {} ", fit(&item.status.0, w.saturating_sub(3), &theme)),
                        theme
                            .fg(Tone::named(&item.status.1))
                            .bg(theme.rgb(theme::RAISED)),
                    ),
                ]));
            }
            para(
                &mut lines,
                &format!("{section} {} {}", theme.glyph("›", ">"), item.group),
                Tone::Faint,
            );
            if !item.badges.is_empty() {
                let badges = item
                    .badges
                    .iter()
                    .filter(|(label, _)| !label.trim().eq_ignore_ascii_case(item.status.0.trim()))
                    .cloned()
                    .collect::<Vec<_>>();
                lines.extend(badge_chips(&theme, &badges, w));
            }
            lines.push(Line::from(""));
            para(&mut lines, "ABOUT THIS ACTION", Tone::Faint);
            para(&mut lines, &item.note, Tone::Text);
            // Aliases are search words; finding works without listing them.
            if !item.context.is_empty() {
                lines.push(Line::from(""));
                para(&mut lines, "TERMINAL SHORTCUT", Tone::Faint);
                para(
                    &mut lines,
                    &format!("Also on a terminal's Enter menu as \"{}\"", item.context),
                    Tone::Muted,
                );
            }
            demo_guard = !item.badges.iter().any(|(_, t)| t == "ok");
            enter = "Open";
        }
        _ => {
            let Some(entry) = ui.snapshot.catalog.get(row.source) else {
                return;
            };
            title(&mut lines, theme.icon(&entry.icon), &entry.title, tone);
            para(
                &mut lines,
                &format!(
                    "{} {} {}",
                    entry.category,
                    theme.glyph("›", ">"),
                    entry.group
                ),
                Tone::Faint,
            );
            lines.push(Line::from(Span::styled(
                format!(" {}", entry.interface),
                theme.fg(Tone::Violet).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            para(&mut lines, &entry.description, Tone::Text);
            lines.push(Line::from(""));
            para(&mut lines, "REFERENCE", Tone::Faint);
            para(&mut lines, &entry.detail, Tone::Muted);
            if !entry.badges.is_empty() {
                lines.push(Line::from(""));
                lines.extend(badge_chips(&theme, &entry.badges, w));
            }
            let mentioned = entry
                .aliases
                .iter()
                .all(|a| entry.detail.contains(a.as_str()));
            if !entry.aliases.is_empty() && !mentioned {
                lines.push(Line::from(""));
                para(&mut lines, "SEARCH ALIASES", Tone::Faint);
                para(&mut lines, &entry.aliases.join(" · "), Tone::Muted);
            }
            let guided = entry.id.starts_with("action:");
            demo_guard = guided && !entry.badges.iter().any(|(_, t)| t == "ok");
            enter = if guided {
                "Run it"
            } else {
                "Read the reference"
            };
        }
    }
    let mut footer = vec![Line::from("")];
    footer.push(Line::from(vec![
        Span::styled(format!(" {} ", theme.glyph("⏎", "Enter")), theme.keycap()),
        Span::styled(format!(" {enter}"), theme.fg(Tone::Text)),
    ]));
    if demo_guard && ui.snapshot.demo {
        footer.push(Line::from(Span::styled(
            " Demo: changes are disabled",
            theme.fg(Tone::Violet),
        )));
    }
    let mut rest = inner;
    if let Some(id) = preview {
        let top: Vec<Line> = lines.drain(..head.min(lines.len())).collect();
        let top_h = (top.len() as u16).min(inner.height);
        f.render_widget(
            Paragraph::new(top),
            Rect {
                height: top_h,
                ..inner
            },
        );
        let y = inner.y + top_h;
        // Leave room for facts, their scroll hint and the persistent action.
        let room = inner.bottom().saturating_sub(y + 6).max(6);
        let height = ((u32::from(inner.height) * u32::from(ui.sizes.screen) + 50) / 100) as u16;
        let height = height.clamp(6, room);
        let label = format!(" {} Live screen", theme.icon("watch"));
        let note = "read-only ";
        let pad = (inner.width as usize).saturating_sub(width(&label) + width(note));
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(label, theme.fg(Tone::Muted).add_modifier(Modifier::BOLD)),
                Span::raw(" ".repeat(pad)),
                Span::styled(note, theme.fg(Tone::Faint)),
            ])),
            Rect::new(inner.x, y, inner.width, 1),
        );
        let screen = Rect::new(
            inner.x + 1,
            y + 1,
            inner.width.saturating_sub(2),
            height - 1,
        );
        preview_screen(f, screen, ui, &id);
        ui.preview_targets.push(id);
        ui.preview_rows = screen.height.max(1);
        // The border row between the screen and the facts can be dragged.
        let border = Rect::new(inner.x, y + height, inner.width, 1);
        let rule = theme
            .glyph("─", "-")
            .repeat(inner.width.saturating_sub(2) as usize);
        let grip = theme.glyph(" ═══ ", " === ");
        let half = (width(&rule).saturating_sub(width(grip))) / 2;
        let rule: String = rule.chars().take(half).collect();
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(rule.clone(), theme.fg(Tone::Faint)),
                Span::styled(grip, theme.fg(Tone::Muted)),
                Span::styled(rule, theme.fg(Tone::Faint)),
            ])),
            border,
        );
        ui.handles.push(Handle {
            area: border,
            split: Split::Screen,
            origin: y,
            length: inner.height,
        });
        let below = y + height + 1;
        rest = Rect {
            y: below.min(inner.bottom()),
            height: inner.bottom().saturating_sub(below),
            ..inner
        };
    }
    lines.extend(map);
    let available = (rest.height as usize).saturating_sub(footer.len());
    let overflow = lines.len() > available;
    let room = available.saturating_sub(usize::from(overflow));
    let window = crate::ui_scroll::Window::new(lines.len(), room, ui.detail_view.scroll);
    ui.detail_view.max_scroll = window.max_offset();
    ui.detail_view.scroll = window.offset;
    let body = Rect::new(rest.x, rest.y, rest.width, room as u16);
    ui.detail_view.area = (room > 0).then_some(body);
    let shown = lines
        .into_iter()
        .skip(ui.detail_view.scroll)
        .take(room)
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(shown), body);
    if overflow && available > 0 {
        let position = format!(" {}", window.label(&theme));
        let hint = if rest.width >= 48 {
            " Alt+PgUp/PgDn or wheel"
        } else {
            " Alt+PgUp/PgDn"
        };
        let pad = (rest.width as usize).saturating_sub(width(hint) + width(&position));
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(hint, theme.fg(Tone::Info)),
                Span::raw(" ".repeat(pad)),
                Span::styled(position, theme.fg(Tone::Muted)),
            ])),
            Rect::new(rest.x, body.bottom(), rest.width, 1),
        );
    }
    let footer_height = (footer.len() as u16).min(rest.height);
    f.render_widget(
        Paragraph::new(footer),
        Rect::new(
            rest.x,
            rest.bottom() - footer_height,
            rest.width,
            footer_height,
        ),
    );
}

/// A tab's split in miniature: each terminal framed where its backend
/// places it and named, `current` (if any) drawn bright and thick.
pub(super) fn layout_map(
    ui: &Ui,
    layout: &crate::model::Layout,
    current: &str,
    room: u16,
) -> Vec<Line<'static>> {
    let theme = ui.theme;
    let w = room.min(60);
    if w < 12 || layout.width == 0 || layout.height == 0 {
        return Vec::new();
    }
    // A cell is about twice as tall as it is wide.
    let h = (u32::from(w) * u32::from(layout.height) / (2 * u32::from(layout.width))) as u16;
    let h = h.clamp(4, 10);
    let mut grid = vec![vec![(" ".to_owned(), Style::default()); w as usize]; h as usize];
    let scale = |at: u16, size: u16, room: u16| {
        (u32::from(at.min(size)) * u32::from(room) / u32::from(size.max(1))) as usize
    };
    for pane in &layout.panes {
        let (x0, x1) = (
            scale(pane.x, layout.width, w),
            scale(pane.x + pane.w, layout.width, w),
        );
        let (y0, y1) = (
            scale(pane.y, layout.height, h),
            scale(pane.y + pane.h, layout.height, h),
        );
        if x1 < x0 + 2 || y1 < y0 + 2 {
            continue;
        }
        let here = pane.id == current;
        let (set, edge) = if here {
            (theme.border_strong(), theme.fg(Tone::Accent))
        } else {
            (theme.border(), theme.fg(Tone::Faint))
        };
        let (right, bottom) = (x1 - 1, y1 - 1);
        for (y, line) in [(y0, set.horizontal_top), (bottom, set.horizontal_bottom)] {
            for cell in &mut grid[y][x0..=right] {
                *cell = (line.to_owned(), edge);
            }
        }
        for row in grid.iter_mut().take(bottom).skip(y0 + 1) {
            row[x0] = (set.vertical_left.to_owned(), edge);
            row[right] = (set.vertical_right.to_owned(), edge);
        }
        grid[y0][x0] = (set.top_left.to_owned(), edge);
        grid[y0][right] = (set.top_right.to_owned(), edge);
        grid[bottom][x0] = (set.bottom_left.to_owned(), edge);
        grid[bottom][right] = (set.bottom_right.to_owned(), edge);
        if right - x0 < 3 || bottom - y0 < 2 {
            continue;
        }
        let name = ui
            .snapshot
            .tree
            .iter()
            .find(|n| n.id == pane.id)
            .map(|n| match n.tile.as_ref().filter(|t| !t.title.is_empty()) {
                Some(tile) => tile.title.clone(),
                None => n.label.clone(),
            })
            .unwrap_or_else(|| "terminal".into());
        let label = if here {
            theme.fg(Tone::Text).add_modifier(Modifier::BOLD)
        } else {
            theme.fg(Tone::Muted)
        };
        let middle = y0 + (bottom - y0) / 2;
        let inside = right - x0 - 1;
        let text = fit(&name, inside.saturating_sub(2).max(1), &theme);
        let start = x0 + 1 + inside.saturating_sub(text.chars().count()) / 2;
        for (i, c) in text.chars().enumerate() {
            // One cell per letter keeps the frame straight.
            let c = c.to_string();
            let c = if width(&c) == 1 { c } else { "?".into() };
            grid[middle][start + i] = (c, label);
        }
    }
    grid.into_iter()
        .map(|row| {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(row.into_iter().map(|(s, style)| Span::styled(s, style)));
            Line::from(spans)
        })
        .collect()
}
