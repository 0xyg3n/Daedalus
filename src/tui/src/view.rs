mod workspaces;
#[cfg(test)]
use workspaces::{
    TILE_HEIGHT, TILE_WIDTH, WALL_CELL, cube_color, cube_faces, cube_slide, cube_turn, grid_shape,
};
pub(crate) use workspaces::{cube_groups, cube_workspace};
use workspaces::{
    cube_panel, detail_tip, focus_panel, focus_screen, focus_structure, session_field,
    session_meta, session_name, terminal_status, tiles_panel, wall_panel,
};
mod details;
mod dialogs;
mod overview;
use crate::backdrop::{self, Look};
use crate::embed::{Mode, Phase};
use crate::model::{
    COMMANDS, Handle, Kind, Node, OVERVIEW, Row, Sizes, Splash, Split, Ui, View, WORKSPACES,
};
use crate::neon;
use crate::theme::{self, Theme, Tone};
use crate::ui_text::{clean, fit, input_tail, width, wrap};
use details::inspector;
#[cfg(test)]
use details::layout_map;
use dialogs::{dialog, options_panel};
use overview::overview;
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
    ("Settings", "options", "System"),
    ("Hooks", "hooks", "Connect"),
    ("Activity", "activity", "System"),
    ("Instances", "instances", "System"),
    ("Armonia", "armonia", "System"),
    ("Help", "help", "System"),
    ("Commands", "commands", "Find"),
];

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
fn page_hint(page: usize) -> &'static str {
    match page {
        0 => "Open a workspace to start work, or finish the setup steps below.",
        1 => "Select a terminal to watch, attach, save a checkpoint or assign a chat.",
        2 => "Save a checkpoint from a terminal, then resume, fork or hand off its context.",
        3 => "Connect a bot, authorize a chat, choose its pane and start the connection.",
        4 => "Manage connections and harness profiles here. Display preferences are in Options.",
        5 => "Install a profile for a running harness, then activate its native hooks.",
        6 => "Inspect action receipts, delivery outcomes and component diagnostics.",
        7 => "Choose the configuration to manage. Each instance keeps its own connections.",
        8 => "Inspect the optional identity registry before previewing organization changes.",
        9 => "Read the everyday controls or open the complete command reference.",
        _ => "Search by action, command or alias. Details show where each command runs.",
    }
}
fn empty_page(page: usize) -> (&'static str, &'static str) {
    match page {
        0 => (
            "Your workspace is ready to set up",
            "Open Settings to choose harnesses and connections.",
        ),
        1 => (
            "No terminals to show",
            "Create a shell in a workspace or refresh connected terminal backends.",
        ),
        2 => (
            "No sessions to show",
            "Open an agent terminal and save a checkpoint from its terminal menu.",
        ),
        3 => (
            "No Telegram connections to show",
            "Open Settings to connect a bot and register your DM or group.",
        ),
        4 => (
            "Settings are not available yet",
            "Refresh the configuration or reopen setup from Options.",
        ),
        5 => (
            "No hook profiles to show",
            "Open a harness conversation, then install its standard hooks.",
        ),
        6 => (
            "No activity to show",
            "Action history and delivery receipts are available after work is recorded.",
        ),
        7 => (
            "No instances to show",
            "Show saved configurations or open another private configuration directory.",
        ),
        8 => (
            "Armonia is optional",
            "Configure an existing identity registry to inspect agents and organization.",
        ),
        9 => (
            "Help is not available yet",
            "Refresh this page or use daedalus --help from a shell.",
        ),
        _ => (
            "No commands to show",
            "Refresh the command catalog or use daedalus --help from a shell.",
        ),
    }
}
fn page_count_label(page: usize, count: usize, total: Option<usize>) -> String {
    let (one, many) = match page {
        WORKSPACES => ("node / action", "nodes / actions"),
        COMMANDS => ("command", "commands"),
        4 => ("setting", "settings"),
        9 => ("guide", "guides"),
        _ => ("entry", "entries"),
    };
    let quantity = total.map_or_else(|| count.to_string(), |total| format!("{count} / {total}"));
    format!(
        "{quantity} {}",
        if count == 1 && total.is_none() {
            one
        } else {
            many
        }
    )
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
    let mut seen = std::collections::HashSet::new();
    let unique = pairs
        .iter()
        .copied()
        .filter(|(key, label)| !key.is_empty() && !label.is_empty() && seen.insert(*key))
        .collect::<Vec<_>>();
    let pairs = unique.as_slice();
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
    ui.reset_hit_regions();
    if let Some(embed) = ui.embed.as_mut() {
        embed.origin = None;
    }
    if ui.view.grid()
        && ui
            .embed
            .as_ref()
            .is_some_and(|e| Some(e.plan.node.clone()) != ui.selected_terminal())
    {
        ui.detach_inline();
    }
    if splash(f, ui) || saver(f, ui) {
        return;
    }
    let theme = ui.theme;
    let area = f.area();
    f.render_widget(Block::default().style(theme.base()), area);
    if area.width < 44 || area.height < 14 {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "DAEDALUS",
                    theme.fg(Tone::Accent).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from("Enlarge this pane to at least 44 x 14."),
                Line::from(Span::styled(
                    if ui.embed.is_some() {
                        "Input paused. Ctrl+] then D detaches."
                    } else {
                        "q Exit"
                    },
                    theme.fg(Tone::Muted),
                )),
            ]),
            area,
        );
        return;
    }
    // Zoomed Focus is the terminal. A header, key bar or border would hide rows.
    if ui.page == WORKSPACES && ui.view == View::Focus && ui.zoomed {
        let cursor = focus_panel(f, area, ui);
        present(f, ui, area, area, cursor, None);
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
    ui.wall_roster = false;
    // Whatever draws a terminal preview this frame asks for it again.
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
    } else if workspaces && ui.view == View::Cube {
        cube_panel(f, content, ui);
    } else if workspaces && ui.view == View::Wall {
        wall_panel(f, content, ui);
    } else if workspaces && ui.view == View::Focus {
        // The terminal fills the page. A side tree only appears when the
        // window is large enough that the screen still shows every column.
        let show_tree = !ui.zoomed && content.width >= 120 && content.height >= 36;
        if show_tree {
            let width = (content.width / 5).clamp(24, 36);
            let columns = split(
                content,
                Direction::Horizontal,
                vec![Constraint::Length(width), Constraint::Min(80)],
            );
            focus_structure(f, columns[0], ui);
            cursor = focus_panel(f, columns[1], ui);
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
    if workspaces && ui.view.grid() {
        if let Some(embed) = &ui.embed {
            ui.preview_targets.retain(|id| id != &embed.plan.node);
        }
        if let Some(screen) = ui.inline_screen {
            cursor = focus_screen(f, screen, ui, true);
            if let Some(embed) = &ui.embed {
                let label = if embed.prefix {
                    " Ctrl+] · D detach · arrows navigate · [ ] layout · O observe "
                } else if embed.mode == Mode::Control {
                    " INPUT HERE · Ctrl+] then D detaches "
                } else {
                    " WATCHING · a types here · Esc detaches "
                };
                f.render_widget(
                    Paragraph::new(label).style(ui.theme.fg(Tone::Warn)),
                    Rect::new(screen.x, screen.y.saturating_sub(1), screen.width, 1),
                );
            }
        } else if let Some(embed) = ui.embed.as_mut() {
            embed.origin = None;
            ui.status = "Input paused: enlarge the terminal window to see the selected pane. Ctrl+] then D detaches.".into();
        }
    }
    present(f, ui, area, content, cursor, Some(rows[2]));
}

/// Dialogs, the key bar and the cursor, after the page itself is drawn.
fn present(
    f: &mut Frame,
    ui: &mut Ui,
    area: Rect,
    content: Rect,
    cursor: Option<(u16, u16)>,
    keys: Option<Rect>,
) {
    let theme = ui.theme;
    light_handle(f, ui);
    if let Some(keys) = keys {
        footer(f, keys, ui);
    }
    // Before the page fade and any dialog, so both dim the background too.
    match ui.look {
        Look::Glow => {
            let hue = page_hue(ui);
            ui.backdrop.glow(f.buffer_mut(), area, &theme, hue);
        }
        Look::Off => {}
        look => ui.backdrop.apply(f.buffer_mut(), area, &theme, look),
    }
    crate::select::paint(f.buffer_mut(), ui);
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
    } else if let Some(position) = cursor.filter(|_| ui.binding_search.is_none()) {
        // The attached terminal's own cursor; hidden whenever a dialog is open.
        f.set_cursor_position(position);
    }
    if ui.binding_search.is_some() {
        dim(f, area, &theme);
        crate::shortcuts::draw(f, area, ui);
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
    /// Stable pin key, set only for a missing pin so it can be removed.
    key: String,
    title: String,
    tone: Tone,
    age: String,
    status: String,
    detail: String,
}

/// Pinned terminals, and the most recently active others.
fn side_terminals(ui: &Ui) -> (Vec<SideTerminal>, Vec<SideTerminal>) {
    let entry = |node: &Node| SideTerminal {
        id: node.id.clone(),
        key: String::new(),
        title: session_name(node),
        tone: terminal_status(node).1,
        age: ui.ago(node),
        status: terminal_status(node).0.into(),
        detail: session_meta(node),
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
                    key: pin.key.clone(),
                    title: pin.label.clone(),
                    tone: Tone::Warn,
                    age: pin.status.clone(),
                    status: "missing".into(),
                    detail: "Pinned terminal is unavailable. Click to remove the pin.".into(),
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
            let age = fit(&item.status, 9, &theme);
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
                    Span::styled(age, base.patch(theme.fg(item.tone))),
                    Span::styled(" ", base),
                ])),
                rect,
            );
            if hovered(ui, rect) {
                ui.tooltip = Some(detail_tip(
                    rect,
                    format!("{} · {} · {}", item.title, item.detail, item.status),
                ));
            }
            if live {
                ui.side_hits.push((rect, item.id));
            } else if !item.key.is_empty() {
                ui.side_hits.push((rect, format!("unpin:{}", item.key)));
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
    let plus = Rect::new(area.x, area.y + 1, room, 1);
    if plus.bottom() <= area.bottom() {
        let over = hovered(ui, plus);
        let base = if over {
            theme.selected(0.45)
        } else {
            theme.surface()
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(" {}", theme.glyph("+", "+")),
                base.patch(theme.fg(Tone::Accent)),
            ))
            .style(base),
            plus,
        );
        ui.quick_hits.push((plus, "terminal.new".into()));
        if over {
            ui.tooltip = Some((
                Rect::new(plus.right() + 1, plus.y, 0, 1),
                "New terminal".into(),
            ));
        }
    }
    let fade = ease(since(ui.paged_at, &theme, 0.18));
    let mut y = area.y + 2;
    let mut last = String::new();
    let tip = |rect: Rect, label: String| (Rect::new(rect.right() + 1, rect.y, 0, 1), label);
    for i in ui.navigation_order() {
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
        if active {
            neon::sweep(
                f.buffer_mut(),
                rect,
                &theme,
                since(ui.paged_at, &theme, 0.32),
                i as u64,
            );
        }
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
        if view == ui.view {
            neon::sweep(
                f.buffer_mut(),
                rect,
                &theme,
                since(ui.paged_at, &theme, 0.32),
                view as u64,
            );
        }
        ui.view_hits.push((rect, view));
        at += w + 1;
    }
    at.saturating_sub(x)
}

