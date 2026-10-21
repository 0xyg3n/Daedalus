//! Instance overview, connection lights and setup progress.
use super::{glow, page_meta, panel, render_list, shortcut, split};
use crate::{
    model::{Kind, OVERVIEW, Ui},
    neon,
    theme::{self, Tone},
    ui_text::{fit, width},
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
pub(super) fn overview(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    let (title, icon, category) = page_meta(ui, OVERVIEW);
    let tone = Tone::category(&category);
    let block = panel(
        &theme,
        Line::from(vec![
            Span::raw(" "),
            Span::styled(theme.icon(&icon), theme.fg(tone)),
            Span::raw(" "),
            Span::styled(title, theme.fg(tone).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    glow(f.buffer_mut(), area, &theme, tone.rgb(), 0.8);
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let s = &ui.snapshot;
    let mut y = inner.y;
    let tagline = "Your agents. Your workspaces. One place to control them.";
    let version = if s.release.is_empty() {
        format!("v{}", env!("CARGO_PKG_VERSION"))
    } else {
        format!("v{}", s.release)
    };
    let banner = neon::banner("DAEDALUS").filter(|_| !theme.ascii);
    let banner_width = banner.as_ref().map_or(0, |b| b[0].chars().count() as u16);
    if let Some(rows) = banner.filter(|_| inner.height >= 28 && inner.width >= banner_width + 2) {
        // Block letters in neon; beside them when there is room, else below.
        neon::draw_banner(
            f.buffer_mut(),
            Rect::new(inner.x, y, banner_width + 1, 3),
            &theme,
            &rows,
        );
        let facts = vec![
            Line::from(Span::styled(
                fit(&s.instance, 30, &theme),
                theme.fg(Tone::Text).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(tagline, theme.fg(Tone::Muted))),
            Line::from(Span::styled(version.clone(), theme.fg(Tone::Faint))),
        ];
        let beside = banner_width + 4;
        if inner.width >= beside + width(tagline) as u16 {
            f.render_widget(
                Paragraph::new(facts),
                Rect::new(inner.x + beside, y, inner.width - beside, 3),
            );
            y += 4;
        } else {
            f.render_widget(
                Paragraph::new(facts[1..].to_vec()),
                Rect::new(inner.x, y + 3, inner.width, 2),
            );
            y += 6;
        }
    } else if inner.height >= 20 {
        let hero = vec![
            Line::from(vec![
                Span::styled(
                    "D A E D A L U S",
                    theme.fg(Tone::Accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("   {}", fit(&s.instance, 30, &theme)),
                    theme.fg(Tone::Text),
                ),
                Span::styled(format!("   {version}"), theme.fg(Tone::Faint)),
            ]),
            Line::from(Span::styled(tagline, theme.fg(Tone::Muted))),
        ];
        f.render_widget(Paragraph::new(hero), Rect::new(inner.x, y, inner.width, 2));
        y += 3;
    }
    // Tiles: one figure each, colored by meaning.
    let tiles = [
        ("workspaces", s.terminals, "terminals", Tone::Accent),
        ("linked", s.views, "views", Tone::Info),
        ("route", s.routes, "saved routes", Tone::Violet),
        ("checkpoint", s.checkpoints, "checkpoints", Tone::Rose),
        (
            "warning",
            s.warnings,
            "warnings",
            if s.warnings > 0 {
                Tone::Warn
            } else {
                Tone::Muted
            },
        ),
    ];
    if inner.height >= 16 && inner.width >= 50 {
        let cells = split(
            Rect::new(inner.x, y, inner.width, 4),
            Direction::Horizontal,
            vec![Constraint::Ratio(1, tiles.len() as u32); tiles.len()],
        );
        for (i, (icon, value, label, tone)) in tiles.iter().enumerate() {
            let rect = Rect {
                width: cells[i].width.saturating_sub(1),
                ..cells[i]
            };
            let tile = Block::default()
                .borders(Borders::ALL)
                .border_set(theme.border())
                .border_style(theme.fg(Tone::Faint))
                .style(if theme.color {
                    Style::default().bg(theme.rgb(theme::RAISED))
                } else {
                    Style::default()
                });
            let content = tile.inner(rect);
            f.render_widget(tile, rect);
            let room = content.width as usize;
            let number = format!("{value:02}");
            let pad = room.saturating_sub(width(&number) + 3);
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled(format!(" {}", theme.icon(icon)), theme.fg(*tone)),
                        Span::raw(" ".repeat(pad)),
                        Span::styled(number, theme.fg(*tone).add_modifier(Modifier::BOLD)),
                        Span::raw(" "),
                    ]),
                    Line::from(Span::styled(
                        format!(
                            " {}",
                            fit(
                                if *label == "saved routes" && room < 13 {
                                    "routes"
                                } else {
                                    label
                                },
                                room.saturating_sub(1),
                                &theme
                            )
                        ),
                        theme.fg(Tone::Muted),
                    )),
                ]),
                content,
            );
        }
        y += 5;
    } else {
        let line: Vec<Span> = tiles
            .iter()
            .flat_map(|(icon, value, label, tone)| {
                [
                    Span::styled(format!("{} ", theme.icon(icon)), theme.fg(*tone)),
                    Span::styled(
                        format!("{value} "),
                        theme.fg(Tone::Text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{label}   "), theme.fg(Tone::Muted)),
                ]
            })
            .collect();
        f.render_widget(
            Paragraph::new(Line::from(line)),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y += 2;
    }
    // Connections and setup, side by side when there is room.
    let items = ui.rows();
    let action_height: u16 = items
        .iter()
        .map(|row| if row.kind == Kind::Header { 1 } else { 2 })
        .sum();
    let side = inner.width >= 76;
    let block_height = (s.overview.connections.len() + 2).max(s.overview.setup.len() + 1) as u16;
    let available = inner
        .bottom()
        .saturating_sub(y.saturating_add(action_height.min(4) + 1));
    let columns = if side {
        split(
            Rect::new(inner.x, y, inner.width, block_height.min(available)),
            Direction::Horizontal,
            vec![Constraint::Percentage(48), Constraint::Percentage(52)],
        )
    } else {
        split(
            Rect::new(
                inner.x,
                y,
                inner.width,
                available.min((s.overview.connections.len() + s.overview.setup.len() + 3) as u16),
            ),
            Direction::Vertical,
            vec![
                Constraint::Length((s.overview.connections.len() as u16 + 2).min(available / 2)),
                Constraint::Min(1),
            ],
        )
    };
    let heading = |text: &str, note: String| {
        Line::from(vec![
            Span::styled(
                text.to_uppercase(),
                theme.fg(tone).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {note}"), theme.fg(Tone::Faint)),
        ])
    };
    let mut left = vec![heading("Connections", "local processes".into())];
    for light in &s.overview.connections {
        let (glyph, t) = match light.state.as_str() {
            "running" => (theme.glyph("●", "*"), Tone::Ok),
            "absent" | "stopped" => (theme.glyph("○", "o"), Tone::Faint),
            "failed" | "error" => (theme.glyph("!", "!"), Tone::Danger),
            _ => (theme.glyph("◌", "?"), Tone::Muted),
        };
        let label_room = columns[0].width.saturating_sub(8).min(16) as usize;
        let label = fit(&light.label, label_room, &theme);
        let pad = label_room.saturating_sub(width(&label)) + 2;
        left.push(Line::from(vec![
            Span::styled(format!("{glyph} "), theme.fg(t)),
            Span::styled(label, theme.fg(Tone::Text)),
            Span::raw(" ".repeat(pad)),
            Span::styled(
                fit(
                    &light.detail,
                    (columns[0].width as usize).saturating_sub(label_room + 4),
                    &theme,
                ),
                theme.fg(if t == Tone::Ok { Tone::Ok } else { Tone::Muted }),
            ),
        ]));
    }
    if s.overview.connections.is_empty() {
        left.push(Line::from(Span::styled(
            "Manage connections in Settings.",
            theme.fg(Tone::Muted),
        )));
    }
    f.render_widget(Paragraph::new(left), columns[0]);
    let done = s.overview.setup.iter().filter(|x| x.done).count();
    let total = s.overview.setup.len();
    let bar: String = s
        .overview
        .setup
        .iter()
        .map(|step| {
            if step.done {
                theme.glyph("▰", "#")
            } else {
                theme.glyph("▱", "-")
            }
        })
        .collect();
    let mut right = vec![Line::from(vec![
        Span::styled("SETUP", theme.fg(tone).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {bar} "), theme.fg(Tone::Ok)),
        Span::styled(format!("{done} of {total} saved"), theme.fg(Tone::Faint)),
    ])];
    let next = s.overview.setup.iter().position(|step| !step.done);
    for (index, step) in s.overview.setup.iter().enumerate() {
        let (glyph, t) = if step.done {
            (theme.icon("done"), Tone::Ok)
        } else if next == Some(index) {
            (theme.glyph("›", ">"), Tone::Accent)
        } else {
            (theme.icon("pending"), Tone::Faint)
        };
        let label_room = columns[1].width.saturating_sub(8).min(15) as usize;
        let label = fit(&step.label, label_room, &theme);
        let pad = label_room.saturating_sub(width(&label)) + 2;
        right.push(Line::from(vec![
            Span::styled(format!("{glyph} "), theme.fg(t)),
            Span::styled(
                label,
                theme.fg(if next == Some(index) {
                    Tone::Accent
                } else {
                    Tone::Text
                }),
            ),
            Span::raw(" ".repeat(pad)),
            Span::styled(
                fit(
                    &step.detail,
                    (columns[1].width as usize).saturating_sub(label_room + 4),
                    &theme,
                ),
                theme.fg(Tone::Muted),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(right), columns[1]);
    y = columns[columns.len() - 1].bottom() + 1;
    // The one action that lives here.
    let start_height = action_height.min(inner.bottom().saturating_sub(y));
    let list = Rect::new(inner.x, y, inner.width, start_height);
    render_list(f, list, ui, &items, true);
    y = list.bottom() + 1;
    // A map of the other sections: descriptions, not repeated actions.
    let guide: Vec<_> = ui
        .navigation_order()
        .into_iter()
        .filter(|&page| page != OVERVIEW)
        .map(|i| {
            let (title, icon, category) = page_meta(ui, i);
            let summary = ui
                .snapshot
                .sections
                .get(i)
                .map(|s| s.summary.clone())
                .unwrap_or_default();
            (i, title, icon, category, summary)
        })
        .collect();
    let per_column = if inner.width >= 100 {
        guide.len().div_ceil(2)
    } else {
        guide.len()
    };
    if inner.bottom() < y + 2 + per_column as u16 {
        return;
    }
    f.render_widget(
        Paragraph::new(heading(
            "Where things live",
            "press a number to jump".into(),
        )),
        Rect::new(inner.x, y, inner.width, 1),
    );
    let half = if per_column < guide.len() {
        inner.width / 2
    } else {
        inner.width
    };
    // Keys are right-aligned within each column, so "0" and "^P" line up.
    let key_width = |column: usize| {
        guide
            .iter()
            .skip(column * per_column)
            .take(per_column)
            .map(|(page, _, _, _, _)| width(&shortcut(*page)))
            .max()
            .unwrap_or(1)
    };
    for (n, (page, title, icon, category, summary)) in guide.iter().enumerate() {
        let column = (n / per_column) as u16;
        let rect = Rect::new(
            inner.x + column * half,
            y + 1 + (n % per_column) as u16,
            half.saturating_sub(1),
            1,
        );
        let tone = Tone::category(category);
        let label = format!(
            "{:>keys$} {} ",
            shortcut(*page),
            theme.icon(icon),
            keys = key_width(column as usize)
        );
        let title_width = 14usize;
        let pad = title_width.saturating_sub(width(title));
        let room = (rect.width as usize).saturating_sub(width(&label) + title_width + 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(label, theme.fg(tone)),
                Span::styled(title.clone(), theme.fg(Tone::Text)),
                Span::raw(" ".repeat(pad)),
                Span::styled(fit(summary, room, &theme), theme.fg(Tone::Muted)),
            ])),
            rect,
        );
        ui.nav_hits.push((rect, *page));
    }
}
