//! Forms, reviewed choices, output and display preferences.
use super::{hovered, keys, since, split};
use crate::{
    model::Ui,
    neon,
    options::{self, Item},
    theme::{self, Theme, Tone},
    ui_text::{clean, fit, input_tail, width, wrap},
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
/// A row of the Options panel.
enum OptionRow {
    Blank,
    Heading(&'static str),
    Item(usize),
}

/// Options: every workspace preference, grouped, each with what it is set
/// to and the key that also changes it.
pub(super) fn options_panel(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    let Some(menu) = ui.options.clone() else {
        return;
    };
    let selected = menu.selected.min(Item::ALL.len() - 1);
    let mut rows = Vec::new();
    let mut last = "";
    for (i, item) in Item::ALL.iter().enumerate() {
        let group = item.group();
        if group != last {
            if !rows.is_empty() {
                rows.push(OptionRow::Blank);
            }
            if !group.is_empty() {
                rows.push(OptionRow::Heading(group));
            }
            last = group;
        }
        rows.push(OptionRow::Item(i));
    }
    let modal_width = area.width.saturating_sub(6).min(78);
    let height = (rows.len() as u16 + 10).min(area.height.saturating_sub(2));
    let modal = Rect::new(
        area.x + (area.width - modal_width) / 2,
        area.y + (area.height - height) / 2,
        modal_width,
        height,
    );
    ui.option_panel = modal;
    f.render_widget(Clear, modal);
    let opening = since(menu.opened, &theme, 0.35);
    let title = neon::decode(
        &format!("{} Options", theme.icon("options")),
        opening,
        0x0907,
        theme.ascii,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border())
        .border_style(theme.fg(Tone::Accent))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(title, theme.fg(Tone::Accent).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]))
        .style(if theme.color {
            Style::default()
                .bg(theme.rgb(theme::RAISED))
                .fg(theme.rgb(theme::TEXT))
        } else {
            Style::default()
        });
    let inner = block.inner(modal);
    f.render_widget(block, modal);
    neon::border(f.buffer_mut(), modal, &theme, opening, 0x0907);
    neon::edge_glitch(
        f.buffer_mut(),
        modal,
        &theme,
        since(menu.moved, &theme, 0.32),
        0x0907,
    );
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if inner.height < 4 || inner.width < 30 {
        return;
    }
    f.render_widget(
        Paragraph::new(Span::styled(
            fit(
                "Changes apply at once and are remembered with the view.",
                inner.width as usize,
                &theme,
            ),
            theme.fg(Tone::Muted),
        )),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let help_height = if inner.height >= 12 { 3 } else { 0 };
    let list = Rect::new(
        inner.x,
        inner.y + 2,
        inner.width,
        inner.height.saturating_sub(3 + help_height),
    );
    let at = rows
        .iter()
        .position(|r| matches!(r, OptionRow::Item(i) if *i == selected))
        .unwrap_or(0);
    let start = (at + 1).saturating_sub(list.height as usize);
    let w = list.width as usize;
    let label_w = Item::ALL
        .iter()
        .map(|i| width(i.label()))
        .max()
        .unwrap_or(0)
        .min(w / 2);
    for (n, row) in rows
        .iter()
        .skip(start)
        .take(list.height as usize)
        .enumerate()
    {
        let y = list.y + n as u16;
        let rect = Rect::new(list.x, y, list.width, 1);
        let i = match row {
            OptionRow::Blank => continue,
            OptionRow::Heading(name) => {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        format!(" {}", name.to_uppercase()),
                        theme.fg(Tone::Faint).add_modifier(Modifier::BOLD),
                    )),
                    rect,
                );
                continue;
            }
            OptionRow::Item(i) => *i,
        };
        let item = Item::ALL[i];
        let active = i == selected;
        let base = if active {
            theme.selected(1.0)
        } else if hovered(ui, rect) {
            theme.selected(0.4)
        } else {
            Style::default()
        };
        let key = item.key();
        let keycap = if key.is_empty() {
            String::new()
        } else {
            format!(" {key} ")
        };
        let on = options::is_on(ui, item);
        let mark = match on {
            Some(true) => format!("{} ", theme.glyph("━●", "-*")),
            Some(false) => format!("{} ", theme.glyph("○─", "o-")),
            None => String::new(),
        };
        let (open, close) = if active {
            (theme.glyph("‹ ", "< "), theme.glyph(" ›", " >"))
        } else {
            ("  ", "  ")
        };
        let value_room = w.saturating_sub(label_w + 6 + width(&keycap) + 1 + width(&mark) + 4);
        let value = fit(&options::value(ui, item), value_room, &theme);
        let label = fit(item.label(), label_w, &theme);
        let pad = w.saturating_sub(
            1 + 3
                + label_w
                + 2
                + width(open)
                + width(&mark)
                + width(&value)
                + width(close)
                + width(&keycap)
                + 1,
        );
        let tone = match on {
            Some(true) => Tone::Ok,
            Some(false) => Tone::Faint,
            None => Tone::Info,
        };
        // The new choice decodes briefly as the selection lands on it.
        let label = if active {
            neon::decode(
                &label,
                since(menu.moved, &theme, 0.16),
                i as u64,
                theme.ascii,
            )
        } else {
            label
        };
        let label = format!(
            "{label}{}",
            " ".repeat(label_w.saturating_sub(width(&label)))
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    if active { theme.glyph("▌", ">") } else { " " },
                    base.patch(theme.fg(Tone::Accent)),
                ),
                Span::styled(
                    format!(" {} ", theme.icon(item.icon())),
                    base.patch(theme.fg(if active { Tone::Accent } else { Tone::Muted })),
                ),
                Span::styled(
                    label,
                    base.patch(theme.fg(if active { Tone::Text } else { Tone::Muted }))
                        .add_modifier(if active {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ),
                Span::styled("  ", base),
                Span::styled(open, base.patch(theme.fg(Tone::Accent))),
                Span::styled(mark, base.patch(theme.fg(tone))),
                Span::styled(value, base.patch(theme.fg(tone))),
                Span::styled(close, base.patch(theme.fg(Tone::Accent))),
                Span::styled(" ".repeat(pad), base),
                Span::styled(keycap, theme.keycap()),
                Span::styled(" ", base),
            ])),
            rect,
        );
        if active {
            neon::sweep(
                f.buffer_mut(),
                rect,
                &theme,
                since(menu.moved, &theme, 0.28),
                i as u64,
            );
        }
        ui.option_hits.push((rect, i));
    }
    let enter = theme.glyph("⏎", "Enter");
    let arrows = theme.glyph("↑↓", "Up/Dn");
    let change = theme.glyph("←→", "Lt/Rt");
    if help_height > 0 {
        let help = Rect::new(inner.x, inner.bottom() - 4, inner.width, 2);
        let text = wrap(Item::ALL[selected].description(), help.width as usize);
        f.render_widget(
            Paragraph::new(
                text.into_iter()
                    .take(2)
                    .map(|text| Line::from(Span::styled(text, theme.fg(Tone::Muted))))
                    .collect::<Vec<_>>(),
            ),
            help,
        );
    }
    f.render_widget(
        Paragraph::new(keys(
            &theme,
            &[
                (arrows, "Choose"),
                (&format!("{change} / {enter}"), "Change"),
                ("Esc", "Close"),
            ],
            inner.width as usize,
        )),
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
    );
}

