//! Searchable shortcut reference. Input remains local while the overlay is open.
use crate::{model::Ui, theme::Tone};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
#[derive(Clone)]
pub struct Binding {
    pub scope: String,
    pub key: String,
    pub description: String,
}
pub fn bindings(ui: &Ui) -> Vec<Binding> {
    let mut rows = [
        ("Dashboard", "F1", "Search all keyboard shortcuts"),
        ("Dashboard", "Ctrl+P", "Search commands and actions"),
        ("Dashboard", "Ctrl+K", "Find and switch terminals"),
        ("Dashboard", ",", "Options and configuration settings"),
        ("Dashboard", "Tab / Shift+Tab", "Next / previous section"),
        ("Dashboard", "1–9 / 0", "Jump to sidebar section"),
        ("Dashboard", "/", "Search current list (outside Focus)"),
        ("Dashboard", "r", "Refresh inventory"),
        ("Dashboard", "?", "Show or hide the shortcut bar"),
        ("Dashboard", "q / Ctrl+C", "Exit Daedalus"),
        ("Dashboard", "Ctrl+U", "Check for updates"),
        (
            "Terminal switcher",
            "Alt+1–9",
            "Open a numbered search result",
        ),
        ("Options", "Up / Down / j / k", "Select a setting"),
        (
            "Options",
            "Left / Right / Enter / Space",
            "Change selected setting",
        ),
        (
            "Attached terminal",
            "Ctrl+] then Alt+arrows",
            "Resize real split panes in Focus",
        ),
        ("Lists", "Up / Down", "Select previous / next item"),
        ("Lists", "PageUp / PageDown", "Move ten rows"),
        ("Lists", "Home / End", "First / last item"),
        (
            "Details",
            "Alt+PageUp / Alt+PageDown",
            "Scroll the details panel without moving the selection",
        ),
        ("Lists", "Enter", "Open selected item or terminal menu"),
        (
            "Workspaces",
            "n",
            "Create a shell, agent terminal or workspace",
        ),
        ("Workspaces", "[ / ]", "Previous / next layout"),
        (
            "Workspaces",
            "z",
            "Focus selected terminal / return to previous layout",
        ),
        ("Workspaces", "m", "All actions for the selected terminal"),
        ("Workspaces", "o", "Order by activity or workspace layout"),
        ("Workspaces", "L", "Save current layout"),
        ("Workspaces", "W / B", "Wall with all / working terminals"),
        (
            "Workspaces",
            "F11",
            "Expand or restore selected terminal view",
        ),
        ("Tree", "Left / Right", "Collapse / expand selected branch"),
        ("Tree", "E / C", "Expand / collapse all branches"),
        ("Tiles / Wall", "Arrows", "Select another terminal"),
        (
            "Tiles",
            "PageUp / PageDown",
            "Previous / next page of terminals",
        ),
        ("Tiles / Wall / Cube", "P", "Show pinned terminals only"),
        (
            "Tiles / Wall / Cube",
            "u",
            "Remove a missing pinned terminal reference",
        ),
        (
            "Cube",
            "Arrows",
            "Rotate terminals within the workspace cube",
        ),
        (
            "Cube",
            "Alt+Left / Alt+Right",
            "Change front terminal without rotating cube",
        ),
        (
            "Tiles / Wall / Cube",
            "Alt+Up / Alt+Down",
            "Switch workspace / cube",
        ),
        ("Focus", "Alt+arrows", "Resize real split panes"),
        ("Focus", "Esc", "Leave Focus or detach"),
        ("Focus", "Arrows", "Select another pane in the same tab"),
        ("Focus", "Z", "Zoom selected pane / show whole tab"),
        (
            "Appearance",
            "< / >",
            "Adjust panel width or Cube terminal width",
        ),
        (
            "Appearance",
            "+ / -",
            "Adjust preview, tile or Cube terminal size",
        ),
        ("Appearance", "=", "Reset panel sizes"),
        ("Appearance", "\\", "Toggle sidebar"),
        ("Appearance", "b", "Cycle background"),
        ("Appearance", "s", "Toggle screensaver"),
        (
            "Attached terminal",
            "Ctrl+] then D",
            "Detach; keep the agent running",
        ),
        (
            "Attached terminal",
            "Ctrl+] then N",
            "Create a new pane in this project",
        ),
        (
            "Attached terminal",
            "Ctrl+] then K",
            "Find and switch terminals",
        ),
        (
            "Attached terminal",
            "Ctrl+] then G",
            "Assign this pane to a Telegram chat",
        ),
        (
            "Attached terminal",
            "Ctrl+] then M",
            "Open terminal actions",
        ),
        (
            "Attached terminal",
            "Ctrl+] then O",
            "Observe without sending input",
        ),
        (
            "Attached terminal",
            "Ctrl+] then A",
            "Request input control",
        ),
        (
            "Attached terminal",
            "Ctrl+] then R",
            "Resynchronize terminal preview",
        ),
        (
            "Attached terminal",
            "Ctrl+] then F",
            "Toggle fit to terminal size",
        ),
        (
            "Attached terminal",
            "Ctrl+] then T, twice",
            "Confirm takeover of a Herdr attach slot",
        ),
        (
            "Attached terminal",
            "Ctrl+] then [ / ]",
            "Switch layout while attached",
        ),
        (
            "Attached terminal",
            "Ctrl+] then arrows",
            "Change selected pane",
        ),
        (
            "Attached terminal",
            "Ctrl+] then W / B",
            "Show all / working terminals in Wall",
        ),
        ("Attached terminal", "Ctrl+] then U", "Check for updates"),
        (
            "Attached terminal",
            "Ctrl+] then Ctrl+]",
            "Send literal Ctrl+] to the terminal",
        ),
        (
            "Windows forms",
            "Ctrl+V / Shift+Insert",
            "Paste clipboard text into the input field",
        ),
        ("Dialogs", "Esc", "Cancel or close without applying changes"),
        (
            "Forms",
            "Left / Right / Home / End",
            "Move the cursor inside the current field",
        ),
        (
            "Forms",
            "Backspace / Delete",
            "Remove the character before / after the cursor",
        ),
        (
            "Dialogs",
            "Enter",
            "Confirm selected choice or entered value",
        ),
        (
            "Shortcut search",
            "Type / Backspace",
            "Filter by key, action or category",
        ),
        ("Shortcut search", "Up / Down", "Scroll matching shortcuts"),
    ]
    .into_iter()
    .map(|(s, k, d)| Binding {
        scope: s.into(),
        key: k.into(),
        description: d.into(),
    })
    .collect::<Vec<_>>();
    for a in &ui.snapshot.terminal_actions {
        if !a.quick.is_empty() {
            rows.push(Binding {
                scope: "Workspaces".into(),
                key: a.quick.clone(),
                description: a.title.clone(),
            });
        }
    }
    let mut seen = std::collections::HashSet::new();
    rows.retain(|r| seen.insert((r.scope.clone(), r.key.clone())));
    rows
}
pub fn matching(ui: &Ui) -> Vec<Binding> {
    let query = ui.binding_search.as_deref().unwrap_or("").to_lowercase();
    bindings(ui)
        .into_iter()
        .filter(|r| {
            let text = format!("{} {} {}", r.scope, r.key, r.description).to_lowercase();
            query.split_whitespace().all(|word| text.contains(word))
        })
        .collect()
}
pub fn key(ui: &mut Ui, key: KeyEvent) -> bool {
    if key.code == KeyCode::F(1) {
        ui.binding_search = if ui.binding_search.is_some() {
            None
        } else {
            Some(String::new())
        };
        ui.binding_scroll = 0;
        if let Some(e) = ui.embed.as_mut() {
            e.prefix = false;
        }
        return true;
    }
    if ui.binding_search.is_none() {
        return false;
    }
    match key.code {
        KeyCode::Esc => ui.binding_search = None,
        KeyCode::Up => ui.binding_scroll = ui.binding_scroll.saturating_sub(1),
        KeyCode::Down => {
            ui.binding_scroll = (ui.binding_scroll + 1).min(matching(ui).len().saturating_sub(1))
        }
        KeyCode::PageDown => {
            ui.binding_scroll = (ui.binding_scroll + 10).min(matching(ui).len().saturating_sub(1))
        }
        KeyCode::PageUp => ui.binding_scroll = ui.binding_scroll.saturating_sub(10),
        KeyCode::Home => ui.binding_scroll = 0,
        KeyCode::End => ui.binding_scroll = matching(ui).len().saturating_sub(1),
        KeyCode::Backspace => {
            ui.binding_search.as_mut().unwrap().pop();
            ui.binding_scroll = 0;
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            let q = ui.binding_search.as_mut().unwrap();
            if q.chars().count() < 160 {
                q.push(c);
            }
            ui.binding_scroll = 0;
        }
        _ => {}
    }
    true
}
pub fn draw(f: &mut Frame, area: Rect, ui: &Ui) {
    let w = area.width.saturating_sub(4).min(104);
    let h = area.height.saturating_sub(4).min(32);
    if w < 4 || h < 5 {
        return;
    }
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, rect);
    let theme = ui.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border())
        .border_style(theme.fg(Tone::Accent))
        .title(" Keyboard shortcuts ")
        .style(theme.surface());
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let rows = matching(ui);
    let room = inner.width.saturating_sub(2) as usize;
    let query = ui.binding_search.as_deref().unwrap_or("");
    let query = crate::ui_text::input_tail(query, room.saturating_sub(9));
    let header = vec![
        Line::from(vec![
            Span::styled(" Search  ", theme.fg(Tone::Info)),
            Span::styled(
                if query.is_empty() {
                    "Type a key or action"
                } else {
                    &query
                },
                theme.fg(if query.is_empty() {
                    Tone::Muted
                } else {
                    Tone::Text
                }),
            ),
        ]),
        Line::from(Span::styled(
            format!(
                " {} {}",
                rows.len(),
                if rows.len() == 1 {
                    "shortcut"
                } else {
                    "shortcuts"
                }
            ),
            theme.fg(Tone::Muted),
        )),
    ];
    f.render_widget(
        Paragraph::new(header),
        Rect::new(inner.x, inner.y, inner.width, 2.min(inner.height)),
    );
    let body = Rect::new(
        inner.x,
        (inner.y + 3).min(inner.bottom()),
        inner.width,
        inner.height.saturating_sub(4),
    );
    let start = ui.binding_scroll.min(rows.len().saturating_sub(1));
    let mut lines = Vec::new();
    let mut shown = 0;
    if rows.is_empty() {
        for line in crate::ui_text::wrap(
            "No shortcuts match. Try Telegram, cube, shell or resize.",
            room,
        ) {
            lines.push(Line::from(Span::styled(
                format!(" {line}"),
                theme.fg(Tone::Muted),
            )));
        }
    }
    for row in rows.iter().skip(start) {
        let mut entry = vec![Line::from(vec![
            Span::styled(" ", theme.fg(Tone::Text)),
            Span::styled(format!(" {} ", row.key), theme.keycap()),
        ])];
        if room >= row.key.chars().count() + row.scope.chars().count() + 6 {
            entry[0].spans.push(Span::styled(
                format!("  {}", row.scope),
                theme.fg(Tone::Muted),
            ));
        }
        entry.extend(
            crate::ui_text::wrap(&row.description, room.saturating_sub(2))
                .into_iter()
                .map(|line| Line::from(Span::styled(format!("  {line}"), theme.fg(Tone::Text)))),
        );
        if lines.len() + entry.len() > body.height as usize {
            break;
        }
        lines.extend(entry);
        shown += 1;
    }
    f.render_widget(Paragraph::new(lines), body);
    if inner.height >= 4 {
        let position = if rows.is_empty() {
            String::new()
        } else {
            format!(" · {}–{} / {}", start + 1, start + shown, rows.len())
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                crate::ui_text::fit(
                    &format!(" Esc Close · ↑↓ Scroll{position}"),
                    inner.width as usize,
                    &theme,
                ),
                theme.fg(Tone::Muted),
            )),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlay_is_visible_in_dashboard_at_common_sizes() {
        for (width, height) in [(60, 18), (100, 30), (160, 50)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            let mut ui = Ui {
                splash: None,
                binding_search: Some("telegram".into()),
                ..Ui::default()
            };
            terminal.draw(|f| crate::view::draw(f, &mut ui)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("Keyboard shortcuts"));
            assert!(text.contains("Telegram chat"));
        }
    }
    #[test]
    fn search_matches_all_words_and_does_not_send_input() {
        let mut ui = Ui::default();
        assert!(key(
            &mut ui,
            KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)
        ));
        for c in "telegram".chars() {
            assert!(key(
                &mut ui,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
            ));
        }
        assert_eq!(matching(&ui).len(), 1);
        assert!(matching(&ui)[0].key.contains("G"));
        assert!(key(
            &mut ui,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
        ));
        assert!(ui.binding_search.is_none());
    }
}
