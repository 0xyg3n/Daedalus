use crate::backdrop::{self, Look};
use crate::embed::{self, Mode, Phase};
use crate::model::{
    COMMANDS, Field, Handle, Kind, Node, OVERVIEW, Order, Row, Sizes, Splash, Split, TileRow, Ui,
    View, WORKSPACES,
};
use crate::neon;
use crate::options::{self, Item};
use crate::theme::{self, Theme, Tone};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::time::Instant;

const FALLBACK: [(&str, &str, &str); crate::model::PAGES] = [
    ("Overview", "overview", "Work"),
    ("Workspaces", "workspaces", "Work"),
    ("Sessions", "sessions", "Work"),
    ("Telegram", "connections", "Connect"),
    ("AI tools", "tools", "Connect"),
    ("Hooks", "hooks", "Connect"),
    ("Activity", "activity", "System"),
    ("Instances", "instances", "System"),
    ("Armonia", "armonia", "System"),
    ("Help", "help", "System"),
    ("Commands", "commands", "Find"),
];

pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}
fn width(s: &str) -> usize {
    Span::raw(s).width()
}
/// Columns a tree row's name keeps before its note gives way.
const NAME_ROOM: usize = 12;
/// A tree row's note, shortened from its end, a part at a time, while the
/// name would get fewer than `NAME_ROOM` of the `free` columns.
fn yielding_note(note: &str, free: usize) -> &str {
    let mut note = note;
    while free.saturating_sub(width(note) + 3) < NAME_ROOM {
        match note.rsplit_once(" · ") {
            Some((head, _)) => note = head,
            None => break,
        }
    }
    note
}
/// Truncate to a display width, marking the cut.
fn fit(s: &str, max: usize, theme: &Theme) -> String {
    let s = clean(s).replace('\n', " ");
    if width(&s) <= max {
        return s;
    }
    let mark = theme.glyph("…", "~");
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let mut buf = [0u8; 4];
        let w = width(ch.encode_utf8(&mut buf));
        if used + w + 1 > max {
            break;
        }
        out.push(ch);
        used += w;
    }
    if max > 0 {
        out.push_str(mark);
    }
    out
}
/// Word wrap by display width; long words are split.
fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut lines = Vec::new();
    for paragraph in clean(text).split('\n') {
        // Leading spaces indent every line the paragraph wraps onto.
        let body = paragraph.trim_start_matches(' ');
        let indent = " ".repeat((paragraph.len() - body.len()).min(max / 2));
        let room = max - indent.len();
        let mut line = String::new();
        for word in body.split(' ') {
            let candidate = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if width(&candidate) <= room {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(format!("{indent}{}", std::mem::take(&mut line)));
            }
            let mut rest = word.to_owned();
            while width(&rest) > room {
                let cut: String = rest.chars().take(room).collect();
                rest = rest.chars().skip(room).collect();
                lines.push(format!("{indent}{cut}"));
            }
            line = rest;
        }
        lines.push(format!("{indent}{line}"));
    }
    lines
}
fn since(at: Option<Instant>, theme: &Theme, seconds: f32) -> f32 {
    match at {
        Some(at) if theme.animated() => (at.elapsed().as_secs_f32() / seconds).min(1.0),
        _ => 1.0,
    }
}
fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}
fn page_meta(ui: &Ui, page: usize) -> (String, String, String) {
    match ui.snapshot.sections.get(page) {
        Some(s) => (s.title.clone(), s.icon.clone(), s.category.clone()),
        None => {
            let (t, i, c) = FALLBACK.get(page).copied().unwrap_or(FALLBACK[0]);
            (t.into(), i.into(), c.into())
        }
    }
}
fn tone_of(ui: &Ui) -> Tone {
    Tone::category(&page_meta(ui, ui.page).2)
}
fn panel<'a>(theme: &Theme, title: Line<'a>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border())
        .border_style(theme.fg(Tone::Faint))
        .title(title)
        .style(theme.surface())
}
fn split(area: Rect, direction: Direction, sizes: Vec<Constraint>) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(direction)
        .constraints(sizes)
        .split(area)
}
/// Key hints in order of importance. The last two (Hide keys and Quit, or an
/// attached terminal's way out) always stay; others drop from the end.
fn keys(theme: &Theme, pairs: &[(&str, &str)], max: usize) -> Line<'static> {
    let size = |(key, label): &(&str, &str)| width(key) + width(label) + 4;
    let tail = pairs.len().saturating_sub(2);
    let reserved: usize = pairs[tail..].iter().map(size).sum();
    let (head, mut room) = if reserved <= max {
        (&pairs[..tail], max - reserved)
    } else {
        (pairs, max)
    };
    let mut shown: Vec<&(&str, &str)> = Vec::new();
    for pair in head {
        if size(pair) > room {
            break;
        }
        room -= size(pair);
        shown.push(pair);
    }
    if reserved <= max {
        shown.extend(&pairs[tail..]);
    }
    let mut spans = Vec::new();
    for (key, label) in shown {
        spans.push(Span::styled(format!(" {key} "), theme.keycap()));
        spans.push(Span::styled(format!(" {label} "), theme.fg(Tone::Muted)));
    }
    Line::from(spans)
}

const TAGLINE: &str = "Your agents. Your workspaces. One place to control them.";

/// The boot splash: the artwork lit from its moon, with the tagline and the
/// progress of the first inventory below it. Without true color it is the
/// name alone. Returns false when there is no splash to draw.
fn splash(f: &mut Frame, ui: &mut Ui) -> bool {
    let theme = ui.theme;
    let kitty = ui.kitty;
    let area = f.area();
    let Some(splash) = ui.splash.as_mut() else {
        return false;
    };
    if area.width < 44 || area.height < 14 {
        return false;
    }
    // Reduced motion shows the finished picture at once.
    let t = if theme.animated() {
        splash.started.elapsed().as_secs_f32()
    } else {
        60.0
    };
    f.render_widget(Block::default().style(theme.base()), area);
    let below = if theme.backdrop {
        let frame = Rect::new(area.x, area.y + 1, area.width, area.height - 6);
        splash.picture.splash(f.buffer_mut(), frame, t);
        // Once the light has spread, a terminal that shows pictures gets
        // the full-resolution one on top, until the fade out.
        splash.hires = kitty
            .filter(|_| t >= backdrop::REVEAL + 0.05 && splash.leaving.is_none())
            .map(|k| crate::graphics::cover(frame, k.cell));
        frame.bottom() + 1
    } else {
        splash.hires = None;
        let name = "D A E D A L U S";
        let shown = ((t / 0.06) as usize).min(name.len());
        let y = area.y + area.height / 2 - 2;
        let x = area.x + (area.width - name.len() as u16) / 2;
        f.render_widget(
            Paragraph::new(Span::styled(
                &name[..shown],
                theme.fg(Tone::Accent).add_modifier(Modifier::BOLD),
            )),
            Rect::new(x, y, name.len() as u16, 1),
        );
        y + 2
    };
    let centered = |width: usize, y: u16| {
        let width = (width as u16).min(area.width);
        Rect::new(area.x + (area.width - width) / 2, y, width, 1)
    };
    let shade = |tone: Tone, share: f32| {
        if theme.color {
            Style::default().fg(theme.mix(theme::BG, tone.rgb(), share))
        } else {
            theme.fg(tone)
        }
    };
    let words = ease(((t - 0.4) / 0.6).clamp(0.0, 1.0));
    f.render_widget(
        Paragraph::new(Span::styled(TAGLINE, shade(Tone::Muted, words))),
        centered(TAGLINE.len(), below),
    );
    // A light runs along the line while the inventory loads; it fills once ready.
    let filled = splash
        .ready
        .map(|at| ease((at.elapsed().as_secs_f32() / 0.4).min(1.0)));
    let width = area.width.saturating_sub(8).min(56);
    let head = (t * 0.8).fract() * 1.4 - 0.2;
    let line: Vec<Span> = (0..width)
        .map(|i| {
            let p = (i as f32 + 0.5) / width as f32;
            let level = match filled {
                Some(fill) if p <= fill => 1.0,
                Some(_) => 0.0,
                None if theme.animated() => (-((p - head) / 0.12).powi(2)).exp(),
                None => 0.0,
            };
            let symbol = if level > 0.3 {
                theme.glyph("━", "=")
            } else {
                theme.glyph("─", "-")
            };
            let style = if theme.color {
                Style::default().fg(theme.mix(
                    theme::mix(theme::BG, theme::EDGE, words),
                    theme::CYAN,
                    level,
                ))
            } else {
                theme.fg(Tone::Faint)
            };
            Span::styled(symbol, style)
        })
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(line)),
        centered(width as usize, below + 1),
    );
    // What is happening while the inventory loads; once it is in, the line
    // fades away as the light above fills.
    let gone = filled.unwrap_or(0.0);
    if gone < 1.0 && (theme.color || splash.ready.is_none()) {
        let text = "Discovering terminals and agents…";
        let skip = "   any key skips";
        let status = Line::from(vec![
            Span::styled(
                format!("{} {text}", theme.spinner()),
                shade(Tone::Muted, words * (1.0 - gone)),
            ),
            Span::styled(skip, shade(Tone::Faint, words * (1.0 - gone))),
        ]);
        let length = 2 + text.chars().count() + skip.len();
        f.render_widget(Paragraph::new(status), centered(length, below + 2));
    }
    // Fading out: every color sinks into the background.
    if let Some(at) = splash.leaving.filter(|_| theme.animated()) {
        let k = ease((at.elapsed().as_secs_f32() / Splash::LEAVE).min(1.0));
        for cell in f.buffer_mut().content.iter_mut() {
            for color in [&mut cell.fg, &mut cell.bg] {
                if let ratatui::style::Color::Rgb(r, g, b) = *color {
                    *color = theme.mix((r, g, b), theme::BG, k);
                }
            }
        }
    }
    if theme.ascii {
        theme::asciify(f.buffer_mut());
    }
    true
}

/// The screensaver: the artwork glitching on an empty screen, and at the
/// bottom the key that turns it off and how to return.
fn saver(f: &mut Frame, ui: &mut Ui) -> bool {
    let Some(since) = ui.saver.since else {
        return false;
    };
    let theme = ui.theme;
    let area = f.area();
    let t = since.elapsed().as_secs_f32();
    f.render_widget(Block::default().style(theme.base()), area);
    let frame = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
    // Always the block picture, so scanlines, color split and glitch bursts
    // show; the full-resolution image is for the splash only.
    ui.saver
        .picture
        .screensaver(f.buffer_mut(), frame, t, theme.animated());
    // It shows once the picture has begun to fade in, and stays.
    if (t >= 0.5 || !theme.animated()) && area.height > 1 {
        let hint = keys(
            &theme,
            &[("s", "Disable screensaver"), ("any key", "Return")],
            area.width as usize,
        );
        let width = (hint.width() as u16).min(area.width);
        f.render_widget(
            Paragraph::new(hint),
            Rect::new(
                area.x + (area.width - width) / 2,
                area.bottom() - 1,
                width,
                1,
            ),
        );
    }
    true
}

pub fn draw(f: &mut Frame, ui: &mut Ui) {
    if splash(f, ui) || saver(f, ui) {
        return;
    }
    let theme = ui.theme;
    let area = f.area();
    f.render_widget(Block::default().style(theme.base()), area);
    ui.nav_hits.clear();
    ui.list_hits.clear();
    ui.focus_tree_hits.clear();
    ui.focus_tree_area = None;
    ui.handles.clear();
    ui.pane_dividers.clear();
    ui.side_hits.clear();
    ui.option_hits.clear();
    ui.tooltip = None;
    if area.width < 44 || area.height < 14 {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "DAEDALUS",
                    theme.fg(Tone::Accent).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from("Enlarge this pane to at least 44 x 14."),
                Line::from(Span::styled("q Exit", theme.fg(Tone::Muted))),
            ]),
            area,
        );
        return;
    }
    let wide = area.width >= 100 && area.height >= 20;
    let rows = split(
        area,
        Direction::Vertical,
        vec![
            Constraint::Length(2),
            Constraint::Min(6),
            Constraint::Length(if ui.hide_keys { 1 } else { 2 }),
        ],
    );
    header(f, rows[0], ui, wide);
    ui.tile_hits.clear();
    ui.pane_hits.clear();
    ui.quick_hits.clear();
    ui.view_hits.clear();
    // Whatever draws a terminal preview this frame asks for it again.
    ui.preview_targets.clear();
    let workspaces = ui.page == WORKSPACES;
    // Preview views reserve the workspace width for terminals; sections stay one key away.
    let side = wide && !(workspaces && ui.view != View::Tree);
    let body = if side {
        // At 100 columns or more, a sidebar of at most 40 leaves the page 60.
        let folded = ui.sizes.sidebar == 0;
        let width = if folded {
            Sizes::RAIL
        } else {
            ui.sizes.sidebar
        };
        let columns = split(
            rows[1],
            Direction::Horizontal,
            vec![Constraint::Length(width), Constraint::Min(40)],
        );
        if folded {
            rail(f, columns[0], ui);
        } else {
            sidebar(f, columns[0], ui);
        }
        ui.handles.push(Handle {
            area: Rect::new(
                columns[0].right().saturating_sub(1),
                rows[1].y,
                2,
                rows[1].height,
            ),
            split: Split::Sidebar,
            origin: rows[1].x,
            length: rows[1].width,
        });
        columns[1]
    } else {
        rows[1]
    };
    let content = Rect {
        x: body.x + u16::from(side),
        width: body.width.saturating_sub(u16::from(side) + 1),
        ..body
    };
    let mut cursor = None;
    if ui.page == OVERVIEW {
        overview(f, content, ui);
    } else if workspaces && ui.view == View::Tiles {
        tiles_panel(f, content, ui);
    } else if workspaces && ui.view == View::Wall {
        wall_panel(f, content, ui);
    } else if workspaces && ui.view == View::Focus {
        if content.width >= 80 && content.height >= 12 {
            let width = (content.width / 4).clamp(24, 38);
            let columns = split(
                content,
                Direction::Horizontal,
                vec![Constraint::Length(width), Constraint::Min(50)],
            );
            focus_structure(f, columns[0], ui);
            cursor = focus_panel(f, columns[1], ui);
        } else if content.height >= 22 {
            let parts = split(
                content,
                Direction::Vertical,
                vec![Constraint::Length(8), Constraint::Min(12)],
            );
            focus_structure(f, parts[0], ui);
            cursor = focus_panel(f, parts[1], ui);
        } else {
            cursor = focus_panel(f, content, ui);
        }
    } else {
        let inspect = content.width >= 92 && ui.prefs.details;
        let details = if inspect {
            details_width(content.width, ui.sizes.details)
        } else {
            0
        };
        let columns = split(
            content,
            Direction::Horizontal,
            vec![Constraint::Min(1), Constraint::Length(details)],
        );
        let list = ui.rows();
        main_panel(f, columns[0], ui, &list, !inspect);
        if inspect {
            ui.handles.push(Handle {
                area: Rect::new(columns[1].x.saturating_sub(1), content.y, 2, content.height),
                split: Split::Details,
                origin: content.right(),
                length: content.width,
            });
            let selected = list.get(ui.selected).filter(|r| r.kind != Kind::Header);
            inspector(f, columns[1], ui, selected);
        }
    }
    light_handle(f, ui);
    footer(f, rows[2], ui);
    // Before the page fade and any dialog, so both dim the background too.
    match ui.look {
        Look::Glow => backdrop::glow(f.buffer_mut(), area, &theme, page_hue(ui)),
        Look::Off => {}
        look => ui.backdrop.apply(f.buffer_mut(), area, &theme, look),
    }
    if ui.paged_at.is_some() {
        fade_in(f, content, ui);
        let seed = (ui.page as u64) << 4 | ui.view as u64;
        neon::bands(
            f.buffer_mut(),
            content,
            &theme,
            since(ui.paged_at, &theme, 0.18),
            seed,
        );
    }
    if let Some((at, label)) = ui.tooltip.take() {
        tooltip(f, area, &theme, at, &label);
    }
    if ui.dialog.is_some() {
        dim(f, area, &theme);
        dialog(f, area, ui);
    } else if ui.options.is_some() {
        dim(f, area, &theme);
        options_panel(f, area, ui);
    } else if let Some(position) = cursor {
        // The attached terminal's own cursor; hidden whenever a dialog is open.
        f.set_cursor_position(position);
    }
    if theme.ascii {
        theme::asciify(f.buffer_mut());
    }
}