pub(super) fn dialog(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    let Some(dialog) = ui.dialog.as_mut() else {
        return;
    };
    let confirm = dialog.options.iter().any(|o| o.0 == "yes")
        && dialog.options.iter().any(|o| o.0 == "cancel");
    let tone = if confirm { Tone::Warn } else { Tone::Accent };
    let modal_width = area.width.saturating_sub(6).min(96);
    let description = if dialog.description.trim().is_empty() {
        Vec::new()
    } else {
        wrap(
            &dialog.description,
            modal_width.saturating_sub(4).max(1) as usize,
        )
    };
    let wanted = match dialog.mode.as_str() {
        "input" => 9 + description.len() as u16,
        "external" => 10 + description.len() as u16,
        "choose" => 5 + description.len() as u16 + dialog.options.len() as u16 * 3,
        _ => 34,
    };
    let height = area.height.saturating_sub(4).min(wanted.max(8));
    let modal = Rect::new(
        area.x + (area.width - modal_width) / 2,
        area.y + (area.height - height) / 2,
        modal_width,
        height,
    );
    f.render_widget(Clear, modal);
    // Menus open with a neon edge and a title that decodes into place; a
    // confirmation keeps its plain amber edge.
    let opening = since(dialog.opened, &theme, 0.35);
    let seed = dialog.id;
    let title = neon::decode(
        &fit(
            &dialog.title,
            (modal_width as usize).saturating_sub(6),
            &theme,
        ),
        opening,
        seed,
        theme.ascii,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border())
        .border_style(theme.fg(tone))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!(
                    "{} ",
                    theme.icon(if confirm {
                        "warning"
                    } else {
                        match dialog.mode.as_str() {
                            "input" | "secret" => "edit",
                            "choose" => "options",
                            "external" => "external",
                            _ => "output",
                        }
                    })
                ),
                theme.fg(tone),
            ),
            Span::styled(title, theme.fg(tone).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]))
        .style(if theme.color {
            Style::default()
                .bg(theme.rgb(theme::RAISED))
                .fg(theme.rgb(theme::TEXT))
        } else {
            Style::default()
        });
    let inner = block.inner(modal);
    f.render_widget(block, modal);
    if !confirm {
        neon::border(f.buffer_mut(), modal, &theme, opening, seed);
    }
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    // Keep two choices visible while long review details scroll independently.
    let reserve = if dialog.mode == "choose" { 6 } else { 5 };
    let desc_height = (description.len() as u16).min(inner.height.saturating_sub(reserve));
    let description_scroll = if dialog.mode == "choose" {
        dialog.scroll = dialog.scroll.min(
            description
                .len()
                .saturating_sub(desc_height as usize)
                .min(u16::MAX as usize) as u16,
        );
        dialog.scroll as usize
    } else {
        0
    };
    let regions = split(
        inner,
        Direction::Vertical,
        vec![
            Constraint::Length(desc_height + u16::from(desc_height > 0)),
            Constraint::Min(1),
            Constraint::Length(1),
        ],
    );
    f.render_widget(
        Paragraph::new(
            description
                .iter()
                .skip(description_scroll)
                .take(desc_height as usize)
                .map(|l| result_line(l, &theme))
                .collect::<Vec<_>>(),
        ),
        regions[0],
    );
    let w = regions[1].width as usize;
    let enter = theme.glyph("⏎", "Enter");
    let arrows = theme.glyph("↑↓", "Up/Dn");
    let mut position = None;
    let hints: Vec<(&str, &str)> = match dialog.mode.as_str() {
        "choose" => {
            if dialog.options.is_empty() {
                f.render_widget(
                    Paragraph::new(Span::styled(
                        fit(
                            "No choices available. Close and refresh the list.",
                            w,
                            &theme,
                        ),
                        theme.fg(Tone::Muted),
                    )),
                    regions[1],
                );
            }
            if !dialog.options.is_empty() {
                dialog.selected = dialog.selected.min(dialog.options.len() - 1);
                position = Some(format!(
                    "{} / {}",
                    dialog.selected + 1,
                    dialog.options.len()
                ));
            }
            let per = if regions[1].height < 6 {
                1usize
            } else {
                3usize
            };
            let capacity = (regions[1].height as usize / per).max(1);
            let start = dialog.selected.saturating_sub(capacity - 1);
            let mut lines = Vec::new();
            for (i, (value, title, note)) in
                dialog.options.iter().enumerate().skip(start).take(capacity)
            {
                let y = regions[1].y + ((i - start) * per) as u16;
                ui.dialog_hits.push((
                    Rect::new(
                        regions[1].x,
                        y,
                        regions[1].width,
                        (per as u16).min(regions[1].bottom().saturating_sub(y)),
                    ),
                    i,
                ));
                let active = i == dialog.selected;
                let base = if active {
                    theme.selected(1.0)
                } else {
                    Style::default()
                };
                let title_tone = match value.as_str() {
                    "yes" => Tone::Warn,
                    "cancel" => Tone::Muted,
                    _ => Tone::Text,
                };
                let bar = Span::styled(
                    if active { theme.glyph("▌", ">") } else { " " },
                    base.patch(theme.fg(tone)),
                );
                // Registry actions carry their icon, in the action's color.
                let icon = dialog
                    .icons
                    .get(i)
                    .filter(|name| !name.is_empty())
                    .map(|name| format!("{} ", theme.icon(name)))
                    .unwrap_or_else(|| {
                        format!(
                            "{} ",
                            theme.icon(match value.as_str() {
                                "cancel" => "close",
                                "yes" => "choose",
                                "done" => "done",
                                _ => value,
                            })
                        )
                    });
                let title = fit(title, w.saturating_sub(3 + width(&icon)), &theme);
                // The new choice decodes briefly as the selection lands on it.
                let title = if active {
                    neon::decode(
                        &title,
                        since(dialog.moved, &theme, 0.16),
                        seed ^ i as u64,
                        theme.ascii,
                    )
                } else {
                    title
                };
                let pad = w.saturating_sub(width(&title) + width(&icon) + 2);
                lines.push(Line::from(vec![
                    bar.clone(),
                    Span::styled(" ", base),
                    Span::styled(icon.clone(), base.patch(theme.fg(tone))),
                    Span::styled(
                        title,
                        base.patch(theme.fg(title_tone))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" ".repeat(pad), base),
                ]));
                let indent = " ".repeat(width(&icon));
                let note = fit(note, w.saturating_sub(3 + indent.len()), &theme);
                let pad = w.saturating_sub(width(&note) + indent.len() + 2);
                if per > 1 {
                    lines.push(Line::from(vec![
                        bar,
                        Span::styled(
                            format!(" {indent}{note}"),
                            base.patch(theme.fg(Tone::Muted)),
                        ),
                        Span::styled(" ".repeat(pad), base),
                    ]));
                    lines.push(Line::from(""));
                }
            }
            f.render_widget(Paragraph::new(lines), regions[1]);
            // A neon wash under the selected choice, brightest at its bar.
            if theme.truecolor && !confirm && !dialog.options.is_empty() {
                let row = regions[1].y + dialog.selected.saturating_sub(start) as u16 * per as u16;
                let area = regions[1];
                for y in row..(row + per.min(2) as u16).min(area.bottom()) {
                    for x in area.left()..area.right() {
                        let fx = (x - area.left()) as f32 / area.width.max(1) as f32;
                        let cell = &mut f.buffer_mut()[(x, y)];
                        cell.bg = theme.mix(
                            theme::SELECT,
                            neon::gradient(fx),
                            0.22 * (1.0 - fx).powf(1.5),
                        );
                        if x == area.left() {
                            cell.fg = theme.rgb(neon::gradient(0.0));
                        }
                    }
                }
            }
            let mut hints = if dialog.options.is_empty() {
                vec![("Esc", "Close")]
            } else {
                vec![(arrows, "Choose"), (enter, "Select"), ("Esc", "Cancel")]
            };
            if description.len() > desc_height as usize {
                hints.push(("PgUp/PgDn", "Details"));
            }
            hints
        }
        "input" => {
            use unicode_segmentation::UnicodeSegmentation;
            let cursor = dialog.cursor();
            let display = |text: &str| {
                if dialog.secret {
                    theme.glyph("•", "*").repeat(text.graphemes(true).count())
                } else {
                    clean(text)
                }
            };
            let before = display(&dialog.input[..cursor]);
            let after = display(&dialog.input[cursor..]);
            let available = w.saturating_sub(3);
            let after_room = width(&after).min(available / 3);
            let shown = input_tail(&before, available.saturating_sub(after_room));
            let after = fit(&after, available.saturating_sub(width(&shown)), &theme);
            let field = if theme.color {
                Style::default().bg(theme.rgb(theme::SELECT))
            } else {
                Style::default().add_modifier(Modifier::UNDERLINED)
            };
            let pad = w.saturating_sub(width(&shown) + width(&after) + 2);
            let caption = if dialog.secret {
                if dialog.input.is_empty() {
                    if cfg!(windows) {
                        "Empty · Ctrl+V to paste, then Enter".to_owned()
                    } else {
                        "Empty · paste or type your token, then Enter".to_owned()
                    }
                } else {
                    format!(
                        "{} characters entered · hidden · Enter to continue",
                        dialog.input.chars().count()
                    )
                }
            } else {
                format!(
                    "{} Move cursor · Enter to continue",
                    theme.glyph("←→", "Left/Right")
                )
            };
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(format!(" {shown}"), field.patch(theme.fg(Tone::Text))),
                        Span::styled(theme.glyph("▏", "_"), field.patch(theme.fg(Tone::Accent))),
                        Span::styled(after, field.patch(theme.fg(Tone::Text))),
                        Span::styled(" ".repeat(pad), field),
                    ]),
                    Line::from(Span::styled(
                        fit(&format!(" {caption}"), w, &theme),
                        theme.fg(if dialog.input.is_empty() {
                            Tone::Muted
                        } else {
                            Tone::Info
                        }),
                    )),
                ]),
                regions[1],
            );
            vec![(enter, "Continue"), ("Esc", "Cancel")]
        }
        "external" => {
            let lines = [
                (
                    "Enter opens the native program in this terminal.",
                    Tone::Text,
                ),
                ("The dashboard returns when the program exits.", Tone::Muted),
            ]
            .into_iter()
            .flat_map(|(text, tone)| {
                wrap(text, w)
                    .into_iter()
                    .map(move |line| Line::from(Span::styled(line, theme.fg(tone))))
            })
            .collect::<Vec<_>>();
            f.render_widget(Paragraph::new(lines), regions[1]);
            vec![(enter, "Open"), ("Esc", "Cancel")]
        }
        _ => {
            let lines = wrap(
                if dialog.text.trim().is_empty() {
                    "No output was returned. Close this view to continue."
                } else {
                    &dialog.text
                },
                w,
            );
            let visible = regions[1].height as usize;
            let most = lines.len().saturating_sub(visible).min(u16::MAX as usize) as u16;
            dialog.scroll = dialog.scroll.min(most);
            let shown: Vec<Line> = lines
                .iter()
                .skip(dialog.scroll as usize)
                .take(visible)
                .map(|l| result_line(l, &theme))
                .collect();
            f.render_widget(Paragraph::new(shown), regions[1]);
            if most > 0 {
                position = Some(format!(
                    "{}–{} / {}",
                    dialog.scroll + 1,
                    (dialog.scroll as usize + visible).min(lines.len()),
                    lines.len()
                ));
            }
            if most > 0 {
                vec![("PgUp/PgDn", "Scroll"), (enter, "Close")]
            } else {
                vec![(enter, "Close")]
            }
        }
    };
    let position_width = position.as_ref().map_or(0, |text| width(text) + 2);
    let show_position = position_width > 0 && w >= position_width + 24;
    let hint_width = w.saturating_sub(if show_position { position_width } else { 0 });
    f.render_widget(
        Paragraph::new(keys(&theme, &hints, hint_width)),
        Rect {
            width: hint_width as u16,
            ..regions[2]
        },
    );
    if show_position && let Some(position) = position {
        f.render_widget(
            Paragraph::new(Span::styled(position, theme.fg(Tone::Muted)))
                .alignment(Alignment::Right),
            Rect::new(
                regions[2].right() - position_width as u16,
                regions[2].y,
                position_width as u16,
                1,
            ),
        );
    }
}

/// Give structured review labels a consistent accent without recoloring values.
fn result_line(line: &str, theme: &Theme) -> Line<'static> {
    if let Some((label, value)) = line.split_once(": ")
        && label.len() < 60
        && !label.contains('/')
    {
        Line::from(vec![
            Span::styled(format!("{label}: "), theme.fg(Tone::Accent)),
            Span::styled(value.to_owned(), theme.fg(Tone::Text)),
        ])
    } else {
        Line::from(Span::styled(line.to_owned(), theme.fg(Tone::Text)))
    }
}
