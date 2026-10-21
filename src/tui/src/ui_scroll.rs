//! Shared viewport bounds and scroll indicators for lists and overlays.
use crate::theme::{Theme, Tone};
use ratatui::{Frame, layout::Rect, style::Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub offset: usize,
    pub visible: usize,
    pub total: usize,
}
impl Window {
    pub fn new(total: usize, visible: usize, requested: usize) -> Self {
        Self {
            offset: requested.min(total.saturating_sub(visible)),
            visible,
            total,
        }
    }
    pub fn following(total: usize, visible: usize, selected: usize) -> Self {
        Self::new(
            total,
            visible,
            selected.saturating_add(1).saturating_sub(visible),
        )
    }
    pub fn max_offset(self) -> usize {
        self.total.saturating_sub(self.visible)
    }
    pub fn overflow(self) -> bool {
        self.total > self.visible
    }
    pub fn end(self) -> usize {
        self.offset.saturating_add(self.visible).min(self.total)
    }
    pub fn label(self, theme: &Theme) -> String {
        if self.total == 0 || self.visible == 0 {
            format!("0 / {}", self.total)
        } else {
            format!(
                "{}{}{} / {}",
                self.offset + 1,
                theme.glyph("–", "-"),
                self.end(),
                self.total
            )
        }
    }
    fn thumb(self, height: usize) -> Option<(usize, usize)> {
        if !self.overflow() || self.visible == 0 || height == 0 {
            return None;
        }
        let size = ((self.visible as u128 * height as u128) / self.total as u128) as usize;
        let size = size.clamp(1, height);
        let start =
            ((self.offset as u128 * (height - size) as u128) / self.max_offset() as u128) as usize;
        Some((start, size))
    }
}

pub fn scrollbar(frame: &mut Frame, area: Rect, window: Window, theme: &Theme, tone: Tone) {
    let Some((start, size)) = window.thumb(area.height as usize) else {
        return;
    };
    if area.width == 0 {
        return;
    }
    for row in 0..area.height {
        let active = (start..start + size).contains(&(row as usize));
        if let Some(cell) = frame.buffer_mut().cell_mut((area.x, area.y + row)) {
            cell.set_symbol(theme.glyph(
                if active { "┃" } else { "│" },
                if active { "#" } else { "|" },
            ));
            cell.set_style(Style::default().patch(theme.fg(if active {
                tone
            } else {
                Tone::Faint
            })));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_and_zero_height_views_do_not_claim_a_visible_item() {
        assert_eq!(Window::new(0, 10, 100).label(&Theme::default()), "0 / 0");
        assert_eq!(
            Window::new(12, 0, usize::MAX).label(&Theme::default()),
            "0 / 12"
        );
        assert_eq!(Window::new(12, 0, 0).thumb(10), None);
    }
    #[test]
    fn long_results_reach_the_last_item_and_the_bottom_of_the_track() {
        let w = Window::new(100_000, 20, usize::MAX);
        assert_eq!((w.offset, w.end()), (99_980, 100_000));
        assert_eq!(w.thumb(20), Some((19, 1)));
        assert_eq!(Window::following(100, 10, 99), Window::new(100, 10, 90));
        let w = Window::new(usize::MAX, usize::MAX / 2, usize::MAX);
        assert!(w.thumb(100).is_some());
    }
}
