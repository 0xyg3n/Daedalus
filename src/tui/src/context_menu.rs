//! Searchable pane actions, using the same contracts as the keyboard menu.
use crate::{
    Step,
    model::{Ui, View, WORKSPACES},
    theme::Tone,
    ui_scroll::Window,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
#[derive(Clone)]
struct Action {
    id: String,
    label: String,
    note: String,
    icon: String,
    key: String,
    group: &'static str,
    enabled: bool,
    danger: bool,
}
pub struct Menu {
    pub target: String,
    pub focused: String,
    anchor: String,
    group_target: bool,
    category: String,
    category_hits: Vec<(Rect, String)>,
    pub x: u16,
    pub y: u16,
    pub selected: usize,
    pub area: Rect,
    query: String,
    actions: Vec<Action>,
    hits: Vec<(Rect, usize)>,
}
fn actions(ui: &Ui, target: &str, focused: &str) -> Vec<Action> {
    let mut rows = Vec::new();
    let registry = daedalus_tui::app::registry::builtin().terminal_actions();
    let item =
        |id: &str, label: &str, note: &str, key: &str, group: &'static str, enabled: bool| Action {
            id: id.into(),
            label: label.into(),
            note: note.into(),
            icon: "pane".into(),
            key: key.into(),
            group,
            enabled,
            danger: false,
        };
    rows.push(item(
        "terminal.attach",
        "Type in pane",
        "Focus this exact terminal and enable keyboard input.",
        "a",
        "Terminal",
        true,
    ));
    rows.push(item(
        "terminal.watch",
        "Watch read-only",
        "Observe output without sending keyboard input.",
        "w",
        "Terminal",
        true,
    ));
    rows.push(item(
        "focus",
        "Expand focused pane",
        "Expand this pane; F3 returns to your layout.",
        "F3",
        "Terminal",
        true,
    ));
    rows.push(item(
        "fullscreen",
        if ui.fullscreen_from.is_some() {
            "Return from full screen"
        } else {
            "Full screen this pane"
        },
        "Expand this exact pane. F11 or the Back button restores your Tetris layout.",
        "F11",
        "Terminal",
        true,
    ));
    rows.push(item(
        "copy",
        "Copy selected text",
        "Copy the text selected in this pane.",
        "Ctrl+C",
        "Terminal",
        ui.selection.as_ref().is_some_and(|(id, _, _)| id == target),
    ));
    for (id, label) in [
        ("terminal.shell-below", "New shell below"),
        ("terminal.shell-above", "New shell above"),
        ("terminal.shell-right", "New shell to the right"),
        ("terminal.shell-left", "New shell to the left"),
    ] {
        rows.push(item(
            id,
            label,
            "Create a fresh shell in this project, with a new terminal identity.",
            "",
            "New pane",
            true,
        ));
    }
    rows.push(item(
        "checkpoint.quick",
        "Quick checkpoint",
        "Save a private session reference and visible output.",
        "F5",
        "Checkpoints",
        true,
    ));
    rows.push(item(
        "checkpoint.save",
        "Save checkpoint…",
        "Name and save this session privately, with optional output.",
        "c",
        "Checkpoints",
        true,
    ));
    rows.push(item(
        "checkpoint.save-open",
        "Save checkpoint & open new pane…",
        "Save this session, then choose fork, resume or handoff in a new pane here.",
        "",
        "Checkpoints",
        true,
    ));
    rows.push(item(
        "checkpoint.open-here",
        "Open saved checkpoint here…",
        "Choose a saved checkpoint and open it in a new pane beside this terminal.",
        "",
        "Checkpoints",
        ui.snapshot.checkpoints > 0,
    ));
    for action in registry {
        if matches!(
            action.id.as_str(),
            "terminal.attach"
                | "terminal.watch"
                | "checkpoint.save"
                | "checkpoint.save-open"
                | "checkpoint.open-here"
        ) {
            continue;
        }
        let group = match action.id.as_str() {
            "project.review" | "workspace.orchestrate" => "Project & agents",
            "checkpoint.save" | "terminal.fork" | "terminal.duplicate" | "terminal.views" => {
                "Session"
            }
            "terminal.new" => "New pane",
            "terminal.organize" | "terminal.rename" | "terminal.pin" => "Organize",
            "terminal.close" | "terminal.interrupt" => "Control",
            _ => "More",
        };
        let danger = matches!(action.id.as_str(), "terminal.close" | "terminal.interrupt");
        rows.push(Action {
            id: action.id.clone(),
            label: action.title,
            note: if action.id == "terminal.native" {
                "Native full screen. F11 or Ctrl+] then D returns; the source keeps running.".into()
            } else {
                action.note
            },
            icon: action.icon,
            key: action.quick,
            group,
            enabled: daedalus_tui::app::session_menu::available(&action.id),
            danger,
        });
    }
    let selected = ui.terminal_node(target);
    let original = ui.terminal_node(focused);
    let same = target != focused
        && selected
            .zip(original)
            .is_some_and(|(a, b)| a.parent == b.parent);
    rows.push(item(
        "swap",
        "Swap with focused pane",
        "Exchange positions within this Tetris tab.",
        "",
        "Workspace view",
        same && ui.view == View::Map,
    ));
    rows.push(item(
        "native-swap",
        "Swap real Herdr panes",
        "Exchange real pane positions on the same Herdr tab.",
        "",
        "Workspace view",
        same && selected.is_some_and(|n| n.backend == "herdr")
            && original.is_some_and(|n| n.backend == "herdr"),
    ));
    let can_split = crate::map::can_compare(ui);
    rows.push(item(
        "columns",
        "Split workspaces side by side",
        "Click a half, then a workspace number to choose its workspace.",
        "",
        "Workspace view",
        can_split,
    ));
    rows.push(item(
        "rows",
        "Split workspaces above / below",
        "Show two workspaces with a horizontal divider.",
        "",
        "Workspace view",
        can_split,
    ));
    let split = matches!(ui.map.split.as_str(), "columns" | "rows");
    rows.push(item(
        "rotate",
        "Rotate workspace split",
        "Change between side by side and stacked halves.",
        "",
        "Workspace view",
        split,
    ));
    rows.push(item(
        "halves",
        "Swap workspace halves",
        "Move the two workspaces to opposite halves.",
        "",
        "Workspace view",
        split,
    ));
    rows.push(item(
        "single",
        "Single workspace",
        "Return to one workspace without losing pane arrangements.",
        "",
        "Workspace view",
        split,
    ));
    rows.push(item(
        "fit",
        "Fit / reset Tetris panes",
        "Restore this tab's native layout, or fit every pane.",
        "",
        "Workspace view",
        true,
    ));
    rows.push(item(
        "map",
        "Open Tetris",
        "Show fitted pane layouts.",
        "F4",
        "Workspace view",
        true,
    ));
    rows.push(item(
        "theme",
        "Change theme colors",
        "Cycle Midnight, Charcoal, Ocean, Forest and Violet. Also in Options.",
        "",
        "Appearance",
        true,
    ));
    rows.push(item(
        "options",
        "Display options",
        "Theme colors, motion, backgrounds, panels and key bar.",
        ",",
        "Appearance",
        true,
    ));
    rows.push(item(
        "menu",
        "Full terminal menu",
        "Open the existing complete terminal menu.",
        "",
        "More",
        true,
    ));
    for (id, label, note, group) in [
        (
            "project.new",
            "New workspace…",
            "Choose the backend, folder and initial shell or tool.",
            "New pane",
        ),
        (
            "tree",
            "Reveal in Tree",
            "Return to the hierarchy with this exact pane selected.",
            "Workspace view",
        ),
        (
            "balance",
            "Equal workspace halves",
            "Give each workspace half of the available screen.",
            "Workspace view",
        ),
        (
            "grow",
            "More space for this workspace",
            "Increase the clicked workspace's share by ten percent.",
            "Workspace view",
        ),
        (
            "shrink",
            "Less space for this workspace",
            "Decrease the clicked workspace's share by ten percent.",
            "Workspace view",
        ),
        (
            "layout.save",
            "Save current layout…",
            "Save Tetris positions, workspace halves and view preferences privately.",
            "Layouts",
        ),
        (
            "layout.open",
            "Open saved layout…",
            "Choose a saved arrangement; existing terminals keep running.",
            "Layouts",
        ),
        (
            "refresh",
            "Refresh workspaces",
            "Read the latest panes, layouts and agent status.",
            "More",
        ),
    ] {
        rows.push(item(
            id,
            label,
            note,
            "",
            group,
            !matches!(id, "balance" | "grow" | "shrink") || split,
        ));
    }
    let order = [
        "Terminal",
        "Checkpoints",
        "New pane",
        "Session",
        "Project & agents",
        "Workspace view",
        "Layouts",
        "Organize",
        "Appearance",
        "More",
        "Control",
    ];
    for row in &mut rows {
        if !row.enabled {
            row.note = match row.id.as_str() {
                "copy" => "Select text in this pane first with Shift+drag, then copy.",
                "swap" => "Select a different pane in this same Tetris tab first.",
                "native-swap" => "Requires two different Herdr panes in the same native tab.",
                "columns" | "rows" => {
                    "Open another live workspace or tab before comparing two groups."
                }
                "rotate" | "halves" | "single" | "balance" | "grow" | "shrink" => {
                    "Compare two workspaces first, then adjust their halves."
                }
                "checkpoint.open-here" => {
                    "Save a checkpoint first; it will then appear in this picker."
                }
                _ => row.note.as_str(),
            }
            .into();
        }
    }
    rows.sort_by_key(|r| {
        order
            .iter()
            .position(|g| *g == r.group)
            .unwrap_or(order.len())
    });
    rows
}
fn scope_actions(ui: &Ui, target: &str, anchor: &str) -> Vec<Action> {
    let live = !anchor.is_empty();
    let children = ui
        .snapshot
        .tree
        .iter()
        .find(|n| n.id == target)
        .is_some_and(|n| n.children || ui.snapshot.tree.iter().any(|child| child.parent == n.id));
    let tree = ui.view == View::Tree;
    [
        (
            "project.review",
            "Project · Git review",
            "Diff, stage paths, commit locally and create worktrees.",
            "F6",
            "Project & agents",
            live,
        ),
        (
            "workspace.orchestrate",
            "Agent orchestration",
            "Plan tasks, dependencies and parallel agent work.",
            "F7",
            "Project & agents",
            true,
        ),
        (
            "map",
            "Open this branch in Tetris",
            "Show this workspace or tab's panes; F4 opens Tetris.",
            "F4",
            "Navigation",
            live,
        ),
        (
            "tree",
            "Reveal in Tree",
            "Show this exact branch in the hierarchy.",
            "",
            "Navigation",
            true,
        ),
        (
            "toggle-group",
            "Expand / collapse branch",
            "Show or hide the branch's children.",
            "",
            "Navigation",
            tree && children,
        ),
        (
            "expand-group",
            "Expand all descendants",
            "Unfold this branch and its nested groups.",
            "",
            "Navigation",
            tree && children,
        ),
        (
            "collapse-group",
            "Collapse this branch",
            "Fold this branch while keeping it selected.",
            "",
            "Navigation",
            tree && children,
        ),
        (
            "terminal.shell-below",
            "New shell here",
            "Create a shell beside the captured pane in this branch.",
            "",
            "New pane",
            live,
        ),
        (
            "terminal.new",
            "New shell or AI tool…",
            "Choose an installed tool and review the new pane before launch.",
            "",
            "New pane",
            live,
        ),
        (
            "project.new",
            "New workspace…",
            "Choose the backend, folder and initial shell or tool.",
            "",
            "New pane",
            true,
        ),
        (
            "terminal.organize",
            "Manage workspace / tab…",
            "Rename or organize this branch using its exact pane identity.",
            "",
            "Organize",
            live,
        ),
        (
            "columns",
            "Compare workspaces side by side",
            "Open this branch's Tetris with another workspace beside it.",
            "",
            "Workspace view",
            live && crate::map::can_compare(ui),
        ),
        (
            "rows",
            "Compare workspaces above / below",
            "Open this branch's Tetris with another workspace below it.",
            "",
            "Workspace view",
            live && crate::map::can_compare(ui),
        ),
        (
            "layout.save",
            "Save current layout…",
            "Save the current arrangement and view preferences privately.",
            "",
            "Layouts",
            true,
        ),
        (
            "layout.open",
            "Open saved layout…",
            "Choose a saved arrangement; terminals keep running.",
            "",
            "Layouts",
            true,
        ),
        (
            "refresh",
            "Refresh workspaces",
            "Read the latest terminal hierarchy and layouts.",
            "",
            "More",
            true,
        ),
        (
            "options",
            "Display options",
            "Theme colors, interface and controls.",
            ",",
            "Appearance",
            true,
        ),
    ]
    .into_iter()
    .map(|(id, label, note, key, group, enabled)| Action {
        id: id.into(),
        label: label.into(),
        note: if !enabled && !live {
            "No available pane in this branch. Refresh or expand it to inspect its contents.".into()
        } else {
            note.into()
        },
        icon: if id == "map" { "workspaces" } else { "pane" }.into(),
        key: key.into(),
        group,
        enabled,
        danger: false,
    })
    .collect()
}
pub fn show(ui: &mut Ui, target: &str, x: u16, y: u16) {
    if ui.page != WORKSPACES {
        return;
    }
    let Some(node) = ui.snapshot.tree.iter().find(|n| n.id == target) else {
        return;
    };
    let pane = ui.terminal_node(target).map(|n| n.id.clone());
    let group_target = pane.is_none();
    if group_target
        && !matches!(
            node.kind.as_str(),
            "server" | "workspace" | "tmux session" | "tab" | "window"
        )
    {
        return;
    }
    let focused = ui
        .selected_terminal()
        .and_then(|id| ui.terminal_node(&id).map(|n| n.id.clone()))
        .unwrap_or_default();
    let target = pane.unwrap_or_else(|| target.into());
    let anchor = ui.terminal_in_scope(&target).unwrap_or_default();
    let actions = if group_target {
        scope_actions(ui, &target, &anchor)
    } else {
        actions(ui, &target, &focused)
    };
    let selected = actions.iter().position(|a| a.enabled).unwrap_or(0);
    if let Some(embed) = ui.embed.as_mut() {
        embed.prefix = false;
    }
    ui.context = Some(Menu {
        target,
        focused,
        anchor,
        group_target,
        category: String::new(),
        category_hits: Vec::new(),
        x,
        y,
        selected,
        area: Rect::default(),
        query: String::new(),
        actions,
        hits: Vec::new(),
    });
}
pub fn open_selected(ui: &mut Ui) {
    let target = if ui.view == View::Tree {
        ui.selected_row()
            .filter(|r| r.kind == crate::model::Kind::Node)
            .map(|r| r.id)
    } else {
        ui.selected_terminal()
    };
    if let Some(target) = target {
        let (x, y) = ui.pointer.unwrap_or((2, 3));
        show(ui, &target, x, y);
    }
}
pub fn open(ui: &mut Ui, mouse: MouseEvent) {
    if ui.page != WORKSPACES {
        return;
    }
    let p = Position::new(mouse.column, mouse.row);
    let tiles = ui.tiles();
    let target = (if ui.view == View::Map {
        crate::map_nav::context_target(ui, p)
    } else {
        None
    })
    .or_else(|| {
        ui.tree_hits
            .iter()
            .find(|(r, _)| r.contains(p))
            .map(|(_, id)| id.clone())
    })
    .or_else(|| {
        ui.tile_hits
            .iter()
            .rev()
            .find(|(r, _)| r.contains(p))
            .and_then(|(_, i)| tiles.get(*i).map(|t| t.id.clone()))
    })
    .or_else(|| {
        ui.screens
            .iter()
            .rev()
            .find(|(r, _, _)| r.contains(p))
            .map(|(_, id, _)| id.clone())
    })
    .or_else(|| {
        ui.pane_hits
            .iter()
            .find(|(r, _)| r.contains(p))
            .map(|(_, id)| id.clone())
    })
    .or_else(|| {
        ui.list_hits
            .iter()
            .find(|(r, _)| r.contains(p))
            .and_then(|(_, i)| {
                ui.rows()
                    .get(*i)
                    .filter(|r| r.kind == crate::model::Kind::Node)
                    .map(|r| r.id.clone())
            })
    })
    .or_else(|| {
        if ui.view == View::Tree {
            ui.selected_row()
                .filter(|r| r.kind == crate::model::Kind::Node)
                .map(|r| r.id)
        } else {
            ui.selected_terminal()
        }
    });
    if let Some(target) = target {
        show(ui, &target, mouse.column, mouse.row);
    }
}
fn categories(menu: &Menu) -> Vec<String> {
    let mut groups = vec![String::new()];
    for a in &menu.actions {
        if !groups.iter().any(|g| g == a.group) {
            groups.push(a.group.into());
        }
    }
    groups
}
fn category(menu: &mut Menu, group: String) {
    menu.category = group;
    menu.query.clear();
    menu.hits.clear();
    menu.selected = filtered(menu)
        .iter()
        .position(|i| menu.actions[*i].enabled)
        .unwrap_or(0);
}
fn cycle_category(menu: &mut Menu, forward: bool) {
    let groups = categories(menu);
    let current = groups.iter().position(|g| g == &menu.category).unwrap_or(0);
    let next = (current + if forward { 1 } else { groups.len() - 1 }) % groups.len();
    category(menu, groups[next].clone());
}
fn filtered(menu: &Menu) -> Vec<usize> {
    let query = menu.query.to_lowercase();
    menu.actions
        .iter()
        .enumerate()
        .filter(|(_, a)| {
            (query.is_empty() && (menu.category.is_empty() || menu.category == a.group)
                || !query.is_empty())
                && query.split_whitespace().all(|q| {
                    format!("{} {} {} {}", a.label, a.note, a.group, a.key)
                        .to_lowercase()
                        .contains(q)
                })
        })
        .map(|(i, _)| i)
        .collect()
}
pub fn draw(f: &mut Frame, area: Rect, ui: &mut Ui) {
    let Some(mut menu) = ui.context.take() else {
        return;
    };
    let theme = ui.theme;
    let rows = filtered(&menu);
    menu.selected = menu.selected.min(rows.len().saturating_sub(1));
    let max_width = if area.width >= 102 { 108 } else { 62 };
    let w = max_width.min(area.width.saturating_sub(2)).max(1);
    let h = (rows.len() as u16 + 9)
        .min(area.height.saturating_sub(2))
        .max(1);
    let r = Rect::new(
        menu.x.min(area.right().saturating_sub(w)).max(area.x),
        menu.y.min(area.bottom().saturating_sub(h)).max(area.y),
        w,
        h,
    );
    menu.area = r;
    menu.hits.clear();
    menu.category_hits.clear();
    let shadow = Rect::new(
        r.x.saturating_add(1),
        r.y.saturating_add(1),
        r.width.min(area.right().saturating_sub(r.x + 1)),
        r.height.min(area.bottom().saturating_sub(r.y + 1)),
    );
    f.render_widget(Block::default().style(theme.screen()), shadow);
    f.render_widget(Clear, r);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_set(theme.border())
            .border_style(theme.fg(Tone::Accent))
            .style(theme.surface())
            .title(if menu.group_target {
                " Workspace actions "
            } else {
                " Pane actions "
            }),
        r,
    );
    let inner = Rect::new(
        r.x + 1,
        r.y + 1,
        r.width.saturating_sub(2),
        r.height.saturating_sub(2),
    );
    if inner.height < 6 {
        ui.context = Some(menu);
        return;
    }
    let name = ui
        .snapshot
        .tree
        .iter()
        .find(|n| n.id == menu.target)
        .map(|n| {
            if menu.group_target {
                n.label.clone()
            } else {
                format!(
                    "{} · {}",
                    crate::map::pane_identity(n),
                    n.tile
                        .as_ref()
                        .map(|t| t.title.as_str())
                        .unwrap_or(&n.label)
                )
            }
        })
        .unwrap_or_else(|| "Target disappeared · refresh before using actions".into());
    f.render_widget(
        Paragraph::new(crate::ui_text::fit(&name, inner.width as usize, &theme))
            .style(theme.fg(Tone::Text).add_modifier(Modifier::BOLD)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    f.render_widget(
        Paragraph::new(format!(
            "Search: {}{}",
            menu.query,
            if menu.query.is_empty() {
                "type to filter"
            } else {
                "_"
            }
        ))
        .style(theme.fg(Tone::Muted)),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );
    let groups = categories(&menu);
    let active = groups.iter().position(|g| g == &menu.category).unwrap_or(0);
    if inner.width >= 14 {
        let slots = (inner.width.saturating_sub(4) / 10).max(1) as usize;
        let window = Window::following(groups.len(), slots, active);
        let previous = Rect::new(inner.x, inner.y + 2, 2, 1);
        let next = Rect::new(inner.right() - 2, inner.y + 2, 2, 1);
        f.render_widget(Paragraph::new("‹").style(theme.fg(Tone::Accent)), previous);
        f.render_widget(Paragraph::new("›").style(theme.fg(Tone::Accent)), next);
        menu.category_hits.push((previous, "previous".into()));
        menu.category_hits.push((next, "next".into()));
        for (column, index) in (window.offset..window.end()).enumerate() {
            let group = &groups[index];
            let label = match group.as_str() {
                "" => "All",
                "Terminal" => "Pane",
                "Workspace view" => "View",
                "New pane" => "New",
                "Checkpoints" => "Save",
                "Appearance" => "Look",
                "Navigation" => "Navigate",
                other => other,
            };
            let r = Rect::new(
                inner.x + 2 + column as u16 * 10,
                inner.y + 2,
                9.min(inner.right() - 2 - (inner.x + 2 + column as u16 * 10)),
                1,
            );
            f.render_widget(
                Paragraph::new(crate::ui_text::fit(label, r.width as usize, &theme)).style(
                    if index == active {
                        theme.selected(1.0)
                    } else {
                        theme.fg(Tone::Muted)
                    },
                ),
                r,
            );
            menu.category_hits.push((r, group.clone()));
        }
    }
    let list = Rect::new(
        inner.x,
        inner.y + 3,
        inner.width,
        inner.height.saturating_sub(6),
    );
    let window = Window::following(rows.len(), list.height as usize, menu.selected);
    for (display, &index) in rows
        .iter()
        .enumerate()
        .take(window.end())
        .skip(window.offset)
    {
        let a = &menu.actions[index];
        let selected = display == menu.selected;
        let tone = if !a.enabled {
            Tone::Faint
        } else if a.danger {
            Tone::Danger
        } else if selected {
            Tone::Accent
        } else {
            Tone::Text
        };
        let style = if selected {
            theme.selected(1.0)
        } else {
            theme.surface()
        };
        let prefix = if selected { "›" } else { " " };
        let icon = theme.icon(&a.icon);
        let key = if !a.enabled {
            "unavailable"
        } else {
            a.key.as_str()
        };
        if list.width >= 98 {
            let label_width = 40;
            let note_width = usize::from(list.width) - label_width - 4;
            let heading = format!("{prefix} {icon} {}", a.label);
            let heading = crate::ui_text::fit(&heading, label_width, &theme);
            let padding = label_width - crate::ui_text::width(&heading);
            let note = if key.is_empty() {
                a.note.clone()
            } else {
                format!("{key} · {}", a.note)
            };
            let hit = Rect::new(
                list.x,
                list.y + (display - window.offset) as u16,
                list.width,
                1,
            );
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(heading, theme.fg(tone)),
                    Span::raw(" ".repeat(padding)),
                    Span::styled(" │ ", theme.fg(Tone::Faint)),
                    Span::styled(
                        crate::ui_text::fit(&note, note_width, &theme),
                        theme.fg(Tone::Muted),
                    ),
                ]))
                .style(style),
                hit,
            );
            menu.hits.push((hit, display));
            continue;
        }
        let room = usize::from(list.width).saturating_sub(crate::ui_text::width(key) + 7);
        let label = crate::ui_text::fit(&a.label, room, &theme);
        let pad = usize::from(list.width).saturating_sub(
            crate::ui_text::width(&label)
                + crate::ui_text::width(icon)
                + crate::ui_text::width(key)
                + 5,
        );
        let line = Line::from(vec![
            Span::styled(
                format!("{prefix} {icon} "),
                style.fg(theme.fg(tone).fg.unwrap_or_default()),
            ),
            Span::styled(label, style.fg(theme.fg(tone).fg.unwrap_or_default())),
            Span::styled(" ".repeat(pad), style),
            Span::styled(key, style.fg(theme.fg(Tone::Muted).fg.unwrap_or_default())),
        ]);
        let hit = Rect::new(
            list.x,
            list.y + (display - window.offset) as u16,
            list.width,
            1,
        );
        f.render_widget(Paragraph::new(line), hit);
        menu.hits.push((hit, display));
    }
    if let Some(&index) = rows.get(menu.selected) {
        let a = &menu.actions[index];
        f.render_widget(
            Paragraph::new(format!("{} · {}", a.group, a.note))
                .style(theme.fg(Tone::Muted))
                .wrap(ratatui::widgets::Wrap { trim: true }),
            Rect::new(inner.x, inner.bottom() - 3, inner.width, 2),
        );
    }
    f.render_widget(
        Paragraph::new(format!(
            "Enter run · Tab category · Esc back   {} / {}",
            menu.selected + usize::from(!rows.is_empty()),
            rows.len()
        ))
        .style(theme.fg(Tone::Faint)),
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
    );
    ui.context = Some(menu);
}
fn activate(ui: &mut Ui) -> Step {
    let Some(menu) = ui.context.as_ref() else {
        return Step::Continue;
    };
    let rows = filtered(menu);
    let Some(&index) = rows.get(menu.selected) else {
        return Step::Continue;
    };
    let action = &menu.actions[index];
    if !action.enabled {
        ui.status = format!("Unavailable: {}", action.note);
        return Step::Continue;
    }
    let id = action.id.clone();
    let Some(menu) = ui.context.take() else {
        return Step::Continue;
    };
    if !ui.snapshot.tree.iter().any(|n| n.id == menu.target) {
        ui.status = if menu.group_target {
            "The clicked branch disappeared. Refresh and open its menu again."
        } else {
            "The clicked pane changed or disappeared. Refresh and open its menu again."
        }
        .into();
        ui.status_error = true;
        return Step::Continue;
    }
    match id.as_str() {
        "workspace.orchestrate" | "project.review" => {
            return Step::Send(crate::terminal_request(ui, &id, &menu.anchor));
        }
        "map" => return crate::open_map_scope(ui, &menu.target),
        "tree" => {
            crate::switch_view(ui, View::Tree);
            ui.reveal(&menu.target);
            return Step::Continue;
        }
        "toggle-group" | "expand-group" | "collapse-group" => {
            let expand =
                id == "expand-group" || id == "toggle-group" && ui.collapsed.contains(&menu.target);
            let branches = ui
                .snapshot
                .tree
                .iter()
                .filter(|n| ui.within_scope(&n.id, &menu.target))
                .map(|n| n.id.clone())
                .collect::<Vec<_>>();
            for branch in branches {
                if expand {
                    ui.collapsed.remove(&branch);
                } else {
                    ui.collapsed.insert(branch);
                }
            }
            ui.select_id(&menu.target);
            return Step::Continue;
        }
        "refresh" => return Step::Send(serde_json::json!({"op":"refresh"})),
        "layout.save" | "layout.open" | "project.new" => {
            return Step::Send(crate::terminal_request(ui, &id, &menu.anchor));
        }
        "theme" => {
            crate::change_option(ui, crate::options::Item::Palette, true);
            return Step::Continue;
        }
        "options" => {
            crate::open_options(ui);
            return Step::Continue;
        }
        _ => {}
    }
    if ui
        .terminal_node(&menu.anchor)
        .is_none_or(|node| node.conflicted)
        || menu.group_target && !ui.within_scope(&menu.anchor, &menu.target)
    {
        ui.status =
            "The clicked pane changed or is ambiguous. Refresh and open its menu again.".into();
        ui.status_error = true;
        return Step::Continue;
    }
    if ui.view == View::Map {
        crate::map::choose(ui, &menu.anchor);
    } else {
        ui.reveal(&menu.anchor);
    }
    match id.as_str() {
        "columns" | "rows" => {
            crate::map::arrangement(ui, &id);
            crate::switch_view(ui, View::Map)
        }
        "balance" | "grow" | "shrink" => {
            crate::map::arrangement(ui, &id);
            Step::Continue
        }
        "rotate" => {
            crate::map::arrangement(ui, "rotate");
            Step::Continue
        }
        "halves" => {
            crate::map::arrangement(ui, "swap");
            Step::Continue
        }
        "single" => {
            crate::map::arrangement(ui, "off");
            Step::Continue
        }
        "fit" => {
            crate::map::reset(ui);
            crate::switch_view(ui, View::Map)
        }
        "map" => crate::switch_view(ui, View::Map),
        "focus" => crate::switch_view(ui, View::Focus),
        "fullscreen" => {
            if ui.view == View::Map {
                crate::map::choose(ui, &menu.target);
            }
            crate::toggle_fullscreen(ui)
        }
        "theme" => {
            crate::change_option(ui, crate::options::Item::Palette, true);
            Step::Continue
        }
        "options" => {
            crate::open_options(ui);
            Step::Continue
        }
        "native-swap" => {
            let mut request = crate::terminal_request(ui, "terminal.swap", &menu.focused);
            request["peer"] = serde_json::json!(menu.anchor);
            Step::Send(request)
        }
        "swap" => {
            crate::map::swap(ui, &menu.target, &menu.focused);
            ui.reveal(&menu.focused);
            Step::Continue
        }
        "copy" => {
            if let Some(text) = crate::select::text(ui) {
                ui.status_error = crate::clipboard::write(
                    &text,
                    ui.settings.clipboard_write_limit_bytes as usize,
                )
                .is_err();
                ui.status = if ui.status_error {
                    "Copy failed"
                } else {
                    "Copied"
                }
                .into();
            }
            Step::Continue
        }
        "menu" => Step::Send(crate::terminal_request(ui, &menu.target, &menu.target)),
        "terminal.attach" | "terminal.watch" => crate::quick_action(ui, &id),
        _ => Step::Send(crate::terminal_request(ui, &id, &menu.anchor)),
    }
}
fn move_choice(menu: &mut Menu, forward: bool) {
    let rows = filtered(menu);
    for _ in 0..rows.len() {
        menu.selected = (menu.selected + if forward { 1 } else { rows.len() - 1 }) % rows.len();
        if menu.actions[rows[menu.selected]].enabled {
            break;
        }
    }
}
pub fn key(ui: &mut Ui, key: KeyEvent) -> Option<Step> {
    let menu = ui.context.as_mut()?;
    let len = filtered(menu).len();
    if let KeyCode::F(number) = key.code {
        let shortcut = format!("F{number}");
        if let Some(index) = menu
            .actions
            .iter()
            .position(|a| a.enabled && a.key == shortcut)
        {
            menu.query.clear();
            menu.category.clear();
            menu.hits.clear();
            menu.selected = index;
            return Some(activate(ui));
        }
    }
    match key.code {
        KeyCode::Tab | KeyCode::Right => cycle_category(menu, true),
        KeyCode::BackTab | KeyCode::Left => cycle_category(menu, false),
        KeyCode::Menu | KeyCode::F(2) => ui.context = None,
        KeyCode::F(10) if key.modifiers.contains(KeyModifiers::SHIFT) => ui.context = None,
        KeyCode::Esc => ui.context = None,
        KeyCode::Enter => return Some(activate(ui)),
        KeyCode::Down => move_choice(menu, true),
        KeyCode::Up => move_choice(menu, false),
        KeyCode::Home => menu.selected = 0,
        KeyCode::End => menu.selected = len.saturating_sub(1),
        KeyCode::PageDown => menu.selected = (menu.selected + 8).min(len.saturating_sub(1)),
        KeyCode::PageUp => menu.selected = menu.selected.saturating_sub(8),
        KeyCode::Backspace => {
            menu.query.pop();
            menu.hits.clear();
            menu.selected = 0;
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && menu.query.chars().count() < 120 =>
        {
            menu.query.push(c);
            menu.hits.clear();
            menu.selected = 0;
        }
        _ => {}
    }
    Some(Step::Continue)
}
pub fn mouse(ui: &mut Ui, mouse: MouseEvent) -> Option<Step> {
    let menu = ui.context.as_mut()?;
    let p = Position::new(mouse.column, mouse.row);
    let len = filtered(menu).len();
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((_, group)) = menu
                .category_hits
                .iter()
                .find(|(r, _)| r.contains(p))
                .cloned()
            {
                match group.as_str() {
                    "previous" => cycle_category(menu, false),
                    "next" => cycle_category(menu, true),
                    _ => category(menu, group),
                }
                return Some(Step::Continue);
            }
            if let Some((_, index)) = menu.hits.iter().find(|(r, _)| r.contains(p)) {
                menu.selected = *index;
                return Some(activate(ui));
            }
            if !menu.area.contains(p) {
                ui.context = None;
            }
        }
        MouseEventKind::Moved => {
            if let Some((_, index)) = menu.hits.iter().find(|(r, _)| r.contains(p)) {
                menu.selected = *index;
            }
        }
        MouseEventKind::ScrollDown => {
            menu.selected = (menu.selected + 3).min(len.saturating_sub(1))
        }
        MouseEventKind::ScrollUp => menu.selected = menu.selected.saturating_sub(3),
        _ => {}
    }
    Some(Step::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Node, Tile};
    fn fixture() -> Ui {
        let mut ui = Ui {
            page: WORKSPACES,
            view: View::Map,
            ..Ui::default()
        };
        for id in ["a", "b"] {
            ui.snapshot.tree.push(Node {
                id: id.into(),
                label: id.into(),
                parent: "tab".into(),
                kind: "pane".into(),
                backend: "herdr".into(),
                tile: Some(Tile::default()),
                ..Node::default()
            });
        }
        ui.tile_hits = vec![(Rect::new(0, 0, 40, 20), 0), (Rect::new(40, 0, 40, 20), 1)];
        ui
    }
    fn open_b(ui: &mut Ui) {
        open(
            ui,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 45,
                row: 5,
                modifiers: KeyModifiers::NONE,
            },
        );
    }
    fn query(ui: &mut Ui, text: &str) {
        for c in text.chars() {
            key(ui, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }
    fn run(ui: &mut Ui, id: &str) -> Step {
        open_b(ui);
        ui.context.as_mut().unwrap().selected = ui
            .context
            .as_ref()
            .unwrap()
            .actions
            .iter()
            .position(|a| a.id == id)
            .unwrap();
        activate(ui)
    }
    #[test]
    fn group_menu_opens_its_own_map_and_rejects_replacement_mutation_targets() {
        let mut ui = fixture();
        ui.view = View::Tree;
        ui.snapshot.tree.push(Node {
            id: "tab".into(),
            kind: "tab".into(),
            label: "Same label".into(),
            children: true,
            ..Default::default()
        });
        show(&mut ui, "tab", 2, 3);
        assert!(ui.context.as_ref().unwrap().group_target);
        assert_eq!(ui.context.as_ref().unwrap().anchor, "a");
        query(&mut ui, "New shell here");
        ui.snapshot.tree.retain(|n| n.id != "a");
        assert!(matches!(activate(&mut ui), Step::Continue));
        assert!(ui.status_error);
        assert_eq!(ui.view, View::Tree);
        show(&mut ui, "tab", 2, 3);
        query(&mut ui, "Open this branch in Tetris");
        assert!(matches!(activate(&mut ui), Step::Continue));
        assert_eq!(ui.view, View::Map);
        assert_eq!(ui.selected_terminal().as_deref(), Some("b"));
    }
    #[test]
    fn categories_mouse_and_keyboard_do_not_overlap_and_search_and_shortcuts_span_all_categories() {
        for width in [44, 62, 100] {
            let mut ui = fixture();
            open_b(&mut ui);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
            terminal.draw(|f| draw(f, f.area(), &mut ui)).unwrap();
            let menu = ui.context.as_ref().unwrap();
            for (i, (r, _)) in menu.category_hits.iter().enumerate() {
                assert!(r.right() <= width);
                for (other, _) in &menu.category_hits[i + 1..] {
                    assert!(r.intersection(*other).is_empty());
                }
                for (other, _) in &menu.hits {
                    assert!(r.intersection(*other).is_empty());
                }
            }
            let next = menu
                .category_hits
                .iter()
                .find(|(_, id)| id == "next")
                .unwrap()
                .0;
            mouse(
                &mut ui,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: next.x,
                    row: next.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
            assert_eq!(ui.context.as_ref().unwrap().category, "Terminal");
            key(&mut ui, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
            assert_eq!(ui.context.as_ref().unwrap().category, "Checkpoints");
            query(&mut ui, "shell above");
            let Some(Step::Send(request)) =
                key(&mut ui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            else {
                panic!("search should find actions in other categories")
            };
            assert_eq!(request["action"], "terminal.shell-above");
            open_b(&mut ui);
            key(&mut ui, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
            let Some(Step::Send(request)) =
                key(&mut ui, KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE))
            else {
                panic!("F5 should use exact target across categories")
            };
            assert_eq!(request["node"], "b");
            assert_eq!(request["action"], "checkpoint.quick");
        }
    }
    #[test]
    fn fit_uses_the_clicked_workspace_half_and_swap_actions_keep_their_targets() {
        let mut ui = fixture();
        ui.snapshot.tree[1].parent = "other-tab".into();
        for id in ["tab", "other-tab"] {
            ui.snapshot.tree.push(Node {
                id: id.into(),
                kind: "tab".into(),
                ..Node::default()
            });
        }
        crate::map::arrangement(&mut ui, "columns");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|f| crate::map::draw(f, f.area(), &mut ui))
            .unwrap();
        let before_a = ui.map.cards.iter().find(|c| c.id == "a").unwrap().clone();
        let button = ui
            .tile_hits
            .iter()
            .find(|(_, i)| ui.tiles()[*i].id == "b")
            .unwrap()
            .0;
        open(
            &mut ui,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: button.x + 1,
                row: button.y + 1,
                modifiers: KeyModifiers::NONE,
            },
        );
        query(&mut ui, "Fit / reset");
        assert!(matches!(activate(&mut ui), Step::Continue));
        assert!(ui.map.cards.iter().any(|c| c == &before_a));
        assert!(!ui.map.cards.iter().any(|c| c.id == "b"));
        terminal
            .draw(|f| crate::map::draw(f, f.area(), &mut ui))
            .unwrap();
        let primary = ui.map.primary.clone();
        let peer = ui.map.peer.clone();
        let button = ui
            .tile_hits
            .iter()
            .find(|(_, i)| ui.tiles()[*i].id == "b")
            .unwrap()
            .0;
        open(
            &mut ui,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: button.x + 1,
                row: button.y + 1,
                modifiers: KeyModifiers::NONE,
            },
        );
        query(&mut ui, "Swap workspace halves");
        assert!(matches!(activate(&mut ui), Step::Continue));
        assert_eq!(ui.map.primary, peer);
        assert_eq!(ui.map.peer, primary);
    }
    #[test]
    fn local_menu_actions_change_the_expected_view_or_preference_and_keep_terminal_identity() {
        for id in ["theme", "options", "map", "focus", "fit", "fullscreen"] {
            let mut ui = fixture();
            let palette = ui.theme.palette;
            let step = run(&mut ui, id);
            assert_eq!(
                ui.selected_terminal().as_deref(),
                Some(if matches!(id, "theme" | "options") {
                    "a"
                } else {
                    "b"
                })
            );
            match id {
                "theme" => {
                    assert_ne!(ui.theme.palette, palette);
                    assert!(matches!(step, Step::Continue));
                }
                "options" => assert!(ui.options.is_some()),
                "map" | "fit" => assert_eq!(ui.view, View::Map),
                "focus" | "fullscreen" => {
                    assert_eq!(ui.view, View::Focus);
                    assert!(ui.zoomed);
                    let Step::Send(request) = step else {
                        panic!("focus must request exact read-only stream")
                    };
                    assert_eq!(request["node"], "b");
                    assert_eq!(request["action"], "terminal.watch");
                }
                _ => unreachable!(),
            }
            if id == "fullscreen" {
                assert_eq!(ui.fullscreen_from, Some(View::Map));
            }
        }
        let mut ui = fixture();
        let Step::Send(request) = run(&mut ui, "native-swap") else {
            panic!("native swap request expected")
        };
        assert_eq!(request["node"], "a");
        assert_eq!(request["peer"], "b");
        for (id, expected) in [
            ("columns", "columns"),
            ("rows", "rows"),
            ("rotate", "rows"),
            ("single", "off"),
        ] {
            let mut ui = fixture();
            ui.snapshot.tree[1].parent = "other-tab".into();
            ui.snapshot.tree.push(Node {
                id: "tab".into(),
                kind: "tab".into(),
                ..Node::default()
            });
            ui.snapshot.tree.push(Node {
                id: "other-tab".into(),
                kind: "tab".into(),
                ..Node::default()
            });
            crate::map::arrangement(&mut ui, "columns");
            assert!(matches!(run(&mut ui, id), Step::Continue));
            assert_eq!(ui.map.split, expected);
        }
    }
    #[test]
    fn displayed_function_shortcuts_work_on_the_clicked_terminal_even_while_searching() {
        let mut ui = fixture();
        open_b(&mut ui);
        query(&mut ui, "shell");
        let Some(Step::Send(request)) =
            key(&mut ui, KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE))
        else {
            panic!("F5 should save clicked pane")
        };
        assert_eq!(request["node"], "b");
        assert_eq!(request["action"], "checkpoint.quick");
    }
    #[test]
    fn every_backend_action_dispatches_to_the_clicked_pane_and_changed_targets_are_rejected() {
        let mut ui = fixture();
        ui.snapshot.checkpoints = 1;
        open_b(&mut ui);
        let actions = ui
            .context
            .as_ref()
            .unwrap()
            .actions
            .iter()
            .filter(|a| {
                a.enabled && (a.id.starts_with("terminal.") || a.id.starts_with("checkpoint."))
            })
            .map(|a| a.id.clone())
            .collect::<Vec<_>>();
        assert!(
            actions.contains(&"checkpoint.save-open".into())
                && actions.contains(&"checkpoint.open-here".into())
        );
        for action in actions {
            let mut ui = fixture();
            ui.snapshot.checkpoints = 1;
            open_b(&mut ui);
            let index = ui
                .context
                .as_ref()
                .unwrap()
                .actions
                .iter()
                .position(|a| a.id == action)
                .unwrap();
            ui.context.as_mut().unwrap().selected = index;
            let Step::Send(request) = activate(&mut ui) else {
                panic!("missing backend request: {action}")
            };
            assert_eq!(request["action"], action);
            assert_eq!(request["node"], "b");
        }
        let mut ui = fixture();
        open_b(&mut ui);
        query(&mut ui, "shell above");
        ui.snapshot.tree.retain(|n| n.id != "b");
        assert!(matches!(activate(&mut ui), Step::Continue));
        assert!(ui.status_error && ui.status.contains("clicked pane changed"));
    }
    #[test]
    fn searching_invalidates_old_mouse_targets_and_full_menu_preserves_exact_target() {
        let mut ui = fixture();
        open_b(&mut ui);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut ui)).unwrap();
        assert!(!ui.context.as_ref().unwrap().hits.is_empty());
        query(&mut ui, "saved checkpoint");
        assert!(ui.context.as_ref().unwrap().hits.is_empty());
        ui.context = None;
        open_b(&mut ui);
        query(&mut ui, "Full terminal menu");
        let Step::Send(request) = activate(&mut ui) else {
            panic!("terminal menu request expected")
        };
        assert_eq!(request["action"], "b");
        assert_eq!(request["node"], "b");
    }
    #[test]
    fn searching_and_mouse_click_use_the_exact_clicked_terminal() {
        let mut ui = fixture();
        open_b(&mut ui);
        query(&mut ui, "quick checkpoint");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| draw(f, f.area(), &mut ui)).unwrap();
        let r = ui.context.as_ref().unwrap().hits[0].0;
        let step = mouse(
            &mut ui,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: r.x,
                row: r.y,
                modifiers: KeyModifiers::NONE,
            },
        )
        .unwrap();
        let Step::Send(request) = step else {
            panic!("checkpoint request expected")
        };
        assert_eq!(request["action"], "checkpoint.quick");
        assert_eq!(request["node"], "b");
        assert!(ui.context.is_none());
    }
    #[test]
    fn shell_and_danger_actions_reuse_backend_contracts_and_disabled_actions_stay_local() {
        let mut ui = fixture();
        open_b(&mut ui);
        query(&mut ui, "shell above");
        let Some(Step::Send(request)) =
            key(&mut ui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        else {
            panic!("shell request expected")
        };
        assert_eq!(request["action"], "terminal.shell-above");
        assert_eq!(request["node"], "b");
        open_b(&mut ui);
        query(&mut ui, "Copy selected");
        assert!(matches!(
            key(&mut ui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Step::Continue)
        ));
        assert!(ui.context.is_some());
    }
}