/// One-key actions for the selected terminal, with click targets.
// Collect local actions for the single global shortcut bar.
fn quick_bar(_f: &mut Frame, _area: Rect, ui: &mut Ui, tail: &[(&str, &str, &str)]) {
    if ui.selected_terminal().is_none() {
        return;
    }
    let mut items: Vec<_> = ui
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
    for item in items {
        if !ui
            .quick_items
            .iter()
            .any(|(key, _, id)| *key == item.0 || *id == item.2)
        {
            ui.quick_items.push(item);
        }
    }
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
    let mut light = |x: u16, y: u16| {
        let cell = &mut buf[(x, y)];
        if !matches!(
            cell.symbol(),
            "─" | "│" | "╭" | "╮" | "╰" | "╯" | "+" | "-" | "|"
        ) {
            return;
        }
        let fx = (x - area.left()) as f32 / (right - area.left()) as f32;
        let fy = (y - area.top()) as f32 / (bottom - area.top()) as f32;
        let level = if y == area.top() {
            1.0 - 0.55 * fx
        } else {
            (1.0 - fy).powf(1.8) * if x == area.left() { 1.0 } else { 0.6 }
        };
        cell.fg = theme.mix(theme::FAINT, hue, strength * level);
    };
    // Touch the perimeter only; large terminal panes have no interior glow.
    for x in area.left()..=right {
        light(x, area.top());
        light(x, bottom);
    }
    for y in area.top() + 1..bottom {
        light(area.left(), y);
        light(right, y);
    }
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
    let pages = ui.navigation_order();
    let full: usize = pages
        .iter()
        .copied()
        .map(|i| {
            let (title, icon, _) = page_meta(ui, i);
            width(&shortcut(i))
                + width(theme.icon(&icon))
                + if i == ui.page { width(&title) + 4 } else { 3 }
        })
        .sum();
    let digits = full <= area.width as usize;
    let mut x = area.x;
    for i in pages {
        let (title, icon, category) = page_meta(ui, i);
        let text = match (i == ui.page, digits) {
            (true, true) => format!(" {} {} {} ", shortcut(i), theme.icon(&icon), title),
            (true, false) => format!(" {} {} ", theme.icon(&icon), title),
            (false, true) => format!(" {}{} ", shortcut(i), theme.icon(&icon)),
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
    for i in ui.navigation_order() {
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
    if y < inner.bottom() {
        let rect = Rect::new(inner.x, y, inner.width, 1);
        let base = if hovered(ui, rect) {
            theme.selected(0.45)
        } else {
            theme.surface()
        };
        let label = "New terminal";
        let shortcut = "N";
        let pad = (inner.width as usize).saturating_sub(5 + width(label) + width(shortcut) + 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!("  {} ", theme.glyph("+", "+")),
                    base.patch(theme.fg(Tone::Accent)),
                ),
                Span::styled(label, base.patch(theme.fg(Tone::Text))),
                Span::styled(" ".repeat(pad), base),
                Span::styled(shortcut, base.patch(theme.fg(Tone::Faint))),
                Span::styled(" ", base),
            ])),
            rect,
        );
        if hovered(ui, rect) {
            ui.tooltip = Some(detail_tip(
                rect,
                "Open a shell in the current Herdr, tmux, or native terminal".into(),
            ));
        }
        ui.quick_hits.push((rect, "terminal.new".into()));
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
    let count_label = page_count_label(
        ui.page,
        count,
        (!ui.filter.trim().is_empty()).then(|| ui.available_count()),
    );
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
            format!(" {count_label} "),
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
            .saturating_sub(width(&format!(" {count_label} ")) as u16 + 6);
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
    if !workspaces && inner.height >= 22 && !ui.switching {
        let hints = wrap(page_hint(ui.page), inner.width.saturating_sub(4) as usize);
        let height = hints.len().min(2) as u16;
        let lines: Vec<_> = hints
            .into_iter()
            .take(usize::from(height))
            .enumerate()
            .map(|(i, hint)| {
                Line::from(vec![
                    Span::styled(
                        if i == 0 {
                            format!(" {} ", theme.glyph("›", ">"))
                        } else {
                            "   ".into()
                        },
                        theme.fg(tone),
                    ),
                    Span::styled(hint, theme.fg(Tone::Muted)),
                ])
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).style(theme.base()),
            Rect::new(inner.x, y, inner.width, height),
        );
        y += height + 1;
    }
    let peek_height = if peek && ui.prefs.details && inner.height >= 16 {
        if inner.height >= 24 { 6 } else { 4 }
    } else {
        0
    };
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
        let area = Rect::new(inner.x, list.bottom(), inner.width, peek_height);
        selection_peek(f, area, ui, selected);
    }
}

