//! Neon and glitch: the brand's gradient, block letters, text that decodes
//! into place and brief color glitches. Each effect is a pure function of
//! time and position, so frames are reproducible. Without motion, color or
//! Unicode they fall back to plain, still text, and they only ever recolor
//! or briefly scramble decoration: what text says is never moved.
use crate::theme::{self, Rgb, Theme};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
};

/// Cyan into violet into rose.
pub fn gradient(t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0) * 2.0;
    if t <= 1.0 {
        theme::mix(theme::CYAN, theme::VIOLET, t)
    } else {
        theme::mix(theme::VIOLET, theme::ROSE, t - 1.0)
    }
}

/// `text` colored letter by letter along the gradient, moved by `phase`.
pub fn spans(theme: &Theme, text: &str, phase: f32, bold: bool) -> Vec<Span<'static>> {
    let count = text.chars().count().max(2) - 1;
    let modifier = if bold {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    if !theme.color {
        return vec![Span::styled(
            text.to_owned(),
            Style::default().add_modifier(modifier),
        )];
    }
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let t = (i as f32 / count as f32 + phase).rem_euclid(2.0);
            let t = if t > 1.0 { 2.0 - t } else { t };
            Span::styled(
                c.to_string(),
                Style::default()
                    .fg(theme.rgb(gradient(t)))
                    .add_modifier(modifier),
            )
        })
        .collect()
}

/// A small, fixed pseudo-random number for `seed`.
pub fn hash(seed: u64) -> u64 {
    let mut x = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

const NOISE: [char; 16] = [
    'ｱ', 'ｶ', 'ﾂ', 'ﾈ', 'ﾜ', '∆', '¥', '§', '▓', '░', '╳', 'Ξ', '◇', '⌁', '≠', '∑',
];
const NOISE_ASCII: [char; 8] = ['#', '%', '&', '*', '+', '=', '?', '@'];

/// `text` decoding into place: at `t` 0..1 the letters settle from the left,
/// the rest show noise. Spaces stay spaces. Noise glyphs are one cell wide,
/// so the text keeps its width.
pub fn decode(text: &str, t: f32, seed: u64, ascii: bool) -> String {
    if t >= 1.0 {
        return text.to_owned();
    }
    let count = text.chars().count();
    let settled = (t.max(0.0) * count as f32) as usize;
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            if i < settled || c == ' ' || Span::raw(c.to_string()).width() != 1 {
                return c;
            }
            let pick = hash(seed ^ (i as u64) << 8) as usize;
            if ascii {
                NOISE_ASCII[pick % NOISE_ASCII.len()]
            } else {
                NOISE[pick % NOISE.len()]
            }
        })
        .collect()
}

/// A rare, short glitch: Some(seed) while one is on. None in the first
/// period, so a fresh start is always calm, and never without motion.
pub fn burst(theme: &Theme, period: f32, length: f32) -> Option<u64> {
    if !theme.animated() {
        return None;
    }
    let clock = theme.clock();
    let window = (clock / period) as u64;
    if window == 0 {
        return None;
    }
    let start = (hash(window) % 1000) as f32 / 1000.0 * (period - length);
    let at = clock - window as f32 * period;
    (at >= start && at < start + length).then_some(hash(window ^ 0xD3AD))
}

/// Block letters five pixels tall, drawn three rows high with half blocks.
fn glyph(c: char) -> Option<[&'static str; 5]> {
    Some(match c {
        'A' => [".###", "#..#", "####", "#..#", "#..#"],
        'D' => ["###.", "#..#", "#..#", "#..#", "###."],
        'E' => ["####", "#...", "###.", "#...", "####"],
        'L' => ["#...", "#...", "#...", "#...", "####"],
        'S' => [".###", "#...", ".##.", "...#", "###."],
        'U' => ["#..#", "#..#", "#..#", "#..#", ".##."],
        ' ' => ["..", "..", "..", "..", ".."],
        _ => return None,
    })
}

/// `text` in block letters: three rows of equal width, or None when a
/// letter has no block form.
pub fn banner(text: &str) -> Option<[String; 3]> {
    let letters: Vec<[&str; 5]> = text.chars().map(glyph).collect::<Option<_>>()?;
    let mut rows: [String; 3] = Default::default();
    for (n, letter) in letters.iter().enumerate() {
        for (r, row) in rows.iter_mut().enumerate() {
            if n > 0 {
                row.push(' ');
            }
            let top = letter[r * 2].as_bytes();
            let bottom = letter.get(r * 2 + 1).map(|s| s.as_bytes());
            for (i, &t) in top.iter().enumerate() {
                let b = bottom.is_some_and(|b| b[i] == b'#');
                row.push(match (t == b'#', b) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                });
            }
        }
    }
    Some(rows)
}

