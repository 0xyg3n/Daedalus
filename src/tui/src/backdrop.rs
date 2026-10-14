//! The background behind the interface, and the boot splash.
//!
//! By default the background is a quiet light: a haze from the top in the
//! page's hue, a faint warm glow at the upper right where the artwork's moon
//! stands, and slightly deeper lower corners. Only background colors change,
//! by a few levels, so text and glyphs stay crisp.
//!
//! The artwork is drawn with quadrant block characters (`▘▝▖▌▞▛▀`): each cell
//! shows four sub-pixels in the two colors and the pattern that match the
//! picture best, which doubles the horizontal detail of half blocks. The
//! picture is scaled by averaging in linear light, then gently sharpened. The
//! boot splash shows it in full. `b` can also blend it far into the
//! background; then only cells painted in the plain base or surface color
//! are touched, so terminal screens, selections and keycaps never carry it.
//! The screensaver shows it full screen with glitches: rows shifted
//! sideways, the red and blue channels split apart, tinted bands, static,
//! scanlines and a light rolling down.
//! Embedded pixels are checked assets derived from `assets/daedalus.png`.
use crate::theme::{self, Rgb, Theme};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier},
};
use std::sync::OnceLock;

const SIZE: usize = 384;
static IMAGE: &[u8; SIZE * SIZE * 3] = include_bytes!("../assets/backdrop.rgb");

/// Share of the picture's side over which its edges fade into the
/// background: behind the interface, and on the splash.
const FEATHER: f32 = 0.08;
const SPLASH_FEATHER: f32 = 0.05;
/// How much of the difference from neighbouring sub-pixels is added back.
const SHARPEN: f32 = 0.45;
/// Glyphs by which of the upper-left, upper-right and lower-left quadrants
/// take the foreground color; the lower right is always background.
const QUADRANTS: [&str; 8] = [" ", "▘", "▝", "▀", "▖", "▌", "▞", "▛"];
/// Where the artwork's moon stands, as shares of its width and height.
const MOON: (f32, f32) = (0.72, 0.25);
/// Splash timing in seconds: the light spreads from the moon over the whole
/// picture, then a glint crosses it.
pub const REVEAL: f32 = 0.9;
const GLINT_AT: f32 = 1.0;
const GLINT: f32 = 0.6;
/// Screensaver timing: each slot of this many seconds holds at most one
/// glitch burst; some stay calm.
const SLOT: f32 = 2.2;
/// Glitch tints: magenta, cyan and white.
const TINTS: [Rgb; 3] = [(255, 40, 160), (40, 240, 255), (255, 255, 255)];

/// What fills the background; `b` cycles through them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Look {
    /// A soft light in the page's hue; background colors only.
    #[default]
    Glow,
    /// The artwork, faint.
    Art,
    /// The artwork, brighter.
    ArtBright,
    /// The plain background color.
    Off,
}
impl Look {
    const ALL: [Look; 4] = [Look::Glow, Look::Art, Look::ArtBright, Look::Off];
    pub fn name(self) -> &'static str {
        match self {
            Look::Glow => "glow",
            Look::Art => "art",
            Look::ArtBright => "art-bright",
            Look::Off => "off",
        }
    }
    pub fn describe(self) -> &'static str {
        match self {
            Look::Glow => "soft light",
            Look::Art => "artwork, faint",
            Look::ArtBright => "artwork, bright",
            Look::Off => "plain",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|&s| s == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
    /// The artwork's share over the base and the surface (panel) colors.
    fn amounts(self) -> (f32, f32) {
        match self {
            Look::Art => (0.26, 0.10),
            Look::ArtBright => (0.45, 0.19),
            Look::Glow | Look::Off => (0.0, 0.0),
        }
    }
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The soft light over every plain base or surface cell of `area`. Panels
/// take less of it than the margins, like frosted glass.
pub fn glow(buf: &mut Buffer, area: Rect, theme: &Theme, hue: Rgb) {
    if !theme.backdrop || area.width == 0 || area.height == 0 {
        return;
    }
    let base = Color::Rgb(theme::BG.0, theme::BG.1, theme::BG.2);
    let surface = Color::Rgb(theme::SURFACE.0, theme::SURFACE.1, theme::SURFACE.2);
    // Distances in units of one column; a row is two.
    let (w, h) = (area.width as f32, area.height as f32 * 2.0);
    let side = w.max(h);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            let (under, share) = if cell.bg == base {
                (theme::BG, 1.0)
            } else if cell.bg == surface {
                (theme::SURFACE, 0.6)
            } else {
                continue;
            };
            let u = (x - area.x) as f32 + 0.5;
            let v = ((y - area.y) as f32 + 0.5) * 2.0;
            let (across, down) = (u / w, v / h);
            let haze = (-(down / 0.5).powi(2)).exp()
                * (0.55 + 0.45 * (-((across - 0.6) / 0.4).powi(2)).exp());
            let (dx, dy) = ((u - 0.86 * w) / side, (v - 0.06 * h) / side);
            let moon = (-(dx * dx + dy * dy) / 0.02).exp();
            let corner =
                ((2.0 * across - 1.0).powi(2) + (2.0 * down - 1.0).max(0.0).powi(2)).sqrt();
            let deep = smooth((corner - 0.75) / 0.6);
            let c = theme::mix(under, hue, 0.08 * haze * share);
            let c = theme::mix(c, theme::AMBER, 0.07 * moon * share);
            let c = theme::mix(c, theme::SCREEN, 0.7 * deep);
            cell.bg = Color::Rgb(c.0, c.1, c.2);
        }
    }
}