fn selection_peek(f: &mut Frame, area: Rect, ui: &Ui, row: Option<&Row>) {
    let theme = ui.theme;
    let room = area.width.saturating_sub(2) as usize;
    let Some(row) = row else {
        return;
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!(" {} Details  ", theme.glyph("─", "-")),
            theme.fg(tone_of(ui)),
        ),
        Span::styled(
            fit(&row.title, room.saturating_sub(12), &theme),
            theme.fg(Tone::Text),
        ),
    ])];
    match row.kind {
        Kind::Node => {
            if let Some(node) = ui.snapshot.tree.get(row.source) {
                for field in &node.fields {
                    let value = format!("{}: {}", field.key, field.value);
                    lines.extend(wrap(&value, room).into_iter().map(|line| {
                        Line::from(Span::styled(format!(" {line}"), theme.fg(Tone::Muted)))
                    }));
                    if lines.len() >= area.height as usize {
                        break;
                    }
                }
            }
        }
        Kind::Action => {
            if let Some(item) = ui
                .section()
                .and_then(|section| section.items.get(row.source))
            {
                let mut badges = item.badges.clone();
                if !item.status.0.is_empty() {
                    badges.insert(0, item.status.clone());
                }
                lines.extend(details::badge_chips(&theme, &badges, room));
                lines.extend(wrap(&item.note, room).into_iter().map(|line| {
                    Line::from(Span::styled(format!(" {line}"), theme.fg(Tone::Muted)))
                }));
                if !item.context.is_empty() {
                    lines.push(Line::from(Span::styled(
                        fit(
                            &format!(" Terminal menu: {}", item.context),
                            area.width as usize,
                            &theme,
                        ),
                        theme.fg(Tone::Info),
                    )));
                }
            }
        }
        Kind::Entry => {
            if let Some(entry) = ui.snapshot.catalog.get(row.source) {
                lines.push(Line::from(Span::styled(
                    format!(" {}", entry.interface),
                    theme.fg(Tone::Violet),
                )));
                lines.extend(wrap(&entry.description, room).into_iter().map(|line| {
                    Line::from(Span::styled(format!(" {line}"), theme.fg(Tone::Muted)))
                }));
            }
        }
        Kind::Header => {}
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn search(f: &mut Frame, area: Rect, ui: &mut Ui, title: &str) {
    ui.search_area = Some(area);
    let theme = ui.theme;
    let over = hovered(ui, area);
    let field = if theme.color {
        Style::default().bg(theme.rgb(if over || ui.searching {
            theme::SELECT
        } else {
            theme::RAISED
        }))
    } else {
        Style::default()
    };
    let icon = theme.icon("search");
    let line = if ui.searching {
        let prefix = format!(" {icon} {}", if ui.switching { "Switch: " } else { "" });
        let query = input_tail(
            &clean(&ui.filter),
            (area.width as usize).saturating_sub(width(&prefix) + 1),
        );
        Line::from(vec![
            Span::styled(prefix, field.patch(theme.fg(Tone::Accent))),
            Span::styled(query, field.patch(theme.fg(Tone::Text))),
            Span::styled(theme.glyph("▏", "_"), field.patch(theme.fg(Tone::Accent))),
        ])
    } else if !ui.filter.is_empty() {
        Line::from(vec![
            Span::styled(format!(" {icon} "), field.patch(theme.fg(Tone::Accent))),
            Span::styled(
                fit(&ui.filter, (area.width as usize).saturating_sub(16), &theme),
                field.patch(theme.fg(Tone::Accent).add_modifier(Modifier::BOLD)),
            ),
            Span::styled("   Esc clears", field.patch(theme.fg(Tone::Faint))),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                format!(" {icon} "),
                field.patch(theme.fg(if over { Tone::Accent } else { Tone::Faint })),
            ),
            Span::styled(
                format!(
                    "Search {}{}   click or /",
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
    if ["error", "failed", "exited"].iter().any(|w| s.contains(w)) {
        Tone::Rose
    } else if ["block", "permission", "input", "wait"]
        .iter()
        .any(|w| s.contains(w))
    {
        Tone::Warn
    } else if ["work", "busy", "running", "think"]
        .iter()
        .any(|w| s.contains(w))
    {
        Tone::Ok
    } else if s == "shell" || ["idle", "ready", "done"].iter().any(|w| s.contains(w)) {
        Tone::Muted
    } else if s == "unknown" {
        Tone::Faint
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

/// Shared node presentation for Tree and Focus: identical role colours and labels.
fn tree_node_spans(
    theme: &Theme,
    row: &Row,
    node: Option<&Node>,
    base: Style,
    active: bool,
    w: usize,
    tone: Tone,
) -> Vec<Span<'static>> {
    let bar = Span::styled(
        if active { theme.glyph("▌", ">") } else { " " },
        base.fg(theme.rgb(tone.rgb())),
    );
    let text = |style: Style| {
        let style = base.patch(style);
        if active {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    };
    let mut first = vec![bar];
    let pulse = theme.breathe(1.6);
    let kind = node.map(|n| n.kind.as_str()).unwrap_or("");
    let state = node.map(|n| n.state.as_str()).unwrap_or("");
    let mut state_tone = node
        .filter(|n| n.tile.is_some())
        .map(|n| terminal_status(n).1)
        .unwrap_or_else(|| node_tone(state, kind));
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
        icon_style = icon_style.fg(theme.mix(theme::FAINT, theme::AMBER, 0.4 + 0.6 * pulse));
    }
    first.push(Span::styled(
        format!(
            "{} ",
            node.and_then(|n| n.tile.as_ref())
                .filter(|t| !t.harness.is_empty())
                .map_or_else(|| node_icon(theme, kind, state), |t| theme.icon(&t.harness))
        ),
        icon_style,
    ));
    let left: usize = first.iter().map(|s| s.width()).sum();
    let note_text = if row.note.is_empty() {
        node.filter(|n| n.tile.is_some())
            .map(|n| terminal_status(n).0)
            .unwrap_or("")
    } else {
        &row.note
    };
    let note = yielding_note(note_text, w.saturating_sub(left)).to_owned();
    let room = w.saturating_sub(left + width(&note) + 3);
    let label = fit(&row.title, room.max(8), theme);
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
        first.extend(part_spans(theme, parts, room.max(8), &text));
    } else {
        first.push(Span::styled(label, text(theme.fg(Tone::Text))));
    }
    if kind != "pane" && kind != "agent session" && node.is_none_or(|n| n.tile.is_none()) {
        state_tone = Tone::Faint;
    }
    let used: usize = first.iter().map(|s| s.width()).sum();
    let pad = w.saturating_sub(used + width(&note) + 1);
    first.push(Span::styled(" ".repeat(pad), base));
    first.push(Span::styled(note, base.patch(theme.fg(state_tone))));
    first
}

fn render_list(f: &mut Frame, area: Rect, ui: &mut Ui, rows: &[Row], two_line: bool) {
    let theme = ui.theme;
    if rows.iter().all(|row| row.kind == Kind::Header) {
        let (title, note) = if ui.busy && ui.snapshot.sections.is_empty() {
            (
                "Loading your workspace",
                "Discovering terminals and reading this instance's configuration.",
            )
        } else if ui.filter.is_empty() {
            empty_page(ui.page)
        } else {
            (
                "No matches",
                "Try a shorter name or press Esc to clear the search.",
            )
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    format!(
                        " {} ",
                        theme.icon(if ui.filter.is_empty() {
                            "info"
                        } else {
                            "search"
                        })
                    ),
                    theme.fg(tone_of(ui)),
                ),
                Span::styled(
                    fit(title, area.width.saturating_sub(4) as usize, &theme),
                    theme.fg(Tone::Text).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
        ];
        lines.extend(
            wrap(note, area.width.saturating_sub(3) as usize)
                .into_iter()
                .map(|line| Line::from(Span::styled(format!(" {line}"), theme.fg(Tone::Muted)))),
        );
        f.render_widget(Paragraph::new(lines), area);
        return;
    }
    let tone = tone_of(ui);
    let detail_row = (ui.page == WORKSPACES && area.height >= 12)
        .then(|| {
            rows.get(ui.selected)
                .filter(|r| r.kind == Kind::Node)
                .filter(|r| {
                    ui.snapshot
                        .tree
                        .get(r.source)
                        .is_some_and(|n| n.tile.is_some())
                })
                .map(|_| ui.selected)
        })
        .flatten();
    let height = |i: usize, top: bool| -> u16 {
        match rows[i].kind {
            Kind::Header if i > 0 && !top => 2,
            Kind::Action if two_line => 2,
            Kind::Node if detail_row == Some(i) => 2,
            _ => 1,
        }
    };
    let selected = ui.selected.min(rows.len() - 1);
    ui.offset = ui.offset.min(selected);
    if ui.offset == selected && selected > 0 && rows[selected - 1].kind == Kind::Header {
        ui.offset = selected - 1;
    }
    let mut used: usize = (ui.offset..=selected)
        .map(|i| height(i, i == ui.offset) as usize)
        .sum();
    while ui.offset < selected && used > area.height as usize {
        used = used.saturating_sub(height(ui.offset, true) as usize);
        ui.offset += 1;
        used = used
            .saturating_sub(height(ui.offset, false) as usize)
            .saturating_add(height(ui.offset, true) as usize);
    }
    let total: usize = (0..rows.len()).map(|i| height(i, i == 0) as usize).sum();
    let scroll = total > area.height as usize;
    let body = Rect {
        width: area.width.saturating_sub(u16::from(scroll)),
        ..area
    };
    let fade = ease(since(ui.moved_at, &theme, 0.16));
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
            } else if hovered(ui, Rect::new(body.x, y, body.width, h)) {
                theme.selected(0.3).remove_modifier(Modifier::BOLD)
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
                    first = tree_node_spans(&theme, row, node, base, active, w, tone);
                    if detail_row == Some(i)
                        && let Some(node) = node
                    {
                        let folder = session_field(node, "Folder").unwrap_or("");
                        let detail = format!(
                            "{}{}",
                            session_meta(node),
                            if folder.is_empty() {
                                String::new()
                            } else {
                                format!(" · {folder}")
                            }
                        );
                        second = Some(Line::from(vec![
                            bar.clone(),
                            Span::styled(
                                format!("     {}", fit(&detail, w.saturating_sub(7), &theme)),
                                base.patch(theme.fg(Tone::Muted)),
                            ),
                        ]));
                    }
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
                                base.patch(theme.fg(Tone::Muted))
                                    .remove_modifier(Modifier::BOLD),
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
        crate::ui_scroll::scrollbar(
            f,
            Rect::new(area.right() - 1, area.y, 1, area.height),
            crate::ui_scroll::Window::new(total, track, before),
            &theme,
            tone,
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

/// The key that opens page `i`: digits 1 to 9, then 0, and Ctrl+P for Commands.
fn shortcut(i: usize) -> String {
    if i == COMMANDS {
        "^P".to_string()
    } else {
        ((i + 1) % 10).to_string()
    }
}

fn footer(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let theme = ui.theme;
    // Grid and Focus positions do not need a second cloned workspace forest.
    let rows = if ui.page == WORKSPACES && ui.view != View::Tree {
        Vec::new()
    } else {
        ui.rows()
    };
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
        (true, View::Tiles | View::Wall | View::Cube) => {
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
        Some(embed)
            if embed.mode == Mode::Control && ui.inline_screen.is_none() && ui.view.grid() =>
        {
            "Input paused · arrows switch terminals · Esc detaches"
        }
        Some(embed) if embed.mode == Mode::Control && ui.inline_screen.is_none() => {
            "Input paused · selected pane is not visible · Ctrl+] then D detaches"
        }
        Some(embed) if ui.page == WORKSPACES && !ui.status_error => {
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
                    None => "Watching: nothing you type reaches it. a attaches; Esc detaches.",
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
    } else if ui.page == WORKSPACES && control {
        vec![
            ("^] D", "Detach"),
            ("^] [ ]", "View"),
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
        let mut pairs = vec![("a", "Attach"), ("w", "Watch"), ("Esc", "Back")];
        if split {
            pairs.push((move4, "Pane"));
            pairs.push(("Z", if ui.zoomed { "Whole tab" } else { "Zoom" }));
            pairs.push(("F11", "Full"));
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
            ("F11", "Full"),
            ("[ ]", "View"),
            ("PgUp/Dn", "Page"),
            ("Alt+↑↓", "Workspace"),
            ("o", "Order"),
            ("P", "Pinned only"),
            ("+ -", "Tile size"),
            ("u", "Forget missing pin"),
            ("L", "Save layout"),
            ("/", "Search"),
            ("q", "Quit"),
        ]
    } else if ui.page == WORKSPACES && matches!(ui.view, View::Wall | View::Cube) {
        vec![
            (
                move4,
                if ui.view == View::Cube {
                    "Rotate"
                } else {
                    "Move"
                },
            ),
            (enter, "Menu"),
            ("z", "Focus"),
            ("F11", "Full"),
            (
                if ui.view == View::Cube {
                    "Alt+arrows"
                } else {
                    ""
                },
                if ui.view == View::Cube {
                    "Cubes/terminals"
                } else {
                    ""
                },
            ),
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
    // Queued actions own their aliases; show each action once in the footer.
    pairs.retain(|(_, label)| {
        if label.is_empty() {
            return false;
        }
        let id = match *label {
            "Attach" => "terminal.attach",
            "Watch" => "terminal.watch",
            "Back" => "detach",
            "More" | "Menu" => "more",
            _ => return true,
        };
        !ui.quick_items.iter().any(|(_, _, drawn)| drawn == id)
    });
    if control && ui.embed.as_ref().is_some_and(|e| e.prefix) {
        pairs = vec![
            ("D", "Detach"),
            ("O", "Observe"),
            ("[ ]", "View"),
            (move4, "Pane"),
            ("M", "More"),
        ];
    }
    if control && !ui.embed.as_ref().is_some_and(|e| e.prefix) {
        pairs.insert(0, ("^] K", "Switch"));
    }
    if !ui.searching && !control {
        if ui.page == WORKSPACES {
            pairs.insert(0, ("n", "New workspace"));
        }
        pairs.insert(0, ("F1", "Bindings"));
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
    let mut items = Vec::<(String, String, String)>::new();
    if control && !ui.searching {
        items.push(("^] G".into(), "Telegram".into(), "telegram.route".into()));
    }
    let priority = |label: &str| {
        matches!(
            label,
            "Switch"
                | "Move"
                | "Rotate"
                | "Pane"
                | "Cubes/terminals"
                | "Focus"
                | "Back"
                | "View"
                | "Options"
                | "Hide keys"
        )
    };
    for (key, label) in pairs.iter().filter(|(_, label)| *label == "Switch") {
        items.push((key.to_string(), label.to_string(), String::new()));
    }
    if !ui.searching && !control {
        items.extend(
            ui.quick_items
                .iter()
                .filter(|(_, _, id)| id == "more" || id == "detach")
                .cloned(),
        );
    }
    for (key, label) in pairs
        .iter()
        .filter(|(_, label)| priority(label) && *label != "Switch")
    {
        items.push((key.to_string(), label.to_string(), String::new()));
    }
    if !ui.searching && !control {
        items.extend(
            ui.quick_items
                .iter()
                .filter(|(_, _, id)| id != "more" && id != "detach")
                .cloned(),
        );
    }
    for (key, label) in pairs.iter().filter(|(_, label)| !priority(label)) {
        items.push((key.to_string(), label.to_string(), String::new()));
    }
    let mut x = area.x + 1;
    let mut seen = std::collections::HashSet::new();
    for (key, label, id) in items {
        if !seen.insert(key.clone()) {
            continue;
        }
        let cap = format!(" {key} ");
        let text = format!(" {label} ");
        let w = (width(&cap) + width(&text)) as u16;
        if x + w > area.right() {
            break;
        }
        let rect = Rect::new(x, area.y + 1, w, 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(cap, theme.keycap()),
                Span::styled(text, theme.fg(Tone::Muted)),
            ])),
            rect,
        );
        if hovered(ui, rect) && theme.animated() {
            neon::sweep(
                f.buffer_mut(),
                rect,
                &theme,
                (theme.clock() * 0.8).fract(),
                u64::from(x),
            );
        }
        if !id.is_empty() {
            ui.quick_hits.push((rect, id));
        }
        x += w + 1;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Dialog, Entry, Field, Item, Node, Order, Section, Snapshot, Split, TileRow, View,
    };
    use crate::options;
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
    fn editable_tail_respects_wide_characters_and_cursor_space() {
        assert_eq!(input_tail("abc界語", 4), "界語");
        assert_eq!(input_tail("abc界語", 3), "語");
        assert_eq!(input_tail("界", 1), "");
        assert_eq!(input_tail("abc", 0), "");
        for room in 0..50 {
            assert!(width(&input_tail("e\u{301} Ελληνικά 日本語 input", room)) <= room);
        }
    }

    #[test]
    fn compact_forms_keep_choices_and_input_feedback_inside_the_dialog() {
        let mut ui = fixture(Theme {
            reduced_motion: true,
            ..Theme::default()
        });
        for (w, h) in [(44, 14), (60, 18), (80, 24), (160, 48)] {
            ui.dialog = Some(Dialog {
                mode: "choose".into(),
                title: "Choose a chat".into(),
                description: "Select the chat to receive this terminal's replies.".into(),
                options: vec![
                    ("dm".into(), "Direct messages".into(), "Owner chat".into()),
                    (
                        "group".into(),
                        "Project group".into(),
                        "Authorized group chat".into(),
                    ),
                ],
                selected: 1,
                ..Dialog::default()
            });
            assert!(
                text(&render(&mut ui, w, h)).contains("Project group"),
                "{w}x{h}"
            );
            ui.dialog = Some(Dialog {
                mode: "input".into(),
                title: "Bot token".into(),
                secret: true,
                input: "never-render-this-token".into(),
                ..Dialog::default()
            });
            let output = text(&render(&mut ui, w, h));
            assert!(!output.contains("never-render-this-token"));
            assert!(output.contains("23 characters"), "{w}x{h}: {output}");
        }
    }

    #[test]
    fn empty_dialogs_explain_their_state_and_do_not_offer_selection() {
        let mut ui = fixture(Theme::default());
        ui.dialog = Some(Dialog {
            mode: "choose".into(),
            title: "Choices".into(),
            ..Dialog::default()
        });
        let output = text(&render(&mut ui, 100, 30));
        assert!(output.contains("No choices available"));
        assert!(!output.contains("Select"));
        assert!(!output.contains("PgUp/PgDn"));
        ui.dialog = Some(Dialog {
            mode: "output".into(),
            title: "Result".into(),
            ..Dialog::default()
        });
        let output = text(&render(&mut ui, 100, 30));
        assert!(output.contains("No output was returned"));
        assert!(!output.contains("PgUp/PgDn"));
    }

    #[test]
    fn header_only_workspace_is_presented_as_empty() {
        let mut ui = fixture(Theme::default());
        ui.snapshot.tree.clear();
        ui.snapshot.sections[WORKSPACES].items.clear();
        assert!(ui.rows().iter().all(|row| row.kind == Kind::Header));
        assert!(text(&render(&mut ui, 120, 40)).contains("No terminals to show"));
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
    fn long_output_scrolls_past_the_old_u16_line_limit() {
        let mut ui = fixture(Theme {
            reduced_motion: true,
            ..Theme::default()
        });
        ui.dialog = Some(Dialog {
            mode: "output".into(),
            title: "Long result".into(),
            text: format!("{}END_OF_RESULT", "v\n".repeat(70_000)),
            scroll: usize::MAX,
            ..Dialog::default()
        });
        assert!(text(&render(&mut ui, 100, 30)).contains("END_OF_RESULT"));
        assert!(ui.dialog.as_ref().unwrap().scroll > 65_535);
    }
    #[test]
    fn shrinking_or_hiding_a_page_drops_preview_targets_and_click_regions() {
        let mut ui = workspace(Theme::default(), View::Tiles);
        render(&mut ui, 160, 48);
        assert!(!ui.preview_targets.is_empty());
        render(&mut ui, 40, 10);
        assert!(ui.preview_targets.is_empty());
        assert!(ui.tile_hits.is_empty());
        assert!(ui.nav_hits.is_empty());
        assert!(ui.search_area.is_none());
    }
    #[test]
    fn long_details_can_be_read_without_moving_the_selected_terminal() {
        let mut ui = fixture(Theme {
            reduced_motion: true,
            ..Theme::default()
        });
        ui.prefs.screen = false;
        ui.snapshot.tree[1].fields = (0..60)
            .map(|i| Field::new(&format!("Field {i}"), &format!("Value {i}"), "text"))
            .collect();
        assert!(ui.select_id("p"));
        let selected = ui.selected;
        let first = text(&render(&mut ui, 160, 30));
        assert!(ui.detail_view.area.is_some());
        assert!(ui.detail_view.max_scroll > 0);
        assert!(!first.contains("Value 59"));
        ui.detail_view.scroll_by(isize::MAX);
        let last = text(&render(&mut ui, 160, 30));
        assert!(last.contains("Value 59"));
        assert_eq!(ui.selected, selected);
        ui.select_id("s");
        render(&mut ui, 160, 30);
        assert_eq!(ui.detail_view.scroll, 0);
        render(&mut ui, 80, 24);
        assert!(
            ui.detail_view.area.is_none(),
            "a hidden panel must not intercept input"
        );
    }
    #[test]
    fn compact_action_pages_keep_status_and_terminal_context_readable() {
        for page in 2..10 {
            let mut ui = fixture(Theme {
                reduced_motion: true,
                ..Theme::default()
            });
            let item = &mut ui.snapshot.sections[page].items[0];
            item.status = ("Ready for review".into(), "info".into());
            item.context = "Session action".into();
            item.note = "Review this action's exact target before continuing.".into();
            ui.navigate(page);
            let output = text(&render(&mut ui, 100, 30));
            assert!(output.contains("Ready for review"), "page {page}");
            assert!(
                output.contains("Terminal menu: Session action"),
                "page {page}"
            );
        }
    }
    #[test]
    fn overview_guide_matches_sidebar_groups_and_preserves_exact_page_targets() {
        let mut ui = fixture(Theme {
            reduced_motion: true,
            ..Theme::default()
        });
        ui.navigate(OVERVIEW);
        render(&mut ui, 180, 48);
        let pages = ui
            .nav_hits
            .iter()
            .filter(|(rect, _)| rect.x > 24)
            .map(|(_, page)| *page)
            .collect::<Vec<_>>();
        assert_eq!(pages, [1, 2, 3, 5, 4, 6, 7, 8, 9, 10]);
    }
    #[test]
    fn repeated_backend_status_badges_are_shown_once_in_the_selection_summary() {
        let lines = details::badge_chips(
            &Theme::default(),
            &[
                ("Unavailable".into(), "warn".into()),
                ("unavailable".into(), "warn".into()),
                ("Read-only".into(), "ok".into()),
            ],
            40,
        );
        let shown = lines
            .into_iter()
            .map(|line| line.to_string())
            .collect::<String>();
        assert_eq!(shown.to_lowercase().matches("unavailable").count(), 1);
        assert!(shown.contains("Read-only"));
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
                x + 7 <= area.width
                    && (0..7)
                        .all(|i| buffer[(x + i, y)].symbol() == &"Running"[i as usize..=i as usize])
                    && (0..area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                        .contains("Action title")
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
    fn compact_overview_keeps_the_setup_action_visible() {
        let mut ui = fixture(Theme::default());
        ui.snapshot.overview = serde_json::from_value(serde_json::json!({
            "connections":[{"label":"Telegram","state":"unknown","detail":"Not configured"}],
            "setup":[
                {"label":"AI tools","done":false,"detail":"Select harnesses"},
                {"label":"Terminals","done":true,"detail":"10 found"},
                {"label":"Telegram","done":false,"detail":"Optional"},
                {"label":"Checkpoints","done":false,"detail":"None saved"}
            ]
        }))
        .unwrap();
        ui.navigate(0);
        let rendered = text(&render(&mut ui, 80, 24));
        assert!(rendered.contains("Action title"));
        assert!(!ui.list_hits.is_empty());
        assert!(rendered.contains("▱▰▱▱"));
    }
    #[test]
    fn compact_navigation_uses_real_help_and_command_shortcuts() {
        let mut ui = fixture(Theme::default());
        ui.navigate(9);
        let rendered = render(&mut ui, 96, 28);
        let tabs: String = (0..96).map(|x| rendered[(x, 1)].symbol()).collect();
        assert!(tabs.contains("0 ? Help"), "{tabs}");
        assert!(tabs.contains("^P"), "{tabs}");
        assert!(!tabs.contains("10?"));
        assert!(!tabs.contains("11"));
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
        assert_eq!(pages[..FALLBACK.len()], [0, 1, 2, 3, 5, 4, 6, 7, 8, 9, 10]);
        assert_eq!(pages[FALLBACK.len()..], [WORKSPACES, WORKSPACES, 3, 2, 6]);
    }
    #[test]
    fn long_unicode_words_stay_inside_their_display_columns() {
        for max in 1..=12 {
            for input in [
                "界面工作区",
                "αβγδεζηθ",
                "path/界面/terminal",
                "a\u{301}bcdefgh",
            ] {
                let lines = wrap(input, max);
                assert!(
                    lines.iter().all(|line| width(line) <= max),
                    "{max}: {lines:?}"
                );
                if max >= 2 {
                    assert_eq!(lines.concat(), input);
                }
            }
        }
    }
    #[test]
    fn output_review_keeps_scroll_position_separate_from_shortcuts() {
        let mut ui = fixture(Theme::default());
        ui.dialog = Some(Dialog {
            mode: "output".into(),
            title: "Review changes".into(),
            text: (0..80).map(|n| format!("Review item {n}\n")).collect(),
            ..Dialog::default()
        });
        let rendered = render(&mut ui, 120, 40);
        let rows: Vec<String> = (0..40)
            .map(|y| (0..120).map(|x| rendered[(x, y)].symbol()).collect())
            .collect();
        let footer = rows
            .iter()
            .find(|line| line.contains("/ 81"))
            .expect("visible scroll position");
        assert!(footer.contains("Close"), "{footer}");
        assert!(footer.find("Close").unwrap() < footer.find("/ 81").unwrap());
    }
    #[test]
    fn empty_screens_offer_a_next_step_without_inventing_records() {
        for page in [2, 3, 4, 5, 6, 7, 8, 9] {
            let mut ui = fixture(Theme::default());
            ui.snapshot.sections[page].items.clear();
            ui.navigate(page);
            let rendered = text(&render(&mut ui, 140, 44));
            let (title, _) = empty_page(page);
            assert!(rendered.contains(title), "page {page}");
            assert!(ui.list_hits.is_empty());
        }
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
        // beta is pinned, alpha active recently; the missing pin is a remove target.
        let ids: Vec<&str> = ui.side_hits.iter().map(|(_, id)| id.as_str()).collect();
        assert_eq!(ids, ["b", "unpin:k", "a"]);
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
        assert!(rendered.contains("18 characters entered"));
        ui.dialog.as_mut().unwrap().input.clear();
        let empty = text(&render(&mut ui, 100, 30));
        assert!(empty.contains(if cfg!(windows) {
            "Empty · Ctrl+V"
        } else {
            "Empty · paste or type"
        }));
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
    fn cube_workspace_badges_follow_identity_and_survive_terminal_reordering() {
        let mut ui = workspace(Theme::default(), View::Cube);
        ui.snapshot.tree.push(Node {
            id: "ws-one".into(),
            kind: "workspace".into(),
            label: "Project".into(),
            ..Node::default()
        });
        ui.snapshot.tree.push(Node {
            id: "ws-two".into(),
            kind: "tmux session".into(),
            label: "Project".into(),
            ..Node::default()
        });
        let source = ui
            .snapshot
            .tree
            .iter()
            .position(|n| n.tile.is_some())
            .unwrap();
        ui.snapshot.tree[source].parent = "ws-two".into();
        let row = TileRow {
            id: ui.snapshot.tree[source].id.clone(),
            source: Some(source),
            pin: None,
        };
        let badge = cube_workspace(&ui, &row);
        assert_eq!(badge.0, "ws-two");
        assert_eq!(badge.2, "Project");
        let color = cube_color(&ui.theme, &badge.0);
        ui.order = Order::Layout;
        assert_eq!(cube_workspace(&ui, &row), badge);
        assert_eq!(cube_color(&ui.theme, &cube_workspace(&ui, &row).0), color);
        ui.snapshot.tree[source].parent = "ws-one".into();
        assert_ne!(cube_workspace(&ui, &row).1, badge.1);
        // Broken ancestry is bounded and explicitly labelled.
        ui.snapshot.tree[source].parent = ui.snapshot.tree[source].id.clone();
        assert_eq!(cube_workspace(&ui, &row).2, "Unassigned");
    }
    #[test]
    fn cube_glitch_preserves_terminal_text_and_reduced_motion_is_static() {
        let mut ui = workspace(
            Theme {
                reduced_motion: true,
                ..Theme::default()
            },
            View::Cube,
        );
        ui.moved_at = None;
        let still = render(&mut ui, 160, 50);
        ui.moved_at = Some(Instant::now());
        let reduced = render(&mut ui, 160, 50);
        assert_eq!(still, reduced);
        assert!(text(&reduced).contains("WS"));
        assert!(text(&reduced).contains("FRONT"));
    }
    #[test]
    fn cubes_group_workspaces_and_keep_neighbours_when_front_terminal_changes() {
        let mut ui = workspace(
            Theme {
                reduced_motion: true,
                ..Theme::default()
            },
            View::Cube,
        );
        ui.snapshot.workspace.pins.clear();
        ui.snapshot.tree.push(Node {
            id: "ws-a".into(),
            kind: "workspace".into(),
            label: "Alpha".into(),
            ..Node::default()
        });
        ui.snapshot.tree.push(Node {
            id: "ws-b".into(),
            kind: "workspace".into(),
            label: "Beta".into(),
            ..Node::default()
        });
        for node in &mut ui.snapshot.tree {
            if node.tile.is_some() {
                node.parent = "ws-a".into();
            }
        }
        let template = ui
            .snapshot
            .tree
            .iter()
            .find(|n| n.tile.is_some())
            .unwrap()
            .clone();
        for i in 0..8 {
            let mut node = template.clone();
            node.id = format!("extra{i}");
            if i == 7 {
                node.parent = "ws-b".into();
            }
            ui.snapshot.tree.push(node);
        }
        let tiles = ui.tiles();
        let groups = cube_groups(&ui, &tiles);
        assert_eq!(groups.len(), 2);
        ui.select_tile(groups[0][0]);
        let before = text(&render(&mut ui, 160, 50));
        assert!(before.contains("Cube 1 of 2") && before.contains("Alpha"));
        let neighbours: Vec<_> = ui.tile_hits.iter().skip(1).map(|(_, i)| *i).collect();
        ui.cube_anchor = Some(tiles[groups[0][0]].id.clone());
        ui.select_tile(groups[0][1]);
        render(&mut ui, 160, 50);
        assert_eq!(
            ui.tile_hits
                .iter()
                .skip(1)
                .map(|(_, i)| *i)
                .collect::<Vec<_>>(),
            neighbours
        );
        assert!(ui.tile_hits.iter().all(|(_, i)| groups[0].contains(i)));
        ui.cube_anchor = None;
        ui.select_tile(groups[1][0]);
        let next = text(&render(&mut ui, 160, 50));
        assert!(next.contains("Cube 2 of 2") && next.contains("Beta"));
        assert_eq!(ui.tile_hits.len(), 1);
    }
    #[test]
    fn cube_directional_motion_stays_inside_the_face_and_settles() {
        let area = Rect::new(5, 8, 90, 32);
        for direction in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            for elapsed in [0, 60, 120, 180, 500] {
                let face = cube_slide(area, elapsed, direction);
                assert_eq!(face.intersection(area), face);
                if elapsed >= 180 {
                    assert_eq!(face, area);
                }
            }
        }
        assert!(cube_slide(area, 0, (-1, 0)).x < cube_slide(area, 0, (1, 0)).x);
    }
    #[test]
    fn cube_turn_finishes_at_full_size_without_moving_the_terminal_rows() {
        let area = Rect::new(4, 5, 90, 30);
        let start = cube_turn(area, 0);
        let middle = cube_turn(area, 90);
        assert!(start.width < middle.width && middle.width < area.width);
        assert_eq!(start.y, area.y);
        assert_eq!(start.height, area.height);
        assert_eq!(cube_turn(area, 180), area);
        assert_eq!(cube_turn(area, 1000), area);
    }
    #[test]
    fn cube_faces_are_bounded_disjoint_and_collapse_on_small_screens() {
        for (w, h) in [(160, 50), (80, 24), (79, 24), (80, 23), (20, 6)] {
            let area = Rect::new(3, 4, w, h);
            for sizes in [
                Sizes::default(),
                Sizes {
                    cube_width: 84,
                    cube_height: 80,
                    ..Sizes::default()
                },
                Sizes {
                    cube_width: 40,
                    cube_height: 30,
                    ..Sizes::default()
                },
            ] {
                let faces = cube_faces(area, sizes);
                assert_eq!(faces.len(), if w >= 80 && h >= 24 { 5 } else { 1 });
                for (i, (_, face)) in faces.iter().enumerate() {
                    assert_eq!(face.intersection(area), *face);
                    for (_, other) in &faces[..i] {
                        assert!(face.intersection(*other).is_empty());
                    }
                }
            }
        }
    }
    #[test]
    fn cube_draws_unique_neighbours_and_keeps_selection_in_front() {
        let mut ui = workspace(Theme::default(), View::Cube);
        let template = ui.snapshot.tree[1].clone();
        for i in 0..8 {
            let mut node = template.clone();
            node.id = format!("cube{i}");
            ui.snapshot.tree.push(node);
        }
        ui.select_tile(4);
        let rendered = text(&render(&mut ui, 160, 50));
        assert!(rendered.contains("Rotate"));
        assert!(!rendered.contains("Unfold/fold all"));
        assert_eq!(ui.tile_hits.len(), 5);
        assert_eq!(ui.tile_hits[0].1, 4);
        let unique: std::collections::HashSet<_> = ui.tile_hits.iter().map(|(_, i)| i).collect();
        assert_eq!(unique.len(), 5);
        assert!(ui.preview_targets.len() <= 5);
        render(&mut ui, 60, 18);
        assert_eq!(ui.tile_hits.len(), 1);
        assert_eq!(ui.tile_hits[0].1, 4);
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
    fn workspace_shortcuts_have_one_row_and_one_menu_entry() {
        for view in View::ALL {
            let mut ui = workspace(Theme::default(), view);
            ui.reveal("a");
            let buffer = render(&mut ui, 190, 40);
            let actions: Vec<_> = ui
                .quick_hits
                .iter()
                .filter(|(_, id)| id == "more" || id == "detach" || id.starts_with("terminal."))
                .collect();
            assert!(!actions.is_empty(), "{view:?}");
            assert!(actions.iter().all(|(rect, _)| rect.y == 39), "{view:?}");
            let footer: String = (0..190).map(|x| buffer[(x, 39)].symbol()).collect();
            assert_eq!(footer.matches("More").count(), 1, "{view:?}");
            assert!(!footer.contains("Menu"), "{view:?}");
            ui.hide_keys = true;
            render(&mut ui, 190, 40);
            assert!(
                !ui.quick_hits
                    .iter()
                    .any(|(_, id)| id == "more" || id.starts_with("terminal."))
            );
        }
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
    fn zoomed_focus_fills_the_pane_without_the_frame() {
        let mut ui = workspace(Theme::default(), View::Focus);
        ui.zoomed = true;
        let rendered = text(&render(&mut ui, 66, 23));
        assert!(rendered.contains("Not connected"), "{rendered}");
        assert!(!rendered.contains("DAEDALUS"), "{rendered}");
        let mut framed = workspace(Theme::default(), View::Focus);
        let framed = text(&render(&mut framed, 66, 23));
        assert!(framed.contains("DAEDALUS"), "{framed}");
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
        // The full fleet stays readable; only the selected large screen is read.
        assert_eq!(ui.tile_hits.len(), 25);
        assert_eq!(ui.preview_targets, ["a"]);
        assert!(ui.wall_roster);
        assert!(!rendered.contains("page"));
        assert!(rendered.contains("1 live"), "{rendered}");
        assert!(rendered.contains("preview-ok"));
        assert!(ui.tile_hits.iter().all(|(r, _)| r.width >= WALL_CELL.0));
        // Status stays visible instead of squeezing another tiny screen.
        assert!(rendered.contains("working"));
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
        assert_eq!(
            ui.preview_targets.len(),
            1,
            "only selected inspection is previewed"
        );
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
        assert!(
            wall.pane_hits[0].0.width > 80,
            "selection gets most of the width"
        );
        let narrow = text(&render(&mut wall, 90, 44));
        assert!(
            narrow.contains("SELECTED TERMINAL"),
            "narrow Wall stacks the selected screen"
        );
        assert!(wall.tile_hits.iter().all(|(r, _)| r.height <= 8));
    }

    #[test]
    fn wall_selected_preview_wraps_the_right_edge_and_keeps_truecolor() {
        let mut ui = workspace(Theme::default(), View::Wall);
        render(&mut ui, 160, 44);
        let (screen, id) = ui.pane_hits[0].clone();
        let output = format!(
            "\x1b[38;2;23;180;210m{} VISIBLE_END\x1b[0m",
            "x".repeat(usize::from(screen.width) + 10)
        );
        ui.previews
            .insert(id, crate::model::Preview::new(&output, ""));
        let buffer = render(&mut ui, 160, 44);
        let rendered = text(&buffer);
        assert!(rendered.contains("VISIBLE_END"), "{rendered}");
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.symbol() == "V" && cell.fg == Color::Rgb(23, 180, 210))
        );
        assert!(
            ui.embed.is_none(),
            "preview must not attach or resize the terminal"
        );
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
        assert!(outline.contains("working"));
    }
    #[test]
    fn focus_hotkeys_do_not_repeat_local_actions_or_alias_enter_to_attach() {
        let mut ui = workspace(Theme::default(), View::Focus);
        let mut repeated = ui
            .snapshot
            .terminal_actions
            .iter()
            .find(|a| a.id == "terminal.attach")
            .unwrap()
            .clone();
        for action in &mut ui.snapshot.terminal_actions {
            if action.id == "terminal.attach" {
                action.short = "Attach".into();
            }
            if action.id == "terminal.watch" {
                action.short = "Watch".into();
            }
        }
        repeated.short = "Attach".into();
        repeated.quick = "x".into();
        ui.snapshot.terminal_actions.push(repeated);
        let rendered = text(&render(&mut ui, 180, 40));
        assert_eq!(rendered.matches("Attach").count(), 1, "{rendered}");
        assert_eq!(
            ui.quick_hits
                .iter()
                .filter(|(_, id)| id == "terminal.attach")
                .count(),
            1
        );
        let buffer = render(&mut ui, 180, 40);
        let footer: String = (0..180).map(|x| buffer[(x, 39)].symbol()).collect();
        assert_eq!(footer.matches("Attach").count(), 1);
        assert_eq!(footer.matches("Watch").count(), 1);
        assert!(ui.quick_hits.iter().all(|(rect, _)| rect.y == 39));
    }
    #[test]
    fn terminal_status_colors_are_explicit_and_blocked_wins_over_working() {
        let mut node = Node::default();
        assert_eq!(terminal_status(&node), ("unknown", Tone::Faint));
        node.state = "idle".into();
        assert_eq!(terminal_status(&node), ("idle", Tone::Muted));
        node.state = "working".into();
        assert_eq!(terminal_status(&node), ("working", Tone::Ok));
        node.activity = "blocked".into();
        assert_eq!(terminal_status(&node), ("blocked", Tone::Warn));
        node.state = "error".into();
        assert_eq!(terminal_status(&node), ("error", Tone::Rose));
        assert_eq!(node_tone("working", "pane"), Tone::Ok);
        assert_eq!(node_tone("blocked", "pane"), Tone::Warn);
        assert_eq!(node_tone("idle", "pane"), Tone::Muted);
        assert_eq!(node_tone("error", "pane"), Tone::Rose);
        let mut ui = workspace(Theme::default(), View::Focus);
        ui.snapshot.tree[1].state = "working".into();
        let buffer = render(&mut ui, 140, 40);
        let hit = ui
            .focus_tree_hits
            .iter()
            .find(|(_, id, _)| id == &ui.snapshot.tree[1].id)
            .unwrap()
            .0;
        let row: String = (hit.left()..hit.right())
            .map(|x| buffer[(x, hit.y)].symbol())
            .collect();
        assert!(row.contains("working"));
        assert!(
            (hit.left()..hit.right()).any(|x| buffer[(x, hit.y)].symbol() == "w"
                && buffer[(x, hit.y)].fg == ui.theme.rgb(Tone::Ok.rgb()))
        );
    }
    #[test]
    fn focus_details_separate_name_identity_and_topic_without_overlapping_rows() {
        let mut ui = workspace(Theme::default(), View::Focus);
        let node = ui.snapshot.tree.iter_mut().find(|n| n.id == "a").unwrap();
        node.parts = vec![
            ("w1:p2".into(), "id".into()),
            ("Release planning".into(), "title".into()),
            ("Codex".into(), "harness".into()),
            ("team".into(), "model".into()),
            ("Review workspace navigation".into(), "topic".into()),
        ];
        node.fields = vec![
            Field::new("Agent session", "abcdef12-3456-7890", "id"),
            Field::new("Folder", "~/projects/release", ""),
        ];
        node.tile.as_mut().unwrap().harness = "codex".into();
        assert_eq!(session_name(node), "Release planning");
        assert_eq!(session_meta(node), "codex · w1:p2 · #abcdef12 · team");
        let buffer = render(&mut ui, 160, 44);
        let hit = ui
            .focus_tree_hits
            .iter()
            .find(|(_, id, _)| id == "a")
            .unwrap()
            .0;
        assert_eq!(hit.height, 3);
        assert!(
            ui.focus_tree_hits
                .windows(2)
                .all(|pair| pair[0].0.bottom() <= pair[1].0.top())
        );
        let rendered = text(&buffer);
        for expected in [
            "Release planning",
            "#abcdef12",
            "Review workspace navigation",
            "~/projects/release",
        ] {
            assert!(rendered.contains(expected), "{expected}: {rendered}");
        }
        ui.pointer = Some((hit.x, hit.y + 1));
        let rendered = text(&render(&mut ui, 160, 44));
        assert!(rendered.contains("team"));
        for (w, h) in [(80, 24), (110, 30), (160, 44)] {
            render(&mut ui, w, h);
            let area = ui.focus_tree_area.unwrap();
            assert!(
                ui.focus_tree_hits
                    .iter()
                    .all(|(r, _, _)| r.bottom() < area.bottom() && r.right() < area.right())
            );
        }
    }
    #[test]
    fn detailed_focus_reveals_entire_selected_entry_after_scrolling() {
        let mut ui = workspace(Theme::default(), View::Focus);
        let template = ui.snapshot.tree[1].clone();
        for i in 0..40 {
            let mut n = template.clone();
            n.id = format!("session-{i}");
            ui.snapshot.tree.push(n);
        }
        ui.reveal("session-39");
        render(&mut ui, 140, 30);
        assert!(ui.focus_tree_offset > 0);
        let area = ui.focus_tree_area.unwrap();
        let hit = ui
            .focus_tree_hits
            .iter()
            .find(|(_, id, _)| id == "session-39")
            .unwrap()
            .0;
        assert_eq!(hit.height, 2);
        assert!(hit.bottom() < area.bottom());
        assert!(!ui.collapsed.contains("server"));
    }
    #[test]
    fn focus_uses_every_tree_node_in_the_same_order_and_collapse_state() {
        for ascii in [false, true] {
            let mut ui = workspace(
                Theme {
                    ascii,
                    ..Theme::default()
                },
                View::Focus,
            );
            ui.snapshot
                .tree
                .iter_mut()
                .find(|n| n.id == "b")
                .unwrap()
                .children = true;
            for order in [Order::Layout, Order::Activity] {
                ui.order = order;
                let expected: Vec<_> = ui
                    .rows()
                    .into_iter()
                    .filter(|r| r.kind == Kind::Node)
                    .map(|r| r.id)
                    .collect();
                render(&mut ui, 160, 60);
                let actual: Vec<_> = ui
                    .focus_tree_hits
                    .iter()
                    .map(|(_, id, _)| id.clone())
                    .collect();
                assert_eq!(actual, expected);
                assert!(actual.iter().any(|id| id == "b-agent"));
                assert!(ui.focus_tree_toggles.iter().any(|(_, id)| id == "b"));
                ui.collapsed.insert("b".into());
                let expected: Vec<_> = ui
                    .rows()
                    .into_iter()
                    .filter(|r| r.kind == Kind::Node)
                    .map(|r| r.id)
                    .collect();
                render(&mut ui, 160, 60);
                assert_eq!(
                    ui.focus_tree_hits
                        .iter()
                        .map(|(_, id, _)| id.clone())
                        .collect::<Vec<_>>(),
                    expected
                );
                assert!(!ui.focus_tree_hits.iter().any(|(_, id, _)| id == "b-agent"));
                ui.collapsed.remove("b");
            }
        }
    }
}
