//! Contextual controls: the displayed shortcut and mouse button use the same path.
use crate::{
    Step,
    embed::Mode,
    model::{Ui, View, WORKSPACES},
    theme::Tone,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    text::{Line, Span},
    widgets::Paragraph,
};

struct Button {
    key: String,
    label: &'static str,
    action: &'static str,
    help: &'static str,
}
impl Button {
    fn new(key: &str, label: &'static str, action: &'static str, help: &'static str) -> Self {
        Self {
            key: key.into(),
            label,
            action,
            help,
        }
    }
    fn width(&self) -> u16 {
        (crate::ui_text::width(&self.key) + self.label.len() + 4) as u16
    }
}
fn buttons(ui: &Ui) -> Vec<Button> {
    let b = Button::new;
    if let Some(dialog) = ui.dialog.as_ref() {
        if !matches!(dialog.mode.as_str(), "choose" | "input" | "external") {
            return vec![b(
                "Esc",
                "Close",
                "esc",
                "Close this result and return to the same view.",
            )];
        }
        return vec![
            b(
                "Enter",
                if dialog.mode == "external" {
                    "Open terminal"
                } else {
                    "Continue"
                },
                "enter",
                "Accept this dialog's selection or input.",
            ),
            b(
                "Esc",
                "Cancel",
                "esc",
                "Cancel this dialog without submitting it.",
            ),
        ];
    }
    if ui.context.is_some() {
        return vec![
            b(
                "Enter",
                "Run action",
                "enter",
                "Run the selected enabled action.",
            ),
            b(
                "Tab",
                "Category",
                "tab",
                "Browse action categories; typing searches every category.",
            ),
            b("Esc", "Close", "esc", "Close the action menu."),
        ];
    }
    if let Some(menu) = ui.options.as_ref() {
        if crate::options::Item::ALL
            .get(menu.selected)
            .is_some_and(|item| item.is_action())
        {
            return vec![
                b(
                    "Enter",
                    "Open",
                    "enter",
                    "Open the selected preference editor or command.",
                ),
                b(
                    "Esc",
                    "Close",
                    "esc",
                    "Close preferences and return to your view.",
                ),
            ];
        }
        return vec![
            b(
                "Enter",
                "Change",
                "enter",
                "Change the selected preference or open its editor.",
            ),
            b(
                "Left",
                "Previous",
                "left",
                "Choose the previous value for this preference.",
            ),
            b(
                "Right",
                "Next",
                "right",
                "Choose the next value for this preference.",
            ),
            b(
                "Esc",
                "Close",
                "esc",
                "Close preferences; applied choices are remembered.",
            ),
        ];
    }
    if ui.binding_search.is_some() {
        return vec![
            b(
                "PgUp",
                "Previous",
                "page-up",
                "Scroll to earlier keyboard shortcuts.",
            ),
            b(
                "PgDn",
                "Next",
                "page-down",
                "Scroll to later keyboard shortcuts.",
            ),
            b(
                "Esc",
                "Close",
                "esc",
                "Close shortcut search and return to your view.",
            ),
        ];
    }
    if ui.searching {
        return vec![
            b(
                "Enter",
                if ui.switching { "Watch pane" } else { "Done" },
                "enter",
                "Accept the search or open the selected terminal result.",
            ),
            b(
                "Esc",
                if ui.switching { "Close" } else { "Clear" },
                "esc",
                "Clear or close this search without launching anything.",
            ),
        ];
    }
    let mut rows = Vec::new();
    if ui.page == WORKSPACES {
        let terminal = ui
            .selected_terminal()
            .and_then(|id| ui.terminal_node(&id))
            .is_some_and(|n| !n.conflicted);
        let control = ui.embed.as_ref().is_some_and(|e| e.mode == Mode::Control);
        if control {
            let prefix = ui.embed.as_ref().is_some_and(|e| e.prefix);
            rows.push(b(
                if prefix { "D" } else { "Ctrl+] D" },
                "Detach",
                "detach",
                "Stop sending keys to the pane; its shell or agent keeps running.",
            ));
            rows.push(b(
                "F2",
                "Actions",
                "actions",
                "Open pane actions: split shells, checkpoints, layouts and workspace controls.",
            ));
            rows.push(b(
                "F6",
                "Git",
                "project",
                "Review changes, stage files, commit locally and create worktrees.",
            ));
            rows.push(b(
                "F7",
                "Tasks",
                "tasks",
                "Build task plans across agents, inspect progress and dependencies.",
            ));
            rows.push(b(
                "F11",
                if ui.fullscreen_from.is_some() {
                    "Restore"
                } else {
                    "Fullscreen"
                },
                "fullscreen",
                "Expand the selected pane or return to the same layout, retaining its stream.",
            ));
            rows.push(b(
                "F5",
                "Checkpoint",
                "checkpoint",
                "Quick-save a private session reference and visible terminal output.",
            ));
            rows.push(b(
                if prefix { "N" } else { "Ctrl+] N" },
                "New pane",
                "new-pane",
                "Choose a shell or installed AI tool in a new pane beside this terminal.",
            ));
            rows.push(b(
                if prefix { "K" } else { "Ctrl+] K" },
                "Switch pane",
                "switch",
                "Search terminals by name or exact identity and choose one.",
            ));
            rows.push(b(
                if prefix { "O" } else { "Ctrl+] O" },
                "Watch",
                "observe",
                "Keep watching output while stopping keyboard input to the pane.",
            ));
        } else {
            if ui.view == View::Tree {
                rows.push(b(
                    "F4",
                    "Open Tetris",
                    "map",
                    "Open Tetris for the selected workspace, tab or pane; double-click also opens it.",
                ));
            }
            rows.push(b(
                "F2",
                "Actions",
                "actions",
                "Open actions for the selected pane, workspace or tab. Right-click does the same.",
            ));
            rows.push(b(
                "F6",
                "Git",
                "project",
                "Review changes, stage files, commit locally and create worktrees.",
            ));
            rows.push(b(
                "F7",
                "Tasks",
                "tasks",
                "Build task plans across agents, inspect progress and dependencies.",
            ));
            if ui.view == View::Cube {
                rows.push(b(
                    "Left",
                    "Rotate",
                    "left",
                    "Rotate the cube to the previous terminal.",
                ));
                rows.push(b(
                    "Right",
                    "Rotate",
                    "right",
                    "Rotate the cube to the next terminal.",
                ));
            }
            if terminal {
                rows.push(b(
                    "a",
                    "Type",
                    "terminal.attach",
                    "Send keyboard input to this exact pane without changing its layout.",
                ));
                if ui.embed.is_none() {
                    rows.push(b(
                        "w",
                        "Watch",
                        "terminal.watch",
                        "Watch output without sending keyboard input.",
                    ));
                } else {
                    rows.push(b(
                        "Esc",
                        "Stop watching",
                        "esc",
                        "Detach the read-only stream; the terminal keeps running.",
                    ));
                }
                rows.push(b(
                    "F11",
                    if ui.fullscreen_from.is_some() {
                        "Restore"
                    } else {
                        "Fullscreen"
                    },
                    "fullscreen",
                    "Expand this pane or return to the same layout.",
                ));
                rows.push(b(
                    "F5",
                    "Checkpoint",
                    "checkpoint",
                    "Quick-save a private session reference and visible terminal output.",
                ));
                rows.push(b(
                    "N",
                    "New pane",
                    "new-pane",
                    "Choose a shell or installed AI tool beside the selected terminal.",
                ));
            } else {
                rows.push(b(
                    "n",
                    "Workspace",
                    "new-workspace",
                    "Choose a folder, backend and initial shell or tool for a new workspace.",
                ));
            }
            if ui.view == View::Map && ui.embed.is_none() {
                rows.push(b(
                    "Esc",
                    "Tree",
                    "esc",
                    "Return to the workspace hierarchy with the same pane selected.",
                ));
            } else if ui.view == View::Focus && ui.embed.is_none() {
                rows.push(b(
                    "Esc",
                    "Back",
                    "esc",
                    "Leave Focus and return to your previous layout.",
                ));
            }
            if ui.view != View::Tree && ui.view != View::Map {
                rows.push(b(
                    "F4",
                    "Tetris",
                    "map",
                    "Open the selected terminal's fitted workspace Tetris.",
                ));
            }
            rows.push(b(
                "Ctrl+K",
                "Switch pane",
                "switch",
                "Search terminals by name or exact identity and choose one.",
            ));
            if ui.view != View::Focus {
                rows.push(b(
                    "/",
                    "Search",
                    "search",
                    "Filter the current workspace list or visible panes.",
                ));
            }
            rows.push(b(
                ",",
                "Options",
                "options",
                "Change theme colors, readability, layout and interface preferences.",
            ));
            rows.push(b(
                "?",
                "Hide keys",
                "hide",
                "Hide this row to give terminals more space; click Show controls to restore it.",
            ));
        }
    } else {
        rows.extend([
            b(
                "Enter",
                "Open",
                "enter",
                "Open the selected item or its available actions.",
            ),
            b("/", "Search", "search", "Search items in this section."),
            b(
                "Ctrl+P",
                "Commands",
                "commands",
                "Search all available commands and actions.",
            ),
            b(
                "Tab",
                "Section",
                "tab",
                "Move to the next section; Shift+Tab moves back.",
            ),
            b(
                "r",
                "Refresh",
                "refresh",
                "Refresh inventory without restarting terminal processes.",
            ),
            b(
                ",",
                "Options",
                "options",
                "Open interface preferences and configuration settings.",
            ),
            b(
                "?",
                "Hide keys",
                "hide",
                "Hide this row; click Show controls to restore it.",
            ),
            b(
                "q",
                "Quit",
                "quit",
                "Exit Daedalus; managed shells and agents keep running.",
            ),
        ]);
    }
    for button in &mut rows {
        if button.action.starts_with("terminal.") {
            button.key = ui
                .snapshot
                .terminal_actions
                .iter()
                .find(|action| action.id == button.action && !action.quick.is_empty())
                .map(|action| action.quick.clone())
                .unwrap_or_else(|| "Click".into());
        }
    }
    rows.push(b(
        "F1",
        "All keys",
        "help",
        "Search every shortcut, including controls hidden when the window is narrow.",
    ));
    rows
}