/// The artwork scaled for one place on the screen.
#[derive(Default)]
pub struct Picture {
    /// Screen size, the frame the picture fits and its feather (as bits)
    /// that the pixels were scaled for.
    key: (u16, u16, Rect, u32),
    /// Two sub-pixels per column and two per row of the whole screen, each
    /// with its coverage: 0 outside the picture, fading in at its edges.
    pixels: Vec<(Rgb, f32)>,
}

fn to_linear(v: u8) -> f32 {
    let c = f32::from(v) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    255.0
        * if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
}

/// The artwork in linear light, so averaging keeps bright detail bright.
fn linear() -> &'static [f32] {
    static LINEAR: OnceLock<Vec<f32>> = OnceLock::new();
    LINEAR.get_or_init(|| IMAGE.iter().map(|&v| to_linear(v)).collect())
}

type Linear = [f32; 3];

fn source(x: usize, y: usize) -> Linear {
    let i = (y.min(SIZE - 1) * SIZE + x.min(SIZE - 1)) * 3;
    let image = linear();
    [image[i], image[i + 1], image[i + 2]]
}

/// Bilinear sample at fractional image coordinates, clamped to the image.
fn sample(x: f32, y: f32) -> Linear {
    let limit = (SIZE - 1) as f32;
    let (x, y) = (x.clamp(0.0, limit), y.clamp(0.0, limit));
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let lerp = |a: Linear, b: Linear, t: f32| {
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    };
    let top = lerp(source(x0, y0), source(x0 + 1, y0), fx);
    let bottom = lerp(source(x0, y0 + 1), source(x0 + 1, y0 + 1), fx);
    lerp(top, bottom, fy)
}

/// Where the picture sits in `frame`: its side and top-left corner, in
/// units of one column (a row is two). The whole artwork is shown, as large
/// as the frame allows, centered.
fn placement(frame: Rect) -> (f32, f32, f32) {
    let (w, h) = (frame.width as f32, frame.height as f32 * 2.0);
    let side = w.min(h);
    (
        side,
        frame.x as f32 + (w - side) / 2.0,
        frame.y as f32 * 2.0 + (h - side) / 2.0,
    )
}

/// The pattern and two colors that best match a cell's four sub-pixels
/// (upper left, upper right, lower left, lower right). Nearly even cells
/// stay plain, so the picture never adds noise.
fn best_split(colors: &[Rgb; 4]) -> (usize, Rgb, Rgb) {
    let channels = |c: &Rgb| [f32::from(c.0), f32::from(c.1), f32::from(c.2)];
    let mut best = (f32::MAX, 0, colors[0], colors[0]);
    for mask in 0..QUADRANTS.len() {
        let mut sums = [[0.0f32; 3]; 2];
        let mut counts = [0.0f32; 2];
        for (i, c) in colors.iter().enumerate() {
            let side = usize::from(mask >> i & 1 == 1);
            for (sum, v) in sums[side].iter_mut().zip(channels(c)) {
                *sum += v;
            }
            counts[side] += 1.0;
        }
        let mean = |side: usize| {
            let n = counts[side].max(1.0);
            [sums[side][0] / n, sums[side][1] / n, sums[side][2] / n]
        };
        let error: f32 = colors
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let m = mean(usize::from(mask >> i & 1 == 1));
                channels(c)
                    .iter()
                    .zip(m)
                    .map(|(v, m)| (v - m) * (v - m))
                    .sum::<f32>()
            })
            .sum();
        // A pattern must earn its place over a plain cell.
        let error = if mask == 0 { error - 24.0 } else { error };
        if error < best.0 {
            let rgb = |m: [f32; 3]| (m[0].round() as u8, m[1].round() as u8, m[2].round() as u8);
            best = (error, mask, rgb(mean(1)), rgb(mean(0)));
        }
    }
    (best.1, best.2, best.3)
}

