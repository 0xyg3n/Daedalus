//! Display-column text operations shared by interface panels and overlays.
//! Wrap each word once; never repeatedly scan or copy its remaining suffix.
use crate::theme::Theme;
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;

pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}

pub fn width(text: &str) -> usize {
    Span::raw(text).width()
}

pub fn fit(text: &str, max: usize, theme: &Theme) -> String {
    let text = clean(text).replace('\n', " ");
    if width(&text) <= max {
        return text;
    }
    let mut result = String::new();
    let mut used = 0;
    for cluster in text.graphemes(true) {
        let columns = width(cluster);
        if used + columns + 1 > max {
            break;
        }
        result.push_str(cluster);
        used += columns;
    }
    if max > 0 {
        result.push_str(theme.glyph("…", "~"));
    }
    result
}

/// Show the newest editable text, keeping the cursor inside its field and
/// preserving combining marks, emoji sequences and wide characters.
pub fn input_tail(text: &str, room: usize) -> String {
    let mut start = text.len();
    let mut used = 0;
    for (at, cluster) in text.grapheme_indices(true).rev() {
        let columns = width(cluster);
        if used + columns > room {
            break;
        }
        start = at;
        used += columns;
    }
    text[start..].to_owned()
}

pub fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut lines = Vec::new();
    for paragraph in clean(text).split('\n') {
        let body = paragraph.trim_start_matches(' ');
        let indent = " ".repeat((paragraph.len() - body.len()).min(max / 2));
        let room = max - indent.len();
        let mut line = String::new();
        let mut used = 0;
        for word in body.split(' ') {
            let columns = width(word);
            let gap = usize::from(!line.is_empty());
            if used + gap + columns <= room {
                if gap > 0 {
                    line.push(' ');
                }
                line.push_str(word);
                used += gap + columns;
                continue;
            }
            if !line.is_empty() {
                lines.push(format!("{indent}{}", std::mem::take(&mut line)));
                used = 0;
            }
            if columns <= room {
                line.push_str(word);
                used = columns;
                continue;
            }
            for cluster in word.graphemes(true) {
                let columns = width(cluster);
                if used + columns > room && !line.is_empty() {
                    lines.push(format!("{indent}{}", std::mem::take(&mut line)));
                    used = 0;
                }
                if columns > room {
                    lines.push(format!("{indent}?"));
                } else {
                    line.push_str(cluster);
                    used += columns;
                }
            }
        }
        lines.push(format!("{indent}{line}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphemes_survive_truncation_wrapping_and_editing() {
        let family = "👨‍👩‍👧‍👦";
        let accent = "e\u{301}";
        assert_eq!(input_tail(&format!("abc{family}"), 2), family);
        assert_eq!(input_tail(&format!("abc{accent}"), 1), accent);
        assert_eq!(
            fit(&format!("{family}abc"), 3, &Theme::default()),
            format!("{family}…")
        );
        assert_eq!(wrap(&format!("{family}{family}"), 2), [family, family]);
        assert_eq!(wrap(&accent.repeat(3), 1), [accent, accent, accent]);
        for room in 1..20 {
            assert!(
                wrap(&format!("  界 {family} {accent} text"), room)
                    .iter()
                    .all(|s| width(s) <= room)
            );
        }
    }

    #[test]
    fn large_unbroken_output_is_preserved_in_bounded_lines() {
        let input = "界".repeat(64_000);
        let lines = wrap(&input, 60);
        assert!(lines.iter().all(|line| width(line) <= 60));
        assert_eq!(lines.concat(), input);
        assert_eq!(wrap("  a b\n\nlast", 5), ["  a b", "", "last"]);
        assert_eq!(clean("ok\x1b\r\t\nnext"), "ok\nnext");
    }
}