/// Draw `rows` of block letters at `area` in the gradient, with a violet
/// echo one cell to the right like a neon sign's shadow. During a glitch
/// one row slips sideways in rose.
pub fn draw_banner(buf: &mut Buffer, area: Rect, theme: &Theme, rows: &[String; 3]) {
    let glitch = burst(theme, 7.0, 0.16);
    let width = rows[0].chars().count().max(2);
    for (r, row) in rows.iter().enumerate() {
        let y = area.y + r as u16;
        if y >= area.bottom() {
            break;
        }
        let slip = glitch
            .filter(|seed| seed % 3 == r as u64)
            .map(|seed| 1 + (seed % 2) as u16);
        let cells: Vec<char> = row.chars().collect();
        for x in 0..cells.len() + 1 {
            let at = area.x + x as u16 + slip.unwrap_or(0);
            if at >= area.right() {
                break;
            }
            let own = cells.get(x).copied().filter(|c| *c != ' ');
            let echo = x
                .checked_sub(1)
                .and_then(|p| cells.get(p).copied())
                .filter(|c| *c != ' ');
            let cell = &mut buf[(at, y)];
            if let Some(c) = own {
                let t = x as f32 / (width - 1) as f32;
                let color = if slip.is_some() {
                    theme::ROSE
                } else {
                    gradient(t)
                };
                cell.set_char(c);
                cell.set_style(
                    Style::default()
                        .fg(theme.rgb(color))
                        .add_modifier(Modifier::BOLD),
                );
            } else if let Some(c) = echo.filter(|_| theme.color) {
                cell.set_char(c);
                cell.set_style(Style::default().fg(theme.mix(theme::VIOLET, theme::BG, 0.65)));
            }
        }
    }
}

/// The view-change glitch: for `t` < 1, a few bands of `area` flash in
/// cyan or rose and fade. Colors only; no text moves.
pub fn bands(buf: &mut Buffer, area: Rect, theme: &Theme, t: f32, seed: u64) {
    if t >= 1.0 || !theme.animated() || !theme.truecolor || area.height == 0 {
        return;
    }
    let strength = (1.0 - t).powi(2) * 0.7;
    for band in 0..3u64 {
        let pick = hash(seed ^ band);
        let y = area.y + (pick % u64::from(area.height)) as u16;
        let x0 = area.x + ((pick >> 16) % u64::from(area.width.max(1))) as u16 / 2;
        let len = (area.width / 3).max(4) + ((pick >> 32) % 12) as u16;
        let hue = if band % 2 == 0 {
            theme::CYAN
        } else {
            theme::ROSE
        };
        for x in x0..(x0 + len).min(area.right()) {
            let cell = &mut buf[(x, y)];
            if let ratatui::style::Color::Rgb(r, g, b) = cell.fg {
                cell.fg = theme.mix((r, g, b), hue, strength);
            }
            if let ratatui::style::Color::Rgb(r, g, b) = cell.bg {
                cell.bg = theme.mix((r, g, b), hue, strength * 0.25);
            }
        }
    }
}

/// Border cells of `area` recolored along the gradient; while `t` < 1 the
/// light sweeps in from the left and a few segments flicker.
pub fn border(buf: &mut Buffer, area: Rect, theme: &Theme, t: f32, seed: u64) {
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
            let fx = (x - area.left()) as f32 / (right - area.left()).max(1) as f32;
            let lit = if t < 1.0 {
                ((t * 1.6 - fx) * 4.0).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let flicker =
                t < 0.5 && hash(seed ^ u64::from(x) << 16 ^ u64::from(y)).is_multiple_of(7);
            let level = if flicker { 0.15 } else { lit };
            cell.fg = theme.mix(theme::EDGE, gradient(fx), level);
        }
    }
}

/// A travelling colour highlight. Glyphs and hit geometry stay untouched.
pub fn sweep(buf: &mut Buffer, area: Rect, theme: &Theme, t: f32, seed: u64) {
    if !theme.animated() || t >= 1.0 {
        return;
    }
    let area = area.intersection(buf.area);
    let head = t.clamp(0.0, 1.0) * (f32::from(area.width) + 12.0) - 6.0;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let strength = (1.0 - ((f32::from(x - area.x) - head) / 6.0).abs()).max(0.0);
            if strength > 0.0 {
                let hue = gradient(((seed % 11) as f32 / 11.0 + t).fract());
                let cell = &mut buf[(x, y)];
                cell.bg = theme.mix(theme::RAISED, hue, strength * 0.22);
            }
        }
    }
}

