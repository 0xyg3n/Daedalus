//! Options: every workspace preference in one place. A change applies at
//! once and is remembered with the view, like the keys that also make it.
use crate::model::{Order, Split, Ui};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    Order,
    Details,
    Screen,
    Tiles,
    Sidebar,
    Pinned,
    Recent,
    Badges,
    Vitals,
    Look,
    Saver,
    Motion,
    Keys,
    Reset,
    Updates,
    Settings,
}

impl Item {
    pub const ALL: [Item; 16] = [
        Item::Settings,
        Item::Order,
        Item::Details,
        Item::Screen,
        Item::Tiles,
        Item::Sidebar,
        Item::Pinned,
        Item::Recent,
        Item::Badges,
        Item::Vitals,
        Item::Look,
        Item::Saver,
        Item::Motion,
        Item::Keys,
        Item::Reset,
        Item::Updates,
    ];
    /// The heading the item is listed under.
    pub fn group(self) -> &'static str {
        match self {
            Item::Order | Item::Details | Item::Screen | Item::Tiles => "Workspaces",
            Item::Sidebar | Item::Pinned | Item::Recent | Item::Badges | Item::Vitals => "Sidebar",
            Item::Look | Item::Saver | Item::Motion | Item::Keys => "Look and feel",
            Item::Reset => "",
            Item::Updates => "Application",
            Item::Settings => "Configuration",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Item::Order => "Terminal order",
            Item::Details => "Details panel",
            Item::Screen => "Live screen in Details",
            Item::Tiles => "Tile size",
            Item::Sidebar => "Sidebar",
            Item::Pinned => "Pinned terminals",
            Item::Recent => "Recent terminals",
            Item::Badges => "Counts beside sections",
            Item::Vitals => "Totals",
            Item::Look => "Background",
            Item::Saver => "Screensaver",
            Item::Motion => "Neon motion and glitches",
            Item::Keys => "Key bar",
            Item::Reset => "Reset panel sizes",
            Item::Updates => "Check for updates",
            Item::Settings => "Configuration settings",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Item::Order => "order",
            Item::Details => "details",
            Item::Screen => "watch",
            Item::Tiles => "size",
            Item::Sidebar => "sidebar",
            Item::Pinned => "pin",
            Item::Recent => "recent",
            Item::Badges => "counts",
            Item::Vitals => "totals",
            Item::Look => "background",
            Item::Saver => "saver",
            Item::Motion => "motion",
            Item::Keys => "keys",
            Item::Reset => "reset",
            Item::Updates => "refresh",
            Item::Settings => "options",
        }
    }
    /// Explain the selected preference without crowding every row.
    pub fn description(self) -> &'static str {
        match self {
            Item::Order => "Choose activity order or keep terminals in their workspace positions.",
            Item::Details => "Show facts and actions beside the selected entry in Tree view.",
            Item::Screen => {
                "Show a read-only terminal preview inside Details. Adjust its share with + and -."
            }
            Item::Tiles => "Change the space allocated to terminal previews in Tiles view.",
            Item::Sidebar => "Keep section names visible or fold the sidebar into an icon rail.",
            Item::Pinned => "Show your pinned terminal shortcuts in the sidebar.",
            Item::Recent => "Show recently visited terminals for quick switching.",
            Item::Badges => "Show section counts beside navigation labels.",
            Item::Vitals => "Show terminal and connection totals in the sidebar.",
            Item::Look => {
                "Choose the dashboard background. Terminal previews keep their own colors."
            }
            Item::Saver => {
                "Show the artwork after the idle interval (15 seconds by default). Any key returns to work."
            }
            Item::Motion => {
                "Enable transition effects. Your system reduced-motion preference takes priority."
            }
            Item::Keys => {
                "Show contextual shortcuts along the bottom. F1 always opens searchable bindings."
            }
            Item::Reset => {
                "Restore panel proportions without changing accounts, routes or terminal sessions."
            }
            Item::Updates => {
                "Review the latest release before downloading. Ctrl+U opens the same update check."
            }
            Item::Settings => {
                "Open persistent configuration for Telegram, detected harnesses, accounts and hooks."
            }
        }
    }
    /// The key that makes the same change outside Options, if any.
    pub fn key(self) -> &'static str {
        match self {
            Item::Order => "o",
            Item::Screen => "+ -",
            Item::Sidebar => "\\",
            Item::Look => "b",
            Item::Saver => "s",
            Item::Keys => "?",
            Item::Reset => "=",
            Item::Updates => "Ctrl+U",
            _ => "",
        }
    }
    /// The switch this item turns on and off, if it is one.
    pub fn switch(self) -> Option<&'static str> {
        Some(match self {
            Item::Details => "details",
            Item::Screen => "screen",
            Item::Pinned => "pinned",
            Item::Recent => "recent",
            Item::Badges => "badges",
            Item::Vitals => "vitals",
            Item::Motion => "motion",
            _ => return None,
        })
    }
    /// The item a typed key changes inside Options.
    pub fn by_key(c: char) -> Option<Item> {
        Some(match c {
            'o' => Item::Order,
            '\\' => Item::Sidebar,
            'b' => Item::Look,
            's' => Item::Saver,
            '?' => Item::Keys,
            '=' => Item::Reset,
            _ => return None,
        })
    }
}

