//! Text selection over every drawn terminal screen.
//!
//! Drag selects. Shift+drag selects even when the program inside wants the
//! mouse. Ctrl+C copies the selection; with nothing selected it keeps its
//! other meaning. Ctrl+V pastes into an attached terminal on Windows.
use crate::model::Ui;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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
            if hit == id {
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

fn screen<'a>(ui: &'a Ui, id: &str) -> Option<&'a vt100::Screen> {
    if let Some(embed) = ui.embed.as_ref().filter(|embed| embed.plan.node == id) {
        return Some(embed.parser.screen());
    }
    ui.previews.get(id).map(|preview| preview.screen.screen())
}

/// The selected text, or nothing when its terminal screen is unavailable.
pub fn text(ui: &Ui) -> Option<String> {
    let (id, anchor, head) = ui.selection.as_ref()?;
    Some(copy_text(screen(ui, id)?, *anchor, *head))
}

pub fn paint(buf: &mut Buffer, ui: &Ui) {
    let Some((id, anchor, head)) = ui.selection.as_ref() else {
        return;
    };
    let (start, end) = screen(ui, id).map_or_else(
        || ordered(*anchor, *head),
        |screen| range(screen, *anchor, *head),
    );
    for (rect, screen_id, rows) in &ui.screens {
        if screen_id != id {
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

fn range(screen: &vt100::Screen, anchor: (u16, u16), head: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    let (mut start, mut end) = ordered(anchor, head);
    if screen
        .cell(start.0, start.1)
        .is_some_and(vt100::Cell::is_wide_continuation)
    {
        start.1 = start.1.saturating_sub(1);
    }
    if screen.cell(end.0, end.1).is_some_and(vt100::Cell::is_wide) {
        end.1 = end.1.saturating_add(1);
    }
    (start, end)
}

/// Copy inclusive cells, joining soft wraps and retaining explicit line breaks.
/// Either half of a wide character selects that whole character.
pub fn copy_text(screen: &vt100::Screen, anchor: (u16, u16), head: (u16, u16)) -> String {
    let (start, end) = range(screen, anchor, head);
    let mut text = String::new();
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
        let wrapped = row < end.0 && screen.row_wrapped(row);
        text.push_str(if wrapped { &line } else { line.trim_end() });
        if row < end.0 && !wrapped {
            text.push('\n');
        }
    }
    text
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
        assert!(!mouse(
            &mut ui,
            event(MouseEventKind::Down(MouseButton::Left), 4, 3)
        ));
        assert!(mouse(
            &mut ui,
            event(MouseEventKind::Drag(MouseButton::Left), 8, 4)
        ));
        assert_eq!(ui.selection, Some(("a".into(), (0, 2), (1, 6))));
        mouse(
            &mut ui,
            event(MouseEventKind::Down(MouseButton::Left), 0, 0),
        );
        assert!(ui.selection.is_none());
    }

    #[test]
    fn selected_text_keeps_every_column_of_the_range() {
        let mut parser = vt100::Parser::new(2, 12, 0);
        parser.process(b"hello world\r\nsecond");
        let text = copy_text(parser.screen(), (0, 6), (1, 5));
        assert_eq!(text, "world\nsecond");
    }

    #[test]
    fn dragging_back_to_the_anchor_replaces_the_previous_range() {
        let mut ui = Ui::default();
        ui.previews
            .insert("a".into(), crate::model::Preview::new("abcdefghij", ""));
        ui.screens
            .push((Rect::new(2, 3, 10, 1), "a".into(), vec![(0, 0)]));
        mouse(
            &mut ui,
            event(MouseEventKind::Down(MouseButton::Left), 4, 3),
        );
        mouse(
            &mut ui,
            event(MouseEventKind::Drag(MouseButton::Left), 8, 3),
        );
        assert_eq!(text(&ui).as_deref(), Some("cdefg"));
        mouse(
            &mut ui,
            event(MouseEventKind::Drag(MouseButton::Left), 4, 3),
        );
        assert_eq!(text(&ui).as_deref(), Some("c"));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 14, 5));
        paint(&mut buffer, &ui);
        assert!(buffer[(4, 3)].modifier.contains(Modifier::REVERSED));
        assert!(!buffer[(5, 3)].modifier.contains(Modifier::REVERSED));
        mouse(
            &mut ui,
            event(MouseEventKind::Drag(MouseButton::Left), 2, 3),
        );
        assert_eq!(text(&ui).as_deref(), Some("abc"));
    }

    #[test]
    fn copying_soft_wrapped_text_keeps_spaces_and_explicit_line_breaks() {
        let mut parser = vt100::Parser::new(5, 6, 0);
        parser.process(b"alpha beta\r\n\r\nlast");
        assert!(parser.screen().row_wrapped(0));
        assert!(!parser.screen().row_wrapped(1));
        for (anchor, head) in [((0, 0), (3, 3)), ((3, 3), (0, 0))] {
            assert_eq!(
                copy_text(parser.screen(), anchor, head),
                "alpha beta\n\nlast"
            );
        }
        assert_eq!(copy_text(parser.screen(), (0, 2), (1, 1)), "pha be");
    }

    #[test]
    fn copying_either_half_of_a_wide_character_keeps_the_whole_character() {
        let mut parser = vt100::Parser::new(2, 12, 0);
        parser.process("A界B🙂Ce\u{301}".as_bytes());
        assert!(parser.screen().cell(0, 2).unwrap().is_wide_continuation());
        assert!(parser.screen().cell(0, 5).unwrap().is_wide_continuation());
        for (anchor, head, expected) in [
            ((0, 2), (0, 5), "界B🙂"),
            ((0, 5), (0, 2), "界B🙂"),
            ((0, 2), (0, 2), "界"),
            ((0, 1), (0, 1), "界"),
            ((0, 7), (0, 7), "e\u{301}"),
        ] {
            assert_eq!(copy_text(parser.screen(), anchor, head), expected);
        }
    }

    #[test]
    fn painted_wide_character_selection_matches_the_copied_text() {
        let mut ui = Ui::default();
        ui.previews
            .insert("a".into(), crate::model::Preview::new("A界B🙂C", ""));
        ui.screens
            .push((Rect::new(0, 0, 12, 1), "a".into(), vec![(0, 0)]));
        ui.selection = Some(("a".into(), (0, 2), (0, 4)));
        assert_eq!(text(&ui).as_deref(), Some("界B🙂"));
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 1));
        paint(&mut buffer, &ui);
        for column in 0..12 {
            assert_eq!(
                buffer[(column, 0)].modifier.contains(Modifier::REVERSED),
                (1..=5).contains(&column),
            );
        }
    }
}
