//! Text selection over every drawn terminal screen.
//!
//! Drag selects. Shift+drag selects even when the program inside wants the
//! mouse. Ctrl+C copies the selection; with nothing selected it keeps its
//! other meaning. Ctrl+V pastes into an attached terminal on Windows.
use crate::model::Ui;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind, KeyModifiers};
use ratatui::{buffer::Buffer, style::Modifier};

/// Map a screen cell to a terminal id and a source row and column.
pub fn cell_at(ui: &Ui, column: u16, row: u16) -> Option<(String, (u16, u16))> {
    let mut best: Option<(u32, String, (u16, u16))> = None;
    for (rect, id, rows) in &ui.screens {
        if column < rect.x || row < rect.y || column >= rect.right() || row >= rect.bottom() {
            continue;
        }
        let Some(&(src_row, left)) = rows.get(usize::from(row - rect.y)) else {
            continue;
        };
        let cell = (src_row, left.saturating_add(column - rect.x));
        let area = u32::from(rect.width) * u32::from(rect.height);
        if best.as_ref().is_none_or(|(size, _, _)| area < *size) {
            best = Some((area, id.clone(), cell));
        }
    }
    best.map(|(_, id, cell)| (id, cell))
}

/// Own a drag that is selecting text. A plain click still reaches the view.
pub fn mouse(ui: &mut Ui, mouse: MouseEvent) -> bool {
    let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let Some((id, cell)) = cell_at(ui, mouse.column, mouse.row) else {
                ui.text_drag = None;
                ui.selection = None;
                return false;
            };
            if shift || !app_wants_mouse(ui, mouse.column, mouse.row) {
                if !shift {
                    ui.selection = None;
                }
                ui.text_drag = Some((id.clone(), cell));
                if shift {
                    ui.selection = Some((id, cell, cell));
                    return true;
                }
            } else {
                ui.text_drag = None;
            }
            false
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let Some((id, anchor)) = ui.text_drag.clone() else {
                return false;
            };
            let Some((hit, cell)) = cell_at(ui, mouse.column, mouse.row) else {
                return true;
            };
            if hit == id && cell != anchor {
                ui.selection = Some((id, anchor, cell));
            }
            true
        }
        MouseEventKind::Up(MouseButton::Left) => {
            ui.text_drag = None;
            false
        }
        _ => false,
    }
}

/// The selected text, or nothing when the range is empty.
pub fn text(ui: &Ui) -> Option<String> {
    let (id, anchor, head) = ui.selection.clone()?;
    if let Some(embed) = ui.embed.as_ref().filter(|embed| embed.plan.node == id) {
        return Some(copy_text(embed.parser.screen(), anchor, head));
    }
    let preview = ui.previews.get(&id)?;
    Some(copy_text(preview.screen.screen(), anchor, head))
}

pub fn paint(buf: &mut Buffer, ui: &Ui) {
    let Some((id, anchor, head)) = ui.selection.clone() else {
        return;
    };
    let (start, end) = ordered(anchor, head);
    for (rect, screen_id, rows) in &ui.screens {
        if screen_id != &id {
            continue;
        }
        for (i, &(src_row, left)) in rows.iter().enumerate() {
            let y = rect.y + i as u16;
            if y >= rect.bottom() {
                break;
            }
            for x in 0..rect.width {
                let col = left.saturating_add(x);
                if (src_row, col) >= start && (src_row, col) <= end {
                    buf[(rect.x + x, y)].modifier.insert(Modifier::REVERSED);
                }
            }
        }
    }
}

fn app_wants_mouse(ui: &Ui, column: u16, row: u16) -> bool {
    let Some(embed) = ui.embed.as_ref() else {
        return false;
    };
    if !embed.is_tmux() || embed.mode != crate::embed::Mode::Control || !embed.live() {
        return false;
    }
    let Some((x, y, w, h)) = embed.origin else {
        return false;
    };
    if column < x || row < y || column >= x + w || row >= y + h {
        return false;
    }
    embed.parser.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
}

fn ordered(a: (u16, u16), b: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    if a <= b { (a, b) } else { (b, a) }
}

pub fn copy_text(screen: &vt100::Screen, anchor: (u16, u16), head: (u16, u16)) -> String {
    let (start, end) = ordered(anchor, head);
    let mut lines = Vec::new();
    for row in start.0..=end.0 {
        let from = if row == start.0 { start.1 } else { 0 };
        let to = if row == end.0 {
            end.1
        } else {
            screen.size().1.saturating_sub(1)
        };
        let mut line = String::new();
        let mut col = from;
        while col <= to {
            let Some(cell) = screen.cell(row, col) else {
                break;
            };
            if cell.is_wide_continuation() {
                col += 1;
                continue;
            }
            let contents = cell.contents();
            if contents.is_empty() {
                line.push(' ');
            } else {
                line.push_str(contents);
            }
            col += if cell.is_wide() { 2 } else { 1 };
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseEvent};
    use ratatui::layout::Rect;

    fn event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn a_drag_selects_text_and_a_click_clears_it() {
        let mut ui = Ui::default();
        ui.screens
            .push((Rect::new(2, 3, 10, 2), "a".into(), vec![(0, 0), (1, 0)]));
        assert!(!mouse(&mut ui, event(MouseEventKind::Down(MouseButton::Left), 4, 3)));
        assert!(mouse(
            &mut ui,
            event(MouseEventKind::Drag(MouseButton::Left), 8, 4)
        ));
        assert_eq!(
            ui.selection,
            Some(("a".into(), (0, 2), (1, 6)))
        );
        mouse(&mut ui, event(MouseEventKind::Down(MouseButton::Left), 0, 0));
        assert!(ui.selection.is_none());
    }

    #[test]
    fn selected_text_keeps_every_column_of_the_range() {
        let mut parser = vt100::Parser::new(2, 12, 0);
        parser.process(b"hello world\r\nsecond");
        let text = copy_text(parser.screen(), (0, 6), (1, 5));
        assert_eq!(text, "world\nsecond");
    }
}