/// What an item is set to now, in words.
pub fn value(ui: &Ui, item: Item) -> String {
    let shown = |on: bool| if on { "Shown" } else { "Hidden" }.to_string();
    match item {
        Item::Order => match ui.order {
            Order::Activity => "Latest activity first".into(),
            Order::Layout => "As laid out".into(),
        },
        Item::Screen if ui.prefs.screen => format!("Shown · {}%", ui.sizes.screen),
        Item::Tiles => format!("{}%", ui.sizes.get(Split::Tiles)),
        Item::Sidebar if ui.sizes.sidebar == 0 => "Icons only".into(),
        Item::Sidebar => format!("Open · {} columns", ui.sizes.sidebar),
        Item::Look if !ui.theme.color || !ui.theme.truecolor => "Plain · needs true color".into(),
        Item::Look if ui.theme.ascii => "Plain · Unicode artwork disabled".into(),
        Item::Look if !ui.theme.backdrop => "Plain · background disabled".into(),
        Item::Look => ui.look.describe().into(),
        Item::Saver if !ui.saver.enabled => "Off".into(),
        Item::Saver => "On".into(),
        Item::Motion if ui.still => "Still · reduced motion is set".into(),
        Item::Motion if ui.prefs.motion => "On".into(),
        Item::Motion => "Still".into(),
        Item::Keys => shown(!ui.hide_keys),
        Item::Reset => "Every border back to its default".into(),
        Item::Settings => "Telegram, harnesses and accounts".into(),
        Item::Updates => format!(
            "Installed {} · check GitHub releases",
            env!("CARGO_PKG_VERSION")
        ),
        _ => shown(item.switch().and_then(|s| ui.prefs.get(s)).unwrap_or(true)),
    }
}

/// Whether a choice is on: switches, the open sidebar, the screensaver.
pub fn is_on(ui: &Ui, item: Item) -> Option<bool> {
    match item {
        Item::Sidebar => Some(ui.sizes.sidebar > 0),
        Item::Saver => Some(ui.saver.enabled),
        Item::Keys => Some(!ui.hide_keys),
        Item::Motion => Some(ui.prefs.motion && !ui.still),
        _ => item.switch().and_then(|s| ui.prefs.get(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_item_is_named_once_and_keys_lead_back_to_their_item() {
        let labels: std::collections::HashSet<_> = Item::ALL.iter().map(|i| i.label()).collect();
        assert_eq!(labels.len(), Item::ALL.len());
        for item in Item::ALL {
            let key = item.key();
            if key.chars().count() == 1 {
                assert_eq!(Item::by_key(key.chars().next().unwrap()), Some(item));
            }
            assert_ne!(
                crate::theme::icon(item.icon()).0,
                "•",
                "{item:?} has an icon"
            );
        }
        for name in crate::model::Prefs::NAMES {
            assert!(Item::ALL.iter().any(|i| i.switch() == Some(name)), "{name}");
        }
    }

    #[test]
    fn values_say_what_is_set() {
        let mut ui = Ui::default();
        assert_eq!(value(&ui, Item::Order), "Latest activity first");
        assert_eq!(value(&ui, Item::Details), "Shown");
        ui.prefs.details = false;
        assert_eq!(value(&ui, Item::Details), "Hidden");
        assert_eq!(is_on(&ui, Item::Details), Some(false));
        ui.sizes.sidebar = 0;
        assert_eq!(value(&ui, Item::Sidebar), "Icons only");
        ui.still = true;
        assert_eq!(is_on(&ui, Item::Motion), Some(false));
        assert_eq!(is_on(&ui, Item::Order), None);
    }
}