impl Picture {
    /// Scale the artwork to fit `frame` on a screen of `cols` by `rows`.
    /// Shrinking averages every source pixel under a sub-pixel instead of
    /// picking one.
    fn prepare(&mut self, cols: u16, rows: u16, frame: Rect, feather: f32) {
        let key = (cols, rows, frame, feather.to_bits());
        if self.key == key && !self.pixels.is_empty() {
            return;
        }
        let (w, h) = (cols as usize * 2, rows as usize * 2);
        let (side, left, top) = placement(frame);
        // Source pixels per unit; a sub-pixel is half a unit wide, one tall.
        let scale = SIZE as f32 / side.max(1.0);
        let taps = |extent: f32, most: f32| (extent * scale).ceil().clamp(1.0, most) as usize;
        let (taps_x, taps_y) = (taps(0.5, 4.0), taps(1.0, 6.0));
        let feather = (side * feather).max(1.0);
        let mut light: Vec<([f32; 3], f32)> = Vec::with_capacity(w * h);
        for j in 0..h {
            for i in 0..w {
                let (x, y) = (i as f32 * 0.5, j as f32);
                let (u, v) = (x + 0.25 - left, y + 0.5 - top);
                if u <= 0.0 || v <= 0.0 || u >= side || v >= side {
                    light.push(([0.0; 3], 0.0));
                    continue;
                }
                let mut sum = [0.0f32; 3];
                for a in 0..taps_x {
                    for b in 0..taps_y {
                        let sx = (x + (a as f32 + 0.5) / taps_x as f32 * 0.5 - left) * scale;
                        let sy = (y + (b as f32 + 0.5) / taps_y as f32 - top) * scale;
                        let c = sample(sx - 0.5, sy - 0.5);
                        for k in 0..3 {
                            sum[k] += c[k];
                        }
                    }
                }
                let n = (taps_x * taps_y) as f32;
                let rgb = sum.map(|s| to_srgb(s / n));
                let edge = (u.min(v).min(side - u).min(side - v) / feather).min(1.0);
                light.push((rgb, smooth(edge)));
            }
        }
        // Unsharp mask on the scaled picture: edges keep their crispness.
        let at = |i: usize, j: usize| light[j * w + i];
        self.pixels = (0..h)
            .flat_map(|j| (0..w).map(move |i| (i, j)))
            .map(|(i, j)| {
                let (c, coverage) = at(i, j);
                if coverage == 0.0 {
                    return ((0, 0, 0), 0.0);
                }
                let near = |i: usize, j: usize| {
                    let (n, cover) = at(i, j);
                    if cover > 0.0 { n } else { c }
                };
                let neighbours = [
                    near(i.saturating_sub(1), j),
                    near((i + 1).min(w - 1), j),
                    near(i, j.saturating_sub(1)),
                    near(i, (j + 1).min(h - 1)),
                ];
                let channel = |k: usize| {
                    let blur = neighbours.iter().map(|n| n[k]).sum::<f32>() / 4.0;
                    (c[k] + SHARPEN * (c[k] - blur)).clamp(0.0, 255.0).round() as u8
                };
                ((channel(0), channel(1), channel(2)), coverage)
            })
            .collect();
        self.key = key;
    }

    fn pixel(&self, x: usize, y: usize) -> (Rgb, f32) {
        self.pixels[y * self.key.0 as usize * 2 + x]
    }

    /// The four sub-pixels of the cell at (x, y): upper left, upper right,
    /// lower left, lower right.
    fn quad(&self, x: u16, y: u16) -> [(usize, usize, Rgb, f32); 4] {
        let (sx, sy) = (x as usize * 2, y as usize * 2);
        [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(dx, dy)| {
            let (c, cover) = self.pixel(sx + dx, sy + dy);
            (sx + dx, sy + dy, c, cover)
        })
    }

