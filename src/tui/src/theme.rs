//! Palette, icons and motion. Honors `NO_COLOR`, `DAEDALUS_ICONS=ascii|unicode`
//! and `DAEDALUS_REDUCED_MOTION=1`; nothing here depends on an icon font.
use ratatui::{
    style::{Color, Modifier, Style},
    symbols::border,
};
use std::time::Instant;

pub type Rgb = (u8, u8, u8);
pub const BG: Rgb = (13, 17, 23);
/// Terminal screens and previews: a slightly deeper inset than the base.
pub const SCREEN: Rgb = (9, 12, 17);
pub const SURFACE: Rgb = (17, 23, 31);
pub const RAISED: Rgb = (25, 33, 45);
pub const SELECT: Rgb = (31, 46, 66);
pub const EDGE: Rgb = (44, 56, 73);
pub const TEXT: Rgb = (221, 229, 239);
pub const MUTED: Rgb = (134, 147, 166);
pub const FAINT: Rgb = (78, 90, 108);
pub const CYAN: Rgb = (102, 204, 238);
pub const VIOLET: Rgb = (184, 160, 247);
pub const GREEN: Rgb = (126, 214, 162);
pub const AMBER: Rgb = (242, 199, 118);
pub const RED: Rgb = (241, 116, 124);
pub const BLUE: Rgb = (130, 172, 255);
pub const ROSE: Rgb = (244, 162, 204);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Text,
    Muted,
    Faint,
    Accent,
    Violet,
    Ok,
    Warn,
    Danger,
    Info,
    Rose,
}

impl Tone {
    pub fn rgb(self) -> Rgb {
        match self {
            Tone::Text => TEXT,
            Tone::Muted => MUTED,
            Tone::Faint => FAINT,
            Tone::Accent => CYAN,
            Tone::Violet => VIOLET,
            Tone::Ok => GREEN,
            Tone::Warn => AMBER,
            Tone::Danger => RED,
            Tone::Info => BLUE,
            Tone::Rose => ROSE,
        }
    }
    pub fn named(name: &str) -> Self {
        match name {
            "ok" => Tone::Ok,
            "warn" => Tone::Warn,
            "danger" => Tone::Danger,
            "accent" => Tone::Violet,
            "info" => Tone::Info,
            _ => Tone::Muted,
        }
    }
    /// Each navigation category keeps one hue so related pages read as a group.
    pub fn category(name: &str) -> Self {
        match name {
            "Work" => Tone::Accent,
            "Connect" => Tone::Violet,
            "System" => Tone::Ok,
            "Find" => Tone::Warn,
            _ => Tone::Accent,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub color: bool,
    pub truecolor: bool,
    pub ascii: bool,
    pub reduced_motion: bool,
    /// The background light and the artwork: truecolor and Unicode only,
    /// and `DAEDALUS_BACKDROP=0` turns them off.
    pub backdrop: bool,
    /// The boot splash; `DAEDALUS_SPLASH=0` skips it.
    pub splash: bool,
    /// Idle seconds before the screensaver starts: `DAEDALUS_SCREENSAVER`,
    /// 15 when unset, at least 3; 0 turns it off.
    pub screensaver: u64,
    pub epoch: Instant,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            color: true,
            truecolor: true,
            ascii: false,
            reduced_motion: false,
            backdrop: false,
            splash: false,
            screensaver: 0,
            epoch: Instant::now(),
        }
    }
}