/// Whether the mouse pointer is over `rect`.
fn hovered(ui: &Ui, rect: Rect) -> bool {
    ui.pointer
        .is_some_and(|(x, y)| rect.contains(ratatui::layout::Position::new(x, y)))
}

/// The button that folds the sidebar to its icon rail, or opens it again:
/// a key cap, named beside the pointer.
fn fold_button(f: &mut Frame, rect: Rect, ui: &mut Ui, folded: bool) {
    let theme = ui.theme;
    let glyph = if folded {
        theme.glyph("»", ">")
    } else {
        theme.glyph("«", "<")
    };
    let over = hovered(ui, rect);
    let style = if over {
        theme.selected(1.0).fg(theme.rgb(theme::CYAN))
    } else {
        theme.keycap()
    };
    f.render_widget(
        Paragraph::new(Span::styled(format!(" {glyph} "), style)),
        rect,
    );
    ui.quick_hits.push((rect, "sidebar".into()));
    if over {
        let label = if folded {
            "Open the sidebar  \\"
        } else {
            "Fold to icons  \\"
        };
        ui.tooltip = Some((Rect::new(rect.right() + 1, rect.y, 0, 1), label.into()));
    }
}

/// The section with this icon.
fn page_of(ui: &Ui, icon: &str) -> Option<usize> {
    (0..ui.page_count()).find(|&i| page_meta(ui, i).1 == icon)
}

/// A count beside a section: terminals, saved routes, checkpoints and
/// warnings, where there are any.
fn page_badge(ui: &Ui, icon: &str) -> Option<(String, Tone)> {
    let s = &ui.snapshot;
    let (count, tone) = match icon {
        "workspaces" => (s.terminals, Tone::Accent),
        "sessions" => (s.checkpoints, Tone::Rose),
        "connections" => (s.routes, Tone::Violet),
        "activity" => (s.warnings, Tone::Warn),
        _ => return None,
    };
    (count > 0).then(|| (count.to_string(), tone))
}

/// A terminal listed in the sidebar.
struct SideTerminal {
    /// Empty when the pinned terminal is gone.
    id: String,
    title: String,
    tone: Tone,
    age: String,
}

/// Pinned terminals, and the most recently active others.
fn side_terminals(ui: &Ui) -> (Vec<SideTerminal>, Vec<SideTerminal>) {
    let entry = |node: &Node| SideTerminal {
        id: node.id.clone(),
        title: node
            .tile
            .as_ref()
            .map(|t| t.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| node.label.clone()),
        tone: heat(ui, node).unwrap_or(Tone::Faint),
        age: ui.ago(node),
    };
    let find = |id: &str| {
        ui.snapshot
            .tree
            .iter()
            .find(|n| n.id == id && n.tile.is_some())
    };
    let pinned: Vec<SideTerminal> = if ui.prefs.pinned {
        ui.snapshot
            .workspace
            .pins
            .iter()
            .map(|pin| match find(&pin.id).filter(|_| !pin.id.is_empty()) {
                Some(node) => entry(node),
                None => SideTerminal {
                    id: String::new(),
                    title: pin.label.clone(),
                    tone: Tone::Faint,
                    age: pin.status.clone(),
                },
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut recent: Vec<&Node> = if ui.prefs.recent {
        ui.snapshot
            .tree
            .iter()
            .filter(|n| n.tile.is_some() && (n.active_at > 0.0 || n.live()))
            .filter(|n| !ui.snapshot.workspace.pins.iter().any(|p| p.id == n.id))
            .collect()
    } else {
        Vec::new()
    };
    recent.sort_by(|a, b| {
        (b.live(), b.active_at)
            .partial_cmp(&(a.live(), a.active_at))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    (pinned, recent.into_iter().take(5).map(entry).collect())
}

/// Pinned and recent terminals under the sections, as far as `area` allows.
/// One click selects a terminal in Workspaces; a click on the selected one
/// opens Focus.
fn side_lists(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    let (pinned, recent) = side_terminals(ui);
    let selected = if ui.page == WORKSPACES {
        ui.selected_terminal()
    } else {
        None
    };
    let mut y = area.y;
    for (heading, entries) in [("PINNED", pinned), ("RECENT", recent)] {
        if entries.is_empty() || y + 3 > area.bottom() {
            continue;
        }
        y += 1;
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {heading}"),
                theme.fg(Tone::Faint).add_modifier(Modifier::BOLD),
            )),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
        for item in entries {
            if y >= area.bottom() {
                break;
            }
            let rect = Rect::new(area.x, y, area.width, 1);
            let live = !item.id.is_empty();
            let active = live && selected.as_deref() == Some(item.id.as_str());
            let base = if active {
                theme.selected(1.0)
            } else if live && hovered(ui, rect) {
                theme.selected(0.45)
            } else {
                theme.surface()
            };
            let dot = if live {
                theme.glyph("●", "*")
            } else {
                theme.glyph("◌", "?")
            };
            let age = fit(&item.age, 9, &theme);
            let room = (area.width as usize).saturating_sub(6 + width(&age));
            let title = fit(&item.title, room, &theme);
            let pad = room.saturating_sub(width(&title)) + 1;
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        if active { theme.glyph("▌", ">") } else { " " },
                        base.patch(theme.fg(Tone::Accent)),
                    ),
                    Span::styled(format!(" {dot} "), base.patch(theme.fg(item.tone))),
                    Span::styled(
                        title,
                        base.patch(theme.fg(if !live {
                            Tone::Faint
                        } else if active {
                            Tone::Text
                        } else {
                            Tone::Muted
                        })),
                    ),
                    Span::styled(" ".repeat(pad), base),
                    Span::styled(age, base.patch(theme.fg(Tone::Faint))),
                    Span::styled(" ", base),
                ])),
                rect,
            );
            if live {
                ui.side_hits.push((rect, item.id));
            }
            y += 1;
        }
    }
}

/// The sidebar folded to the left: one icon per section, with the same keys
/// and clicks, names beside the pointer, and » to unfold it.
fn rail(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    f.render_widget(Block::default().style(theme.surface()), area);
    let edge = Rect {
        x: area.right().saturating_sub(1),
        width: 1,
        ..area
    };
    let line: Vec<Line> = (0..edge.height)
        .map(|_| Line::from(Span::styled(theme.glyph("│", "|"), theme.fg(Tone::Faint))))
        .collect();
    f.render_widget(Paragraph::new(line), edge);
    let room = area.width.saturating_sub(1);
    fold_button(f, Rect::new(area.x + 1, area.y, 3, 1), ui, true);
    let fade = ease(since(ui.paged_at, &theme, 0.18));
    let mut y = area.y + 2;
    let mut last = String::new();
    let tip = |rect: Rect, label: String| (Rect::new(rect.right() + 1, rect.y, 0, 1), label);
    for i in 0..ui.page_count() {
        let (title, icon, category) = page_meta(ui, i);
        if !last.is_empty() && category != last {
            y += 1;
        }
        last = category.clone();
        if y >= area.bottom() {
            break;
        }
        let tone = Tone::category(&category);
        let active = i == ui.page;
        let rect = Rect::new(area.x, y, room, 1);
        let over = hovered(ui, rect);
        let base = if active {
            theme.selected(fade)
        } else if over {
            theme.selected(0.45)
        } else {
            theme.surface()
        };
        // A section with something to count carries a dot in its color.
        let mark = match page_badge(ui, &icon).filter(|_| ui.prefs.badges) {
            Some((_, badge)) => Span::styled(theme.glyph("•", "."), base.patch(theme.fg(badge))),
            None => Span::styled(" ", base),
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    if active { theme.glyph("▌", ">") } else { " " },
                    base.fg(theme.rgb(tone.rgb())),
                ),
                Span::styled(
                    format!(" {}", theme.icon(&icon)),
                    if active {
                        base.fg(theme.rgb(tone.rgb()))
                    } else {
                        base.patch(theme.fg(Tone::Muted))
                    },
                ),
                mark,
            ]))
            .style(base),
            rect,
        );
        ui.nav_hits.push((rect, i));
        if over {
            let badge = page_badge(ui, &icon)
                .map(|(n, _)| format!(" · {n}"))
                .unwrap_or_default();
            ui.tooltip = Some(tip(rect, format!("{title}{badge}  {}", shortcut(i))));
        }
        y += 1;
    }
    // Options, then the pinned and recent terminals as colored dots.
    y += 1;
    if y < area.bottom() {
        let rect = Rect::new(area.x, y, room, 1);
        let over = hovered(ui, rect);
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("  {}", theme.icon("options")),
                if over {
                    theme.selected(0.45).fg(theme.rgb(theme::CYAN))
                } else {
                    theme.fg(Tone::Muted)
                },
            )),
            rect,
        );
        ui.quick_hits.push((rect, "options".into()));
        if over {
            ui.tooltip = Some(tip(rect, "Options  ,".into()));
        }
        y += 2;
    }
    let (pinned, recent) = side_terminals(ui);
    let selected = if ui.page == WORKSPACES {
        ui.selected_terminal()
    } else {
        None
    };
    for item in pinned
        .into_iter()
        .chain(recent)
        .filter(|t| !t.id.is_empty())
    {
        if y >= area.bottom() {
            break;
        }
        let rect = Rect::new(area.x, y, room, 1);
        let over = hovered(ui, rect);
        let active = selected.as_deref() == Some(item.id.as_str());
        let base = if active {
            theme.selected(1.0)
        } else if over {
            theme.selected(0.45)
        } else {
            theme.surface()
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("  {}", theme.glyph("●", "*")),
                base.patch(theme.fg(item.tone)),
            ))
            .style(base),
            rect,
        );
        if over {
            let age = if item.age.is_empty() {
                String::new()
            } else {
                format!(" · {}", item.age)
            };
            ui.tooltip = Some(tip(rect, format!("{}{age}", item.title)));
        }
        ui.side_hits.push((rect, item.id));
        y += 1;
    }
}

/// Width of the Details panel: its share of the page, leaving the list at
/// least 32 columns and itself at least 30.
pub fn details_width(total: u16, percent: u16) -> u16 {
    let wanted = (u32::from(total) * u32::from(percent) + 50) / 100;
    (wanted as u16).clamp(30, total.saturating_sub(32).max(30))
}

/// The border being dragged, or under the pointer, is drawn in the accent.
fn light_handle(f: &mut Frame, ui: &Ui) {
    let active = ui.dragging.map(|h| h.split).or(ui.hovered);
    let Some(handle) = ui.handles.iter().find(|h| Some(h.split) == active) else {
        return;
    };
    let accent = ui.theme.fg(Tone::Accent).fg.unwrap_or(Color::Reset);
    let area = handle.area.intersection(f.area());
    let buffer = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buffer[(x, y)];
            if cell.symbol() != " " {
                cell.fg = accent;
            }
        }
    }
}

/// Rows and columns of the Focus terminal area in a screen of this size:
/// header and footer (2 + 2), panel border (2), mode and escape lines (1 + 1).
pub fn focus_size(cols: u16, rows: u16) -> (u16, u16) {
    (rows.saturating_sub(8).max(2), cols.saturating_sub(3).max(2))
}

/// Tree · Tiles · Wall · Focus on a panel's top border; each label is clickable.
fn switcher(f: &mut Frame, x: u16, y: u16, max: u16, ui: &mut Ui) -> u16 {
    let theme = ui.theme;
    let mut at = x;
    for view in View::ALL {
        let icon = theme.icon(view.name());
        let text = format!(" {icon} {} ", view.title());
        let w = width(&text) as u16;
        if at + w > x + max {
            break;
        }
        let style = if view == ui.view {
            theme
                .selected(1.0)
                .fg(theme.rgb(Tone::Accent.rgb()))
                .add_modifier(Modifier::BOLD)
        } else {
            theme.fg(Tone::Muted)
        };
        let rect = Rect::new(at, y, w, 1);
        f.render_widget(Paragraph::new(Span::styled(text, style)), rect);
        ui.view_hits.push((rect, view));
        at += w + 1;
    }
    at.saturating_sub(x)
}

/// One-key actions for the selected terminal, with click targets.
fn quick_bar(f: &mut Frame, area: Rect, ui: &mut Ui, tail: &[(&str, &str, &str)]) {
    let theme = ui.theme;
    if ui.selected_terminal().is_none() {
        f.render_widget(
            Paragraph::new(Span::styled(
                " Select a terminal for quick actions",
                theme.fg(Tone::Faint),
            )),
            area,
        );
        return;
    }
    let mut items: Vec<(String, String, String)> = ui
        .snapshot
        .terminal_actions
        .iter()
        .filter(|a| !a.quick.is_empty())
        .map(|a| (a.quick.clone(), a.short.clone(), a.id.clone()))
        .collect();
    items.extend(
        tail.iter()
            .map(|(k, l, id)| (k.to_string(), l.to_string(), id.to_string())),
    );
    let mut x = area.x + 1;
    for (key, label, id) in items {
        let cap = format!(" {key} ");
        let text = format!(" {label} ");
        let w = (width(&cap) + width(&text)) as u16;
        if x + w > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y, w, 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(cap, theme.keycap()),
                Span::styled(text, theme.fg(Tone::Muted)),
            ])),
            rect,
        );
        ui.quick_hits.push((rect, id));
        x += w + 1;
    }
}

const TILE_WIDTH: u16 = 44;
const TILE_HEIGHT: u16 = 10;

/// Grid of live terminal previews; only on-screen tiles are ever read.
fn tiles_panel(f: &mut Frame, area: Rect, ui: &mut Ui) {
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
    search(
        f,
        Rect::new(inner.x, inner.y, inner.width, 1),
        ui,
        "Terminals",
    );
    let grid = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(2),
    );
    quick_bar(
        f,
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        ui,
        &[("m", "More", "more")],
    );
    // + and - scale the smallest tile; fewer, larger tiles fit.
    let scale = |cells: u16| (u32::from(cells) * u32::from(ui.sizes.tiles) / 100).max(1) as u16;
    let fit_columns = (grid.width / scale(TILE_WIDTH)).clamp(1, 6) as usize;
    let fit_rows = (grid.height / scale(TILE_HEIGHT)).max(1) as usize;
    let per_page = fit_columns * fit_rows;
    let count = tiles.len();
    let pages = count.div_ceil(per_page).max(1);
    ui.tile = ui.tile.min(count.saturating_sub(1));
    let page = ui.tile / per_page;
    let shown = &tiles[(page * per_page).min(count)..((page + 1) * per_page).min(count)];
    // Few terminals get larger tiles; navigation follows the drawn grid.
    let (columns, used_rows) = grid_shape(grid, shown.len(), fit_columns, fit_rows);
    ui.tile_columns = columns;
    ui.tiles_per_page = per_page;
    let mut summary = format!(" {count} terminals · large previews");
    if pages > 1 {
        summary += &format!(" · page {}/{pages}", page + 1);
    }
    grid_summary(f, area, ui, summary);
    if shown.is_empty() {
        no_tiles(f, grid, ui);
        return;
    }
    let tile_w = grid.width / columns as u16;
    let tile_h = grid.height / used_rows as u16;
    let mut most = 0;
    for (n, row) in shown.iter().enumerate() {
        let index = page * per_page + n;
        let (c, r) = ((n % columns) as u16, (n / columns) as u16);
        let last = c as usize + 1 == columns;
        let rect = Rect::new(
            grid.x + c * tile_w,
            grid.y + r * tile_h,
            if last {
                grid.right() - (grid.x + c * tile_w)
            } else {
                tile_w.saturating_sub(1)
            },
            tile_h,
        );
        most = most.max(draw_tile(f, rect, ui, row, index == ui.tile));
        ui.tile_hits.push((rect, index));
        if !row.id.is_empty() {
            ui.preview_targets.push(row.id.clone());
        }
    }
    ui.preview_rows = most.max(1);
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
const WALL_CELL: (u16, u16) = (18, 4);
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