pub fn draw(f: &mut Frame, area: Rect, ui: &mut Ui) -> Option<&'static str> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let rows = buttons(ui);
    let theme = ui.theme;
    let reserve = rows.last().map_or(0, |b| b.width() + 1);
    let mut x = area.x;
    let mut hovered = None;
    for (index, button) in rows.iter().enumerate() {
        let last = index + 1 == rows.len();
        let available = area.right().saturating_sub(x);
        if button.width() + if last { 0 } else { reserve } > available {
            continue;
        }
        if last {
            x = area.right() - button.width();
        }
        let rect = Rect::new(x, area.y, button.width(), 1);
        let over = ui
            .pointer
            .is_some_and(|(px, py)| rect.contains(Position::new(px, py)));
        let label_style = if over {
            theme.focus_label()
        } else {
            theme.fg(Tone::Text)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", button.key),
                    if over {
                        theme.focus_label()
                    } else {
                        theme.keycap()
                    },
                ),
                Span::styled(format!(" {} ", button.label), label_style),
            ])),
            rect,
        );
        let action = if button.action.starts_with("terminal.") {
            button.action.into()
        } else {
            format!("bar:{}", button.action)
        };
        ui.quick_hits.push((rect, action));
        if over {
            hovered = Some(button.help);
        }
        x = rect.right().saturating_add(1);
    }
    hovered
}