    /// Blend the artwork faintly into every plain base or surface cell of
    /// `area`, as large as the screen allows.
    pub fn apply(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme, look: Look) {
        let (over_base, over_surface) = look.amounts();
        if !theme.backdrop || over_base == 0.0 || area.width == 0 || area.height == 0 {
            return;
        }
        self.prepare(area.right(), area.bottom(), area, FEATHER);
        let base = Color::Rgb(theme::BG.0, theme::BG.1, theme::BG.2);
        let surface = Color::Rgb(theme::SURFACE.0, theme::SURFACE.1, theme::SURFACE.2);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &mut buf[(x, y)];
                let (under, amount) = if cell.bg == base {
                    (theme::BG, over_base)
                } else if cell.bg == surface {
                    (theme::SURFACE, over_surface)
                } else {
                    continue;
                };
                let quad = self.quad(x, y);
                if quad.iter().all(|q| q.3 == 0.0) {
                    continue;
                }
                let colors = quad.map(|(_, _, c, cover)| theme::mix(under, c, amount * cover));
                if cell.symbol() == " " {
                    paint(cell, &colors);
                } else {
                    let mean = |f: fn(&Rgb) -> u8| {
                        (colors.iter().map(|c| u32::from(f(c))).sum::<u32>() / 4) as u8
                    };
                    cell.bg = Color::Rgb(mean(|c| c.0), mean(|c| c.1), mean(|c| c.2));
                }
            }
        }
    }

    /// The artwork in full inside `frame`, `t` seconds into the splash: light
    /// spreads out from the moon, then a glint crosses the picture. Every
    /// cell of `frame` is drawn.
    pub fn splash(&mut self, buf: &mut Buffer, frame: Rect, t: f32) {
        let screen = buf.area;
        let frame = frame.intersection(screen);
        if frame.width == 0 || frame.height == 0 {
            return;
        }
        self.prepare(screen.right(), screen.bottom(), frame, SPLASH_FEATHER);
        let (side, left, top) = placement(frame);
        for y in frame.top()..frame.bottom() {
            for x in frame.left()..frame.right() {
                let colors = self.quad(x, y).map(|(i, j, c, cover)| {
                    if cover == 0.0 {
                        return theme::BG;
                    }
                    // The sub-pixel's place in the picture, 0 to 1 each way.
                    let u = (i as f32 * 0.5 + 0.25 - left) / side;
                    let v = (j as f32 + 0.5 - top) / side;
                    theme::mix(theme::BG, glint(c, t, u, v), cover * reveal(t, u, v))
                });
                let cell = &mut buf[(x, y)];
                cell.set_symbol(" ");
                cell.bg = Color::Rgb(theme::BG.0, theme::BG.1, theme::BG.2);
                paint(cell, &colors);
            }
        }
    }

    /// The artwork filling `frame` with glitches, `t` seconds after the
    /// screensaver started. It fades in; reduced motion shows it still,
    /// with scanlines only.
    pub fn screensaver(&mut self, buf: &mut Buffer, frame: Rect, t: f32, animated: bool) {
        let screen = buf.area;
        let frame = frame.intersection(screen);
        if frame.width == 0 || frame.height == 0 {
            return;
        }
        self.prepare(screen.right(), screen.bottom(), frame, SPLASH_FEATHER);
        let glitch = if animated { burst(t) } else { None };
        let fade = if animated { smooth(t / 1.2) } else { 1.0 };
        let rows = f32::from(screen.bottom()) * 2.0;
        let roll = (t * 0.11).fract();
        for y in frame.top()..frame.bottom() {
            for x in frame.left()..frame.right() {
                let colors = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(dx, dy)| {
                    let (i, j) = (x as usize * 2 + dx, y as usize * 2 + dy);
                    let (mut c, cover) = self.glitched(i, j, glitch);
                    // Scanlines: every other sub-pixel row a little darker.
                    if j % 2 == 1 {
                        c = theme::mix(c, (0, 0, 0), 0.14);
                    }
                    // A light rolls down, faint over the empty background.
                    if animated {
                        let v = j as f32 / rows;
                        let band = (-((v - roll) / 0.025).powi(2)).exp();
                        c = theme::mix(c, (190, 225, 255), 0.10 * band * (0.3 + 0.7 * cover));
                    }
                    theme::mix(theme::BG, c, fade)
                });
                let cell = &mut buf[(x, y)];
                cell.set_symbol(" ");
                cell.bg = Color::Rgb(theme::BG.0, theme::BG.1, theme::BG.2);
                paint(cell, &colors);
            }
        }
    }

    /// Sub-pixel (i, j) under a glitch, and how much picture is there:
    /// rows in some bands shift sideways, red and blue come from either
    /// side, and on the picture some bands flash a tint and a few blocks
    /// turn to static or invert. Calm, red and blue still sit one sub-pixel
    /// apart.
    fn glitched(&self, i: usize, j: usize, glitch: Option<(u64, f32)>) -> (Rgb, f32) {
        let width = self.key.0 as isize * 2;
        let at = |i: isize| {
            if (0..width).contains(&i) {
                let (c, cover) = self.pixel(i as usize, j);
                (theme::mix(theme::BG, c, cover), cover)
            } else {
                (theme::BG, 0.0)
            }
        };
        let (seed, strength) = glitch.unwrap_or((0, 0.0));
        let height = 2 + (unit(seed, 5) * 14.0) as usize;
        let band = (j / height) as u64;
        let mut x = i as isize;
        if glitch.is_some() && unit(seed, 1000 + band) < 0.3 * strength {
            x -= ((unit(seed, 2000 + band) - 0.5) * 40.0 * strength) as isize;
        }
        let split = 1 + (3.0 * strength * unit(seed, 6)) as isize;
        let (middle, cover) = at(x);
        let mut c = (at(x - split).0.0, middle.1, at(x + split).0.2);
        if glitch.is_none() || cover == 0.0 {
            return (c, cover);
        }
        if unit(seed, 3000 + band) < 0.12 * strength {
            let tint = TINTS[(unit(seed, 4000 + band) * 3.0) as usize % 3];
            c = theme::mix(c, tint, 0.35 * cover);
        }
        let block = hash(seed, ((j / 4) as u64) << 20 | (i / 6) as u64);
        if cover > 0.5 && (block >> 40) as f32 / 16_777_216.0 < 0.015 * strength {
            let v = (hash(block, (j * 31 + i) as u64) >> 56) as u8;
            c = if block & 1 == 0 {
                (v, v, v)
            } else {
                (255 - c.0, 255 - c.1, 255 - c.2)
            };
        }
        (c, cover)
    }
}

