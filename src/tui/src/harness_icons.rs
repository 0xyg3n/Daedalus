//! Bundled native harness logos, with text underneath every image placement.
//! Upload once per provider; moving between views changes placements only.
use crate::{
    graphics,
    model::{Ui, WORKSPACES},
    theme::{Theme, Tone},
};
use ratatui::layout::Rect;
use std::{
    collections::BTreeSet,
    io::{self, Write},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Brand {
    Codex,
    Claude,
    Grok,
    Hermes,
    Cursor,
    Pi,
    Opencode,
}
impl Brand {
    pub const ALL: [Self; 7] = [
        Self::Codex,
        Self::Claude,
        Self::Grok,
        Self::Hermes,
        Self::Cursor,
        Self::Pi,
        Self::Opencode,
    ];
    pub fn parse(name: &str) -> Option<Self> {
        let alias = match name.trim().to_ascii_lowercase().as_str() {
            "claude code" => Some("claude"),
            "grok bot" => Some("grok"),
            _ => None,
        };
        match alias.or_else(|| daedalus_tui::adapters::harnesses::canonical_id(name.trim()))? {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            "grok" => Some(Self::Grok),
            "hermes" => Some(Self::Hermes),
            "cursor" => Some(Self::Cursor),
            "pi" => Some(Self::Pi),
            "opencode" => Some(Self::Opencode),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Grok => "grok",
            Self::Hermes => "hermes",
            Self::Cursor => "cursor",
            Self::Pi => "pi",
            Self::Opencode => "opencode",
        }
    }
    fn short(self) -> &'static str {
        match self {
            Self::Codex => "CX",
            Self::Claude => "CL",
            Self::Grok => "GK",
            Self::Hermes => "HM",
            Self::Cursor => "CU",
            Self::Pi => "PI",
            Self::Opencode => "OC",
        }
    }
    pub fn tone(self) -> Tone {
        match self {
            Self::Codex => Tone::Info,
            Self::Claude => Tone::Warn,
            Self::Grok => Tone::Text,
            Self::Hermes => Tone::Rose,
            Self::Cursor => Tone::Violet,
            Self::Pi => Tone::Warn,
            Self::Opencode => Tone::Info,
        }
    }
    fn image_id(self) -> u32 {
        7200 + self as u32
    }
    fn png(self) -> &'static [u8] {
        match self {
            Self::Codex => include_bytes!("../../../assets/icons/codex.png"),
            Self::Claude => include_bytes!("../../../assets/icons/claude.png"),
            Self::Grok => include_bytes!("../../../assets/icons/grok.png"),
            Self::Hermes => include_bytes!("../../../assets/icons/hermes.png"),
            Self::Cursor => include_bytes!("../../../assets/icons/raster/cursor.png"),
            Self::Pi => include_bytes!("../../../assets/icons/raster/pi.png"),
            Self::Opencode => include_bytes!("../../../assets/icons/raster/opencode.png"),
        }
    }
    pub fn text(self, theme: &Theme) -> String {
        if theme.ascii {
            format!("{} ", self.short())
        } else {
            format!("{} ", theme.icon(self.name()))
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub brand: Brand,
    pub area: Rect,
}
#[derive(Default)]
pub struct Cache {
    uploaded: BTreeSet<Brand>,
    shown: Vec<Placement>,
}
impl Cache {
    pub fn invalidate(&mut self) {
        self.uploaded.clear();
    }
    pub fn hide(&mut self, out: &mut impl Write) -> io::Result<()> {
        for brand in Brand::ALL {
            if self.shown.iter().any(|p| p.brand == brand) {
                graphics::remove_image(out, brand.image_id(), false)?;
            }
        }
        self.shown.clear();
        Ok(())
    }
}
pub fn requested(ui: &Ui) -> bool {
    ui.settings.harness_logos == "auto" && ui.theme.color && !ui.theme.ascii
}
pub fn brand(ui: &Ui, harness: &str) -> Option<Brand> {
    (ui.settings.harness_logos != "off")
        .then(|| Brand::parse(harness))
        .flatten()
}
pub fn prefix(ui: &Ui, harness: &str, fallback: &str) -> String {
    brand(ui, harness).map_or_else(|| format!("{fallback} "), |brand| brand.text(&ui.theme))
}
pub fn text(ui: &Ui, harness: &str) -> String {
    brand(ui, harness).map_or_else(String::new, |brand| brand.text(&ui.theme))
}
pub fn label(ui: &Ui, harness: &str, title: &str) -> String {
    let Some(brand) = brand(ui, harness) else {
        return title.into();
    };
    if title.to_ascii_lowercase().contains(brand.name()) {
        return title.into();
    }
    let name = match brand {
        Brand::Codex => "Codex",
        Brand::Claude => "Claude",
        Brand::Grok => "Grok",
        Brand::Hermes => "Hermes",
        Brand::Cursor => "Cursor",
        Brand::Pi => "Pi",
        Brand::Opencode => "OpenCode",
    };
    if title.is_empty() {
        name.into()
    } else {
        format!("{name} · {title}")
    }
}
pub fn mark(ui: &Ui, harness: &str, area: Rect) {
    if area.width < 2 || area.height == 0 {
        return;
    }
    let Some(brand) = brand(ui, harness) else {
        return;
    };
    let placement = Placement {
        brand,
        area: Rect::new(area.x, area.y, 2, 1),
    };
    let mut targets = ui.harness_logo_targets.borrow_mut();
    if !targets.contains(&placement) {
        targets.push(placement);
    }
}
pub fn show(ui: &mut Ui, out: &mut impl Write) -> io::Result<()> {
    if ui.kitty.is_none() {
        ui.harness_logo_cache.shown.clear();
        return Ok(());
    }
    let desired = if requested(ui)
        && ui.page == WORKSPACES
        && ui.dialog.is_none()
        && ui.options.is_none()
        && ui.binding_search.is_none()
        && ui.splash.is_none()
        && ui.saver.since.is_none()
    {
        ui.harness_logo_targets.get_mut().clone()
    } else {
        Vec::new()
    };
    if desired != ui.harness_logo_cache.shown {
        ui.harness_logo_cache.hide(out)?;
    }
    let cell = ui.kitty.expect("graphics capability").cell;
    for (index, placement) in desired.iter().enumerate() {
        if ui.harness_logo_cache.uploaded.insert(placement.brand) {
            graphics::transmit_png(out, placement.brand.image_id(), placement.brand.png())?;
        }
        // Reassert cached placements after every frame: terminal clears can
        // remove placements while retaining their pixel data.
        graphics::place_square(
            out,
            placement.brand.image_id(),
            index as u32 + 1,
            placement.area,
            cell,
        )?;
    }
    ui.harness_logo_cache.shown = desired;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_supported_harness_has_a_native_logo_and_distinct_ascii_fallback() {
        let mut fallbacks = BTreeSet::new();
        for tool in daedalus_tui::adapters::harnesses::tools() {
            let brand = Brand::parse(tool.id).unwrap();
            assert!(brand.png().starts_with(b"\x89PNG\r\n\x1a\n"));
            assert!(fallbacks.insert(brand.short()));
        }
        assert!(Brand::parse("shell").is_none());
        assert!(Brand::parse("a prompt mentioning codex").is_none());
    }
    #[test]
    fn native_logo_pixels_upload_once_and_dialogs_remove_only_owned_placements() {
        let mut ui = Ui {
            page: WORKSPACES,
            kitty: Some(graphics::Kitty { cell: (8.0, 16.0) }),
            ..Ui::default()
        };
        ui.theme = Theme {
            color: true,
            ascii: false,
            ..Theme::default()
        };
        mark(&ui, "codex", Rect::new(4, 3, 2, 1));
        let mut first = Vec::new();
        show(&mut ui, &mut first).unwrap();
        let first = String::from_utf8(first).unwrap();
        assert!(first.contains("a=t,f=100"));
        assert!(first.contains("a=p,i=7200"));
        assert!(!first.contains("i=7183"));
        let mut second = Vec::new();
        show(&mut ui, &mut second).unwrap();
        let second = String::from_utf8(second).unwrap();
        assert!(!second.contains("a=t"));
        assert!(second.contains("a=p"));
        ui.dialog = Some(crate::model::Dialog::default());
        let mut hidden = Vec::new();
        show(&mut ui, &mut hidden).unwrap();
        let hidden = String::from_utf8(hidden).unwrap();
        assert!(hidden.contains("a=d,d=i,i=7200"));
        assert!(!hidden.contains("a=p"));
        ui.dialog = None;
        ui.settings.harness_logos = "glyphs".into();
        let mut glyphs = Vec::new();
        show(&mut ui, &mut glyphs).unwrap();
        assert!(glyphs.is_empty());
    }
    #[test]
    fn unsupported_terminals_ascii_and_disabled_logos_send_no_images() {
        let mut ui = Ui {
            page: WORKSPACES,
            ..Ui::default()
        };
        mark(&ui, "claude", Rect::new(1, 1, 2, 1));
        let mut out = Vec::new();
        show(&mut ui, &mut out).unwrap();
        assert!(out.is_empty());
        ui.kitty = Some(graphics::Kitty { cell: (8.0, 16.0) });
        ui.theme.ascii = true;
        show(&mut ui, &mut out).unwrap();
        assert!(out.is_empty());
        assert_eq!(prefix(&ui, "codex", "*"), "CX ");
        ui.settings.harness_logos = "off".into();
        assert_eq!(prefix(&ui, "codex", "*"), "* ");
    }
}