pub fn mouse(ui: &mut Ui, mouse: MouseEvent) -> Option<Step> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    let p = Position::new(mouse.column, mouse.row);
    let id = ui
        .quick_hits
        .iter()
        .find(|(r, id)| r.contains(p) && id.starts_with("bar:"))?
        .1
        .clone();
    Some(activate(ui, id.strip_prefix("bar:")?))
}

fn press(ui: &mut Ui, code: KeyCode, modifiers: KeyModifiers) -> Step {
    let key = KeyEvent::new(code, modifiers);
    if ui.dialog.is_some() {
        // Dialog replies need the running backend. The event loop converts
        // their footer clicks to keyboard events before reaching this path.
        return Step::Continue;
    }
    if ui.options.is_some() {
        crate::options_key(ui, key);
        return Step::Continue;
    }
    crate::key_event(ui, key)
}
pub fn key_for_click(ui: &Ui, mouse: MouseEvent) -> Option<KeyEvent> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    let p = Position::new(mouse.column, mouse.row);
    let id = &ui
        .quick_hits
        .iter()
        .find(|(r, id)| r.contains(p) && id.starts_with("bar:"))?
        .1;
    let code = match id.strip_prefix("bar:")? {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "page-up" => KeyCode::PageUp,
        "page-down" => KeyCode::PageDown,
        _ => return None,
    };
    Some(KeyEvent::new(code, KeyModifiers::NONE))
}
pub fn activate(ui: &mut Ui, action: &str) -> Step {
    match action {
        "actions" => {
            crate::context_menu::open_selected(ui);
            Step::Continue
        }
        "map" => press(ui, KeyCode::F(4), KeyModifiers::NONE),
        "fullscreen" => press(ui, KeyCode::F(11), KeyModifiers::NONE),
        "checkpoint" => press(ui, KeyCode::F(5), KeyModifiers::NONE),
        "project" => press(ui, KeyCode::F(6), KeyModifiers::NONE),
        "tasks" => press(ui, KeyCode::F(7), KeyModifiers::NONE),
        "new-pane" => {
            if ui.embed.as_ref().is_some_and(|e| e.mode == Mode::Control) {
                crate::prefix_command(ui, KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE))
            } else {
                press(ui, KeyCode::Char('N'), KeyModifiers::NONE)
            }
        }
        "new-workspace" => press(ui, KeyCode::Char('n'), KeyModifiers::NONE),
        "detach" => {
            crate::close_focus(ui);
            Step::Continue
        }
        "switch" => crate::open_switcher(ui),
        "observe" => {
            if let Some(e) = ui.embed.as_mut() {
                e.prefix = false;
                e.observe();
            }
            Step::Continue
        }
        "options" => {
            crate::open_options(ui);
            Step::Continue
        }
        "hide" => {
            crate::change_option(ui, crate::options::Item::Keys, true);
            Step::Continue
        }
        "saver" => {
            crate::toggle_saver(ui);
            Step::Continue
        }
        "help" => {
            crate::shortcuts::key(ui, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
            Step::Continue
        }
        "enter" => press(ui, KeyCode::Enter, KeyModifiers::NONE),
        "esc" => press(ui, KeyCode::Esc, KeyModifiers::NONE),
        "tab" => press(ui, KeyCode::Tab, KeyModifiers::NONE),
        "left" => press(ui, KeyCode::Left, KeyModifiers::NONE),
        "right" => press(ui, KeyCode::Right, KeyModifiers::NONE),
        "page-up" => press(ui, KeyCode::PageUp, KeyModifiers::NONE),
        "page-down" => press(ui, KeyCode::PageDown, KeyModifiers::NONE),
        "search" => press(ui, KeyCode::Char('/'), KeyModifiers::NONE),
        "commands" => press(ui, KeyCode::Char('p'), KeyModifiers::CONTROL),
        "refresh" => press(ui, KeyCode::Char('r'), KeyModifiers::NONE),
        "quit" => press(ui, KeyCode::Char('q'), KeyModifiers::NONE),
        _ => Step::Continue,
    }
}