impl Theme {
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok())
    }
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name| get(name).is_some_and(|v| !v.is_empty());
        let term = get("TERM").unwrap_or_default();
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .find_map(|name| get(name).filter(|v| !v.is_empty()));
        let utf8 = locale.is_none_or(|v| {
            let v = v.to_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        });
        let ascii = match get("DAEDALUS_ICONS").as_deref() {
            Some("ascii") => true,
            Some("unicode") => false,
            _ => !utf8 || term == "linux" || term == "dumb" || term.starts_with("vt"),
        };
        let color = !set("NO_COLOR") && term != "dumb";
        // Windows Terminal and the Windows console speak 24-bit color and do not
        // set COLORTERM. Without it the blue background maps to the gray ramp
        // and the window looks black.
        let truecolor = color
            && (get("COLORTERM").is_some_and(|v| v == "truecolor" || v == "24bit")
                || get("WT_SESSION").is_some()
                || cfg!(windows));
        Self {
            color,
            truecolor,
            ascii,
            reduced_motion: get("DAEDALUS_REDUCED_MOTION").is_some_and(|v| v == "1"),
            backdrop: color
                && truecolor
                && !ascii
                && get("DAEDALUS_BACKDROP").is_none_or(|v| v != "0"),
            splash: term != "dumb" && get("DAEDALUS_SPLASH").is_none_or(|v| v != "0"),
            screensaver: match get("DAEDALUS_SCREENSAVER").map(|v| v.trim().parse::<u64>()) {
                Some(Ok(0)) => 0,
                Some(Ok(seconds)) => seconds.max(3),
                _ => 15,
            },
            epoch: Instant::now(),
        }
    }
    pub fn animated(&self) -> bool {
        self.color && !self.reduced_motion
    }
    pub fn rgb(&self, rgb: Rgb) -> Color {
        if !self.color {
            Color::Reset
        } else if self.truecolor {
            Color::Rgb(rgb.0, rgb.1, rgb.2)
        } else {
            Color::Indexed(nearest(rgb))
        }
    }
    pub fn fg(&self, tone: Tone) -> Style {
        if self.color {
            return Style::default().fg(self.rgb(tone.rgb()));
        }
        match tone {
            Tone::Muted | Tone::Faint => Style::default().add_modifier(Modifier::DIM),
            Tone::Text | Tone::Info | Tone::Ok => Style::default(),
            _ => Style::default().add_modifier(Modifier::BOLD),
        }
    }
    pub fn mix(&self, a: Rgb, b: Rgb, t: f32) -> Color {
        self.rgb(mix(a, b, t))
    }
    pub fn surface(&self) -> Style {
        Style::default().bg(self.rgb(SURFACE)).fg(self.rgb(TEXT))
    }
    pub fn base(&self) -> Style {
        Style::default().bg(self.rgb(BG)).fg(self.rgb(TEXT))
    }
    pub fn screen(&self) -> Style {
        Style::default().bg(self.rgb(SCREEN)).fg(self.rgb(TEXT))
    }
    /// Selection bar. Without color it must still be visible, so it reverses.
    pub fn selected(&self, fade: f32) -> Style {
        if self.color {
            Style::default()
                .bg(self.mix(SURFACE, SELECT, fade))
                .fg(self.rgb(TEXT))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }
    pub fn keycap(&self) -> Style {
        if self.color {
            Style::default()
                .bg(self.rgb(RAISED))
                .fg(self.rgb(CYAN))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED)
        }
    }
    pub fn border(&self) -> border::Set<'static> {
        if self.ascii {
            border::Set {
                top_left: "+",
                top_right: "+",
                bottom_left: "+",
                bottom_right: "+",
                vertical_left: "|",
                vertical_right: "|",
                horizontal_top: "-",
                horizontal_bottom: "-",
            }
        } else {
            border::ROUNDED
        }
    }
    /// The selected tile's frame: heavy lines where Unicode is available.
    pub fn border_strong(&self) -> border::Set<'static> {
        if self.ascii {
            self.border()
        } else {
            border::THICK
        }
    }
    pub fn glyph(&self, unicode: &'static str, ascii: &'static str) -> &'static str {
        if self.ascii { ascii } else { unicode }
    }
    pub fn icon(&self, name: &str) -> &'static str {
        let (unicode, ascii) = icon(&name.to_ascii_lowercase());
        self.glyph(unicode, ascii)
    }
    /// Tree connectors arrive as box-drawing text; map them for ASCII terminals.
    pub fn branch(&self, text: &str) -> String {
        if !self.ascii {
            return text.to_owned();
        }
        text.chars()
            .map(|c| match c {
                '├' | '│' => '|',
                '└' => '`',
                '─' => '-',
                c if c.is_ascii() => c,
                _ => ' ',
            })
            .collect()
    }
    /// Seconds since start, for motion. Reduced motion freezes time at zero.
    pub fn clock(&self) -> f32 {
        if self.animated() {
            self.epoch.elapsed().as_secs_f32()
        } else {
            0.0
        }
    }
    /// Smooth 0..1 wave with the given period; constant without motion.
    pub fn breathe(&self, period: f32) -> f32 {
        if !self.animated() {
            return 1.0;
        }
        0.5 + 0.5 * (self.clock() * std::f32::consts::TAU / period).sin()
    }
    pub fn spinner(&self) -> &'static str {
        if !self.animated() {
            return self.glyph("◌", "*");
        }
        let frames: [&str; 4] = if self.ascii {
            ["-", "\\", "|", "/"]
        } else {
            ["◐", "◓", "◑", "◒"]
        };
        frames[(self.clock() * 8.0) as usize % 4]
    }
}

pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let channel = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}

/// Nearest entry of the xterm 256-color cube or gray ramp.
pub fn nearest(rgb: Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, l)| (**l as i32 - v as i32).abs())
            .map(|(i, _)| i as u8)
            .unwrap_or(0)
    };
    let distance = |a: Rgb| {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a.0, rgb.0) + d(a.1, rgb.1) + d(a.2, rgb.2)
    };
    let (r, g, b) = (level(rgb.0), level(rgb.1), level(rgb.2));
    let cube = 16 + 36 * r + 6 * g + b;
    let cube_rgb = (LEVELS[r as usize], LEVELS[g as usize], LEVELS[b as usize]);
    let average = (rgb.0 as u32 + rgb.1 as u32 + rgb.2 as u32) / 3;
    let gray = ((average.saturating_sub(8) + 5) / 10).min(23) as u8;
    let gray_value = 8 + 10 * gray;
    if distance((gray_value, gray_value, gray_value)) < distance(cube_rgb) {
        232 + gray
    } else {
        cube
    }
}

/// ASCII stand-in for one rendered character; unknown symbols become `?`.
pub fn ascii_char(c: char) -> char {
    match c {
        c if c.is_ascii() => c,
        '·' | '—' | '–' | '─' | '━' | '┄' | '┈' | '═' | '▰' => '-',
        '│' | '┃' | '║' | '▏' | '▌' => '|',
        '›' | '→' | '⇢' | '»' | '❯' | '➤' | '▸' | '▷' => '>',
        '‹' | '←' | '«' => '<',
        '…' => '~',
        '“' | '”' | '„' => '"',
        '‘' | '’' => '\'',
        '×' => 'x',
        '•' | '●' | '◆' => '*',
        '○' | '◌' | '◎' | '◉' => 'o',
        c if ('\u{2500}'..='\u{257F}').contains(&c) => '+',
        _ => '?',
    }
}

/// In ASCII mode, rewrite every cell so no backend text can escape the fallback.
pub fn asciify(buffer: &mut ratatui::buffer::Buffer) {
    let mut replaced = false;
    for cell in buffer.content.iter_mut() {
        let symbol = cell.symbol();
        if symbol.is_empty() {
            // Continuation of a wide character that was just replaced.
            if replaced {
                cell.set_symbol(" ");
            }
            continue;
        }
        replaced = !symbol.is_ascii();
        if replaced {
            let c = ascii_char(symbol.chars().next().unwrap_or('?'));
            let mut buf = [0u8; 4];
            cell.set_symbol(c.encode_utf8(&mut buf));
        }
    }
}