fn wall_panel(f: &mut Frame, area: Rect, ui: &mut Ui) {
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
    search(
        f,
        Rect::new(inner.x, inner.y, inner.width, 1),
        ui,
        "Terminals",
    );
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
    // Wall keeps a compact overview and a large selected screen side by side.
    // Tiles instead spends its entire area on paged, readable previews.
    let (grid, inspect) = if full_grid.width >= 110 && count > 1 {
        let width = (full_grid.width / 3).clamp(36, 64);
        let parts = split(
            full_grid,
            Direction::Horizontal,
            vec![Constraint::Min(50), Constraint::Length(width)],
        );
        (parts[0], Some(parts[1]))
    } else if count > 1
        && full_grid.height >= 24
        && count
            <= usize::from((full_grid.width / WALL_CELL.0).max(1))
                * usize::from((full_grid.height / 2 / WALL_CELL.1).max(1))
    {
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
        most = most.max(wall_cell(f, rect, ui, row, n == ui.tile));
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
fn heat(ui: &Ui, node: &Node) -> Option<Tone> {
    match node.activity.as_str() {
        "working" => return Some(Tone::Ok),
        "blocked" => return Some(Tone::Warn),
        _ => {}
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
    let tone = node_tone(&tile.state, "pane");
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
fn grid_shape(grid: Rect, count: usize, max_columns: usize, max_rows: usize) -> (usize, usize) {
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
fn draw_tile(f: &mut Frame, rect: Rect, ui: &Ui, row: &TileRow, selected: bool) -> u16 {
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
    let tone = node_tone(&tile.state, "pane");
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
    let state = fit(&tile.state, 16, &theme);
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
    // The preview reads as a small screen inset in the tile.
    preview_screen(f, screen, ui, &node.id);
    screen.height
}

/// The hue of the current page's category, for its panel borders.
fn page_hue(ui: &Ui) -> theme::Rgb {
    Tone::category(&page_meta(ui, ui.page).2).rgb()
}

/// Borders lit from above in `hue`: brightest at the top-left corner, fading
/// down the sides to the plain edge color at the bottom. Only border glyphs
/// change, so titles, tabs and scrollbar thumbs keep their own colors.
fn glow(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    theme: &Theme,
    hue: theme::Rgb,
    strength: f32,
) {
    if !theme.color || area.width < 2 || area.height < 2 {
        return;
    }
    let (right, bottom) = (area.right() - 1, area.bottom() - 1);
    for y in area.top()..=bottom {
        for x in area.left()..=right {
            if !(y == area.top() || y == bottom || x == area.left() || x == right) {
                continue;
            }
            let cell = &mut buf[(x, y)];
            if !matches!(
                cell.symbol(),
                "─" | "│" | "╭" | "╮" | "╰" | "╯" | "+" | "-" | "|"
            ) {
                continue;
            }
            let fx = (x - area.left()) as f32 / (right - area.left()) as f32;
            let fy = (y - area.top()) as f32 / (bottom - area.top()) as f32;
            let level = if y == area.top() {
                1.0 - 0.55 * fx
            } else {
                (1.0 - fy).powf(1.8) * if x == area.left() { 1.0 } else { 0.6 }
            };
            cell.fg = theme.mix(theme::FAINT, hue, strength * level);
        }
    }
}

/// A terminal's last read screen, drawn as a dark inset; read-only.
fn preview_screen(f: &mut Frame, screen: Rect, ui: &Ui, id: &str) {
    let theme = ui.theme;
    f.render_widget(Block::default().style(theme.screen()), screen);
    match ui.previews.get(id) {
        Some(preview) if preview.error.is_empty() => {
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

/// A persistent outline beside Focus; it never captures the terminal's keys.
fn focus_structure(f: &mut Frame, area: Rect, ui: &mut Ui) {
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
    let rows: Vec<_> = ui
        .rows()
        .into_iter()
        .filter(|r| r.kind == Kind::Node)
        .filter(|r| {
            ui.snapshot
                .tree
                .get(r.source)
                .is_some_and(|n| n.kind != "agent session" && (n.children || n.tile.is_some()))
        })
        .collect();
    let visible = inner.height as usize;
    if ui.focus_tree_reveal {
        if let Some(index) = rows.iter().position(|r| r.id == ui.focus_target)
            && (index < ui.focus_tree_offset
                || index >= ui.focus_tree_offset.saturating_add(visible))
        {
            ui.focus_tree_offset = index.saturating_sub(visible.saturating_sub(1));
        }
        ui.focus_tree_reveal = false;
    }
    ui.focus_tree_offset = ui.focus_tree_offset.min(rows.len().saturating_sub(visible));
    for (line, row) in rows
        .iter()
        .skip(ui.focus_tree_offset)
        .take(visible)
        .enumerate()
    {
        let node = &ui.snapshot.tree[row.source];
        let active = ui
            .terminal_node(&ui.focus_target)
            .is_some_and(|n| n.id == row.id);
        let branch = node.tile.is_none() && row.children;
        let badge = if branch {
            if ui.collapsed.contains(&row.id) {
                theme.glyph("▸", ">")
            } else {
                theme.glyph("▾", "v")
            }
        } else if active {
            theme.glyph("●", "*")
        } else {
            theme.glyph("·", ".")
        };
        let style = if active {
            theme.selected(1.0)
        } else {
            theme.fg(if row.children {
                Tone::Muted
            } else {
                Tone::Text
            })
        };
        let harness = node.tile.as_ref().map(|t| t.harness.as_str()).unwrap_or("");
        let icon = if harness.is_empty() {
            theme.icon(&node.kind)
        } else {
            theme.icon(harness)
        };
        let prefix = format!("{}{badge} {icon} ", row.branch);
        let state = if branch {
            ""
        } else if node.live() {
            if node.state == "blocked" || node.activity == "blocked" {
                " !"
            } else {
                " +"
            }
        } else {
            ""
        };
        let room = (inner.width as usize).saturating_sub(width(&prefix) + width(state));
        let label = fit(&clean(&node.label), room, &theme);
        let pad =
            (inner.width as usize).saturating_sub(width(&prefix) + width(&label) + width(state));
        let hit = Rect::new(inner.x, inner.y + line as u16, inner.width, 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(label, style),
                Span::styled(" ".repeat(pad), style),
                Span::styled(
                    state,
                    if active {
                        style
                    } else {
                        theme.fg(heat(ui, node).unwrap_or(Tone::Muted))
                    },
                ),
            ]))
            .style(style),
            hit,
        );
        ui.focus_tree_hits.push((hit, row.id.clone(), branch));
    }
}

/// The focused terminal: a live embedded view, or how to open one.
fn focus_panel(f: &mut Frame, area: Rect, ui: &mut Ui) -> Option<(u16, u16)> {
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
    let screen = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 2);
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
fn focus_screen(f: &mut Frame, screen: Rect, ui: &mut Ui, known: bool) -> Option<(u16, u16)> {
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
                "Enter watches it again; a attaches; Esc returns.",
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

/// Page transition: new content rises from the background over a moment.
fn fade_in(f: &mut Frame, area: Rect, ui: &Ui) {
    let theme = ui.theme;
    let t = ease(since(ui.paged_at, &theme, 0.22));
    if t >= 1.0 || !theme.truecolor {
        return;
    }
    let buffer = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y))
                && let ratatui::style::Color::Rgb(r, g, b) = cell.fg
            {
                cell.set_fg(theme.mix(theme::SURFACE, (r, g, b), t));
            }
        }
    }
}

/// Backdrop behind a dialog, so the dialog is the only focus.
fn dim(f: &mut Frame, area: Rect, theme: &Theme) {
    let buffer = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let Some(cell) = buffer.cell_mut((x, y)) else {
                continue;
            };
            if !theme.color {
                cell.modifier.insert(Modifier::DIM);
                continue;
            }
            let shade = |c, amount| match c {
                ratatui::style::Color::Rgb(r, g, b) => theme.mix((r, g, b), theme::BG, amount),
                _ => theme.rgb(theme::FAINT),
            };
            cell.fg = shade(cell.fg, 0.6);
            cell.bg = match cell.bg {
                ratatui::style::Color::Rgb(..) => shade(cell.bg, 0.5),
                other => other,
            };
        }
    }
}

fn header(f: &mut Frame, area: Rect, ui: &mut Ui, wide: bool) {
    let theme = ui.theme;
    let (title, icon, category) = page_meta(ui, ui.page);
    let tone = Tone::category(&category);
    // The wordmark in neon; now and then one letter glitches for a moment.
    let mut brand = neon::spans(&theme, "DAEDALUS", theme.clock() * 0.03, true);
    if let Some(seed) = neon::burst(&theme, 9.0, 0.14) {
        let i = (seed % brand.len() as u64) as usize;
        brand[i] = Span::styled(
            neon::decode(&brand[i].content, 0.0, seed, theme.ascii),
            theme.fg(Tone::Rose).add_modifier(Modifier::BOLD),
        );
    }
    let kept = brand.len() + 1;
    let mut left = vec![Span::styled(
        format!(" {} ", theme.glyph("◆", "*")),
        theme.fg(Tone::Accent).add_modifier(Modifier::BOLD),
    )];
    left.extend(brand);
    left.extend([
        Span::styled(
            if wide {
                format!(" v{}", env!("CARGO_PKG_VERSION"))
            } else {
                String::new()
            },
            theme.fg(Tone::Faint),
        ),
        // Named only when it is not the default, so the brand is not repeated.
        Span::styled(
            match ui.snapshot.instance.as_str() {
                name if name.is_empty() || name.eq_ignore_ascii_case("daedalus") => String::new(),
                name => format!(" {} {}", theme.glyph("·", "-"), fit(name, 24, &theme)),
            },
            theme.fg(Tone::Muted),
        ),
        Span::styled(
            format!("   {} {} ", category, theme.glyph("›", ">")),
            theme.fg(Tone::Faint),
        ),
        Span::styled(format!("{} ", theme.icon(&icon)), theme.fg(tone)),
        Span::styled(title, theme.fg(tone).add_modifier(Modifier::BOLD)),
    ]);
    if ui.page == WORKSPACES {
        left.push(Span::styled(
            format!(" {} {}", theme.glyph("›", ">"), ui.view.title()),
            theme.fg(Tone::Muted),
        ));
    }
    let state = ui
        .snapshot
        .freshness
        .split(" · ")
        .next()
        .unwrap_or("")
        .to_owned();
    let (chip, chip_tone) = if ui.busy && ui.dialog.is_none() {
        (format!("{} UPDATING", theme.spinner()), Tone::Info)
    } else {
        match state.as_str() {
            "DEMO" => (format!("{} DEMO", theme.glyph("◇", "o")), Tone::Violet),
            "STALE" => (format!("{} STALE", theme.glyph("!", "!")), Tone::Danger),
            "PARTIAL" => (format!("{} PARTIAL", theme.glyph("●", "*")), Tone::Warn),
            "CHECKING" | "" => (format!("{} CHECKING", theme.spinner()), Tone::Info),
            _ => (format!("{} LIVE", theme.glyph("●", "*")), Tone::Ok),
        }
    };
    let age = if ui.snapshot.observed_at > 0.0 {
        let seconds = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            - ui.snapshot.observed_at)
            .max(0.0) as u64;
        format!("checked {seconds}s ago")
    } else {
        "awaiting first snapshot".into()
    };
    let chip_style = if chip_tone == Tone::Ok && theme.animated() {
        // The live dot breathes slowly; it does not imply connection health.
        let level = 0.55 + 0.45 * theme.breathe(2.4);
        Style::default().fg(theme.mix(theme::FAINT, theme::GREEN, level))
    } else {
        theme.fg(chip_tone)
    };
    let mut right = vec![
        Span::styled(chip, chip_style.add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {age}"), theme.fg(Tone::Muted)),
        Span::raw(" "),
    ];
    let left_width: usize = left.iter().map(|s| s.width()).sum();
    while right.len() > 1
        && left_width + right.iter().map(|s| s.width()).sum::<usize>() + 2 > area.width as usize
    {
        right.remove(right.len() - 1);
    }
    if left_width + right.iter().map(|s| s.width()).sum::<usize>() + 2 > area.width as usize {
        left.truncate(kept);
    }
    let top = Rect { height: 1, ..area };
    f.render_widget(Paragraph::new(Line::from(left)), top);
    f.render_widget(
        Paragraph::new(Line::from(right)).alignment(Alignment::Right),
        top,
    );
    let second = Rect {
        y: area.y + 1,
        height: 1,
        ..area
    };
    if wide {
        rule(f, second, ui);
    } else {
        tabs(f, second, ui);
    }
}