/// Short chromatic border breaks, bounded by the visible frame perimeter.
pub fn edge_glitch(buf: &mut Buffer, area: Rect, theme: &Theme, t: f32, seed: u64) {
    if !theme.animated() || t >= 1.0 || area.width < 2 || area.height < 2 {
        return;
    }
    let area = area.intersection(buf.area);
    if area.width < 2 || area.height < 2 {
        return;
    }
    let phase = (t * 16.0) as u64;
    let points = (area.left()..area.right())
        .flat_map(|x| [(x, area.y), (x, area.bottom() - 1)])
        .chain((area.top()..area.bottom()).flat_map(|y| [(area.x, y), (area.right() - 1, y)]));
    for (x, y) in points {
        let cell = &mut buf[(x, y)];
        if !matches!(
            cell.symbol(),
            "─" | "━"
                | "│"
                | "┃"
                | "╭"
                | "╮"
                | "╰"
                | "╯"
                | "┏"
                | "┓"
                | "┗"
                | "┛"
                | "+"
                | "-"
                | "|"
        ) {
            continue;
        }
        let pick = hash(seed ^ u64::from(x / 3) ^ (u64::from(y) << 16) ^ phase);
        if pick % 5 < 2 {
            cell.fg = theme.mix(
                theme::EDGE,
                gradient((pick % 100) as f32 / 100.0),
                1.0 - t * 0.7,
            );
        }
    }
}

/// Persistent circuit lattice in unused cube space. Faces and existing text are
/// exclusion zones. Reduced motion keeps the lattice still, including its lights.
pub fn cube_field(
    buf: &mut Buffer,
    area: Rect,
    faces: &[Rect],
    theme: &Theme,
    seconds: f32,
    seed: u64,
) {
    let area = area.intersection(buf.area);
    let phase = if theme.animated() {
        (seconds.max(0.0) * 8.0) as u32
    } else {
        0
    };
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if faces.iter().any(|r| r.contains((x, y).into())) || buf[(x, y)].symbol() != " " {
                continue;
            }
            let dx = (i32::from(x - area.x) * 2 - i32::from(area.width)).unsigned_abs();
            let dy = (i32::from(y - area.y) * 2 - i32::from(area.height)).unsigned_abs();
            let track = (dx / 2 + dy) % 12;
            let node = (x - area.x).is_multiple_of(8) && (y - area.y).is_multiple_of(3);
            if track != 0 && !node {
                continue;
            }
            let pulse = track == 0 && (dx / 2 + phase + seed as u32 % 32) % 32 < 3;
            let glyph = if pulse {
                theme.glyph("◇", "*")
            } else if node {
                theme.glyph("·", ".")
            } else {
                theme.glyph("╱", "/")
            };
            let hue = gradient(((dx + dy) % 80) as f32 / 80.0);
            buf[(x, y)]
                .set_symbol(glyph)
                .set_style(theme.surface().fg(theme.mix(
                    theme::BG,
                    hue,
                    if pulse { 0.65 } else { 0.25 },
                )));
        }
    }
}