/// Semantic icon names from the backend → (unicode, ascii). One cell each.
pub fn icon(name: &str) -> (&'static str, &'static str) {
    match name {
        "overview" => ("◈", "*"),
        "workspaces" => ("▦", "#"),
        "sessions" => ("↻", "@"),
        "connections" => ("⇄", "~"),
        "tools" => ("✦", "+"),
        "activity" => ("◷", "%"),
        "instances" => ("◫", "="),
        "hooks" => ("↯", "&"),
        "armonia" => ("⊛", "*"),
        "help" | "guide" => ("?", "?"),
        "info" => ("ⓘ", "i"),
        "commands" => ("⌘", ">"),
        "setup" => ("✧", "*"),
        "add" => ("⊕", "+"),
        "viewers" => ("◎", "o"),
        "server" => ("▣", "#"),
        "output" => ("▤", "="),
        "focus" => ("⇥", ">"),
        "linked" => ("⇆", "="),
        "send" => ("⇢", ">"),
        "rename" | "edit" => ("✎", "~"),
        "organize" => ("▥", "#"),
        "interrupt" => ("⎋", "^"),
        "close" => ("✕", "x"),
        "save" | "checkpoint" => ("⚑", "@"),
        "secret" => ("◆", "*"),
        "signin" => ("◇", "@"),
        "route" => ("↪", ">"),
        "start" => ("▷", ">"),
        "stop" => ("■", "x"),
        "connect" => ("◉", "o"),
        "status" => ("≈", "~"),
        "disconnect" => ("⊘", "x"),
        "choose" => ("✓", "v"),
        "open" => ("→", ">"),
        "install" => ("⇣", "v"),
        "profile" => ("⌂", "~"),
        "gateway" => ("⇌", "="),
        "history" => ("≡", "="),
        "tasks" => ("≣", "="),
        "warning" => ("△", "!"),
        "health" => ("♡", "+"),
        "toggle" => ("◐", "o"),
        "sources" => ("¶", "p"),
        "reference" => ("§", "s"),
        "instance" => ("●", "*"),
        "instance-saved" => ("○", "o"),
        "telegram" => ("➤", ">"),
        "terminal" => ("▣", ">"),
        "tmux" => ("▦", "T"),
        "herdr" => ("▥", "H"),
        "claude" | "claude code" => ("✳", "C"),
        "codex" => ("⬡", "X"),
        "grok" | "grok bot" => ("✧", "G"),
        "hermes" => ("⚚", "H"),
        "cursor" => ("➚", "C"),
        "pi" => ("π", "P"),
        "opencode" => ("◇", "O"),
        "agy" => ("△", "A"),
        "refresh" => ("↻", "r"),
        "memory" | "hindsight" => ("◈", "M"),
        "feeder" => ("⇢", ">"),
        "goal" => ("◎", "G"),
        "shell" => ("$", "$"),
        "agent" => ("❯", ">"),
        "tab" => ("▭", "-"),
        "project" => ("▤", "="),
        "view" => ("↳", "\\"),
        "done" => ("✓", "v"),
        "pending" => ("○", "o"),
        "search" => ("⌕", "/"),
        "attach" => ("⇘", ">"),
        "watch" => ("⊙", "o"),
        "external" => ("⇱", "^"),
        "fork" => ("⋔", "Y"),
        "pin" => ("★", "+"),
        "layout" => ("▦", "#"),
        "workflow" => ("⇶", ">"),
        "tiles" => ("▚", "#"),
        "wall" => ("⊞", "W"),
        "cube" => ("◇", "C"),
        "tree" => ("≡", "="),
        "missing" => ("◌", "?"),
        "options" => ("⚙", "*"),
        "order" => ("⇅", "^"),
        "sidebar" => ("◧", "|"),
        "details" => ("◨", "|"),
        "recent" => ("◔", "o"),
        "counts" => ("№", "#"),
        "totals" => ("∑", "="),
        "size" => ("⊡", "+"),
        "background" => ("◑", "o"),
        "saver" => ("☾", "c"),
        "motion" => ("≋", "~"),
        "keys" => ("⌨", "k"),
        "reset" => ("↺", "0"),
        _ => ("•", "-"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn theme(vars: &[(&str, &str)]) -> Theme {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Theme::from_vars(|name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    }
    #[test]
    fn environment_controls_color_icons_and_motion() {
        assert!(theme(&[]).color);
        assert!(!theme(&[("NO_COLOR", "1")]).color);
        // An empty NO_COLOR does not disable color (no-color.org).
        assert!(theme(&[("NO_COLOR", "")]).color);
        assert!(theme(&[("LANG", "C")]).ascii);
        assert!(!theme(&[("LANG", "en_US.UTF-8")]).ascii);
        assert!(theme(&[("TERM", "linux")]).ascii);
        assert!(!theme(&[("TERM", "linux"), ("DAEDALUS_ICONS", "unicode")]).ascii);
        assert!(theme(&[("DAEDALUS_ICONS", "ascii")]).ascii);
        assert!(theme(&[("DAEDALUS_REDUCED_MOTION", "1")]).reduced_motion);
        assert!(theme(&[("COLORTERM", "truecolor")]).truecolor);
        assert!(theme(&[("WT_SESSION", "1"), ("LANG", "en_US.UTF-8")]).truecolor);
        assert!(theme(&[("WT_SESSION", "1"), ("LANG", "en_US.UTF-8")]).backdrop);
        assert!(!theme(&[("WT_SESSION", "1"), ("NO_COLOR", "1")]).truecolor);
        // The splash shows unless turned off or on a dumb terminal.
        assert!(theme(&[]).splash);
        assert!(theme(&[("TERM", "xterm-256color")]).splash);
        assert!(theme(&[("DAEDALUS_SPLASH", "1")]).splash);
        assert!(!theme(&[("DAEDALUS_SPLASH", "0")]).splash);
        assert!(!theme(&[("TERM", "dumb")]).splash);
        assert_eq!(theme(&[]).screensaver, 15);
        assert_eq!(theme(&[("DAEDALUS_SCREENSAVER", "0")]).screensaver, 0);
        assert_eq!(theme(&[("DAEDALUS_SCREENSAVER", "1")]).screensaver, 3);
        assert_eq!(theme(&[("DAEDALUS_SCREENSAVER", "600")]).screensaver, 600);
        assert_eq!(theme(&[("DAEDALUS_SCREENSAVER", "soon")]).screensaver, 15);
    }
    #[test]
    fn ascii_icons_and_branches_are_single_ascii_cells() {
        let ascii = Theme {
            ascii: true,
            ..Theme::default()
        };
        for name in [
            "overview", "commands", "telegram", "unknown", "secret", "agent", "attach", "fork",
            "workflow", "tiles", "wall", "cube",
        ] {
            let glyph = ascii.icon(name);
            assert_eq!(glyph.len(), 1);
            assert!(glyph.is_ascii());
        }
        assert_eq!(ascii.branch("│  ├─ └─ "), "|  |- `- ");
    }
    #[test]
    fn indexed_fallback_finds_close_colors() {
        assert_eq!(nearest((0, 0, 0)), 16);
        assert_eq!(nearest((255, 255, 255)), 231);
        assert!((232..=255).contains(&nearest((128, 128, 128))));
        let plain = Theme {
            truecolor: false,
            ..Theme::default()
        };
        assert!(matches!(plain.rgb(CYAN), Color::Indexed(_)));
    }
    #[test]
    fn reduced_motion_freezes_animation() {
        let still = Theme {
            reduced_motion: true,
            ..Theme::default()
        };
        assert_eq!(still.breathe(2.0), 1.0);
        assert_eq!(still.clock(), 0.0);
        assert_eq!(still.spinner(), "◌");
    }
}