/// A hairline under the header. While discovery runs, a light sweeps across it.
fn rule(f: &mut Frame, area: Rect, ui: &Ui) {
    let theme = ui.theme;
    let total = area.width as f32;
    let head = ((theme.clock() * 0.7).fract()) * (total + 24.0) - 12.0;
    let spans: Vec<Span> = (0..area.width)
        .map(|x| {
            let x = x as f32;
            let lead = (1.0 - x / 18.0).max(0.0);
            let sweep = if ui.busy && theme.animated() {
                (1.0 - (x - head).abs() / 12.0).max(0.0)
            } else {
                0.0
            };
            let base = theme::mix(theme::EDGE, theme::CYAN, lead);
            let color = theme::mix(base, theme::CYAN, sweep);
            let symbol = if lead > 0.0 || sweep > 0.3 {
                theme.glyph("━", "=")
            } else {
                theme.glyph("─", "-")
            };
            if theme.color {
                Span::styled(symbol, Style::default().fg(theme.rgb(color)))
            } else {
                Span::styled(symbol, theme.fg(Tone::Faint))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Narrow navigation: numbered icons, with the current section spelled out.
fn tabs(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    let count = ui.page_count();
    let full: usize = (0..count)
        .map(|i| {
            let (t, ..) = page_meta(ui, i);
            if i == ui.page { width(&t) + 6 } else { 4 }
        })
        .sum();
    let digits = full <= area.width as usize;
    let mut x = area.x;
    for i in 0..count {
        let (title, icon, category) = page_meta(ui, i);
        let text = match (i == ui.page, digits) {
            (true, true) => format!(" {} {} {} ", i + 1, theme.icon(&icon), title),
            (true, false) => format!(" {} {} ", theme.icon(&icon), title),
            (false, true) => format!(" {}{} ", i + 1, theme.icon(&icon)),
            (false, false) => format!("{} ", theme.icon(&icon)),
        };
        let w = width(&text) as u16;
        if x + w > area.right() {
            break;
        }
        let style = if i == ui.page {
            theme
                .selected(1.0)
                .fg(theme.rgb(Tone::category(&category).rgb()))
        } else {
            theme.fg(Tone::Muted)
        };
        let rect = Rect::new(x, area.y, w, 1);
        f.render_widget(Paragraph::new(Span::styled(text, style)), rect);
        ui.nav_hits.push((rect, i));
        x += w;
    }
}

fn sidebar(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    f.render_widget(Block::default().style(theme.surface()), area);
    let edge = Rect {
        x: area.right().saturating_sub(1),
        width: 1,
        ..area
    };
    let line: Vec<Line> = (0..edge.height)
        .map(|_| Line::from(Span::styled(theme.glyph("│", "|"), theme.fg(Tone::Faint))))
        .collect();
    f.render_widget(Paragraph::new(line), edge);
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(3),
        height: area.height.saturating_sub(1),
    };
    // Folds the sidebar to its icon rail.
    fold_button(
        f,
        Rect::new(area.right().saturating_sub(5), area.y, 3, 1),
        ui,
        false,
    );
    let fade = ease(since(ui.paged_at, &theme, 0.18));
    let mut y = inner.y;
    let mut last = String::new();
    for i in 0..ui.page_count() {
        let (title, icon, category) = page_meta(ui, i);
        let tone = Tone::category(&category);
        if category != last {
            if !last.is_empty() {
                y += 1;
            }
            if y >= inner.bottom() {
                break;
            }
            let label = if category == "Find" {
                theme.glyph("─", "-").repeat(inner.width as usize)
            } else {
                category.to_uppercase()
            };
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {label}"),
                    theme.fg(Tone::Faint).add_modifier(Modifier::BOLD),
                )),
                Rect::new(inner.x, y, inner.width, 1),
            );
            y += 1;
            last = category.clone();
        }
        if y >= inner.bottom() {
            break;
        }
        let active = i == ui.page;
        let rect = Rect::new(inner.x, y, inner.width, 1);
        let shortcut = shortcut(i);
        let badge = page_badge(ui, &icon).filter(|_| ui.prefs.badges);
        let badge_text = badge
            .as_ref()
            .map(|(n, _)| format!(" {n}"))
            .unwrap_or_default();
        // The bar, icon, at least one space, the key and a margin.
        let label = fit(
            &title,
            (inner.width as usize).saturating_sub(6 + width(&badge_text) + width(&shortcut)),
            &theme,
        );
        let pad = (inner.width as usize)
            .saturating_sub(5 + width(&label) + width(&badge_text) + width(&shortcut))
            .max(1);
        let base = if active {
            theme.selected(fade)
        } else if hovered(ui, rect) {
            theme.selected(0.45)
        } else {
            theme.surface()
        };
        let line = Line::from(vec![
            Span::styled(
                if active { theme.glyph("▌", ">") } else { " " },
                base.fg(theme.rgb(tone.rgb())),
            ),
            Span::styled(
                format!(" {} ", theme.icon(&icon)),
                if active {
                    base.fg(theme.rgb(tone.rgb()))
                } else {
                    base.patch(theme.fg(Tone::Muted))
                },
            ),
            Span::styled(
                label,
                if active {
                    base
                } else {
                    base.patch(theme.fg(Tone::Muted))
                },
            ),
            Span::styled(
                badge_text,
                base.patch(theme.fg(badge.map_or(Tone::Faint, |(_, t)| t)))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ".repeat(pad), base),
            Span::styled(shortcut, base.patch(theme.fg(Tone::Faint))),
            Span::styled(" ", base),
        ]);
        f.render_widget(Paragraph::new(line), rect);
        ui.nav_hits.push((rect, i));
        y += 1;
    }
    // Options, beside the sections it changes.
    if y < inner.bottom() {
        let rect = Rect::new(inner.x, y, inner.width, 1);
        let base = if hovered(ui, rect) {
            theme.selected(0.45)
        } else {
            theme.surface()
        };
        let label = "Options";
        let pad = (inner.width as usize).saturating_sub(5 + width(label) + 1 + 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("  {} ", theme.icon("options")),
                    base.patch(theme.fg(Tone::Muted)),
                ),
                Span::styled(label, base.patch(theme.fg(Tone::Muted))),
                Span::styled(" ".repeat(pad), base),
                Span::styled(",", base.patch(theme.fg(Tone::Faint))),
                Span::styled(" ", base),
            ])),
            rect,
        );
        ui.quick_hits.push((rect, "options".into()));
        y += 1;
    }
    // Totals at the bottom; pinned and recent terminals in between.
    let s = &ui.snapshot;
    let vitals = [
        (
            "workspaces",
            s.terminals,
            "terminals",
            Tone::Accent,
            Some(WORKSPACES),
        ),
        ("linked", s.views, "views", Tone::Info, Some(WORKSPACES)),
        (
            "route",
            s.routes,
            "saved routes",
            Tone::Violet,
            page_of(ui, "connections"),
        ),
        (
            "checkpoint",
            s.checkpoints,
            "checkpoints",
            Tone::Rose,
            page_of(ui, "sessions"),
        ),
        (
            "warning",
            s.warnings,
            "warnings",
            if s.warnings > 0 {
                Tone::Warn
            } else {
                Tone::Faint
            },
            page_of(ui, "activity"),
        ),
    ];
    let needed = vitals.len() as u16 + 2;
    let totals = ui.prefs.vitals && inner.bottom() > y + needed;
    let lists_end = if totals {
        inner.bottom() - needed
    } else {
        inner.bottom()
    };
    side_lists(
        f,
        Rect::new(inner.x, y, inner.width, lists_end.saturating_sub(y)),
        ui,
    );
    if !totals {
        return;
    }
    let mut row = inner.bottom() - needed;
    f.render_widget(
        Paragraph::new(Span::styled(
            format!(
                " {}",
                theme
                    .glyph("─", "-")
                    .repeat((inner.width as usize).saturating_sub(1))
            ),
            theme.fg(Tone::Faint),
        )),
        Rect::new(inner.x, row, inner.width, 1),
    );
    row += 1;
    // Each total opens the section it counts.
    for (icon, value, label, tone, page) in vitals {
        let rect = Rect::new(inner.x, row, inner.width, 1);
        let base = if hovered(ui, rect) {
            theme.selected(0.45)
        } else {
            theme.surface()
        };
        let line = Line::from(vec![
            Span::styled(
                format!(" {} ", theme.icon(icon)),
                base.patch(theme.fg(tone)),
            ),
            Span::styled(
                format!("{value:>3} "),
                base.patch(theme.fg(Tone::Text))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(label, base.patch(theme.fg(Tone::Muted))),
        ]);
        f.render_widget(Paragraph::new(line).style(base), rect);
        if let Some(page) = page {
            ui.nav_hits.push((rect, page));
        }
        row += 1;
    }
}

fn main_panel(f: &mut Frame, area: Rect, ui: &mut Ui, rows: &[Row], peek: bool) {
    let theme = ui.theme;
    let (title, icon, _) = page_meta(ui, ui.page);
    let tone = tone_of(ui);
    let count = rows.iter().filter(|r| r.kind != Kind::Header).count();
    let noun = match ui.page {
        WORKSPACES => "nodes & actions",
        COMMANDS => "commands",
        _ => "items",
    };
    let workspaces = ui.page == WORKSPACES;
    let heading = if workspaces {
        // The view switcher takes the title's place.
        Line::from("")
    } else {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(theme.icon(&icon), theme.fg(tone)),
            Span::raw(" "),
            Span::styled(title.clone(), theme.fg(tone).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ])
    };
    let block = panel(&theme, heading).title_top(
        Line::from(Span::styled(
            format!(" {count} {noun} "),
            theme.fg(Tone::Faint),
        ))
        .right_aligned(),
    );
    let inner = block.inner(area);
    f.render_widget(block, area);
    glow(f.buffer_mut(), area, &theme, page_hue(ui), 0.8);
    if workspaces {
        let room = area
            .width
            .saturating_sub(width(&format!(" {count} {noun} ")) as u16 + 6);
        switcher(f, area.x + 2, area.y, room, ui);
    }
    if inner.height < 3 || inner.width < 10 {
        return;
    }
    let inner = if workspaces && inner.height >= 8 {
        quick_bar(
            f,
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            ui,
            &[("m", "More", "more")],
        );
        Rect {
            height: inner.height - 1,
            ..inner
        }
    } else {
        inner
    };
    let mut y = inner.y;
    if inner.height >= 12 {
        let summary = ui.section().map(|s| s.summary.clone()).unwrap_or_default();
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", fit(&summary, inner.width as usize - 1, &theme)),
                theme.fg(Tone::Muted),
            )),
            Rect::new(inner.x, y, inner.width, 1),
        );
        y += 1;
    }
    search(f, Rect::new(inner.x, y, inner.width, 1), ui, &title);
    y += if inner.height >= 12 { 2 } else { 1 };
    let peek_height = if peek && inner.height >= 16 { 3 } else { 0 };
    let list = Rect::new(
        inner.x,
        y,
        inner.width,
        inner.bottom().saturating_sub(y + peek_height),
    );
    let roomy = rows.len() * 2 <= list.height as usize + 4;
    render_list(f, list, ui, rows, ui.page != COMMANDS && roomy);
    if peek_height > 0 {
        let selected = rows.get(ui.selected).filter(|r| r.kind != Kind::Header);
        let text = selected.map(|r| peek_text(ui, r)).unwrap_or_default();
        let area = Rect::new(inner.x, list.bottom(), inner.width, peek_height);
        let mut lines = vec![Line::from(Span::styled(
            theme.glyph("┄", "-").repeat(inner.width as usize),
            theme.fg(Tone::Faint),
        ))];
        for line in wrap(&text, inner.width as usize - 2).into_iter().take(2) {
            lines.push(Line::from(Span::styled(
                format!(" {line}"),
                theme.fg(Tone::Muted),
            )));
        }
        f.render_widget(Paragraph::new(lines), area);
    }
}