/// Small glitch fragments in unused cube space; never write over a face.
pub fn fragments(buf: &mut Buffer, area: Rect, faces: &[Rect], theme: &Theme, t: f32, seed: u64) {
    if !theme.animated() || t >= 1.0 {
        return;
    }
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return;
    }
    for n in 0..24 {
        let pick = hash(seed ^ n ^ ((t * 12.0) as u64).rotate_left(24));
        let x = area.x + (pick % u64::from(area.width)) as u16;
        let y = area.y + ((pick >> 16) % u64::from(area.height)) as u16;
        if faces.iter().any(|r| r.contains((x, y).into())) || buf[(x, y)].symbol() != " " {
            continue;
        }
        let glyph = if theme.ascii {
            ["+", ":", "=", "."]
        } else {
            ["░", "▱", "⌁", "╳"]
        };
        buf[(x, y)]
            .set_symbol(glyph[(pick % 4) as usize])
            .set_fg(theme.mix(
                theme::BG,
                gradient((n % 12) as f32 / 12.0),
                (1.0 - t) * 0.55,
            ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_field_fills_gaps_without_touching_faces_and_freezes_with_reduced_motion() {
        let area = Rect::new(0, 0, 100, 32);
        let face = Rect::new(30, 8, 40, 16);
        let mut base = Buffer::empty(area);
        base[(0, 0)].set_symbol("X");
        for y in face.top()..face.bottom() {
            for x in face.left()..face.right() {
                base[(x, y)].set_symbol("T");
            }
        }
        let theme = Theme::default();
        let mut a = base.clone();
        cube_field(&mut a, area, &[face], &theme, 0.0, 1);
        assert!(a.content().iter().filter(|c| c.symbol() != " ").count() > 641);
        assert_eq!(a[(0, 0)].symbol(), "X");
        for y in face.top()..face.bottom() {
            for x in face.left()..face.right() {
                assert_eq!(a[(x, y)], base[(x, y)]);
            }
        }
        let mut still = theme;
        still.reduced_motion = true;
        let mut a = base.clone();
        let mut b = base.clone();
        cube_field(&mut a, area, &[face], &still, 0.0, 1);
        cube_field(&mut b, area, &[face], &still, 9.0, 1);
        assert_eq!(a, b);
    }
    #[test]
    fn interface_effects_preserve_text_and_respect_disabled_motion() {
        let area = Rect::new(2, 3, 30, 8);
        let mut original = Buffer::empty(area);
        for cell in &mut original.content {
            cell.set_symbol("─");
        }
        let theme = Theme::default();
        let mut animated = original.clone();
        sweep(&mut animated, area, &theme, 0.4, 7);
        edge_glitch(&mut animated, area, &theme, 0.4, 7);
        assert_ne!(animated, original);
        assert!(
            animated
                .content
                .iter()
                .zip(&original.content)
                .all(|(a, b)| a.symbol() == b.symbol())
        );
        for disabled in [
            Theme {
                reduced_motion: true,
                ..theme
            },
            Theme {
                color: false,
                ..theme
            },
        ] {
            let mut buf = original.clone();
            sweep(&mut buf, area, &disabled, 0.4, 7);
            edge_glitch(&mut buf, area, &disabled, 0.4, 7);
            fragments(&mut buf, area, &[], &disabled, 0.4, 7);
            assert_eq!(buf, original);
        }
        let mut settled = original.clone();
        sweep(&mut settled, area, &theme, 1.0, 7);
        edge_glitch(&mut settled, area, &theme, 1.0, 7);
        assert_eq!(settled, original);
    }

    #[test]
    fn fragments_are_clipped_and_do_not_enter_terminal_faces() {
        let area = Rect::new(4, 5, 40, 16);
        let face = Rect::new(12, 9, 18, 8);
        let theme = Theme::default();
        let mut buf = Buffer::empty(area);
        fragments(
            &mut buf,
            Rect::new(0, 0, 100, 100),
            &[face],
            &theme,
            0.2,
            19,
        );
        assert!(buf.content.iter().any(|c| c.symbol() != " "));
        for y in face.top()..face.bottom() {
            for x in face.left()..face.right() {
                assert_eq!(buf[(x, y)].symbol(), " ");
            }
        }
        // Even decorations outside the buffer are safe.
        sweep(&mut buf, Rect::new(90, 90, 2, 2), &theme, 0.2, 0);
        edge_glitch(&mut buf, Rect::new(90, 90, 2, 2), &theme, 0.2, 0);
    }

    #[test]
    fn the_gradient_runs_cyan_violet_rose() {
        assert_eq!(gradient(0.0), theme::CYAN);
        assert_eq!(gradient(0.5), theme::VIOLET);
        assert_eq!(gradient(1.0), theme::ROSE);
    }

    #[test]
    fn decoding_keeps_width_spaces_and_ends_on_the_text() {
        let text = "Watch read-only";
        let start = decode(text, 0.0, 7, false);
        assert_eq!(start.chars().count(), text.chars().count());
        assert_ne!(start, text);
        assert_eq!(start.chars().nth(5), Some(' '));
        assert_eq!(
            start,
            decode(text, 0.0, 7, false),
            "the same seed, the same frame"
        );
        assert!(decode(text, 0.0, 7, true).is_ascii());
        let half = decode(text, 0.5, 7, false);
        assert!(half.starts_with("Watch r"));
        assert_eq!(decode(text, 1.0, 7, false), text);
    }

    #[test]
    fn block_letters_spell_the_brand_and_refuse_the_rest() {
        let rows = banner("DAEDALUS").unwrap();
        let width = rows[0].chars().count();
        assert_eq!(width, 8 * 4 + 7);
        assert!(rows.iter().all(|r| r.chars().count() == width));
        assert!(rows[0].starts_with("█▀▀▄"), "{}", rows[0]);
        assert!(banner("DAEDALUS 2").is_none());
    }

    #[test]
    fn no_glitch_on_a_fresh_start_or_without_motion() {
        let theme = Theme::default();
        assert_eq!(burst(&theme, 7.0, 0.2), None);
        let still = Theme {
            reduced_motion: true,
            ..Theme::default()
        };
        assert_eq!(burst(&still, 7.0, 0.2), None);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 4));
        let before = buf.clone();
        bands(&mut buf, Rect::new(0, 0, 20, 4), &still, 0.0, 1);
        assert_eq!(buf, before);
    }
}