/// A well-mixed hash, so the same moment always glitches the same way.
fn hash(a: u64, b: u64) -> u64 {
    let mut z = a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_add(0x632B_E59B_D9B4_E019);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A number from 0 up to 1, from the hash.
fn unit(a: u64, b: u64) -> f32 {
    (hash(a, b) >> 40) as f32 / 16_777_216.0
}

/// The glitch burst at `t`, if one is running: a seed that changes every
/// 60 ms, and its strength. Each `SLOT` holds at most one burst of 0.12 to
/// 0.62 seconds, at a random moment; about one slot in five stays calm.
fn burst(t: f32) -> Option<(u64, f32)> {
    let slot = (t / SLOT).floor().max(0.0) as u64;
    if unit(slot, 1) < 0.2 {
        return None;
    }
    let length = 0.12 + 0.5 * unit(slot, 2);
    let start = slot as f32 * SLOT + unit(slot, 3) * (SLOT - length);
    let within = t - start;
    if !(0.0..length).contains(&within) {
        return None;
    }
    let step = (within / 0.06) as u64;
    Some((hash(slot, 100 + step), 0.35 + 0.65 * unit(slot, 4)))
}

/// Whether a glitch burst runs `t` seconds into the screensaver.
#[cfg(test)]
pub fn glitching(t: f32) -> bool {
    burst(t).is_some()
}

/// Draw a plain cell as the quadrant pattern that best matches `colors`.
/// Bold or other attributes would brighten the glyph, so none are kept.
fn paint(cell: &mut ratatui::buffer::Cell, colors: &[Rgb; 4]) {
    let (mask, fg, bg) = best_split(colors);
    cell.bg = Color::Rgb(bg.0, bg.1, bg.2);
    if mask > 0 {
        cell.set_symbol(QUADRANTS[mask]);
        cell.fg = Color::Rgb(fg.0, fg.1, fg.2);
        cell.modifier = Modifier::empty();
    }
}

/// How lit a point of the picture is `t` seconds into the splash: the light
/// starts at the moon and reaches the farthest corner after `REVEAL`.
fn reveal(t: f32, u: f32, v: f32) -> f32 {
    const FARTHEST: f32 = 1.05;
    const SOFT: f32 = 0.3;
    let distance = ((u - MOON.0).powi(2) + (v - MOON.1).powi(2)).sqrt();
    let front = t / REVEAL * (FARTHEST + SOFT);
    smooth((front - distance) / SOFT)
}

/// A soft diagonal band of light that crosses the picture once.
fn glint(c: Rgb, t: f32, u: f32, v: f32) -> Rgb {
    let progress = (t - GLINT_AT) / GLINT;
    if !(0.0..=1.0).contains(&progress) {
        return c;
    }
    let band = u * 0.8 + v * 0.45 - (progress * 1.65 - 0.2);
    theme::mix(c, (255, 244, 220), 0.25 * (-(band / 0.04).powi(2)).exp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    fn on() -> Theme {
        Theme {
            backdrop: true,
            ..Theme::default()
        }
    }

    #[test]
    fn only_plain_backgrounds_change_and_text_stays() {
        let theme = on();
        // 40 by 12 cells: the picture is 24 units square, 8 columns in.
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 12));
        buf.set_style(buf.area, theme.base());
        buf.set_string(18, 5, "text", theme.surface());
        let screen = Style::default().bg(theme.rgb(theme::SCREEN));
        buf.set_string(18, 7, "    ", screen);
        buf.set_string(20, 8, " ", theme.base().add_modifier(Modifier::BOLD));
        let area = buf.area;
        Picture::default().apply(&mut buf, area, &theme, Look::Art);
        // Text keeps its glyph and color; its background carries the image.
        assert_eq!(buf[(18, 5)].symbol(), "t");
        assert_eq!(buf[(18, 5)].fg, theme.rgb(theme::TEXT));
        assert_ne!(buf[(18, 5)].bg, theme.rgb(theme::SURFACE));
        // Empty cells inside the picture changed; a glyph never stays bold.
        let changed = (8..32u16)
            .flat_map(|x| (1..11u16).map(move |y| (x, y)))
            .filter(|&p| buf[p].bg != theme.rgb(theme::BG) || buf[p].symbol() != " ")
            .count();
        assert!(changed > 150, "{changed}");
        if buf[(20, 8)].symbol() != " " {
            assert_eq!(buf[(20, 8)].modifier, Modifier::empty());
        }
        // Terminal screens are never overlaid.
        assert_eq!(buf[(18, 7)].symbol(), " ");
        assert_eq!(buf[(18, 7)].bg, theme.rgb(theme::SCREEN));
        // Margins beside the picture stay plain.
        assert_eq!(buf[(0, 0)].symbol(), " ");
        assert_eq!(buf[(0, 0)].bg, theme.rgb(theme::BG));
        assert_eq!(buf[(39, 11)].symbol(), " ");
        // Dim: no color is brighter than the image blended in by its share.
        let limit = 13 + (255.0 * Look::Art.amounts().0) as u8 + 2;
        for cell in buf.content() {
            // A plain cell's foreground is never drawn; a glyph's is the image.
            let glyph = QUADRANTS[1..].contains(&cell.symbol());
            for color in [Some(cell.bg), glyph.then_some(cell.fg)]
                .into_iter()
                .flatten()
            {
                if let Color::Rgb(r, g, b) = color
                    && cell.bg != theme.rgb(theme::SURFACE)
                    && color != theme.rgb(theme::SCREEN)
                {
                    assert!(r.max(g).max(b) <= limit, "{color:?}");
                }
            }
        }
    }

    #[test]
    fn the_whole_picture_fits_the_screen_centered() {
        // 150 columns by 47 rows is 94 units tall, so the picture is 94
        // units square with 28 columns to either side.
        assert_eq!(placement(Rect::new(0, 0, 150, 47)), (94.0, 28.0, 0.0));
        // A narrow screen fits the width and centers vertically instead.
        assert_eq!(placement(Rect::new(0, 0, 20, 30)), (20.0, 0.0, 20.0));
        // A frame below a title row starts two units down.
        assert_eq!(placement(Rect::new(0, 1, 150, 40)), (80.0, 35.0, 2.0));
        let mut backdrop = Picture::default();
        backdrop.prepare(150, 47, Rect::new(0, 0, 150, 47), FEATHER);
        // Sub-pixels: two per column, two per row.
        let cover = |col: usize, sub_row: usize| backdrop.pixel(col * 2, sub_row).1;
        assert_eq!(cover(27, 47), 0.0);
        assert_eq!(cover(122, 47), 0.0);
        assert_eq!(cover(75, 47), 1.0);
        // The edges fade in rather than stopping at a hard line.
        assert!(cover(28, 47) > 0.0 && cover(28, 47) < cover(40, 47));
        assert!(cover(121, 47) > 0.0 && cover(121, 47) < 1.0);
        assert!(cover(75, 0) > 0.0 && cover(75, 0) < 1.0);
        assert!(cover(75, 93) > 0.0 && cover(75, 93) < 1.0);
    }

    #[test]
    fn each_cell_takes_the_pattern_that_matches_its_quarters() {
        let (dark, light) = ((10, 10, 10), (200, 180, 120));
        // Left half light: the left half block, light over dark.
        assert_eq!(best_split(&[light, dark, light, dark]), (5, light, dark));
        // Only the lower right differs: three quarters in the foreground.
        assert_eq!(best_split(&[light, light, light, dark]), (7, light, dark));
        // A diagonal.
        assert_eq!(best_split(&[dark, light, light, dark]).0, 6);
        // Nearly even cells stay plain, colored by their mean.
        let near = (12, 10, 10);
        let (mask, _, bg) = best_split(&[dark, near, dark, near]);
        assert_eq!(mask, 0);
        assert_eq!(bg, (11, 10, 10));
    }

    #[test]
    fn scaling_averages_light_and_sharpening_keeps_detail() {
        // Black and white average to a light grey, not the darker 128 that
        // averaging the encoded values gives.
        let grey = to_srgb((to_linear(0) + to_linear(255)) / 2.0);
        assert!((186.0..190.0).contains(&grey), "{grey}");
        assert_eq!(to_srgb(to_linear(77)).round() as u8, 77);
        // A rendered screen uses quadrant patterns, not only half blocks.
        let theme = on();
        let mut buf = Buffer::empty(Rect::new(0, 0, 160, 48));
        buf.set_style(buf.area, theme.base());
        let area = buf.area;
        Picture::default().apply(&mut buf, area, &theme, Look::ArtBright);
        let quarters = buf
            .content()
            .iter()
            .filter(|c| ["▘", "▝", "▖", "▞", "▛"].contains(&c.symbol()))
            .count();
        assert!(quarters > 100, "{quarters}");
    }

    #[test]
    fn looks_brighten_dim_or_leave_out_the_picture() {
        let theme = on();
        let brightest = |look| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
            buf.set_style(buf.area, theme.base());
            let area = buf.area;
            Picture::default().apply(&mut buf, area, &theme, look);
            buf.content()
                .iter()
                .flat_map(|c| {
                    let glyph = QUADRANTS[1..].contains(&c.symbol());
                    [Some(c.bg), glyph.then_some(c.fg)].into_iter().flatten()
                })
                .filter_map(|c| match c {
                    Color::Rgb(r, g, b) => Some(u32::from(r) + u32::from(g) + u32::from(b)),
                    _ => None,
                })
                .max()
                .unwrap_or(0)
        };
        let base = u32::from(theme::BG.0) + u32::from(theme::BG.1) + u32::from(theme::BG.2);
        assert_eq!(brightest(Look::Off), base);
        assert_eq!(
            brightest(Look::Glow),
            base,
            "the soft light never draws the picture"
        );
        assert!(brightest(Look::ArtBright) > brightest(Look::Art) + 40);
        // b: soft light, faint artwork, bright artwork, plain, and around.
        let mut look = Look::default();
        let mut seen = vec![look.name()];
        for _ in 0..4 {
            look = look.next();
            seen.push(look.name());
        }
        assert_eq!(seen, ["glow", "art", "art-bright", "off", "glow"]);
        assert_eq!(Look::parse("art-bright"), Some(Look::ArtBright));
        // Names from before the soft light are not reused.
        assert_eq!(Look::parse("normal"), None);
        assert_eq!(Look::parse("bright"), None);
    }

    fn total(c: Color) -> u32 {
        match c {
            Color::Rgb(r, g, b) => u32::from(r) + u32::from(g) + u32::from(b),
            _ => 0,
        }
    }

    #[test]
    fn the_soft_light_tints_only_plain_backgrounds_and_stays_quiet() {
        let theme = on();
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        buf.set_style(buf.area, theme.base());
        buf.set_string(40, 1, "text", theme.surface().fg(theme.rgb(theme::TEXT)));
        let screen = Style::default().bg(theme.rgb(theme::SCREEN));
        buf.set_string(10, 2, "    ", screen);
        let area = buf.area;
        glow(&mut buf, area, &theme, theme::CYAN);
        // Glyphs and their colors stay; only backgrounds take the light.
        assert_eq!(buf[(40, 1)].symbol(), "t");
        assert_eq!(buf[(40, 1)].fg, theme.rgb(theme::TEXT));
        assert_ne!(buf[(40, 1)].bg, theme.rgb(theme::SURFACE));
        assert!(
            buf.content()
                .iter()
                .all(|c| [" ", "t", "e", "x"].contains(&c.symbol()))
        );
        assert_eq!(buf[(10, 2)].bg, theme.rgb(theme::SCREEN));
        // Lit from the top, warm at the upper right, deeper in the lower corners.
        let base = total(theme.rgb(theme::BG));
        assert!(total(buf[(60, 0)].bg) > total(buf[(60, 20)].bg));
        let warmth = |x, y| match buf[(x, y)].bg {
            Color::Rgb(r, _, b) => i32::from(r) - i32::from(b),
            _ => 0,
        };
        assert!(warmth(86, 1) > warmth(14, 1) + 4);
        assert!(total(buf[(0, 29)].bg) < base);
        // Quiet: about twenty levels at most per channel, never a picture.
        for cell in buf.content() {
            assert!(total(cell.bg) <= base + 70, "{:?}", cell.bg);
        }
    }

    #[test]
    fn the_splash_lights_up_from_the_moon_and_ends_fully_drawn() {
        let frame = Rect::new(0, 1, 120, 40);
        let draw = |t: f32| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 120, 46));
            // Leftover text never shows through the picture.
            for y in frame.top()..frame.bottom() {
                buf.set_string(0, y, "x".repeat(120), Style::default());
            }
            Picture::default().splash(&mut buf, frame, t);
            buf
        };
        let (side, left, top) = placement(frame);
        // The cell under a point of the picture.
        let at = |u: f32, v: f32| ((left + u * side) as u16, ((top + v * side) / 2.0) as u16);
        let dark = draw(0.0);
        assert!(dark.content()[120..120 * 41].iter().all(
            |c| c.symbol() == " " && c.bg == Color::Rgb(theme::BG.0, theme::BG.1, theme::BG.2)
        ));
        let early = draw(REVEAL * 0.35);
        assert!(total(early[at(MOON.0, MOON.1)].bg) > total(dark[at(MOON.0, MOON.1)].bg) + 150);
        assert_eq!(
            early[at(0.05, 0.95)].bg,
            dark[at(0.05, 0.95)].bg,
            "far corner still dark"
        );
        let lit = draw(GLINT_AT + GLINT + 1.0);
        assert!(total(lit[at(0.05, 0.95)].bg) > 0);
        let glyphs = lit.content()[120..120 * 41]
            .iter()
            .filter(|c| QUADRANTS[1..].contains(&c.symbol()))
            .count();
        assert!(glyphs > 1000, "{glyphs}");
        assert!(lit.content().iter().all(|c| c.symbol() != "x"));
        // The glint brightens the picture while it crosses, then passes.
        let brightness = |buf: &Buffer| {
            buf.content()
                .iter()
                .map(|c| total(c.bg) + total(c.fg))
                .sum::<u32>()
        };
        assert!(brightness(&draw(GLINT_AT + GLINT * 0.5)) > brightness(&lit));
        // Outside its frame the splash draws nothing.
        assert_eq!(lit[(0, 0)].symbol(), " ");
        assert_eq!(lit[(0, 45)].bg, Color::Reset);
    }

    #[test]
    fn the_screensaver_glitches_in_bursts_and_holds_still_with_reduced_motion() {
        // Bursts come and go: some moments calm, some glitching, and the
        // same moment always the same.
        let moments: Vec<f32> = (0..2000).map(|n| n as f32 * 0.05).collect();
        let glitching = moments.iter().filter(|&&t| burst(t).is_some()).count();
        assert!(glitching > 50 && glitching < 1000, "{glitching}");
        assert!(moments.iter().all(|&t| burst(t) == burst(t)));
        assert!(
            moments
                .iter()
                .all(|&t| super::glitching(t) == burst(t).is_some())
        );
        let strengths = moments.iter().filter_map(|&t| burst(t)).map(|b| b.1);
        assert!(strengths.into_iter().all(|s| (0.35..=1.0).contains(&s)));
        let draw = |t: f32, animated: bool| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
            buf.set_string(0, 0, "x".repeat(100), Style::default());
            let area = buf.area;
            Picture::default().screensaver(&mut buf, area, t, animated);
            buf
        };
        let calm = moments
            .iter()
            .copied()
            .find(|&t| t > 2.0 && burst(t).is_none())
            .unwrap();
        let wild = moments
            .iter()
            .copied()
            .find(|&t| t > 2.0 && burst(t).is_some_and(|b| b.1 > 0.7))
            .unwrap();
        let (a, b) = (draw(calm, true), draw(wild, true));
        // The whole screen is drawn, and a burst changes much of it.
        assert!(a.content().iter().all(|c| c.symbol() != "x"));
        let changed = a
            .content()
            .iter()
            .zip(b.content())
            .filter(|(x, y)| x != y)
            .count();
        assert!(changed > 300, "{changed}");
        // The same moment draws the same frame.
        assert_eq!(draw(wild, true), b);
        // It fades in from the background.
        let brightness = |buf: &Buffer| {
            buf.content()
                .iter()
                .map(|c| total(c.bg) + total(c.fg))
                .sum::<u32>()
        };
        assert!(brightness(&draw(0.1, true)) < brightness(&a) / 2);
        // Reduced motion: fully shown at once, and the same at any moment.
        assert_eq!(draw(0.0, false), draw(wild, false));
        assert!(brightness(&draw(0.0, false)) > brightness(&a) / 2);
    }

    #[test]
    fn a_glitch_seed_always_corrupts_the_picture_the_same_way() {
        let area = Rect::new(0, 0, 100, 30);
        let mut picture = Picture::default();
        picture.prepare(area.width, area.height, area, SPLASH_FEATHER);
        let (w, h) = (area.width as usize * 2, area.height as usize * 2);
        let frame = |glitch: Option<(u64, f32)>| -> Vec<(Rgb, f32)> {
            (0..h)
                .flat_map(|j| (0..w).map(move |i| (i, j)))
                .map(|(i, j)| picture.glitched(i, j, glitch))
                .collect()
        };
        let calm = frame(None);
        let seeded = frame(Some((0xDAED_A105, 1.0)));
        // Deterministic: one seed, one frame.
        assert_eq!(seeded, frame(Some((0xDAED_A105, 1.0))));
        // Another seed corrupts other places.
        assert_ne!(seeded, frame(Some((0x5EED, 1.0))));
        // Calm, red and blue still sit one sub-pixel either side of green.
        let source = |i: usize, j: usize| {
            let (c, cover) = picture.pixel(i, j);
            theme::mix(theme::BG, c, cover)
        };
        let (i, j) = (w / 2, h / 2);
        assert_eq!(
            calm[j * w + i].0,
            (source(i - 1, j).0, source(i, j).1, source(i + 1, j).2)
        );
        // A full-strength burst shifts bands and wrecks a share of the picture,
        // and only where there is picture: the empty edges stay background.
        let differ = calm.iter().zip(&seeded).filter(|(a, b)| a != b).count();
        assert!(differ > w * h / 50, "{differ}");
        assert_eq!(seeded[0], (theme::BG, 0.0));
        // Some cells flash a tint, turn to static or invert.
        let wrecked = (0..64u64)
            .map(|seed| frame(Some((seed, 1.0))))
            .flat_map(|f| f.into_iter().zip(calm.iter().copied()))
            .filter(|((a, ca), (b, cb))| ca == cb && a != b)
            .count();
        assert!(wrecked > 0);
    }

    #[test]
    fn off_without_truecolor_or_when_disabled() {
        let theme = Theme::default();
        assert!(!theme.backdrop);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
        let area = buf.area;
        buf.set_style(area, theme.base());
        Picture::default().apply(&mut buf, area, &theme, Look::Art);
        glow(&mut buf, area, &theme, theme::CYAN);
        assert_eq!(buf[(0, 0)].symbol(), " ");
        assert_eq!(buf[(0, 0)].bg, theme.rgb(theme::BG));
        let vars = |extra: &'static [(&str, &str)]| {
            move |name: &str| {
                [
                    ("COLORTERM", "truecolor"),
                    ("LANG", "C.UTF-8"),
                    ("TERM", "xterm"),
                ]
                .iter()
                .chain(extra)
                .rev()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
            }
        };
        assert!(Theme::from_vars(vars(&[])).backdrop);
        assert!(!Theme::from_vars(vars(&[("DAEDALUS_BACKDROP", "0")])).backdrop);
        assert!(!Theme::from_vars(vars(&[("NO_COLOR", "1")])).backdrop);
        assert!(!Theme::from_vars(vars(&[("COLORTERM", "")])).backdrop);
        assert!(!Theme::from_vars(vars(&[("DAEDALUS_ICONS", "ascii")])).backdrop);
    }
}