fn peek_text(ui: &Ui, row: &Row) -> String {
    match row.kind {
        Kind::Node => ui
            .snapshot
            .tree
            .get(row.source)
            .map(|n| {
                n.fields
                    .iter()
                    .take(3)
                    .map(|f| format!("{}: {}", f.key, f.value))
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
            .unwrap_or_default(),
        Kind::Entry => ui
            .snapshot
            .catalog
            .get(row.source)
            .map(|e| format!("{} · {}", e.interface, e.description))
            .unwrap_or_default(),
        _ => row.note.clone(),
    }
}

fn search(f: &mut Frame, area: Rect, ui: &Ui, title: &str) {
    let theme = ui.theme;
    let field = if theme.color {
        Style::default().bg(theme.rgb(theme::RAISED))
    } else {
        Style::default()
    };
    let icon = theme.icon("search");
    let line = if ui.searching {
        Line::from(vec![
            Span::styled(
                format!(" {icon} {}", if ui.switching { "Switch: " } else { "" }),
                field.patch(theme.fg(Tone::Accent)),
            ),
            Span::styled(clean(&ui.filter), field.patch(theme.fg(Tone::Text))),
            Span::styled(theme.glyph("▏", "_"), field.patch(theme.fg(Tone::Accent))),
        ])
    } else if !ui.filter.is_empty() {
        Line::from(vec![
            Span::styled(format!(" {icon} "), field.patch(theme.fg(Tone::Accent))),
            Span::styled(
                clean(&ui.filter),
                field.patch(theme.fg(Tone::Accent).add_modifier(Modifier::BOLD)),
            ),
            Span::styled("   Esc clears", field.patch(theme.fg(Tone::Faint))),
        ])
    } else {
        Line::from(vec![
            Span::styled(format!(" {icon} "), field.patch(theme.fg(Tone::Faint))),
            Span::styled(
                format!(
                    "Search {}{}   press /",
                    title.to_lowercase(),
                    theme.glyph("…", "...")
                ),
                field.patch(theme.fg(Tone::Faint)),
            ),
        ])
    };
    f.render_widget(Paragraph::new(line).style(field), area);
}

fn node_tone(state: &str, kind: &str) -> Tone {
    let s = state.to_lowercase();
    if kind == "checkpoint" {
        return Tone::Rose;
    }
    if kind == "saved route" {
        return Tone::Violet;
    }
    if ["block", "error", "permission", "input", "wait"]
        .iter()
        .any(|w| s.contains(w))
    {
        Tone::Danger
    } else if ["work", "busy", "running", "think"]
        .iter()
        .any(|w| s.contains(w))
    {
        Tone::Warn
    } else if ["idle", "ready", "done"].iter().any(|w| s.contains(w)) {
        Tone::Ok
    } else if s == "shell" {
        Tone::Muted
    } else {
        Tone::Info
    }
}

fn node_icon(theme: &Theme, kind: &str, state: &str) -> &'static str {
    match kind {
        "server" => theme.icon("server"),
        "workspace" | "tmux session" => theme.icon("project"),
        "tab" | "window" => theme.icon("tab"),
        "agent session" => theme.icon("agent"),
        "checkpoint" => theme.icon("checkpoint"),
        "saved route" => theme.icon("route"),
        "nested view" | "declared view" => theme.icon("view"),
        _ if state == "shell" => theme.glyph("○", "o"),
        _ => theme.glyph("●", "*"),
    }
}

fn render_list(f: &mut Frame, area: Rect, ui: &mut Ui, rows: &[Row], two_line: bool) {
    let theme = ui.theme;
    if rows.is_empty() {
        let text = if ui.busy && ui.snapshot.sections.is_empty() {
            "Discovering…"
        } else if ui.filter.is_empty() {
            "Nothing here yet."
        } else {
            "No matches. Esc clears the search."
        };
        f.render_widget(
            Paragraph::new(Span::styled(format!(" {text}"), theme.fg(Tone::Muted))),
            area,
        );
        return;
    }
    let tone = tone_of(ui);
    let height = |i: usize, top: bool| -> u16 {
        match rows[i].kind {
            Kind::Header if i > 0 && !top => 2,
            Kind::Action if two_line => 2,
            _ => 1,
        }
    };
    let selected = ui.selected.min(rows.len() - 1);
    ui.offset = ui.offset.min(selected);
    if ui.offset == selected && selected > 0 && rows[selected - 1].kind == Kind::Header {
        ui.offset = selected - 1;
    }
    let used = |offset: usize| -> u16 { (offset..=selected).map(|i| height(i, i == offset)).sum() };
    while ui.offset < selected && used(ui.offset) > area.height {
        ui.offset += 1;
    }
    let total: u16 = (0..rows.len()).map(|i| height(i, i == 0)).sum();
    let scroll = total > area.height;
    let body = Rect {
        width: area.width.saturating_sub(u16::from(scroll)),
        ..area
    };
    let fade = ease(since(ui.moved_at, &theme, 0.16));
    let pulse = theme.breathe(1.6);
    let mut y = area.y;
    let title_col = rows
        .iter()
        .filter(|r| r.kind == Kind::Entry)
        .map(|r| width(&r.title))
        .max()
        .unwrap_or(0)
        .min(body.width as usize / 2)
        + 2;
    for (i, row) in rows.iter().enumerate().skip(ui.offset) {
        let h = height(i, i == ui.offset);
        if y + h > area.bottom() {
            break;
        }
        let w = body.width as usize;
        if row.kind == Kind::Header {
            let y = y + h - 1;
            let label = row.title.to_uppercase();
            let note = &row.note;
            let line_len = w.saturating_sub(width(&label) + width(note) + 5);
            let line = Line::from(vec![
                Span::styled(
                    format!(" {label} "),
                    theme.fg(tone).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    theme.glyph("─", "-").repeat(line_len),
                    theme.fg(Tone::Faint),
                ),
                Span::styled(format!(" {note} "), theme.fg(Tone::Faint)),
            ]);
            f.render_widget(Paragraph::new(line), Rect::new(body.x, y, body.width, 1));
        } else {
            let active = i == selected;
            let base = if active {
                theme.selected(fade)
            } else {
                theme.surface()
            };
            let bar = Span::styled(
                if active { theme.glyph("▌", ">") } else { " " },
                base.fg(theme.rgb(tone.rgb())),
            );
            let text = |style: Style| {
                if active {
                    base.patch(style).add_modifier(Modifier::BOLD)
                } else {
                    base.patch(style)
                }
            };
            let mut first = vec![bar.clone()];
            let mut second = None;
            // A record's state sits at the right edge of its first line.
            let status = (row.kind == Kind::Action)
                .then(|| ui.section().and_then(|s| s.items.get(row.source)))
                .flatten()
                .map(|item| item.status.clone())
                .filter(|(label, _)| !label.is_empty() && width(label) + 4 < w / 3);
            let status_room = status.as_ref().map_or(0, |(label, _)| width(label) + 2);
            match row.kind {
                Kind::Node => {
                    let node = ui.snapshot.tree.get(row.source);
                    let kind = node.map(|n| n.kind.as_str()).unwrap_or("");
                    let state = node.map(|n| n.state.as_str()).unwrap_or("");
                    let mut state_tone = node_tone(state, kind);
                    let marker = if row.children {
                        if row.expanded {
                            theme.glyph("▾", "v")
                        } else {
                            theme.glyph("▸", ">")
                        }
                    } else {
                        " "
                    };
                    // Roots need no connector; deeper levels keep theirs.
                    let branch: String = row.branch.chars().skip(3).collect();
                    first.push(Span::styled(
                        theme.branch(&branch),
                        base.patch(theme.fg(Tone::Faint)),
                    ));
                    first.push(Span::styled(
                        format!("{marker} "),
                        base.patch(theme.fg(Tone::Muted)),
                    ));
                    let mut icon_style = base.patch(theme.fg(match kind {
                        "pane" => state_tone,
                        "agent session" => Tone::Accent,
                        "server" => Tone::Violet,
                        _ => node_tone("", kind),
                    }));
                    if state_tone == Tone::Warn && kind == "pane" && theme.animated() {
                        // Busy terminals pulse gently.
                        icon_style =
                            icon_style.fg(theme.mix(theme::FAINT, theme::AMBER, 0.4 + 0.6 * pulse));
                    }
                    first.push(Span::styled(
                        format!("{} ", node_icon(&theme, kind, state)),
                        icon_style,
                    ));
                    let left: usize = first.iter().map(|s| s.width()).sum();
                    let note = yielding_note(&row.note, w.saturating_sub(left)).to_owned();
                    let room = w.saturating_sub(left + width(&note) + 3);
                    let label = fit(&row.title, room.max(8), &theme);
                    if kind == "server" {
                        let (tag, rest) = label.split_once(" · ").unwrap_or(("", &label));
                        first.push(Span::styled(
                            format!("{tag} "),
                            text(theme.fg(if tag == "HERDR" {
                                Tone::Violet
                            } else {
                                Tone::Accent
                            })),
                        ));
                        first.push(Span::styled(rest.to_owned(), text(theme.fg(Tone::Text))));
                    } else if let Some(parts) = node.map(|n| &n.parts).filter(|p| !p.is_empty()) {
                        first.extend(part_spans(&theme, parts, room.max(8), &text));
                    } else {
                        first.push(Span::styled(label, text(theme.fg(Tone::Text))));
                    }
                    if kind != "pane" && kind != "agent session" {
                        state_tone = Tone::Faint;
                    }
                    let used: usize = first.iter().map(|s| s.width()).sum();
                    let pad = w.saturating_sub(used + width(&note) + 1);
                    first.push(Span::styled(" ".repeat(pad), base));
                    first.push(Span::styled(note, base.patch(theme.fg(state_tone))));
                }
                Kind::Action => {
                    first.push(Span::styled(
                        format!(" {}  ", theme.icon(&row.icon)),
                        base.fg(theme.rgb(tone.rgb())),
                    ));
                    let hint = if row.hint.is_empty() {
                        String::new()
                    } else {
                        format!("  matches \"{}\"", row.hint)
                    };
                    let left: usize = first.iter().map(|s| s.width()).sum();
                    first.push(Span::styled(
                        fit(
                            &row.title,
                            w.saturating_sub(left + width(&hint) + 1 + status_room),
                            &theme,
                        ),
                        text(theme.fg(Tone::Text)),
                    ));
                    if two_line {
                        first.push(Span::styled(hint, base.patch(theme.fg(Tone::Accent))));
                        second = Some(Line::from(vec![
                            bar.clone(),
                            Span::styled(
                                format!("     {}", fit(&row.note, w.saturating_sub(7), &theme)),
                                base.patch(theme.fg(if active { Tone::Text } else { Tone::Muted })),
                            ),
                        ]));
                    } else {
                        let used: usize = first.iter().map(|s| s.width()).sum();
                        first.push(Span::styled(
                            format!(
                                "  {}",
                                fit(
                                    &row.note,
                                    w.saturating_sub(used + width(&hint) + 3 + status_room),
                                    &theme
                                )
                            ),
                            base.patch(theme.fg(Tone::Muted)),
                        ));
                        first.push(Span::styled(hint, base.patch(theme.fg(Tone::Accent))));
                    }
                }
                _ => {
                    first.push(Span::styled(
                        format!(" {} ", theme.icon(&row.icon)),
                        base.patch(theme.fg(tone)),
                    ));
                    let title = fit(&row.title, title_col.saturating_sub(1), &theme);
                    let pad = title_col.saturating_sub(width(&title));
                    first.push(Span::styled(title, text(theme.fg(Tone::Text))));
                    first.push(Span::styled(" ".repeat(pad), base));
                    let hint = if row.hint.is_empty() {
                        String::new()
                    } else {
                        format!("  alias {}", row.hint)
                    };
                    let used: usize = first.iter().map(|s| s.width()).sum();
                    first.push(Span::styled(
                        fit(&row.note, w.saturating_sub(used + width(&hint) + 1), &theme),
                        base.patch(theme.fg(Tone::Muted)),
                    ));
                    first.push(Span::styled(hint, base.patch(theme.fg(Tone::Accent))));
                }
            }
            let used: usize = first.iter().map(|s| s.width()).sum();
            if let Some((label, tone)) = &status {
                let pad = w.saturating_sub(used + width(label) + 1);
                first.push(Span::styled(" ".repeat(pad), base));
                first.push(Span::styled(
                    label.clone(),
                    base.patch(theme.fg(Tone::named(tone))),
                ));
                first.push(Span::styled(" ", base));
            } else {
                first.push(Span::styled(" ".repeat(w.saturating_sub(used)), base));
            }
            f.render_widget(
                Paragraph::new(Line::from(first)),
                Rect::new(body.x, y, body.width, 1),
            );
            if let Some(mut line) = second.filter(|_| h > 1) {
                let used: usize = line.spans.iter().map(|s| s.width()).sum();
                line.spans
                    .push(Span::styled(" ".repeat(w.saturating_sub(used)), base));
                f.render_widget(
                    Paragraph::new(line),
                    Rect::new(body.x, y + 1, body.width, 1),
                );
            }
            ui.list_hits.push((Rect::new(body.x, y, body.width, h), i));
        }
        y += h;
    }
    if scroll {
        let track = area.height as usize;
        let before: usize = (0..ui.offset).map(|i| height(i, i == 0) as usize).sum();
        let thumb = (track * track / total as usize).clamp(1, track);
        let start = (before * track / total as usize).min(track - thumb);
        let lines: Vec<Line> = (0..track)
            .map(|i| {
                if (start..start + thumb).contains(&i) {
                    Line::from(Span::styled(theme.glyph("┃", "#"), theme.fg(tone)))
                } else {
                    Line::from(Span::styled(theme.glyph("│", "|"), theme.fg(Tone::Faint)))
                }
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines),
            Rect::new(area.right() - 1, area.y, 1, area.height),
        );
    }
}

/// Each harness keeps one hue wherever it is named.
fn harness_tone(name: &str) -> Tone {
    match name.to_lowercase().as_str() {
        "claude" => Tone::Warn,
        "codex" => Tone::Ok,
        "grok" => Tone::Violet,
        "hermes" => Tone::Accent,
        "cursor" | "pi" => Tone::Rose,
        "opencode" => Tone::Text,
        "agy" => Tone::Info,
        _ => Tone::Info,
    }
}

/// A tree label by role: quiet identifiers, a bold title, the harness in its
/// hue, the model in blue and the program's own topic muted. Fits `room`.
fn part_spans(
    theme: &Theme,
    parts: &[(String, String)],
    room: usize,
    text: &dyn Fn(Style) -> Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut left = room;
    for (i, (part, role)) in parts.iter().enumerate() {
        let (sep, style) = match role.as_str() {
            "id" => ("", theme.fg(Tone::Faint)),
            "title" => (" ", theme.fg(Tone::Text).add_modifier(Modifier::BOLD)),
            "harness" => (" ", theme.fg(harness_tone(part))),
            "model" => (" ", theme.fg(Tone::Info)),
            "topic" => ("  ", theme.fg(Tone::Muted).add_modifier(Modifier::ITALIC)),
            "sid" => (
                " ",
                Style::default().fg(theme.mix(theme::FAINT, theme::CYAN, 0.55)),
            ),
            // A short reason, such as why a session ID is missing.
            "warn" => (" · ", theme.fg(Tone::Warn)),
            _ => (" ", theme.fg(Tone::Faint)),
        };
        // Title, harness and model read as one name; a dot separates them.
        let dot = i > 0 && matches!(role.as_str(), "harness" | "model")
            || (role == "title" && parts[..i].iter().any(|(_, r)| r == "title"));
        let sep = if dot {
            format!(" {} ", theme.glyph("·", "-"))
        } else if i == 0 {
            String::new()
        } else {
            sep.to_owned()
        };
        if left <= width(&sep) + 1 {
            break;
        }
        left -= width(&sep);
        spans.push(Span::styled(sep, text(theme.fg(Tone::Faint))));
        let label = if role == "harness" {
            format!("{} {}", theme.icon(part), part)
        } else {
            part.clone()
        };
        let shown = fit(&label, left, theme);
        left = left.saturating_sub(width(&shown));
        spans.push(Span::styled(shown, text(style)));
    }
    spans
}

/// Badges as colored chips, as many to a line as fit in `room`.
fn badge_chips<'a>(theme: &Theme, badges: &[(String, String)], room: usize) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0;
    for (label, tone) in badges {
        let label = clean(label);
        let chip = 2 + width(&label);
        if used > 0 && used + 2 + chip > room {
            lines.push(Line::from(std::mem::take(&mut spans)));
            used = 0;
        }
        spans.push(Span::raw(if used == 0 { " " } else { "   " }));
        let style = theme.fg(Tone::named(tone));
        spans.push(Span::styled(format!("{} ", theme.glyph("●", "*")), style));
        spans.push(Span::styled(label, style));
        used += chip + if used == 0 { 1 } else { 3 };
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

fn inspector(f: &mut Frame, area: Rect, ui: &mut Ui, row: Option<&Row>) {
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
            para(
                &mut lines,
                &format!("{section} {} {}", theme.glyph("›", ">"), item.group),
                Tone::Faint,
            );
            if !item.badges.is_empty() {
                lines.extend(badge_chips(&theme, &item.badges, w));
            }
            lines.push(Line::from(""));
            para(&mut lines, &item.note, Tone::Text);
            // Aliases are search words; finding works without listing them.
            if !item.context.is_empty() {
                lines.push(Line::from(""));
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
                para(&mut lines, "Also found by", Tone::Faint);
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
        // Its share of the panel, leaving the border row and three for facts.
        let room = inner.bottom().saturating_sub(y + 4).max(6);
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
    let room = (rest.height as usize).saturating_sub(footer.len());
    if lines.len() + map.len() <= room {
        lines.extend(map);
    }
    lines.truncate(room);
    lines.extend(footer);
    f.render_widget(Paragraph::new(lines), rest);
}

/// A tab's split in miniature: each terminal framed where its backend
/// places it and named, `current` (if any) drawn bright and thick.
fn layout_map(
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

fn overview(f: &mut Frame, area: Rect, ui: &mut Ui) {
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
                        format!(" {}", fit(label, room.saturating_sub(1), &theme)),
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
    let side = inner.width >= 76;
    let block_height = (s.overview.connections.len() + 2).max(s.overview.setup.len() + 1) as u16;
    let columns = if side {
        split(
            Rect::new(inner.x, y, inner.width, block_height),
            Direction::Horizontal,
            vec![Constraint::Percentage(48), Constraint::Percentage(52)],
        )
    } else {
        split(
            Rect::new(inner.x, y, inner.width, block_height * 2 + 1),
            Direction::Vertical,
            vec![Constraint::Length(block_height + 1), Constraint::Min(1)],
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
            "absent" => (theme.glyph("○", "o"), Tone::Faint),
            _ => (theme.glyph("◌", "?"), Tone::Warn),
        };
        let pad = 18usize.saturating_sub(width(&light.label));
        left.push(Line::from(vec![
            Span::styled(format!("{glyph} "), theme.fg(t)),
            Span::styled(light.label.clone(), theme.fg(Tone::Text)),
            Span::raw(" ".repeat(pad)),
            Span::styled(
                light.detail.clone(),
                theme.fg(if t == Tone::Ok { Tone::Ok } else { Tone::Muted }),
            ),
        ]));
    }
    left.push(Line::from(Span::styled(
        "A running process is not proof of delivery.",
        theme.fg(Tone::Faint),
    )));
    f.render_widget(Paragraph::new(left), columns[0]);
    let done = s.overview.setup.iter().filter(|x| x.done).count();
    let total = s.overview.setup.len();
    let bar: String = (0..total)
        .map(|i| {
            if i < done {
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
    for step in &s.overview.setup {
        let (glyph, t) = if step.done {
            (theme.icon("done"), Tone::Ok)
        } else {
            (theme.icon("pending"), Tone::Faint)
        };
        let pad = 15usize.saturating_sub(width(&step.label));
        right.push(Line::from(vec![
            Span::styled(format!("{glyph} "), theme.fg(t)),
            Span::styled(step.label.clone(), theme.fg(Tone::Text)),
            Span::raw(" ".repeat(pad)),
            Span::styled(
                fit(
                    &step.detail,
                    (columns[1].width as usize).saturating_sub(19),
                    &theme,
                ),
                theme.fg(Tone::Muted),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(right), columns[1]);
    y = columns[columns.len() - 1].bottom() + 1;
    // The one action that lives here.
    let wanted: u16 = items
        .iter()
        .map(|r| if r.kind == Kind::Header { 1 } else { 2 })
        .sum();
    let start_height = wanted.min(inner.bottom().saturating_sub(y));
    let list = Rect::new(inner.x, y, inner.width, start_height);
    render_list(f, list, ui, &items, true);
    y = list.bottom() + 1;
    // A map of the other sections: descriptions, not repeated actions.
    let guide: Vec<(String, String, String, String)> = (1..ui.page_count())
        .map(|i| {
            let (title, icon, category) = page_meta(ui, i);
            let summary = ui
                .snapshot
                .sections
                .get(i)
                .map(|s| s.summary.clone())
                .unwrap_or_default();
            (title, icon, category, summary)
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
        (column * per_column + 1..((column + 1) * per_column + 1).min(guide.len() + 1))
            .map(|i| width(&shortcut(i)))
            .max()
            .unwrap_or(1)
    };
    for (n, (title, icon, category, summary)) in guide.iter().enumerate() {
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
            shortcut(n + 1),
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
        ui.nav_hits.push((rect, n + 1));
    }
}

/// The key that opens page `i`: digits 1 to 9, then 0, and Ctrl+P for Commands.
fn shortcut(i: usize) -> String {
    if i == COMMANDS {
        "^P".to_string()
    } else {
        ((i + 1) % 10).to_string()
    }
}

fn footer(f: &mut Frame, area: Rect, ui: &Ui) {
    let theme = ui.theme;
    let rows = ui.rows();
    let position = rows
        .iter()
        .filter(|r| r.kind != Kind::Header)
        .position(|r| rows.get(ui.selected).is_some_and(|s| s.id == r.id));
    let count = rows.iter().filter(|r| r.kind != Kind::Header).count();
    let (glyph, tone) = if ui.status_error {
        (theme.glyph("!", "!"), Tone::Warn)
    } else if ui.busy {
        (theme.spinner(), Tone::Info)
    } else {
        (theme.glyph("›", ">"), Tone::Faint)
    };
    let where_ = match (ui.page == WORKSPACES, ui.view) {
        (true, View::Tiles | View::Wall) => {
            let tiles = ui.tiles().len();
            if tiles > 0 {
                format!("{} of {tiles} ", ui.tile + 1)
            } else {
                String::new()
            }
        }
        (true, View::Focus) => String::new(),
        _ => position
            .map(|p| format!("{} of {count} ", p + 1))
            .unwrap_or_default(),
    };
    // The screensaver starts after a few idle seconds, so its switch stays
    // in sight, even with the key bar hidden, wherever it leaves the status
    // room. Not while a terminal is open or a search is typed: s goes there.
    let saver = (theme.backdrop
        && theme.screensaver > 0
        && area.width >= 90
        && ui.embed.is_none()
        && !ui.searching)
        .then_some(if ui.saver.enabled {
            " Disable screensaver  "
        } else {
            " Enable screensaver  "
        });
    let reminder = if ui.hide_keys { 8 } else { 0 } + saver.map_or(0, |label| width(label) + 3);
    let room = (area.width as usize).saturating_sub(width(&where_) + 4 + reminder);
    let top = Rect { height: 1, ..area };
    // In Focus the input mode is the status; it can never go stale.
    let held;
    let status = match ui.embed.as_ref() {
        Some(embed) if ui.view == View::Focus && ui.page == WORKSPACES && !ui.status_error => {
            match (&embed.phase, embed.mode) {
                // Say what it is waiting on (a retry, another client), if anything.
                (Phase::Connecting, _) if !embed.notice.is_empty() => embed.notice.as_str(),
                (Phase::Connecting, _) => "Connecting to the terminal…",
                (Phase::Ended(reason), _) if !reason.is_empty() => {
                    held = format!("{reason} The terminal itself was not touched.");
                    held.as_str()
                }
                (Phase::Ended(_), _) => "Stream ended. The terminal itself was not touched.",
                (Phase::Live, Mode::Control) => {
                    "Attached: keys go to the terminal. Ctrl+] then D detaches; it keeps running."
                }
                (Phase::Live, Mode::Observe) => match &embed.held {
                    Some(by) => {
                        held = format!(
                            "Watching: input is held by {by}. Ctrl+] then T takes over; Esc returns."
                        );
                        held.as_str()
                    }
                    None => "Watching: nothing you type reaches it. Enter attaches; Esc returns.",
                },
            }
        }
        _ => ui.status.as_str(),
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {glyph} "), theme.fg(tone)),
            Span::styled(
                fit(status, room, &theme),
                theme.fg(if ui.status_error {
                    Tone::Warn
                } else {
                    Tone::Muted
                }),
            ),
        ])),
        top,
    );
    let mut right = vec![Span::styled(where_, theme.fg(Tone::Faint))];
    if let Some(label) = saver {
        right.insert(0, Span::styled(" s ", theme.keycap()));
        right.insert(1, Span::styled(label, theme.fg(Tone::Muted)));
    }
    if ui.hide_keys {
        right.insert(0, Span::styled("?", theme.keycap()));
        right.insert(1, Span::styled(" keys  ", theme.fg(Tone::Muted)));
    }
    f.render_widget(
        Paragraph::new(Line::from(right)).alignment(Alignment::Right),
        top,
    );
    if ui.hide_keys {
        return;
    }
    let enter = theme.glyph("⏎", "Enter");
    let arrows = theme.glyph("↑↓", "Up/Dn");
    let fold = theme.glyph("←→", "Lt/Rt");
    let move4 = theme.glyph("↑↓←→", "Arrows");
    let streaming = ui.embed.as_ref().is_some_and(|e| e.live());
    let control = ui
        .embed
        .as_ref()
        .is_some_and(|e| e.live() && e.mode == Mode::Control);
    let pairs: Vec<(&str, &str)> = if ui.searching {
        if ui.switching {
            vec![
                (enter, "Watch"),
                ("Alt+1–9", "Open"),
                ("Esc", "Close"),
                (arrows, "Move"),
            ]
        } else {
            vec![(enter, "Done"), ("Esc", "Clear"), (arrows, "Move")]
        }
    } else if ui.page == WORKSPACES && ui.view == View::Focus && control {
        vec![
            ("^] D", "Detach"),
            ("^] O", "Observe"),
            ("^] F", "Fit size"),
            ("^] M", "More"),
            ("^] ^]", "Send ^]"),
        ]
    } else if ui.page == WORKSPACES && ui.view == View::Focus {
        let split = ui
            .terminal_node(&ui.focus_target)
            .and_then(|n| ui.tab_layout(&n.id))
            .is_some();
        let mut pairs = vec![
            (enter, if streaming { "Attach" } else { "Watch" }),
            ("a", "Attach"),
            ("Esc", "Back"),
        ];
        if split {
            pairs.push((move4, "Pane"));
            pairs.push(("Z", if ui.zoomed { "Whole tab" } else { "Zoom" }));
        }
        pairs.extend([
            ("[ ]", "View"),
            ("m", "More"),
            ("Tab", "Section"),
            ("q", "Quit"),
        ]);
        pairs
    } else if ui.page == WORKSPACES && ui.view == View::Tiles {
        vec![
            (move4, "Move"),
            (enter, "Menu"),
            ("z", "Focus"),
            ("[ ]", "View"),
            ("PgUp/Dn", "Page"),
            ("o", "Order"),
            ("P", "Pinned only"),
            ("+ -", "Tile size"),
            ("u", "Forget missing pin"),
            ("L", "Save layout"),
            ("/", "Search"),
            ("q", "Quit"),
        ]
    } else if ui.page == WORKSPACES && ui.view == View::Wall {
        vec![
            (move4, "Move"),
            (enter, "Menu"),
            ("z", "Focus"),
            ("[ ]", "View"),
            ("o", "Order"),
            ("P", "Pinned only"),
            ("L", "Save layout"),
            ("/", "Search"),
            ("q", "Quit"),
        ]
    } else if ui.page == WORKSPACES {
        vec![
            (arrows, "Move"),
            (enter, "Menu"),
            (fold, "Fold"),
            ("[ ]", "View"),
            ("o", "Order"),
            ("/", "Search"),
            ("^P", "Commands"),
            ("Tab", "Section"),
            ("< > + -", "Resize"),
            ("E/C", "Unfold/fold all"),
            ("r", "Refresh"),
            ("q", "Quit"),
        ]
    } else {
        vec![
            (arrows, "Move"),
            (enter, "Open"),
            ("/", "Search"),
            ("^P", "Commands"),
            ("Tab", "Section"),
            ("0-9", "Jump"),
            ("< >", "Resize"),
            ("r", "Refresh"),
            ("q", "Quit"),
        ]
    };
    let mut pairs = pairs;
    if control {
        pairs.insert(0, ("^] K", "Switch"));
    }
    if !ui.searching && !control {
        pairs.insert(0, ("^K", "Switch"));
        // Before Quit, so it survives a narrow screen as long as Quit does.
        let at = pairs.len().saturating_sub(1);
        pairs.insert(at, ("?", "Hide keys"));
        // Next to the views, so it stays when a narrow bar drops keys.
        let at = pairs
            .iter()
            .position(|p| p.0 == "[ ]")
            .map_or(at, |i| i + 1);
        pairs.insert(at, (",", "Options"));
    }
    f.render_widget(
        Paragraph::new(keys(&theme, &pairs, area.width as usize - 1)),
        Rect {
            y: area.y + 1,
            height: 1,
            x: area.x + 1,
            width: area.width - 1,
        },
    );
}

/// A one-line label beside the pointer, kept on screen.
fn tooltip(f: &mut Frame, area: Rect, theme: &Theme, at: Rect, label: &str) {
    let w = (width(label) as u16 + 2).min(area.width);
    let x = at.x.min(area.right().saturating_sub(w));
    let rect = Rect::new(x, at.y, w, 1);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(Span::styled(format!(" {label} "), theme.keycap())),
        rect,
    );
}

/// A row of the Options panel.
enum OptionRow {
    Blank,
    Heading(&'static str),
    Item(usize),
}

/// Options: every workspace preference, grouped, each with what it is set
/// to and the key that also changes it.
fn options_panel(f: &mut Frame, area: Rect, ui: &mut Ui) {
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
    let height = (rows.len() as u16 + 6).min(area.height.saturating_sub(2));
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
    let list = Rect::new(
        inner.x,
        inner.y + 2,
        inner.width,
        inner.height.saturating_sub(3),
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
            Some(true) => format!("{} ", theme.glyph("●", "*")),
            Some(false) => format!("{} ", theme.glyph("○", "o")),
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
        let label = format!("{label:<label_w$}");
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
        ui.option_hits.push((rect, i));
    }
    let enter = theme.glyph("⏎", "Enter");
    let arrows = theme.glyph("↑↓", "Up/Dn");
    let change = theme.glyph("←→", "Lt/Rt");
    f.render_widget(
        Paragraph::new(keys(
            &theme,
            &[
                (arrows, "Choose"),
                (change, "Change"),
                (enter, "Change"),
                ("Esc", "Close"),
            ],
            inner.width as usize,
        )),
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
    );
}

fn dialog(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    let Some(dialog) = ui.dialog.as_mut() else {
        return;
    };
    let confirm = dialog.options.iter().any(|o| o.0 == "yes")
        && dialog.options.iter().any(|o| o.0 == "cancel");
    let tone = if confirm { Tone::Warn } else { Tone::Accent };
    let modal_width = area.width.saturating_sub(6).min(96);
    let description = wrap(
        &dialog.description,
        modal_width.saturating_sub(4).max(1) as usize,
    );
    let wanted = match dialog.mode.as_str() {
        "input" => 9 + description.len() as u16,
        "external" => 8,
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
        &fit(&dialog.title, modal_width as usize - 6, &theme),
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
    let reserve = if dialog.mode == "choose" { 8 } else { 3 };
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
    let hints: Vec<(&str, &str)> = match dialog.mode.as_str() {
        "choose" => {
            let per = 3usize;
            let capacity = (regions[1].height as usize / per).max(1);
            let start = dialog.selected.saturating_sub(capacity - 1);
            let mut lines = Vec::new();
            for (i, (value, title, note)) in
                dialog.options.iter().enumerate().skip(start).take(capacity)
            {
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
            f.render_widget(Paragraph::new(lines), regions[1]);
            // A neon wash under the selected choice, brightest at its bar.
            if theme.truecolor && !confirm {
                let row = regions[1].y + (dialog.selected - start) as u16 * 3;
                let area = regions[1];
                for y in row..(row + 2).min(area.bottom()) {
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
            let mut hints = vec![(arrows, "Choose"), (enter, "Select"), ("Esc", "Cancel")];
            if description.len() > desc_height as usize {
                hints.push(("PgUp/PgDn", "Details"));
            }
            hints
        }
        "input" => {
            let value = if dialog.secret {
                theme.glyph("•", "*").repeat(dialog.input.chars().count())
            } else {
                clean(&dialog.input)
            };
            let shown = if width(&value) + 3 > w {
                value
                    .chars()
                    .rev()
                    .take(w.saturating_sub(4))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            } else {
                value
            };
            let field = if theme.color {
                Style::default().bg(theme.rgb(theme::SELECT))
            } else {
                Style::default().add_modifier(Modifier::UNDERLINED)
            };
            let pad = w.saturating_sub(width(&shown) + 2);
            let caption = if dialog.secret {
                "Hidden · saved privately outside the repository"
            } else {
                "Paste or type, then press Enter"
            };
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(format!(" {shown}"), field.patch(theme.fg(Tone::Text))),
                        Span::styled(theme.glyph("▏", "_"), field.patch(theme.fg(Tone::Accent))),
                        Span::styled(" ".repeat(pad), field),
                    ]),
                    Line::from(Span::styled(format!(" {caption}"), theme.fg(Tone::Faint))),
                ]),
                regions[1],
            );
            vec![(enter, "Save"), ("Esc", "Cancel")]
        }
        "external" => {
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "Enter opens the native program in this terminal.",
                        theme.fg(Tone::Text),
                    )),
                    Line::from(Span::styled(
                        "The dashboard returns when the program exits.",
                        theme.fg(Tone::Muted),
                    )),
                ]),
                regions[1],
            );
            vec![(enter, "Open"), ("Esc", "Cancel")]
        }
        _ => {
            let lines = wrap(&dialog.text, w);
            let visible = regions[1].height as usize;
            let most = lines.len().saturating_sub(visible) as u16;
            dialog.scroll = dialog.scroll.min(most);
            let shown: Vec<Line> = lines
                .iter()
                .skip(dialog.scroll as usize)
                .take(visible)
                .map(|l| result_line(l, &theme))
                .collect();
            f.render_widget(Paragraph::new(shown), regions[1]);
            if most > 0 {
                let position = format!(
                    " {}–{} of {} ",
                    dialog.scroll + 1,
                    (dialog.scroll as usize + visible).min(lines.len()),
                    lines.len()
                );
                f.render_widget(
                    Paragraph::new(Span::styled(position, theme.fg(Tone::Faint)))
                        .alignment(Alignment::Right),
                    regions[2],
                );
            }
            vec![(arrows, "Scroll"), ("PgUp/PgDn", "Page"), (enter, "Close")]
        }
    };
    f.render_widget(Paragraph::new(keys(&theme, &hints, w)), regions[2]);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dialog, Entry, Item, Node, Section, Snapshot, Split, View};
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    fn fixture(theme: Theme) -> Ui {
        let sections = FALLBACK
            .iter()
            .enumerate()
            .map(|(i, (title, icon, category))| Section {
                title: title.to_string(),
                icon: icon.to_string(),
                category: category.to_string(),
                summary: "Summary".into(),
                items: if i == COMMANDS {
                    vec![]
                } else {
                    vec![Item {
                        id: format!("item.{i}"),
                        group: "Group".into(),
                        title: "Action title".into(),
                        note: "What it does".into(),
                        icon: "setup".into(),
                        context: if i == 3 {
                            "Connect chat".into()
                        } else {
                            String::new()
                        },
                        badges: vec![("Asks before acting".into(), "warn".into())],
                        aliases: vec!["alias".into()],
                        status: Default::default(),
                    }]
                },
            })
            .collect();
        Ui {
            theme,
            page: WORKSPACES,
            snapshot: Snapshot {
                sections,
                tree: vec![
                    Node {
                        id: "s".into(),
                        label: "HERDR · demo".into(),
                        kind: "server".into(),
                        backend: "herdr".into(),
                        children: true,
                        count: 1,
                        branch: "└─ ".into(),
                        ..Node::default()
                    },
                    Node {
                        id: "p".into(),
                        parent: "s".into(),
                        label: "w1:p1 · worker".into(),
                        kind: "pane".into(),
                        backend: "herdr".into(),
                        state: "working".into(),
                        actionable: true,
                        branch: "   └─ ".into(),
                        fields: vec![Field::new("Reported state", "working", "state")],
                        ..Node::default()
                    },
                ],
                catalog: vec![Entry {
                    id: "telegram:jobs".into(),
                    category: "Telegram".into(),
                    group: "Sessions & jobs".into(),
                    title: "/jobs".into(),
                    description: "list jobs".into(),
                    interface: "Telegram command".into(),
                    icon: "telegram".into(),
                    aliases: vec!["/fleet".into()],
                    ..Entry::default()
                }],
                instance: "demo-instance".into(),
                freshness: "DEMO · synthetic data".into(),
                demo: true,
                ..Snapshot::default()
            },
            status: "Ready".into(),
            ..Ui::default()
        }
    }
    fn render(ui: &mut Ui, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, ui)).unwrap();
        terminal.backend().buffer().clone()
    }
    fn text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer.content().iter().map(|c| c.symbol()).collect()
    }
    #[test]
    fn every_page_renders_at_small_and_large_sizes() {
        for (w, h) in [(40, 12), (60, 16), (80, 24), (120, 36), (160, 48)] {
            for page in 0..FALLBACK.len() {
                let mut ui = fixture(Theme::default());
                ui.navigate(page);
                let buffer = render(&mut ui, w, h);
                if w >= 80 {
                    let rendered = text(&buffer);
                    assert!(rendered.contains("DAEDALUS"), "{w}x{h} page {page}");
                }
            }
        }
        let mut empty = Ui::default();
        render(&mut empty, 120, 40);
    }
    #[test]
    fn record_status_sits_at_the_right_edge_of_its_row() {
        let mut ui = fixture(Theme {
            reduced_motion: true,
            ..Theme::default()
        });
        ui.snapshot.sections[5].items[0].status = ("Running".into(), "ok".into());
        ui.navigate(5);
        let buffer = render(&mut ui, 140, 44);
        let area = buffer.area;
        let (x, y) = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .find(|&(x, y)| {
                (0..7).all(|i| buffer[(x + i, y)].symbol() == &"Running"[i as usize..=i as usize])
            })
            .expect("status rendered");
        let row: String = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
        assert!(row.contains("Action title"), "{row}");
        assert_eq!(buffer[(x, y)].fg, ui.theme.fg(Tone::Ok).fg.unwrap());
        // One space, then the list's border.
        assert_eq!(buffer[(x + 7, y)].symbol(), " ");
        assert_eq!(buffer[(x + 8, y)].symbol(), "│");
    }
    #[test]
    fn overview_map_shows_the_keys_that_open_each_page() {
        let mut ui = fixture(Theme::default());
        ui.navigate(0);
        let rendered = text(&render(&mut ui, 160, 48));
        let entry =
            |key: &str, icon: &str, title: &str| format!("{key} {} {title}", ui.theme.icon(icon));
        assert!(rendered.contains("WHERE THINGS LIVE"));
        assert!(rendered.contains(&entry("2", "workspaces", "Workspaces")));
        assert!(rendered.contains(&entry(" 0", "help", "Help")));
        assert!(rendered.contains(&entry("^P", "commands", "Commands")));
        assert!(!rendered.contains(&entry("10", "help", "Help")));
        assert!(!rendered.contains(&entry("11", "commands", "Commands")));
    }
    #[test]
    fn sidebar_lists_each_section_once_with_categories() {
        let mut ui = fixture(Theme::default());
        let rendered = text(&render(&mut ui, 140, 44));
        for (title, ..) in FALLBACK {
            assert!(rendered.contains(title), "{title}");
        }
        for category in ["WORK", "CONNECT", "SYSTEM"] {
            assert!(rendered.contains(category));
        }
        // Every section once, in order; then the totals, each opening its section.
        let pages: Vec<usize> = ui.nav_hits.iter().map(|(_, p)| *p).collect();
        assert_eq!(
            pages[..FALLBACK.len()],
            (0..FALLBACK.len()).collect::<Vec<_>>()
        );
        assert_eq!(pages[FALLBACK.len()..], [WORKSPACES, WORKSPACES, 3, 2, 6]);
    }
    #[test]
    fn the_sidebar_counts_folds_and_lists_terminals_to_jump_to() {
        let mut ui = crate::model::tests::tiles_fixture();
        ui.snapshot.sections = fixture(Theme::default()).snapshot.sections;
        ui.snapshot.terminals = 12;
        ui.snapshot.warnings = 2;
        ui.snapshot.observed_at = 1_000.0;
        ui.snapshot.tree[1].active_at = 990.0;
        let rendered = text(&render(&mut ui, 140, 44));
        assert!(
            rendered.contains("Workspaces 12"),
            "a count beside its section"
        );
        assert!(rendered.contains("Activity 2"));
        assert!(rendered.contains("Options"));
        assert!(rendered.contains("PINNED") && rendered.contains("RECENT"));
        // beta is pinned, alpha active recently; the missing pin cannot be clicked.
        let ids: Vec<&str> = ui.side_hits.iter().map(|(_, id)| id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
        assert!(ui.quick_hits.iter().any(|(_, a)| a == "sidebar"));
        assert!(ui.quick_hits.iter().any(|(_, a)| a == "options"));
        // Switched off in Options, the lists, counts and totals go.
        ui.prefs.pinned = false;
        ui.prefs.recent = false;
        ui.prefs.badges = false;
        ui.prefs.vitals = false;
        let rendered = text(&render(&mut ui, 140, 44));
        assert!(!rendered.contains("PINNED") && !rendered.contains("RECENT"));
        assert!(!rendered.contains("Workspaces 12"));
        assert!(!rendered.contains("warnings"));
        assert!(ui.side_hits.is_empty());
    }
    #[test]
    fn the_folded_rail_names_what_is_under_the_pointer() {
        let mut ui = fixture(Theme::default());
        ui.sizes.sidebar = 0;
        render(&mut ui, 140, 40);
        let (rect, page) = ui.nav_hits[2];
        assert_eq!(page, 2);
        ui.pointer = Some((rect.x + 1, rect.y));
        let rendered = text(&render(&mut ui, 140, 40));
        assert!(
            rendered.contains(" Sessions  3 "),
            "the section's name and key"
        );
        ui.pointer = Some((2, 2));
        let rendered = text(&render(&mut ui, 140, 40));
        assert!(
            rendered.contains("Open the sidebar"),
            "the » button says what it does"
        );
    }
    #[test]
    fn options_list_every_choice_with_its_value_and_key() {
        let mut ui = fixture(Theme::default());
        ui.options = Some(crate::model::OptionsMenu::default());
        let rendered = text(&render(&mut ui, 140, 44));
        for item in options::Item::ALL {
            assert!(rendered.contains(item.label()), "{item:?}");
        }
        assert!(rendered.contains("Latest activity first"));
        assert!(rendered.contains("SIDEBAR") && rendered.contains("LOOK AND FEEL"));
        assert_eq!(ui.option_hits.len(), options::Item::ALL.len());
        assert!(ui.option_panel.width > 0);
        // A short window scrolls to keep the choice in sight.
        ui.options.as_mut().unwrap().selected = options::Item::ALL.len() - 1;
        let rendered = text(&render(&mut ui, 100, 18));
        assert!(rendered.contains("Reset panel sizes"));
        assert!(
            ui.option_hits
                .iter()
                .any(|(_, i)| *i == options::Item::ALL.len() - 1)
        );
    }
    #[test]
    fn details_and_its_live_screen_can_be_switched_off() {
        let mut ui = fixture(Theme::default());
        ui.selected = 1;
        let rendered = text(&render(&mut ui, 140, 44));
        assert!(rendered.contains("Details"));
        ui.prefs.screen = false;
        let rendered = text(&render(&mut ui, 140, 44));
        assert!(rendered.contains("Details") && !rendered.contains("Live screen"));
        assert!(ui.preview_targets.is_empty());
        ui.prefs.details = false;
        let rendered = text(&render(&mut ui, 140, 44));
        assert!(!rendered.contains(" Details "));
    }
    #[test]
    fn secret_input_is_masked_in_render_buffer() {
        let mut ui = Ui {
            dialog: Some(Dialog {
                mode: "input".into(),
                secret: true,
                input: "private-test-value".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let rendered = text(&render(&mut ui, 100, 30));
        assert!(!rendered.contains("private-test-value"));
        assert!(rendered.contains("•"));
    }
    #[test]
    fn no_color_uses_attributes_not_colors() {
        let theme = Theme {
            color: false,
            ..Theme::default()
        };
        let mut ui = fixture(theme);
        let buffer = render(&mut ui, 140, 40);
        assert!(
            buffer
                .content()
                .iter()
                .all(|c| matches!(c.fg, Color::Reset) && matches!(c.bg, Color::Reset))
        );
        assert!(
            buffer
                .content()
                .iter()
                .any(|c| c.modifier.contains(Modifier::REVERSED))
        );
    }
    #[test]
    fn ascii_mode_draws_only_ascii() {
        let theme = Theme {
            ascii: true,
            ..Theme::default()
        };
        for page in 0..9 {
            let mut ui = fixture(theme);
            ui.navigate(page);
            for (w, h) in [(90, 24), (140, 40)] {
                let rendered = text(&render(&mut ui, w, h));
                let foreign: String = rendered.chars().filter(|c| !c.is_ascii()).collect();
                assert!(foreign.is_empty(), "page {page} {w}x{h}: {foreign}");
            }
        }
    }
    #[test]
    fn inspector_shows_badges_and_terminal_context() {
        let mut ui = fixture(Theme {
            reduced_motion: true,
            ..Theme::default()
        });
        ui.navigate(3);
        let buffer = render(&mut ui, 150, 40);
        let rendered = text(&buffer);
        assert!(rendered.contains("Enter menu as"));
        assert!(rendered.contains("Connect"));
        // Search words stay out of the details.
        assert!(!rendered.contains("alias"));
        // Badges are chips in their own color.
        let word = "Asks";
        let start = ui
            .handles
            .iter()
            .find(|h| h.split == Split::Details)
            .map_or(0, |h| h.area.x + 2);
        let (x, y) = (0..40u16)
            .flat_map(|y| (start..147u16).map(move |x| (x, y)))
            .find(|&(x, y)| {
                (0..4).all(|i| buffer[(x + i, y)].symbol() == &word[i as usize..=i as usize])
            })
            .expect("badge shown");
        assert_eq!(buffer[(x, y)].fg, ui.theme.fg(Tone::Warn).fg.unwrap());
        assert!(rendered.contains("Demo: changes are disabled"));
    }
    #[test]
    fn confirmation_dialog_starts_on_cancel() {
        let mut ui = fixture(Theme::default());
        ui.dialog = Some(Dialog {
            mode: "choose".into(),
            title: "Confirm action".into(),
            options: vec![
                ("cancel".into(), "Cancel".into(), "Leave everything".into()),
                ("yes".into(), "Continue".into(), "Close pane".into()),
            ],
            ..Dialog::default()
        });
        let rendered = text(&render(&mut ui, 120, 36));
        let cancel = rendered.find("Cancel").unwrap();
        let cont = rendered.find("Continue").unwrap();
        assert!(cancel < cont);
        assert_eq!(ui.dialog.as_ref().unwrap().selected, 0);
    }
    #[test]
    fn long_choice_review_scrolls_without_hiding_choices() {
        let mut ui = fixture(Theme::default());
        ui.dialog = Some(Dialog {
            mode: "choose".into(),
            title: "Review changes".into(),
            description: (0..40)
                .map(|i| format!("Field {i}: Value {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
            options: vec![
                (
                    "cancel".into(),
                    "Cancel".into(),
                    "Keep configuration".into(),
                ),
                ("yes".into(), "Apply".into(), "Use reviewed values".into()),
            ],
            ..Dialog::default()
        });
        let first = text(&render(&mut ui, 80, 30));
        assert!(first.contains("Field 0:"));
        assert!(first.contains("Cancel") && first.contains("Apply"));
        assert!(first.contains("PgUp/PgDn"));
        ui.dialog.as_mut().unwrap().scroll = 100;
        let last = text(&render(&mut ui, 80, 30));
        assert!(last.contains("Field 39:"));
        assert!(last.contains("Cancel") && last.contains("Apply"));
    }
    fn workspace(theme: Theme, view: View) -> Ui {
        let mut ui = crate::model::tests::tiles_fixture();
        ui.theme = theme;
        ui.view = view;
        ui.focus_target = "a".into();
        ui.previews.insert(
            "a".into(),
            crate::model::Preview::new("$ make\n\x1b[32mpreview-ok\x1b[0m", ""),
        );
        ui
    }
    #[test]
    fn tiles_show_previews_pins_and_missing_references() {
        let mut ui = workspace(Theme::default(), View::Tiles);
        let rendered = text(&render(&mut ui, 140, 40));
        assert!(rendered.contains("preview-ok"));
        assert!(rendered.contains("MISSING"));
        assert!(rendered.contains("never rebound"));
        assert!(rendered.contains("Terminal %9 no longer exists"));
        assert_eq!(ui.tile_hits.len(), 3);
        // Only live tiles on screen are previewed; a missing pin never is.
        assert_eq!(ui.preview_targets, ["b", "a"]);
        assert_eq!(ui.view_hits.len(), View::ALL.len());
        assert!(ui.quick_hits.iter().any(|(_, id)| id == "terminal.attach"));
        assert!(ui.preview_rows >= 2);
    }
    #[test]
    fn panel_borders_are_lit_from_above_in_the_page_hue() {
        let mut ui = workspace(Theme::default(), View::Tree);
        let buffer = render(&mut ui, 140, 36);
        let theme = Theme::default();
        let corner = |glyph: &str| {
            buffer
                .content()
                .iter()
                .find(|c| c.symbol() == glyph)
                .map(|c| c.fg)
                .unwrap()
        };
        // The focused list: brightest at its top-left corner, plain at the bottom.
        assert_eq!(corner("╭"), theme.mix(theme::FAINT, theme::CYAN, 0.8));
        assert_eq!(corner("╰"), theme.rgb(theme::FAINT));
    }
    #[test]
    fn shortcut_bar_hides_and_returns_the_row() {
        let mut ui = workspace(Theme::default(), View::Tree);
        let shown = text(&render(&mut ui, 140, 36));
        assert!(shown.contains("Hide keys"));
        ui.hide_keys = true;
        let hidden = text(&render(&mut ui, 140, 36));
        assert!(!hidden.contains("Unfold/fold all"));
        // The way back stays visible on the status line.
        assert!(hidden.contains("? keys"));
    }
    #[test]
    fn tree_labels_color_each_part_by_role() {
        let mut ui = workspace(Theme::default(), View::Tree);
        ui.snapshot.tree[1].parts = [
            ("w1:p1", "id"),
            ("BUILDER", "title"),
            ("Codex", "harness"),
            ("haiku", "model"),
            ("Readiness check", "topic"),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        assert!(ui.select_id("server"));
        let buffer = render(&mut ui, 160, 40);
        let rendered = text(&buffer);
        let fg = |needle: &str| {
            let at = rendered.find(needle).unwrap();
            let index = rendered[..at].chars().count();
            buffer.content()[index].fg
        };
        let theme = Theme::default();
        assert_eq!(fg("Codex"), theme.rgb(theme::GREEN));
        assert_eq!(fg("haiku"), theme.rgb(theme::BLUE));
        assert_eq!(fg("w1:p1"), theme.rgb(theme::FAINT));
        assert_eq!(fg("BUILDER"), theme.rgb(theme::TEXT));
        assert_eq!(fg("Readiness check"), theme.rgb(theme::MUTED));
        // The topic is searchable although it is not in the plain label.
        ui.filter = "readiness".into();
        assert!(ui.rows().iter().any(|r| r.id == "a"));
    }
    #[test]
    fn a_long_note_gives_way_to_the_name_from_its_end() {
        assert_eq!(yielding_note("idle · 13m", 26), "idle · 13m");
        assert_eq!(yielding_note("unknown · 2h · server focus", 26), "unknown");
        assert_eq!(
            yielding_note("unknown · 2h · server focus", 60),
            "unknown · 2h · server focus"
        );
        assert_eq!(yielding_note("33", 4), "33");
        let mut ui = workspace(Theme::default(), View::Tree);
        ui.snapshot.tree[1].label = "w1:p2 · VOICE DESK · Shell".into();
        ui.snapshot.tree[1].state = "unknown".into();
        ui.snapshot.tree[1].linked = 6;
        ui.snapshot.tree[1].server_focused = true;
        for w in [44, 50, 56, 64] {
            let buffer = render(&mut ui, w, 30);
            let cells: Vec<&str> = buffer.content().iter().map(|c| c.symbol()).collect();
            let row = cells
                .chunks(w as usize)
                .map(|line| line.concat())
                .find(|line| line.contains("w1:p2"))
                .unwrap();
            assert!(row.contains("VOI") && row.contains("unknown"), "{w}: {row}");
        }
    }
    #[test]
    fn tree_details_preview_the_selected_terminal() {
        let mut ui = workspace(Theme::default(), View::Tree);
        ui.snapshot.tree[1].fields = vec![Field::new("Server", "alpha-server", "herdr")];
        assert!(ui.select_id("a"));
        let rendered = text(&render(&mut ui, 160, 44));
        assert!(rendered.contains("preview-ok"));
        assert!(rendered.contains("read-only"));
        // The information stays, under the screen.
        assert!(rendered.contains("alpha-server"));
        assert!(rendered.find("preview-ok") < rendered.find("alpha-server"));
        assert_eq!(ui.preview_targets, ["a"]);
        assert!(ui.preview_rows >= 5);
        // An agent session previews the terminal it runs in.
        assert!(ui.select_id("b-agent"));
        let rendered = text(&render(&mut ui, 160, 44));
        assert_eq!(ui.preview_targets, ["b"]);
        assert!(rendered.contains("Reading screen"));
        // Not a terminal, or no room for Details: nothing is read.
        assert!(ui.select_id("server"));
        render(&mut ui, 160, 44);
        assert!(ui.preview_targets.is_empty());
        assert!(ui.select_id("a"));
        render(&mut ui, 100, 44);
        assert!(ui.preview_targets.is_empty());
        // Short screens keep the information only.
        let rendered = text(&render(&mut ui, 160, 18));
        assert!(ui.preview_targets.is_empty());
        assert!(rendered.contains("alpha-server"));
    }
    #[test]
    fn panels_follow_the_chosen_sizes_and_borders_can_be_grabbed() {
        let mut ui = workspace(Theme::default(), View::Tree);
        assert!(ui.select_id("a"));
        let details = |ui: &Ui| {
            *ui.handles
                .iter()
                .find(|h| h.split == Split::Details)
                .unwrap()
        };
        render(&mut ui, 160, 44);
        let splits: Vec<Split> = ui.handles.iter().map(|h| h.split).collect();
        assert_eq!(splits, [Split::Sidebar, Split::Details, Split::Screen]);
        let narrow = details(&ui).area.x;
        let short = ui.preview_rows;
        ui.sizes.details = 70;
        ui.sizes.screen = 85;
        render(&mut ui, 160, 44);
        // A wider Details panel starts further left; its screen is taller.
        assert!(details(&ui).area.x + 30 < narrow);
        assert!(
            ui.preview_rows > short + 5,
            "{short} -> {}",
            ui.preview_rows
        );
        // The list keeps its minimum width however wide Details is.
        assert_eq!(details_width(100, 80), 68);
        assert_eq!(details_width(100, 20), 30);
        // The sidebar follows its size too.
        ui.sizes.sidebar = 36;
        render(&mut ui, 160, 44);
        let sidebar = ui
            .handles
            .iter()
            .find(|h| h.split == Split::Sidebar)
            .unwrap();
        assert_eq!(sidebar.area.x, 35);
    }
    #[test]
    fn a_grabbed_border_is_lit() {
        let mut ui = workspace(
            Theme {
                reduced_motion: true,
                ..Theme::default()
            },
            View::Tree,
        );
        render(&mut ui, 160, 44);
        let handle = *ui
            .handles
            .iter()
            .find(|h| h.split == Split::Details)
            .unwrap();
        let (x, y) = (handle.area.x + 1, handle.area.y + 10);
        assert_ne!(
            render(&mut ui, 160, 44)[(x, y)].fg,
            ui.theme.fg(Tone::Accent).fg.unwrap()
        );
        ui.hovered = Some(Split::Details);
        assert_eq!(
            render(&mut ui, 160, 44)[(x, y)].fg,
            ui.theme.fg(Tone::Accent).fg.unwrap()
        );
    }
    #[test]
    fn focus_shows_the_whole_tab_where_its_backend_places_it() {
        let mut ui = workspace(Theme::default(), View::Focus);
        crate::model::tests::split(&mut ui);
        ui.focus_target = "b".into();
        let rendered = text(&render(&mut ui, 140, 40));
        // The neighbor is previewed in its place, left of the focused one.
        assert!(rendered.contains("preview-ok"));
        assert!(rendered.contains("2 terminals in this tab"));
        assert_eq!(ui.preview_targets, ["a"]);
        let hit = |id: &str| ui.pane_hits.iter().find(|(_, p)| p == id).unwrap().0;
        let (a, b) = (hit("a"), hit("b"));
        assert_eq!(a.right(), b.x, "neighbors share their border");
        assert_eq!(a.y, b.y);
        assert!(a.width > b.width, "60 to 40, as laid out");
        // Zoomed, the focused terminal is alone and nothing else is read.
        ui.zoomed = true;
        let rendered = text(&render(&mut ui, 140, 40));
        assert!(!rendered.contains("preview-ok"));
        assert!(ui.preview_targets.is_empty());
        assert!(ui.pane_hits.is_empty());
    }
    #[test]
    fn details_map_a_split_tab_with_the_selected_terminal_bright() {
        let mut ui = workspace(Theme::default(), View::Tree);
        crate::model::tests::split(&mut ui);
        let layout = ui.snapshot.tree[0].layout.clone().unwrap();
        let lines: Vec<String> = layout_map(&ui, &layout, "b", 40)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!((4..=10).contains(&lines.len()), "{lines:#?}");
        // Left to right as laid out: alpha in a thin frame, beta in a thick one.
        assert!(lines[0].starts_with(" ╭"), "{lines:#?}");
        assert!(lines[0].contains("╮┏"), "{lines:#?}");
        let named = lines.iter().find(|l| l.contains("alpha")).unwrap();
        assert!(named.find("alpha") < named.find("beta"));
        assert!(lines.iter().all(|l| width(l) == 41));
        // Too narrow to draw: nothing rather than a broken frame.
        assert!(layout_map(&ui, &layout, "b", 8).is_empty());
    }
    #[test]
    fn tiles_count_the_terminals_sharing_a_tab() {
        let mut ui = workspace(Theme::default(), View::Tiles);
        for node in ui.snapshot.tree.iter_mut().filter(|n| n.tile.is_some()) {
            node.tile.as_mut().unwrap().split = 3;
        }
        let rendered = text(&render(&mut ui, 140, 40));
        assert!(rendered.contains("⊞3"), "{rendered}");
    }
    #[test]
    fn the_wall_fits_every_terminal_without_paging() {
        let mut ui = workspace(Theme::default(), View::Wall);
        let template = ui.snapshot.tree[1].clone();
        for i in 0..22 {
            let mut node = template.clone();
            node.id = format!("n{i}");
            ui.snapshot.tree.push(node);
        }
        ui.snapshot.observed_at = 1_000.0;
        ui.snapshot.tree[1].active_at = 1_000.0;
        ui.snapshot.tree[1].activity = "working".into();
        let rendered = text(&render(&mut ui, 160, 44));
        // 24 live terminals and a missing pin, all on screen and all previewed.
        assert_eq!(ui.tile_hits.len(), 25);
        assert_eq!(ui.preview_targets.len(), 24);
        assert!(!rendered.contains("page"));
        assert!(rendered.contains("1 live"), "{rendered}");
        assert!(rendered.contains("preview-ok"));
        assert!(ui.tile_hits.iter().all(|(r, _)| r.width >= WALL_CELL.0));
        // The working terminal's age shows on its frame.
        assert!(rendered.contains(" now"));
    }
    #[test]
    fn a_crowded_wall_lists_one_terminal_per_line() {
        let mut ui = workspace(Theme::default(), View::Wall);
        let template = ui.snapshot.tree[1].clone();
        for i in 0..300 {
            let mut node = template.clone();
            node.id = format!("n{i}");
            ui.snapshot.tree.push(node);
        }
        render(&mut ui, 100, 30);
        assert!(ui.tile_hits.iter().all(|(r, _)| r.height == 1));
        assert!(ui.preview_targets.is_empty(), "lines are not previewed");
        // The selection stays in sight past the first screenful.
        ui.select_tile(250);
        render(&mut ui, 100, 30);
        assert!(ui.tile_hits.iter().any(|&(_, i)| i == 250));
    }
    #[test]
    fn larger_tiles_fit_fewer_to_a_page() {
        let mut ui = workspace(Theme::default(), View::Tiles);
        let template = ui.snapshot.tree[1].clone();
        for i in 0..30 {
            let mut node = template.clone();
            node.id = format!("n{i}");
            ui.snapshot.tree.push(node);
        }
        render(&mut ui, 160, 44);
        let small = ui.tiles_per_page;
        ui.sizes.tiles = 200;
        render(&mut ui, 160, 44);
        assert!(
            ui.tiles_per_page < small,
            "{small} -> {}",
            ui.tiles_per_page
        );
        assert!(ui.tile_hits[0].0.width >= TILE_WIDTH * 2 - 1);
    }
    #[test]
    fn tiles_page_instead_of_shrinking_below_the_minimum() {
        let mut ui = workspace(Theme::default(), View::Tiles);
        let template = ui.snapshot.tree[1].clone();
        for i in 0..30 {
            let mut node = template.clone();
            node.id = format!("n{i}");
            ui.snapshot.tree.push(node);
        }
        let rendered = text(&render(&mut ui, 120, 36));
        assert!(rendered.contains("page 1/"));
        let first = ui.tile_hits[0].0;
        assert!(first.width >= TILE_WIDTH - 1 && first.height >= TILE_HEIGHT);
        assert_eq!(ui.tile_hits.len(), ui.tiles_per_page);
        ui.select_tile(ui.tiles_per_page);
        render(&mut ui, 120, 36);
        assert_eq!(ui.tile_hits[0].1, ui.tiles_per_page);
    }
    #[test]
    fn focus_without_a_stream_explains_and_always_shows_the_way_back() {
        let mut ui = workspace(Theme::default(), View::Focus);
        let rendered = text(&render(&mut ui, 120, 36));
        assert!(rendered.contains("NOT CONNECTED"));
        assert!(rendered.contains("Esc returns"));
        assert!(rendered.contains("watches it read-only"));
        assert!(ui.quick_hits.iter().any(|(_, id)| id == "detach"));
    }
    #[test]
    fn workspace_views_render_at_every_size_in_ascii_and_no_color() {
        for view in View::ALL {
            for (w, h) in [(44, 14), (60, 16), (90, 24), (160, 48)] {
                let mut ui = workspace(Theme::default(), view);
                render(&mut ui, w, h);
            }
            let mut ascii = workspace(
                Theme {
                    ascii: true,
                    ..Theme::default()
                },
                view,
            );
            let rendered = text(&render(&mut ascii, 140, 40));
            let foreign: String = rendered.chars().filter(|c| !c.is_ascii()).collect();
            assert!(foreign.is_empty(), "{view:?}: {foreign}");
            let mut plain = workspace(
                Theme {
                    color: false,
                    ..Theme::default()
                },
                view,
            );
            let buffer = render(&mut plain, 140, 40);
            assert!(
                buffer
                    .content()
                    .iter()
                    .all(|c| matches!(c.fg, Color::Reset) && matches!(c.bg, Color::Reset)),
                "{view:?}"
            );
        }
    }
    #[test]
    fn grid_prefers_terminal_shaped_tiles() {
        let grid = Rect::new(0, 0, 114, 37);
        assert_eq!(grid_shape(grid, 3, 3, 5), (2, 2));
        assert_eq!(grid_shape(grid, 1, 3, 5), (1, 1));
        assert_eq!(grid_shape(grid, 40, 3, 5), (3, 5));
        assert_eq!(focus_size(120, 40), (32, 117));
    }
    #[test]
    fn strips_terminal_controls_and_truncates_by_width() {
        assert_eq!(clean("label\x1b\x07\r\n"), "label\n");
        let theme = Theme::default();
        assert_eq!(fit("abcdef", 4, &theme), "abc…");
        assert_eq!(fit("abc", 4, &theme), "abc");
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        // Indented lines keep their indent, also where they wrap.
        assert_eq!(wrap("a\n  b c d", 5), ["a", "  b c", "  d"]);
    }

    #[test]
    fn every_supported_tool_keeps_its_own_hue() {
        for (name, tone) in [
            ("Claude", Tone::Warn),
            ("codex", Tone::Ok),
            ("grok", Tone::Violet),
            ("hermes", Tone::Accent),
            ("cursor", Tone::Rose),
            ("pi", Tone::Rose),
            ("OpenCode", Tone::Text),
            ("bash", Tone::Info),
        ] {
            assert_eq!(harness_tone(name), tone, "{name}");
        }
    }
    #[test]
    fn tiles_use_full_width_and_wall_has_distinct_overview_and_inspection() {
        let mut tiles = workspace(Theme::default(), View::Tiles);
        let tiled = text(&render(&mut tiles, 160, 44));
        assert!(tiles.tile_hits.iter().map(|(r, _)| r.x).min().unwrap() < 5);
        assert!(!tiled.contains("SELECTED TERMINAL"));
        assert!(tiles.tile_hits.iter().all(|(r, _)| r.height >= TILE_HEIGHT));
        let mut wall = workspace(Theme::default(), View::Wall);
        let overview = text(&render(&mut wall, 160, 44));
        assert!(overview.contains("SELECTED TERMINAL"));
        assert_eq!(wall.tile_hits.len(), tiles.tiles().len());
        assert!(wall.tile_hits.iter().all(|(r, _)| r.height <= 8));
        assert!(
            wall.preview_rows > 8,
            "selected inspection uses the full height"
        );
        assert!(wall.tile_hits.iter().all(|(r, _)| r.right() < 120));
        let narrow = text(&render(&mut wall, 90, 44));
        assert!(
            narrow.contains("SELECTED TERMINAL"),
            "narrow Wall stacks the selected screen"
        );
        assert!(wall.tile_hits.iter().all(|(r, _)| r.height <= 8));
    }
    #[test]
    fn focus_outline_shows_harness_icons_and_working_status() {
        let mut ui = workspace(Theme::default(), View::Focus);
        let node = ui.snapshot.tree.iter_mut().find(|n| n.id == "a").unwrap();
        node.state = "working".into();
        node.tile.as_mut().unwrap().harness = "claude".into();
        render(&mut ui, 140, 40);
        assert!(
            ui.focus_tree_hits
                .iter()
                .any(|(_, id, branch)| id == "a" && !branch)
        );
        let area = ui.focus_tree_area.unwrap();
        let buffer = render(&mut ui, 140, 40);
        let outline: String = (area.top()..area.bottom())
            .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
            .map(|position| buffer[position].symbol())
            .collect();
        assert!(outline.contains(ui.theme.icon("claude")));
        assert!(outline.contains('+'));
    }
}
