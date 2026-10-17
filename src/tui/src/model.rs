use crate::embed::Embed;
use crate::theme::Theme;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

pub const OVERVIEW: usize = 0;
pub const WORKSPACES: usize = 1;
pub const COMMANDS: usize = 10;
/// Fallback navigation size before the first snapshot arrives.
pub const PAGES: usize = 11;

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Node {
    pub id: String,
    pub parent: String,
    pub label: String,
    pub branch: String,
    pub kind: String,
    pub backend: String,
    pub children: bool,
    pub count: usize,
    pub state: String,
    pub linked: usize,
    pub viewed: bool,
    pub server_focused: bool,
    pub usage: WorkspaceUsage,
    pub actionable: bool,
    pub fields: Vec<Field>,
    /// The label split by role (id, title, harness, model, topic, sid,
    /// muted) so each part can be colored; the label stays for search.
    pub parts: Vec<(String, String)>,
    /// Present on terminal (pane) nodes: what a tile shows.
    pub tile: Option<Tile>,
    /// When the terminal last did something (epoch seconds, 0 unknown), and
    /// how: working, blocked, status, input or output.
    pub active_at: f64,
    pub activity: String,
    /// On a tab or window: its panes where the backend places them.
    pub layout: Option<Layout>,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkspaceUsage {
    pub visits: u64,
    pub seconds: f64,
    pub last_selected: f64,
}
/// A tab's split layout in cells, as Herdr or tmux reports it.
#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default)]
pub struct Layout {
    pub width: u16,
    pub height: u16,
    pub zoomed: bool,
    /// In reading order: top to bottom, then left to right.
    pub panes: Vec<LayoutPane>,
}
#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default)]
pub struct LayoutPane {
    /// The pane's tree node ID.
    pub id: String,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub focused: bool,
}
/// A shared native pane edge projected into the Focus screen.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneDivider {
    pub area: ratatui::layout::Rect,
    pub leading: String,
    pub trailing: String,
    pub horizontal: bool,
    pub native_length: u16,
    pub screen_length: u16,
    pub minimum: i32,
    pub maximum: i32,
}
impl PaneDivider {
    pub fn cells(&self, start: (u16, u16), end: (u16, u16)) -> i32 {
        let (start, end) = if self.horizontal {
            (start.0, end.0)
        } else {
            (start.1, end.1)
        };
        let delta = i32::from(end) - i32::from(start);
        let scaled = delta * i32::from(self.native_length) / i32::from(self.screen_length.max(1));
        scaled.clamp(self.minimum, self.maximum)
    }
}
#[derive(Clone)]
pub struct PaneResizeDrag {
    pub divider: PaneDivider,
    pub layout: Layout,
    pub start: (u16, u16),
    pub cells: i32,
}
impl Layout {
    pub fn same_geometry(&self, other: &Self) -> bool {
        let canonical = |layout: &Self| {
            let mut layout = layout.clone();
            for pane in &mut layout.panes {
                pane.focused = false;
            }
            layout.panes.sort_by(|a, b| a.id.cmp(&b.id));
            layout
        };
        canonical(self) == canonical(other)
    }
    pub fn dividers(&self, screen: ratatui::layout::Rect) -> Vec<PaneDivider> {
        if self.zoomed || self.width == 0 || self.height == 0 {
            return Vec::new();
        }
        let scale = |at: u16, size: u16, origin: u16, length: u16| {
            origin.saturating_add(
                (u32::from(at.min(size)) * u32::from(length) / u32::from(size.max(1))) as u16,
            )
        };
        let mut edges = Vec::new();
        for a in &self.panes {
            for b in &self.panes {
                if a.id == b.id {
                    continue;
                }
                for horizontal in [true, false] {
                    let (end, start, a0, a1, b0, b1, a_size, b_size) = if horizontal {
                        (
                            a.x.saturating_add(a.w),
                            b.x,
                            a.y,
                            a.y.saturating_add(a.h),
                            b.y,
                            b.y.saturating_add(b.h),
                            a.w,
                            b.w,
                        )
                    } else {
                        (
                            a.y.saturating_add(a.h),
                            b.y,
                            a.x,
                            a.x.saturating_add(a.w),
                            b.x,
                            b.x.saturating_add(b.w),
                            a.h,
                            b.h,
                        )
                    };
                    let overlap0 = a0.max(b0);
                    let overlap1 = a1.min(b1);
                    if end > start || start - end > 1 || overlap1 <= overlap0 {
                        continue;
                    }
                    let min_size = if horizontal { 4 } else { 3 };
                    if a_size < min_size
                        || b_size < min_size
                        || (a_size == min_size && b_size == min_size)
                    {
                        continue;
                    }
                    let area = if horizontal {
                        let x = scale(start, self.width, screen.x, screen.width)
                            .saturating_sub(1)
                            .max(screen.x);
                        let y0 =
                            scale(overlap0, self.height, screen.y, screen.height).saturating_add(1);
                        let y1 =
                            scale(overlap1, self.height, screen.y, screen.height).saturating_sub(1);
                        ratatui::layout::Rect::new(
                            x,
                            y0,
                            2.min(screen.right().saturating_sub(x)),
                            y1.saturating_sub(y0),
                        )
                    } else {
                        let y = scale(start, self.height, screen.y, screen.height)
                            .saturating_sub(1)
                            .max(screen.y);
                        let x0 =
                            scale(overlap0, self.width, screen.x, screen.width).saturating_add(1);
                        let x1 =
                            scale(overlap1, self.width, screen.x, screen.width).saturating_sub(1);
                        ratatui::layout::Rect::new(
                            x0,
                            y,
                            x1.saturating_sub(x0),
                            2.min(screen.bottom().saturating_sub(y)),
                        )
                    };
                    if area.width == 0 || area.height == 0 {
                        continue;
                    }
                    edges.push(PaneDivider {
                        area,
                        leading: a.id.clone(),
                        trailing: b.id.clone(),
                        horizontal,
                        native_length: if horizontal { self.width } else { self.height },
                        screen_length: if horizontal {
                            screen.width
                        } else {
                            screen.height
                        },
                        minimum: -(i32::from(a_size) - i32::from(min_size)),
                        maximum: i32::from(b_size) - i32::from(min_size),
                    });
                }
            }
        }
        edges
    }
}
impl Node {
    /// Working or blocked right now.
    pub fn live(&self) -> bool {
        matches!(self.activity.as_str(), "working" | "blocked")
    }
}
/// One inspector fact, sent as `[label, value]` or `[label, value, tone]`.
/// The tone colors the value: a badge tone, or a role such as `state`,
/// `harness`, `herdr`, `tmux`, `id` or `faint`.
#[derive(Clone, Default, Deserialize, Debug, PartialEq)]
#[serde(from = "Vec<String>")]
pub struct Field {
    pub key: String,
    pub value: String,
    pub tone: String,
}
impl Field {
    #[cfg(test)]
    pub fn new(key: &str, value: &str, tone: &str) -> Self {
        Field {
            key: key.into(),
            value: value.into(),
            tone: tone.into(),
        }
    }
}
impl From<Vec<String>> for Field {
    fn from(parts: Vec<String>) -> Self {
        let mut parts = parts.into_iter();
        let mut next = || parts.next().unwrap_or_default();
        Field {
            key: next(),
            value: next(),
            tone: next(),
        }
    }
}
#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Tile {
    pub title: String,
    pub place: String,
    pub project: String,
    pub harness: String,
    pub state: String,
    pub views: usize,
    pub pinned: bool,
    pub viewer: bool,
    /// How many terminals share its tab or window; 0 when not reported.
    pub split: usize,
}
/// A terminal action with an optional single-key shortcut.
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct QuickAction {
    pub id: String,
    /// One-word label for the quick bar.
    pub short: String,
    pub title: String,
    pub note: String,
    pub quick: String,
    pub icon: String,
    pub badges: Vec<(String, String)>,
}
/// A pinned terminal reference, resolved by the backend. Missing and stale
/// references stay visible; they are never rebound to another terminal.
#[derive(Clone, Default, Deserialize, Debug)]
#[serde(default)]
pub struct Pin {
    pub id: String,
    pub label: String,
    pub place: String,
    pub status: String,
    pub reason: String,
    pub key: String,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Apply {
    pub mode: String,
    pub selected: String,
    /// Panel sizes remembered with the view or a saved layout.
    pub sizes: Option<Sizes>,
    /// How strongly the artwork shows: off, normal or bright.
    pub backdrop: String,
    /// The screensaver: "on" or "off".
    pub saver: String,
    /// Terminal order: "activity" or "layout".
    pub order: String,
    /// Option switches ("details", "keys", …): "on" or "off".
    #[serde(flatten)]
    pub switches: HashMap<String, Value>,
}
/// A border the user can move: by dragging it, or with the resize keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    Sidebar,
    /// Between the list and the Details panel.
    Details,
    /// Between the live screen and the facts in the Details panel.
    Screen,
    /// Tile size in Tiles (keys only).
    Tiles,
}
impl Split {
    /// (minimum, maximum); the backend checks the same limits.
    pub fn limits(self) -> (u16, u16) {
        match self {
            Split::Sidebar => (0, 40),
            Split::Details => (20, 80),
            Split::Screen => (20, 90),
            Split::Tiles => (100, 300),
        }
    }
    pub fn describe(self, value: u16) -> String {
        match self {
            Split::Sidebar if value == 0 => "Sidebar folded · \\ or » opens it".into(),
            Split::Sidebar => format!("Sidebar {value} columns"),
            Split::Details => format!("Details {value}% of the width"),
            Split::Screen => format!("Live screen {value}% of Details"),
            Split::Tiles => format!("Tiles {value}% of the smallest size"),
        }
    }
}
/// Panel sizes. All but the sidebar are percentages, so the layout scales
/// with the window.
#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Sizes {
    /// Sidebar width in columns.
    pub sidebar: u16,
    /// The Details panel's share of the page width.
    pub details: u16,
    /// The live screen's share of the Details panel height.
    pub screen: u16,
    /// Tile size relative to the smallest tile.
    pub tiles: u16,
}
impl Default for Sizes {
    fn default() -> Self {
        Sizes {
            sidebar: 24,
            details: 44,
            screen: 60,
            tiles: 100,
        }
    }
}
impl Sizes {
    pub fn get(&self, split: Split) -> u16 {
        match split {
            Split::Sidebar => self.sidebar,
            Split::Details => self.details,
            Split::Screen => self.screen,
            Split::Tiles => self.tiles,
        }
    }
    /// Columns of the folded sidebar: a rail of section icons.
    pub const RAIL: u16 = 5;
    /// Set one size, kept within its limits. Returns whether it changed.
    /// A sidebar narrower than 12 columns folds; 12 to 15 opens at 16.
    pub fn set(&mut self, split: Split, value: i32) -> bool {
        let (low, high) = split.limits();
        let value = match (split, value) {
            (Split::Sidebar, 1..12) => 0,
            (Split::Sidebar, 12..16) => 16,
            _ => value,
        };
        let value = value.clamp(low as i32, high as i32) as u16;
        let slot = match split {
            Split::Sidebar => &mut self.sidebar,
            Split::Details => &mut self.details,
            Split::Screen => &mut self.screen,
            Split::Tiles => &mut self.tiles,
        };
        let changed = *slot != value;
        *slot = value;
        changed
    }
    pub fn reset(&mut self, split: Split) -> bool {
        self.set(split, Sizes::default().get(split) as i32)
    }
    pub fn clamped(mut self) -> Self {
        for split in [Split::Sidebar, Split::Details, Split::Screen, Split::Tiles] {
            self.set(split, self.get(split) as i32);
        }
        self
    }
}
/// A border that can be dragged in the last frame: where to grab it, and
/// the line it moves along (`origin` and `length`, in cells).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle {
    pub area: ratatui::layout::Rect,
    pub split: Split,
    pub origin: u16,
    pub length: u16,
}
impl Handle {
    /// The size that puts the border under the pointer.
    pub fn value(&self, column: u16, row: u16) -> i32 {
        let percent =
            |cells: i32| (cells * 100 + i32::from(self.length) / 2) / i32::from(self.length.max(1));
        match self.split {
            // The sidebar's last column is its border.
            Split::Sidebar => i32::from(column) - i32::from(self.origin) + 1,
            // Measured from the right edge: the Details panel starts here.
            Split::Details => percent(i32::from(self.origin) - i32::from(column)),
            // The screen ends on the row above the border.
            Split::Screen => percent(i32::from(row) - i32::from(self.origin)),
            Split::Tiles => 0,
        }
    }
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkspaceState {
    pub pins: Vec<Pin>,
    /// Restore a saved view: set once by the backend, then applied.
    pub apply: Apply,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum View {
    #[default]
    Tree,
    Tiles,
    /// Every terminal on one screen, however many there are.
    Wall,
    Focus,
}
impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Tree => "tree",
            View::Tiles => "tiles",
            View::Wall => "wall",
            View::Focus => "focus",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            View::Tree => "Tree",
            View::Tiles => "Tiles",
            View::Wall => "Wall",
            View::Focus => "Focus",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "tree" => Some(View::Tree),
            "tiles" => Some(View::Tiles),
            "wall" => Some(View::Wall),
            "focus" => Some(View::Focus),
            _ => None,
        }
    }
    /// Terminals laid out as a grid of screens, sharing selection and keys.
    pub fn grid(self) -> bool {
        matches!(self, View::Tiles | View::Wall)
    }
    pub const ALL: [View; 4] = [View::Tree, View::Tiles, View::Wall, View::Focus];
}
/// How Tree, Tiles and the Wall order terminals.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Order {
    /// Working or blocked first, then the latest activity.
    #[default]
    Activity,
    /// As the backends lay them out.
    Layout,
}
impl Order {
    pub fn name(self) -> &'static str {
        match self {
            Order::Activity => "activity",
            Order::Layout => "layout",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "activity" => Some(Order::Activity),
            "layout" => Some(Order::Layout),
            _ => None,
        }
    }
}
/// Switches chosen in Options (`,`), remembered with the view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prefs {
    /// The Details panel beside lists.
    pub details: bool,
    /// A terminal's live screen inside Details.
    pub screen: bool,
    /// The sidebar's pinned and most recent terminals.
    pub pinned: bool,
    pub recent: bool,
    /// Counts beside the sidebar's sections, and its totals.
    pub badges: bool,
    pub vitals: bool,
    /// Neon motion and glitches; off keeps everything still.
    pub motion: bool,
}
impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            details: true,
            screen: true,
            pinned: true,
            recent: true,
            badges: true,
            vitals: true,
            motion: true,
        }
    }
}
impl Prefs {
    pub const NAMES: [&'static str; 7] = [
        "details", "screen", "pinned", "recent", "badges", "vitals", "motion",
    ];
    pub fn get(&self, name: &str) -> Option<bool> {
        Some(match name {
            "details" => self.details,
            "screen" => self.screen,
            "pinned" => self.pinned,
            "recent" => self.recent,
            "badges" => self.badges,
            "vitals" => self.vitals,
            "motion" => self.motion,
            _ => return None,
        })
    }
    pub fn set(&mut self, name: &str, on: bool) {
        let slot = match name {
            "details" => &mut self.details,
            "screen" => &mut self.screen,
            "pinned" => &mut self.pinned,
            "recent" => &mut self.recent,
            "badges" => &mut self.badges,
            "vitals" => &mut self.vitals,
            "motion" => &mut self.motion,
            _ => return,
        };
        *slot = on;
    }
    /// Remembered switches: "on" or "off"; anything else is ignored.
    pub fn apply(&mut self, values: &HashMap<String, Value>) {
        for name in Self::NAMES {
            match values.get(name).and_then(Value::as_str) {
                Some("on") => self.set(name, true),
                Some("off") => self.set(name, false),
                _ => {}
            }
        }
    }
}
/// The Options panel while it is open.
#[derive(Clone, Debug, Default)]
pub struct OptionsMenu {
    pub selected: usize,
    pub opened: Option<Instant>,
    pub moved: Option<Instant>,
}
/// Tree nodes with their children and each subtree's activity, for drawing
/// in activity order.
struct Forest<'a> {
    nodes: &'a [Node],
    children: HashMap<&'a str, Vec<usize>>,
    key: Vec<(bool, f64)>,
}
impl Forest<'_> {
    fn walk(&self, items: &[usize], prefix: &str, out: &mut Vec<(usize, String)>) {
        let mut sorted = items.to_vec();
        sorted.sort_by_key(|&i| recency(self.key[i].0, self.key[i].1));
        for (n, &i) in sorted.iter().enumerate() {
            let last = n + 1 == sorted.len();
            out.push((i, format!("{prefix}{}", if last { "└─ " } else { "├─ " })));
            if let Some(kids) = self.children.get(self.nodes[i].id.as_str()) {
                let deeper = format!("{prefix}{}", if last { "   " } else { "│  " });
                self.walk(kids, &deeper, out);
            }
        }
    }
}
/// Sort key for activity order: live first, then the latest; ties keep
/// layout order because the sort is stable.
fn recency(live: bool, at: f64) -> (bool, i64) {
    (!live, -(at as i64))
}
/// One tile in Tiles: a live terminal or a pinned reference that is gone.
#[derive(Clone, Debug, PartialEq)]
pub struct TileRow {
    /// Tree node ID; empty for a missing pin.
    pub id: String,
    /// Index into `snapshot.tree` for live terminals.
    pub source: Option<usize>,
    /// Index into the pins for pinned or missing terminals.
    pub pin: Option<usize>,
}
/// A bounded snapshot of a terminal's visible screen, parsed once on receipt.
pub struct Preview {
    pub screen: vt100::Parser,
    /// Lines with content, so a preview can be bottom-aligned.
    pub lines: u16,
    pub error: String,
}
impl Preview {
    pub const COLUMNS: u16 = 240;
    /// Larger than the backend's per-preview limit, which already trims.
    pub const BYTES: usize = 256 * 1024;
    pub fn new(text: &str, error: &str) -> Self {
        // Keep the newest lines: drop whole lines from the top, so a color
        // code is never cut.
        let mut text = text;
        while text.len() > Self::BYTES {
            text = text.split_once('\n').map_or("", |(_, rest)| rest);
        }
        // The newest 200 lines at most.
        let all: Vec<&str> = text.split('\n').collect();
        let kept = &all[all.len().saturating_sub(200)..];
        let lines = kept.len().max(1) as u16;
        let mut screen = vt100::Parser::new(lines, Self::COLUMNS, 0);
        // Captured lines end in LF only; draw each on its own row.
        for (i, line) in kept.iter().enumerate() {
            if i > 0 {
                screen.process(b"\r\n");
            }
            screen.process(line.as_bytes());
            screen.process(b"\x1b[0m");
        }
        Self {
            screen,
            lines,
            error: error.to_owned(),
        }
    }
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Item {
    pub id: String,
    pub group: String,
    pub title: String,
    pub note: String,
    pub icon: String,
    pub context: String,
    pub badges: Vec<(String, String)>,
    pub aliases: Vec<String>,
    /// Current state of a record (label, tone), shown at the row's right edge.
    pub status: (String, String),
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Section {
    pub title: String,
    pub icon: String,
    pub category: String,
    pub summary: String,
    pub items: Vec<Item>,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    pub category: String,
    pub group: String,
    pub title: String,
    pub description: String,
    pub interface: String,
    pub detail: String,
    pub icon: String,
    pub aliases: Vec<String>,
    pub badges: Vec<(String, String)>,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Light {
    pub label: String,
    pub state: String,
    pub detail: String,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Step {
    pub label: String,
    pub done: bool,
    pub detail: String,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct Overview {
    pub connections: Vec<Light>,
    pub setup: Vec<Step>,
}
#[derive(Default, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub tree: Vec<Node>,
    pub sections: Vec<Section>,
    /// Sent with the first snapshot only; later snapshots keep the previous one.
    pub catalog: Vec<Entry>,
    pub overview: Overview,
    pub instance: String,
    /// The release, and the commit when running from a checkout.
    pub release: String,
    pub demo: bool,
    pub freshness: String,
    pub observed_at: f64,
    pub terminals: usize,
    pub views: usize,
    pub routes: usize,
    pub checkpoints: usize,
    pub warnings: usize,
    pub message: String,
    pub page: usize,
    pub focus: String,
    pub terminal_actions: Vec<QuickAction>,
    pub workspace: WorkspaceState,
}
#[derive(Default)]
pub struct Dialog {
    pub id: u64,
    pub mode: String,
    pub title: String,
    pub description: String,
    pub options: Vec<(String, String, String)>,
    /// Icon names beside options, where the backend sends them.
    pub icons: Vec<String>,
    pub text: String,
    pub secret: bool,
    pub input: String,
    pub selected: usize,
    pub scroll: u16,
    /// When it opened and when the choice last moved, for the menu's
    /// decode effect; unset, everything shows at once.
    pub opened: Option<Instant>,
    pub moved: Option<Instant>,
}
impl Dialog {
    pub fn from(value: Value) -> Self {
        let s = |name| value[name].as_str().unwrap_or("").to_owned();
        Self {
            id: value["id"].as_u64().unwrap_or(0),
            mode: s("mode"),
            title: s("title"),
            description: s("description"),
            text: s("text"),
            secret: value["secret"].as_bool().unwrap_or(false),
            options: serde_json::from_value(value["options"].clone()).unwrap_or_default(),
            icons: serde_json::from_value(value["icons"].clone()).unwrap_or_default(),
            ..Self::default()
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A group title; never selectable.
    Header,
    /// A live tree node (`source` indexes `snapshot.tree`).
    Node,
    /// A guided action or live record on a page (`source` indexes its items).
    Action,
    /// A command library entry (`source` indexes `snapshot.catalog`).
    Entry,
}
#[derive(Clone)]
pub struct Row {
    pub kind: Kind,
    pub id: String,
    pub title: String,
    pub note: String,
    pub icon: String,
    pub source: usize,
    pub parent: String,
    pub branch: String,
    pub children: bool,
    pub expanded: bool,
    /// Search alias that matched when the visible text did not.
    pub hint: String,
}
impl Row {
    fn header(title: impl Into<String>, note: impl Into<String>) -> Self {
        Row {
            kind: Kind::Header,
            id: String::new(),
            title: title.into(),
            note: note.into(),
            icon: String::new(),
            source: 0,
            parent: String::new(),
            branch: String::new(),
            children: false,
            expanded: false,
            hint: String::new(),
        }
    }
}
/// The boot splash: the artwork lit from its moon while the first
/// inventory loads. Any key or click skips it.
pub struct Splash {
    pub started: Instant,
    /// When the first snapshot arrived.
    pub ready: Option<Instant>,
    /// When it began to fade out.
    pub leaving: Option<Instant>,
    pub picture: crate::backdrop::Picture,
    /// Where the full-resolution picture goes this frame, when the
    /// terminal shows kitty graphics.
    pub hires: Option<ratatui::layout::Rect>,
}
impl Splash {
    /// Seconds it shows at least (when ready) and at most, and its fade out.
    pub const MIN: f32 = 1.8;
    pub const MAX: f32 = 4.0;
    pub const LEAVE: f32 = 0.35;
    /// Seconds more it shows while the full-resolution picture is up, so
    /// that picture is seen.
    pub const SHARP: f32 = 0.8;
    pub fn new() -> Self {
        Splash {
            started: Instant::now(),
            ready: None,
            leaving: None,
            picture: crate::backdrop::Picture::default(),
            hires: None,
        }
    }
    /// Start fading out once it has shown long enough and the inventory is
    /// ready, or at the latest after `MAX`. Returns false once it is over.
    pub fn advance(&mut self, animated: bool) -> bool {
        let shown = self.started.elapsed().as_secs_f32();
        let least = Self::MIN
            + if self.hires.is_some() {
                Self::SHARP
            } else {
                0.0
            };
        if self.leaving.is_none()
            && (shown >= Self::MAX || (shown >= least && self.ready.is_some()))
        {
            self.leaving = Some(Instant::now());
        }
        !self
            .leaving
            .is_some_and(|at| !animated || at.elapsed().as_secs_f32() >= Self::LEAVE)
    }
}
/// The screensaver: the artwork, glitching, after a while without input.
/// Any key or mouse movement returns; `s` turns it on and off.
pub struct Saver {
    pub enabled: bool,
    /// The last key, click, mouse movement or paste.
    pub last_input: Instant,
    /// When it started, while it shows.
    pub since: Option<Instant>,
    pub picture: crate::backdrop::Picture,
}
impl Default for Saver {
    fn default() -> Self {
        Saver {
            enabled: true,
            last_input: Instant::now(),
            since: None,
            picture: crate::backdrop::Picture::default(),
        }
    }
}
impl Saver {
    /// Whether it has waited `after` seconds of no input; 0 means never.
    pub fn due(&self, after: u64, now: Instant) -> bool {
        self.enabled
            && self.since.is_none()
            && after > 0
            && now.saturating_duration_since(self.last_input).as_secs() >= after
    }
    /// Input arrived: restart the wait. True when it woke the screensaver.
    pub fn touch(&mut self, now: Instant) -> bool {
        self.last_input = now;
        self.since.take().is_some()
    }
}
#[derive(Default)]
pub struct Ui {
    pub snapshot: Snapshot,
    pub theme: Theme,
    pub page: usize,
    pub selected: usize,
    /// First visible row of the main list.
    pub offset: usize,
    pub filter: String,
    pub searching: bool,
    pub switching: bool,
    pub collapsed: HashSet<String>,
    pub dialog: Option<Dialog>,
    pub busy: bool,
    pub status: String,
    pub status_error: bool,
    pub action_pending: bool,
    /// Screen row → list row, for mouse selection.
    pub list_hits: Vec<(ratatui::layout::Rect, usize)>,
    pub nav_hits: Vec<(ratatui::layout::Rect, usize)>,
    /// When the selection or page last changed, for transitions.
    pub moved_at: Option<Instant>,
    pub paged_at: Option<Instant>,
    /// Workspaces view: tree, tiles or the focused terminal.
    pub view: View,
    /// View to return to when Focus closes.
    pub previous: View,
    /// Selected index into `tiles()`.
    pub tile: usize,
    pub tiles_per_page: usize,
    pub tile_columns: usize,
    pub pinned_only: bool,
    pub previews: HashMap<String, Preview>,
    pub preview_pending: bool,
    pub last_preview: Option<Instant>,
    /// Terminal shown in Focus (tree node ID).
    pub focus_target: String,
    /// Live stream of the focused terminal, observing or controlling.
    pub embed: Option<Embed>,
    pub tile_hits: Vec<(ratatui::layout::Rect, usize)>,
    pub quick_hits: Vec<(ratatui::layout::Rect, String)>,
    pub view_hits: Vec<(ratatui::layout::Rect, View)>,
    /// Last tile click, to open Focus on a second click.
    pub clicked: Option<(usize, Instant)>,
    /// Takeover needs a second, deliberate key press.
    pub takeover_armed: bool,
    /// Terminals drawn in the last frame (tiles, or the Tree's Details
    /// panel); only these are previewed.
    pub preview_targets: Vec<String>,
    /// Targets of the last preview request, to refresh a new selection soon.
    pub preview_sent: Vec<String>,
    /// Text rows a preview can show.
    pub preview_rows: u16,
    /// Last terminal selected in Workspaces, reported from other pages too.
    pub last_selected: String,
    /// The shortcut row is hidden (`?` toggles it) for one more content row.
    pub hide_keys: bool,
    /// What fills the background (`b`), and the artwork scaled for the
    /// screen when it is the faint or bright artwork.
    pub look: crate::backdrop::Look,
    pub backdrop: crate::backdrop::Picture,
    /// The boot splash, until it has faded out.
    pub splash: Option<Splash>,
    pub saver: Saver,
    /// Set when the terminal shows graphics; splash and screensaver use original pixels.
    pub kitty: Option<crate::graphics::Kitty>,
    /// Where that picture is on screen now.
    pub kitty_shown: Option<ratatui::layout::Rect>,
    pub sizes: Sizes,
    /// Borders drawn in the last frame that can be dragged.
    pub handles: Vec<Handle>,
    /// The border being dragged, and the one under the pointer.
    pub dragging: Option<Handle>,
    pub hovered: Option<Split>,
    /// Last click on a border, so a double click resets it.
    pub handle_clicked: Option<(Split, Instant)>,
    /// When sizes last changed; they are remembered once they settle.
    pub sizes_changed: Option<Instant>,
    /// Sidebar width to restore when it is unfolded.
    pub sidebar_open: u16,
    /// Terminal order in Tree, Tiles and the Wall.
    pub order: Order,
    /// Focus shows one terminal alone instead of its whole tab.
    pub zoomed: bool,
    /// Panes of the tab drawn in Focus, for clicks.
    pub pane_hits: Vec<(ratatui::layout::Rect, String)>,
    /// Native split edges shown in Focus; drag submits one checked backend resize.
    pub pane_dividers: Vec<PaneDivider>,
    pub pane_resize: Option<PaneResizeDrag>,
    pub prefs: Prefs,
    /// Reduced motion asked for by the environment; Options cannot undo it.
    pub still: bool,
    pub options: Option<OptionsMenu>,
    /// Rows of the Options panel drawn in the last frame, and the panel,
    /// for clicks.
    pub option_hits: Vec<(ratatui::layout::Rect, usize)>,
    pub option_panel: ratatui::layout::Rect,
    /// Terminals listed in the sidebar, for clicks.
    pub side_hits: Vec<(ratatui::layout::Rect, String)>,
    /// Where the mouse pointer last was, for hover highlights and labels.
    pub pointer: Option<(u16, u16)>,
    /// A label beside the pointer, drawn over everything else.
    pub tooltip: Option<(ratatui::layout::Rect, String)>,
}

/// Every search term must appear in the haystack.
fn matches(terms: &[String], haystack: &str) -> bool {
    let haystack = haystack.to_lowercase();
    terms.iter().all(|t| haystack.contains(t.as_str()))
}

/// Search labels and aliases together, so "telegram fleet" finds /jobs.
fn search_hint(terms: &[String], shown: &str, aliases: &[String]) -> Option<String> {
    if matches(terms, shown) {
        return Some(String::new());
    }
    if !matches(terms, &format!("{shown} {}", aliases.join(" "))) {
        return None;
    }
    let shown = shown.to_lowercase();
    Some(
        aliases
            .iter()
            .filter(|alias| {
                let alias = alias.to_lowercase();
                terms
                    .iter()
                    .any(|term| !shown.contains(term) && alias.contains(term))
            })
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
    )
}

impl Ui {
    pub fn page_count(&self) -> usize {
        if self.snapshot.sections.is_empty() {
            PAGES
        } else {
            self.snapshot.sections.len()
        }
    }
    pub fn section(&self) -> Option<&Section> {
        self.snapshot.sections.get(self.page)
    }
    fn terms(&self) -> Vec<String> {
        self.filter
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }
    pub fn rows(&self) -> Vec<Row> {
        let terms = self.terms();
        let mut rows = Vec::new();
        if self.switching {
            return self.switch_rows(&terms);
        }
        if self.page == WORKSPACES {
            self.tree_rows(&terms, &mut rows);
        }
        if self.page == COMMANDS {
            self.catalog_rows(&terms, &mut rows);
        } else {
            self.item_rows(&terms, &mut rows);
        }
        rows
    }
    fn switch_rows(&self, terms: &[String]) -> Vec<Row> {
        let mut entries: Vec<_> = self
            .snapshot
            .tree
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                if n.kind != "pane" || n.tile.is_none() {
                    return None;
                }
                let mut path = Vec::new();
                let mut parent = n.parent.as_str();
                let mut seen = HashSet::new();
                while let Some(p) = self.snapshot.tree.iter().find(|p| p.id == parent) {
                    if !seen.insert(&p.id) {
                        break;
                    }
                    path.push(p.label.as_str());
                    parent = &p.parent;
                }
                path.reverse();
                let place = path.join(" › ");
                let fields = n
                    .fields
                    .iter()
                    .map(|f| f.value.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !matches(
                    terms,
                    &format!("{} {} {} {}", n.label, place, n.backend, fields),
                ) {
                    return None;
                }
                let pinned = self
                    .snapshot
                    .workspace
                    .pins
                    .iter()
                    .position(|p| p.id == n.id && p.status == "live");
                Some((i, n, place, pinned))
            })
            .collect();
        entries.sort_by(|a, b| {
            a.3.unwrap_or(usize::MAX)
                .cmp(&b.3.unwrap_or(usize::MAX))
                .then_with(|| b.1.usage.last_selected.total_cmp(&a.1.usage.last_selected))
                .then_with(|| b.1.usage.visits.cmp(&a.1.usage.visits))
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
        let mut rows = vec![Row::header(
            "Switch terminal",
            "Pinned first · recent Herdr selections · type to filter",
        )];
        for (rank, (i, n, place, pin)) in entries.into_iter().enumerate() {
            let mut note = format!("{} · {}", place, n.state);
            if pin.is_some() {
                note.push_str(" · pinned");
            }
            if n.usage.visits > 0 {
                note.push_str(&format!(
                    " · {:.1}m selected · {} visits",
                    n.usage.seconds / 60.0,
                    n.usage.visits
                ));
            }
            if rank < 9 {
                note = format!("Alt+{} · {note}", rank + 1);
            }
            rows.push(Row {
                kind: Kind::Node,
                id: n.id.clone(),
                title: n.label.clone(),
                note,
                icon: "pane".into(),
                source: i,
                parent: String::new(),
                branch: String::new(),
                children: false,
                expanded: false,
                hint: String::new(),
            });
        }
        rows
    }
    fn tree_rows(&self, terms: &[String], rows: &mut Vec<Row>) {
        let nodes = &self.snapshot.tree;
        let mut visible: HashSet<&str> = HashSet::new();
        if !terms.is_empty() {
            for n in nodes {
                // Field values, and label parts such as the program's topic.
                let fields: Vec<&str> = n
                    .fields
                    .iter()
                    .map(|f| f.value.as_str())
                    .chain(n.parts.iter().map(|(part, _)| part.as_str()))
                    .collect();
                if matches(
                    terms,
                    &format!("{} {} {} {}", n.label, n.state, n.kind, fields.join(" ")),
                ) {
                    let mut key = n.id.as_str();
                    while let Some(node) = nodes.iter().find(|n| n.id == key) {
                        if !visible.insert(key) {
                            break;
                        }
                        key = &node.parent;
                    }
                }
            }
        }
        let mut hidden: HashSet<&str> = HashSet::new();
        let start = rows.len();
        for (i, branch) in self.tree_order() {
            let n = &nodes[i];
            if terms.is_empty() {
                if hidden.contains(n.parent.as_str()) || self.collapsed.contains(&n.parent) {
                    hidden.insert(&n.id);
                    continue;
                }
            } else if !visible.contains(n.id.as_str()) {
                continue;
            }
            // An agent session shares its pane's state; show it once, on the pane.
            let mut note = if n.kind == "agent session" {
                String::new()
            } else if n.kind == "pane" {
                match self.ago(n) {
                    age if age.is_empty() || n.live() => n.state.clone(),
                    age => format!("{} · {age}", n.state),
                }
            } else if n.children {
                n.count.to_string()
            } else {
                String::new()
            };
            if n.linked > 1 {
                note += &format!(" · {} linked views", n.linked);
            }
            if n.viewed {
                note += " · viewed";
            }
            if n.server_focused {
                note += " · server focus";
            }
            rows.push(Row {
                kind: Kind::Node,
                id: n.id.clone(),
                title: n.label.clone(),
                note,
                icon: n.kind.clone(),
                source: i,
                parent: n.parent.clone(),
                branch,
                children: n.children,
                expanded: !terms.is_empty() || !self.collapsed.contains(&n.id),
                hint: String::new(),
            });
        }
        let found = rows.len() - start;
        if found > 0 || terms.is_empty() {
            let label = format!(
                "{} terminals · {} views",
                self.snapshot.terminals, self.snapshot.views
            );
            rows.insert(start, Row::header("Live topology", label));
        }
    }
    /// Tree nodes in display order, each with its branch drawing. In
    /// activity order, siblings with the latest activity anywhere below
    /// them come first.
    pub fn tree_order(&self) -> Vec<(usize, String)> {
        let nodes = &self.snapshot.tree;
        if self.order == Order::Layout {
            return nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (i, n.branch.clone()))
                .collect();
        }
        let index: HashMap<&str, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut roots = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            match index.get(n.parent.as_str()) {
                Some(_) => children.entry(n.parent.as_str()).or_default().push(i),
                None => roots.push(i),
            }
        }
        // Rows arrive parents first, so a backward pass has every subtree
        // complete before it reaches the parent.
        let mut key: Vec<(bool, f64)> = nodes.iter().map(|n| (n.live(), n.active_at)).collect();
        for i in (0..nodes.len()).rev() {
            if let Some(&p) = index.get(nodes[i].parent.as_str())
                && p != i
            {
                let (live, at) = key[i];
                key[p] = (key[p].0 || live, key[p].1.max(at));
            }
        }
        let mut out = Vec::with_capacity(nodes.len());
        Forest {
            nodes,
            children,
            key,
        }
        .walk(&roots, "", &mut out);
        out
    }
    /// The split layout of the tab holding terminal `id`, when the tab
    /// holds more than one terminal.
    pub fn tab_layout(&self, id: &str) -> Option<&Layout> {
        let node = self.terminal_node(id)?;
        let group = self.snapshot.tree.iter().find(|n| n.id == node.parent)?;
        group
            .layout
            .as_ref()
            .filter(|l| l.panes.len() > 1 && l.panes.iter().any(|p| p.id == node.id))
    }
    /// The terminal beside `id` in its tab, the way an arrow points: the
    /// nearest one across that edge, preferring the most shared border.
    pub fn neighbor(&self, id: &str, dx: i32, dy: i32) -> Option<String> {
        let layout = self.tab_layout(id)?;
        let id = &self.terminal_node(id)?.id;
        let here = layout.panes.iter().find(|p| &p.id == id)?;
        let span = |a: u16, len: u16| (i32::from(a), i32::from(a) + i32::from(len));
        let overlap = |(a0, a1): (i32, i32), (b0, b1): (i32, i32)| (a1.min(b1) - a0.max(b0)).max(0);
        let (hx, hy) = (span(here.x, here.w), span(here.y, here.h));
        layout
            .panes
            .iter()
            .filter(|p| &p.id != id)
            .filter_map(|p| {
                let (px, py) = (span(p.x, p.w), span(p.y, p.h));
                let (gap, shared) = match (dx, dy) {
                    (1, _) => (px.0 - hx.1, overlap(hy, py)),
                    (-1, _) => (hx.0 - px.1, overlap(hy, py)),
                    (_, 1) => (py.0 - hy.1, overlap(hx, px)),
                    _ => (hy.0 - py.1, overlap(hx, px)),
                };
                (gap >= 0).then_some((shared == 0, gap, -shared, p.id.clone()))
            })
            .min()
            .map(|(.., id)| id)
    }
    /// How long ago a terminal was last active: "now", "4m", "2h", "3d";
    /// empty when unknown.
    pub fn ago(&self, node: &Node) -> String {
        if node.active_at <= 0.0 {
            return String::new();
        }
        let now = if self.snapshot.observed_at > 0.0 {
            self.snapshot.observed_at
        } else {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        };
        let seconds = (now - node.active_at).max(0.0) as u64;
        match seconds {
            0..45 => "now".into(),
            45..3600 => format!("{}m", (seconds / 60).max(1)),
            3600..86400 => format!("{}h", seconds / 3600),
            _ => format!("{}d", seconds / 86400),
        }
    }
    fn item_rows(&self, terms: &[String], rows: &mut Vec<Row>) {
        let Some(section) = self.section() else {
            return;
        };
        let mut group = None;
        for (i, item) in section.items.iter().enumerate() {
            let shown = format!("{} {} {}", item.title, item.note, item.group);
            let Some(hint) = search_hint(terms, &shown, &item.aliases) else {
                continue;
            };
            if group != Some(&item.group) {
                let count = section
                    .items
                    .iter()
                    .filter(|other| other.group == item.group)
                    .count();
                rows.push(Row::header(item.group.clone(), count.to_string()));
                group = Some(&item.group);
            }
            rows.push(Row {
                kind: Kind::Action,
                id: item.id.clone(),
                title: item.title.clone(),
                note: item.note.clone(),
                icon: item.icon.clone(),
                source: i,
                parent: String::new(),
                branch: String::new(),
                children: false,
                expanded: false,
                hint,
            });
        }
    }
    fn catalog_rows(&self, terms: &[String], rows: &mut Vec<Row>) {
        let mut group = None;
        for (i, e) in self.snapshot.catalog.iter().enumerate() {
            let shown = format!(
                "{} {} {} {} {}",
                e.title, e.description, e.group, e.category, e.interface
            );
            let Some(hint) = search_hint(terms, &shown, &e.aliases) else {
                continue;
            };
            let heading = if e.category == e.group {
                e.category.clone()
            } else {
                format!("{} › {}", e.category, e.group)
            };
            if group.as_ref() != Some(&heading) {
                rows.push(Row::header(heading.clone(), e.interface.clone()));
                group = Some(heading);
            }
            rows.push(Row {
                kind: Kind::Entry,
                id: e.id.clone(),
                title: e.title.clone(),
                note: e.description.clone(),
                icon: e.icon.clone(),
                source: i,
                parent: String::new(),
                branch: String::new(),
                children: false,
                expanded: false,
                hint,
            });
        }
    }
    fn tile_matches(&self, terms: &[String], node: &Node) -> bool {
        let Some(tile) = &node.tile else {
            return false;
        };
        terms.is_empty()
            || matches(
                terms,
                &format!(
                    "{} {} {} {} {} {}",
                    tile.title, tile.place, tile.state, tile.harness, node.backend, node.label
                ),
            )
    }
    /// Tiles in order: pinned terminals (missing ones too), then the rest,
    /// latest activity first unless the layout order is chosen.
    pub fn tiles(&self) -> Vec<TileRow> {
        let terms = self.terms();
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for (p, pin) in self.snapshot.workspace.pins.iter().enumerate() {
            let source = self
                .snapshot
                .tree
                .iter()
                .position(|n| !pin.id.is_empty() && n.id == pin.id);
            match source {
                Some(i) => {
                    if self.tile_matches(&terms, &self.snapshot.tree[i]) {
                        seen.insert(pin.id.clone());
                        rows.push(TileRow {
                            id: pin.id.clone(),
                            source: Some(i),
                            pin: Some(p),
                        });
                    }
                }
                None => {
                    if terms.is_empty()
                        || matches(
                            &terms,
                            &format!("{} {} {}", pin.label, pin.place, pin.status),
                        )
                    {
                        rows.push(TileRow {
                            id: String::new(),
                            source: None,
                            pin: Some(p),
                        });
                    }
                }
            }
        }
        if !self.pinned_only {
            let mut rest: Vec<TileRow> = self
                .snapshot
                .tree
                .iter()
                .enumerate()
                .filter(|(_, node)| !seen.contains(&node.id) && self.tile_matches(&terms, node))
                .map(|(i, node)| TileRow {
                    id: node.id.clone(),
                    source: Some(i),
                    pin: None,
                })
                .collect();
            if self.order == Order::Activity {
                let tree = &self.snapshot.tree;
                rest.sort_by_key(|t| {
                    let n = &tree[t.source.unwrap_or_default()];
                    recency(n.live(), n.active_at)
                });
            }
            rows.extend(rest);
        }
        rows
    }
    /// The exact terminal the quick actions apply to, in any Workspaces view.
    pub fn selected_terminal(&self) -> Option<String> {
        if self.page != WORKSPACES {
            return None;
        }
        match self.view {
            View::Tree => {
                let row = self.selected_row()?;
                let node = self.snapshot.tree.get(row.source)?;
                if row.kind != Kind::Node {
                    return None;
                }
                if node.tile.is_some() || node.kind == "agent session" {
                    Some(node.id.clone())
                } else {
                    None
                }
            }
            View::Tiles | View::Wall => self
                .tiles()
                .get(self.tile)
                .filter(|t| !t.id.is_empty())
                .map(|t| t.id.clone()),
            View::Focus => (!self.focus_target.is_empty()).then(|| self.focus_target.clone()),
        }
    }
    /// The terminal node behind an ID; agent sessions resolve to their pane.
    pub fn terminal_node(&self, id: &str) -> Option<&Node> {
        let node = self.snapshot.tree.iter().find(|n| n.id == id)?;
        if node.tile.is_some() {
            return Some(node);
        }
        self.snapshot
            .tree
            .iter()
            .find(|n| n.id == node.parent && n.tile.is_some())
    }
    pub fn select_tile(&mut self, index: usize) {
        let count = self.tiles().len();
        let next = index.min(count.saturating_sub(1));
        if next != self.tile {
            self.moved_at = Some(Instant::now());
        }
        self.tile = next;
    }
    pub fn move_tile(&mut self, delta: isize) {
        let count = self.tiles().len() as isize;
        if count == 0 {
            return;
        }
        let next = (self.tile as isize + delta).clamp(0, count - 1);
        self.select_tile(next as usize);
    }
    /// Switch Workspaces view, carrying the selected terminal across.
    pub fn set_view(&mut self, view: View) {
        if view == self.view {
            return;
        }
        let current = self.selected_terminal();
        if view == View::Focus {
            self.previous = self.view;
            self.focus_target = current.clone().unwrap_or_default();
        }
        self.view = view;
        self.paged_at = Some(Instant::now());
        if let Some(id) = current {
            self.reveal(&id);
        }
    }
    /// Select a terminal by ID in the current view.
    pub fn reveal(&mut self, id: &str) {
        match self.view {
            View::Tiles | View::Wall => {
                let id = self
                    .terminal_node(id)
                    .map(|n| n.id.clone())
                    .unwrap_or_default();
                if let Some(i) = self.tiles().iter().position(|t| t.id == id) {
                    self.select_tile(i);
                }
            }
            View::Tree => {
                let mut key = id.to_owned();
                while let Some(n) = self.snapshot.tree.iter().find(|n| n.id == key) {
                    self.collapsed.remove(&n.parent);
                    key = n.parent.clone();
                }
                self.select_id(id);
            }
            View::Focus => self.focus_target = id.to_owned(),
        }
    }
    /// Nearest selectable row, preferring the direction of travel.
    fn snap(rows: &[Row], index: usize, forward: bool) -> usize {
        let selectable = |i: &usize| rows[*i].kind != Kind::Header;
        let index = index.min(rows.len().saturating_sub(1));
        let found = if forward {
            (index..rows.len())
                .find(selectable)
                .or_else(|| (0..index).rev().find(selectable))
        } else {
            (0..=index)
                .rev()
                .find(selectable)
                .or_else(|| (index + 1..rows.len()).find(selectable))
        };
        found.unwrap_or(0)
    }
    pub fn selected_row(&self) -> Option<Row> {
        self.rows()
            .get(self.selected)
            .filter(|r| r.kind != Kind::Header)
            .cloned()
    }
    pub fn navigate(&mut self, page: usize) {
        if page != self.page {
            self.paged_at = Some(Instant::now());
        }
        self.page = page;
        self.switching = false;
        self.offset = 0;
        self.filter.clear();
        self.searching = false;
        self.select(0, true);
    }
    pub fn select(&mut self, index: usize, forward: bool) {
        let rows = self.rows();
        let next = if rows.is_empty() {
            0
        } else {
            Self::snap(&rows, index, forward)
        };
        if next != self.selected {
            self.moved_at = Some(Instant::now());
        }
        self.selected = next;
    }
    pub fn move_by(&mut self, delta: isize) {
        let last = self.rows().len().saturating_sub(1) as isize;
        let target = (self.selected as isize + delta).clamp(0, last.max(0));
        self.select(target as usize, delta >= 0);
    }
    pub fn select_id(&mut self, id: &str) -> bool {
        match self.rows().iter().position(|r| r.id == id) {
            Some(i) => {
                self.select(i, true);
                true
            }
            None => false,
        }
    }
    pub fn update(&mut self, mut snapshot: Snapshot) {
        let old = self.selected_row();
        let mut ancestry = Vec::new();
        if let Some(ref row) = old {
            let mut key = row.parent.clone();
            while let Some(n) = self.snapshot.tree.iter().find(|n| n.id == key) {
                ancestry.push(n.id.clone());
                key = n.parent.clone();
            }
        }
        if snapshot.catalog.is_empty() {
            snapshot.catalog = std::mem::take(&mut self.snapshot.catalog);
        }
        let old_tile = self.tiles().get(self.tile).map(|t| (t.id.clone(), t.pin));
        let focus = std::mem::take(&mut snapshot.focus);
        let apply = std::mem::take(&mut snapshot.workspace.apply);
        self.snapshot = snapshot;
        let tiles = self.tiles();
        self.tile = old_tile
            .and_then(|(id, pin)| {
                tiles
                    .iter()
                    .position(|t| (!id.is_empty() && t.id == id) || (id.is_empty() && t.pin == pin))
            })
            .unwrap_or(self.tile.min(tiles.len().saturating_sub(1)));
        // Previews of terminals that are gone are dropped, never reassigned.
        self.previews
            .retain(|id, _| tiles.iter().any(|t| &t.id == id));
        if let Some(sizes) = apply.sizes {
            self.sizes = sizes.clamped();
        }
        if let Some(look) = crate::backdrop::Look::parse(&apply.backdrop) {
            self.look = look;
        }
        match apply.saver.as_str() {
            "on" => self.saver.enabled = true,
            "off" => self.saver.enabled = false,
            _ => {}
        }
        if let Some(order) = Order::parse(&apply.order) {
            self.order = order;
        }
        self.prefs.apply(&apply.switches);
        if apply.switches.contains_key("motion") {
            self.theme.reduced_motion = self.still || !self.prefs.motion;
        }
        match apply.switches.get("keys").and_then(Value::as_str) {
            Some("on") => self.hide_keys = false,
            Some("off") => self.hide_keys = true,
            _ => {}
        }
        if let Some(view) = View::parse(&apply.mode) {
            let view = if view == View::Focus && apply.selected.is_empty() {
                View::Tiles
            } else {
                view
            };
            if view == View::Focus {
                self.previous = View::Tiles;
                self.focus_target = apply.selected.clone();
            }
            self.view = view;
            if !apply.selected.is_empty() {
                self.reveal(&apply.selected);
            }
        }
        if !focus.is_empty() {
            // Reveal the focused item: clear the filter and unfold its ancestors.
            self.filter.clear();
            self.searching = false;
            let mut key = focus.clone();
            while let Some(n) = self.snapshot.tree.iter().find(|n| n.id == key) {
                self.collapsed.remove(&n.parent);
                key = n.parent.clone();
            }
            if self.select_id(&focus) {
                return;
            }
        }
        let rows = self.rows();
        let index = old
            .and_then(|r| rows.iter().position(|n| n.id == r.id))
            .or_else(|| {
                ancestry
                    .iter()
                    .find_map(|id| rows.iter().position(|n| &n.id == id))
            })
            .unwrap_or(0);
        self.selected = if rows.is_empty() {
            0
        } else {
            Self::snap(&rows, index, true)
        };
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    fn fixture() -> Ui {
        let mut ui = Ui {
            page: WORKSPACES,
            ..Ui::default()
        };
        ui.snapshot.tree = vec![
            Node {
                id: "server".into(),
                label: "Server".into(),
                kind: "server".into(),
                children: true,
                ..Node::default()
            },
            Node {
                id: "pane".into(),
                parent: "server".into(),
                label: "Worker".into(),
                kind: "pane".into(),
                ..Node::default()
            },
        ];
        ui.snapshot.sections = (0..PAGES)
            .map(|i| Section {
                title: format!("Page {i}"),
                ..Section::default()
            })
            .collect();
        ui.snapshot.sections[3].items = vec![
            Item {
                id: "telegram.route".into(),
                group: "Telegram".into(),
                title: "Route a chat to a terminal".into(),
                aliases: vec!["bind".into()],
                ..Item::default()
            },
            Item {
                id: "telegram.connect".into(),
                group: "Voice".into(),
                title: "Connect Telegram".into(),
                ..Item::default()
            },
        ];
        ui.snapshot.catalog = vec![Entry {
            id: "telegram:jobs".into(),
            category: "Telegram".into(),
            group: "Sessions & jobs".into(),
            title: "/jobs".into(),
            description: "list jobs".into(),
            aliases: vec!["/fleet".into()],
            ..Entry::default()
        }];
        ui
    }
    fn nodes(ui: &Ui) -> Vec<String> {
        ui.rows()
            .into_iter()
            .filter(|r| r.kind == Kind::Node)
            .map(|r| r.id)
            .collect()
    }
    #[test]
    fn filtering_keeps_ancestors_and_overrides_collapse() {
        let mut ui = fixture();
        ui.collapsed.insert("server".into());
        assert_eq!(nodes(&ui), ["server"]);
        ui.filter = "worker".into();
        assert_eq!(nodes(&ui), ["server", "pane"]);
    }
    #[test]
    fn vanished_selection_moves_to_parent() {
        let mut ui = fixture();
        assert!(ui.select_id("pane"));
        let snapshot = Snapshot {
            tree: vec![ui.snapshot.tree[0].clone()],
            sections: std::mem::take(&mut ui.snapshot.sections),
            ..Snapshot::default()
        };
        ui.update(snapshot);
        assert_eq!(ui.selected_row().unwrap().id, "server");
    }
    #[test]
    fn group_headers_are_never_selected() {
        let mut ui = fixture();
        ui.navigate(3);
        let rows = ui.rows();
        assert_eq!(rows[0].kind, Kind::Header);
        assert_eq!(ui.selected_row().unwrap().id, "telegram.route");
        ui.move_by(1);
        // The "Voice" header is skipped.
        assert_eq!(ui.selected_row().unwrap().id, "telegram.connect");
        ui.move_by(-1);
        assert_eq!(ui.selected_row().unwrap().id, "telegram.route");
        ui.move_by(-10);
        assert_eq!(ui.selected_row().unwrap().id, "telegram.route");
        let headers: Vec<_> = rows.iter().filter(|r| r.kind == Kind::Header).collect();
        assert_eq!(headers.len(), 2);
    }
    #[test]
    fn alias_search_finds_canonical_entry_once_with_hint() {
        let mut ui = fixture();
        ui.navigate(COMMANDS);
        ui.filter = "fleet".into();
        let entries: Vec<Row> = ui
            .rows()
            .into_iter()
            .filter(|r| r.kind == Kind::Entry)
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "/jobs");
        assert_eq!(entries[0].hint, "/fleet");
        ui.filter = "jobs".into();
        assert!(ui.rows().iter().all(|r| r.hint.is_empty()));
        ui.navigate(3);
        ui.filter = "bind".into();
        ui.select(0, true);
        assert_eq!(ui.selected_row().unwrap().hint, "bind");
    }
    #[test]
    fn every_search_term_must_match() {
        let mut ui = fixture();
        ui.navigate(3);
        ui.filter = "connect telegram".into();
        let found: Vec<_> = ui
            .rows()
            .into_iter()
            .filter(|r| r.kind == Kind::Action)
            .map(|r| r.id)
            .collect();
        assert_eq!(found, ["telegram.connect"]);
    }
    #[test]
    fn search_combines_alias_with_category_or_title() {
        let mut ui = fixture();
        ui.navigate(COMMANDS);
        ui.filter = "telegram fleet".into();
        ui.select(0, true);
        assert_eq!(ui.selected_row().unwrap().id, "telegram:jobs");
        assert_eq!(ui.selected_row().unwrap().hint, "/fleet");
        ui.filter = "telegram fleet nonexistent".into();
        assert!(ui.rows().is_empty());
        ui.navigate(3);
        ui.filter = "bind terminal".into();
        ui.select(0, true);
        assert_eq!(ui.selected_row().unwrap().id, "telegram.route");
        assert_eq!(ui.selected_row().unwrap().hint, "bind");
    }
    #[test]
    fn remembered_switches_apply_and_unknown_values_are_ignored() {
        let mut ui = fixture();
        let apply: Apply = serde_json::from_value(serde_json::json!({
            "mode": "tree", "details": "off", "pinned": "maybe", "keys": "off",
            "motion": "off", "sizes": {"sidebar": 0}, "other": 3,
        }))
        .unwrap();
        assert_eq!(apply.mode, "tree");
        let mut snapshot = Snapshot {
            sections: ui.snapshot.sections.clone(),
            ..Snapshot::default()
        };
        snapshot.workspace.apply = apply;
        ui.update(snapshot);
        assert!(!ui.prefs.details && ui.prefs.pinned);
        assert!(ui.hide_keys && ui.theme.reduced_motion);
    }
    pub fn tiles_fixture() -> Ui {
        let mut ui = fixture();
        let pane = |id: &str, title: &str| Node {
            id: id.into(),
            parent: "server".into(),
            label: title.into(),
            kind: "pane".into(),
            actionable: true,
            tile: Some(Tile {
                title: title.into(),
                place: format!("project / {id}"),
                ..Tile::default()
            }),
            ..Node::default()
        };
        ui.snapshot.tree = vec![
            ui.snapshot.tree[0].clone(),
            pane("a", "alpha"),
            pane("b", "beta"),
            Node {
                id: "b-agent".into(),
                parent: "b".into(),
                label: "codex".into(),
                kind: "agent session".into(),
                ..Node::default()
            },
        ];
        ui.snapshot.workspace.pins = vec![
            Pin {
                id: "b".into(),
                label: "beta".into(),
                status: "live".into(),
                ..Pin::default()
            },
            Pin {
                label: "gone".into(),
                status: "missing".into(),
                reason: "Terminal %9 no longer exists".into(),
                key: "k".into(),
                ..Pin::default()
            },
        ];
        ui.snapshot.terminal_actions = ["a:terminal.attach", "w:terminal.watch", "p:terminal.pin"]
            .iter()
            .map(|spec| {
                let (quick, id) = spec.split_once(':').unwrap();
                QuickAction {
                    id: id.into(),
                    quick: quick.into(),
                    short: id.into(),
                    ..QuickAction::default()
                }
            })
            .collect();
        ui
    }
    /// Puts `a` left of `b` in one tab: the server node's layout.
    pub fn split(ui: &mut Ui) {
        let pane = |id: &str, x, w| LayoutPane {
            id: id.into(),
            x,
            y: 0,
            w,
            h: 20,
            focused: false,
        };
        ui.snapshot.tree[0].layout = Some(Layout {
            width: 100,
            height: 20,
            zoomed: false,
            panes: vec![pane("a", 0, 60), pane("b", 60, 40)],
        });
    }
    #[test]
    fn a_terminal_knows_its_tab_and_neighbors() {
        let mut ui = tiles_fixture();
        assert!(ui.tab_layout("a").is_none());
        split(&mut ui);
        assert_eq!(ui.tab_layout("a").unwrap().panes.len(), 2);
        assert_eq!(ui.neighbor("a", 1, 0).as_deref(), Some("b"));
        assert_eq!(ui.neighbor("a", -1, 0), None);
        // An agent session finds its terminal's neighbors.
        assert_eq!(ui.neighbor("b-agent", -1, 0).as_deref(), Some("a"));
        // One wide pane on top of three: the most shared border wins.
        let pane = |id: &str, x, y, w, h| LayoutPane {
            id: id.into(),
            x,
            y,
            w,
            h,
            focused: false,
        };
        let ids = ["top", "big", "right1", "right2"];
        for id in ids {
            ui.snapshot.tree.push(Node {
                id: id.into(),
                parent: "server".into(),
                kind: "pane".into(),
                tile: Some(Tile::default()),
                ..Node::default()
            });
        }
        ui.snapshot.tree[0].layout = Some(Layout {
            width: 100,
            height: 40,
            zoomed: false,
            panes: vec![
                pane("top", 0, 0, 100, 10),
                pane("big", 0, 10, 70, 30),
                pane("right1", 70, 10, 30, 15),
                pane("right2", 70, 25, 30, 15),
            ],
        });
        assert_eq!(ui.neighbor("top", 0, 1).as_deref(), Some("big"));
        assert_eq!(ui.neighbor("right2", 0, -1).as_deref(), Some("right1"));
        assert_eq!(ui.neighbor("right1", 0, -1).as_deref(), Some("top"));
        assert_eq!(ui.neighbor("right2", -1, 0).as_deref(), Some("big"));
        assert_eq!(ui.neighbor("big", 0, 1), None);
    }
    fn tile_ids(ui: &Ui) -> Vec<String> {
        ui.tiles().into_iter().map(|t| t.id).collect()
    }
    /// Panes `a` to `e` under two projects, with activity times.
    fn busy_fixture() -> Ui {
        let mut ui = fixture();
        ui.snapshot.observed_at = 10_000.0;
        let group = |id: &str| Node {
            id: id.into(),
            parent: "server".into(),
            kind: "workspace".into(),
            children: true,
            ..Node::default()
        };
        let pane = |id: &str, parent: &str, at: f64, activity: &str| Node {
            id: id.into(),
            parent: parent.into(),
            kind: "pane".into(),
            state: "idle".into(),
            tile: Some(Tile::default()),
            active_at: at,
            activity: activity.into(),
            ..Node::default()
        };
        ui.snapshot.tree = vec![
            ui.snapshot.tree[0].clone(),
            group("one"),
            pane("a", "one", 9_000.0, "input"),
            pane("b", "one", 0.0, ""),
            group("two"),
            pane("c", "two", 9_990.0, "output"),
            pane("d", "two", 10_000.0, "blocked"),
            pane("e", "two", 4_000.0, "status"),
        ];
        ui
    }
    #[test]
    fn switcher_searches_parent_names_ranks_pins_then_recent_and_skips_stale_pins() {
        let mut ui = busy_fixture();
        ui.switching = true;
        ui.snapshot.tree[1].label = "Project Alpha".into();
        ui.snapshot.tree[4].label = "Project Beta".into();
        ui.snapshot.tree[2].usage = WorkspaceUsage {
            visits: 3,
            seconds: 60.0,
            last_selected: 100.0,
        };
        ui.snapshot.tree[5].usage = WorkspaceUsage {
            visits: 5,
            seconds: 120.0,
            last_selected: 200.0,
        };
        ui.snapshot.workspace.pins = vec![
            Pin {
                id: "missing".into(),
                status: "missing".into(),
                ..Pin::default()
            },
            Pin {
                id: "b".into(),
                status: "live".into(),
                ..Pin::default()
            },
        ];
        let ids = || {
            ui.rows()
                .into_iter()
                .filter(|r| r.kind == Kind::Node)
                .map(|r| r.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(&ids()[..3], ["b", "c", "a"]);
        ui.filter = "alpha".into();
        let rows = ui.rows();
        assert_eq!(rows.iter().filter(|r| r.kind == Kind::Node).count(), 2);
        assert!(rows.iter().any(|r| r.note.contains("3 visits")));
        ui.filter = "no-match".into();
        assert!(ui.rows().iter().all(|r| r.kind == Kind::Header));
    }
    #[test]
    fn activity_order_puts_live_terminals_then_the_latest_first() {
        let mut ui = busy_fixture();
        assert_eq!(tile_ids(&ui), ["d", "c", "a", "e", "b"]);
        ui.order = Order::Layout;
        assert_eq!(tile_ids(&ui), ["a", "b", "c", "d", "e"]);
        // Pins keep their place in front, whatever the order.
        ui.order = Order::Activity;
        ui.snapshot.workspace.pins = vec![Pin {
            id: "b".into(),
            status: "live".into(),
            ..Pin::default()
        }];
        assert_eq!(tile_ids(&ui), ["b", "d", "c", "a", "e"]);
    }
    #[test]
    fn tree_siblings_follow_the_latest_activity_below_them() {
        let mut ui = busy_fixture();
        let order: Vec<(String, String)> = ui
            .tree_order()
            .into_iter()
            .map(|(i, branch)| (ui.snapshot.tree[i].id.clone(), branch))
            .collect();
        let ids: Vec<&str> = order.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["server", "two", "d", "c", "e", "one", "a", "b"]);
        let branch = |id: &str| order.iter().find(|(n, _)| n == id).unwrap().1.clone();
        assert_eq!(branch("two"), "   ├─ ");
        assert_eq!(branch("e"), "   │  └─ ");
        assert_eq!(branch("b"), "      └─ ");
        // The rows show how long ago each terminal was active.
        let rows = ui.rows();
        let note = |id: &str| rows.iter().find(|r| r.id == id).unwrap().note.clone();
        assert_eq!(note("a"), "idle · 16m");
        assert_eq!(note("d"), "idle", "live terminals need no age");
        assert_eq!(note("b"), "idle");
        ui.order = Order::Layout;
        let ids: Vec<String> = ui.rows().into_iter().skip(1).map(|r| r.id).collect();
        assert_eq!(ids, ["server", "one", "a", "b", "two", "c", "d", "e"]);
    }
    #[test]
    fn ages_read_short() {
        let mut ui = busy_fixture();
        let mut node = Node {
            active_at: 9_980.0,
            ..Node::default()
        };
        assert_eq!(ui.ago(&node), "now");
        node.active_at = 10_000.0 - 7_300.0;
        assert_eq!(ui.ago(&node), "2h");
        ui.snapshot.observed_at = 400_000.0;
        node.active_at = 400_000.0 - 3.0 * 86_400.0;
        assert_eq!(ui.ago(&node), "3d");
        node.active_at = 0.0;
        assert_eq!(ui.ago(&node), "");
        ui.snapshot.workspace.apply.order = "layout".into();
        let snapshot = std::mem::take(&mut ui.snapshot);
        ui.update(snapshot);
        assert_eq!(ui.order, Order::Layout);
    }
    #[test]
    fn tiles_list_pins_first_and_keep_missing_pins_visible() {
        let mut ui = tiles_fixture();
        assert_eq!(tile_ids(&ui), ["b", "", "a"]);
        ui.pinned_only = true;
        assert_eq!(tile_ids(&ui), ["b", ""]);
        ui.pinned_only = false;
        ui.filter = "alpha".into();
        assert_eq!(tile_ids(&ui), ["a"]);
        ui.filter = "gone".into();
        assert_eq!(tile_ids(&ui), [""]);
    }
    #[test]
    fn selected_terminal_is_exact_in_every_view() {
        let mut ui = tiles_fixture();
        assert!(ui.select_id("b-agent"));
        assert_eq!(ui.selected_terminal().as_deref(), Some("b-agent"));
        assert!(ui.select_id("server"));
        assert_eq!(ui.selected_terminal(), None);
        ui.view = View::Tiles;
        ui.select_tile(1);
        // A missing pin is never a target.
        assert_eq!(ui.selected_terminal(), None);
        ui.select_tile(2);
        assert_eq!(ui.selected_terminal().as_deref(), Some("a"));
        ui.set_view(View::Focus);
        assert_eq!((ui.focus_target.as_str(), ui.previous), ("a", View::Tiles));
        assert_eq!(ui.selected_terminal().as_deref(), Some("a"));
    }
    #[test]
    fn switching_views_carries_the_selected_terminal() {
        let mut ui = tiles_fixture();
        assert!(ui.select_id("a"));
        ui.set_view(View::Tiles);
        assert_eq!(ui.tiles()[ui.tile].id, "a");
        ui.select_tile(0);
        ui.set_view(View::Tree);
        assert_eq!(ui.selected_row().unwrap().id, "b");
        // An agent session resolves to its terminal's tile.
        assert!(ui.select_id("b-agent"));
        ui.set_view(View::Tiles);
        assert_eq!(ui.tiles()[ui.tile].id, "b");
    }
    #[test]
    fn refresh_keeps_tile_selection_and_drops_gone_previews() {
        let mut ui = tiles_fixture();
        ui.view = View::Tiles;
        ui.select_tile(2);
        ui.previews
            .insert("a".into(), Preview::new("alpha screen", ""));
        ui.previews.insert("old".into(), Preview::new("stale", ""));
        let mut snapshot = Snapshot {
            tree: ui.snapshot.tree.clone(),
            sections: ui.snapshot.sections.clone(),
            ..Snapshot::default()
        };
        snapshot.workspace.pins = ui.snapshot.workspace.pins.clone();
        snapshot.tree.swap(1, 2);
        ui.update(snapshot);
        assert_eq!(ui.tiles()[ui.tile].id, "a");
        assert!(ui.previews.contains_key("a"));
        assert!(!ui.previews.contains_key("old"));
    }
    #[test]
    fn saved_view_is_applied_once_and_focus_needs_a_terminal() {
        let mut ui = tiles_fixture();
        let mut snapshot = Snapshot {
            tree: ui.snapshot.tree.clone(),
            sections: ui.snapshot.sections.clone(),
            ..Snapshot::default()
        };
        snapshot.workspace.apply = Apply {
            mode: "focus".into(),
            selected: String::new(),
            sizes: None,
            ..Apply::default()
        };
        ui.update(snapshot);
        assert_eq!(ui.view, View::Tiles);
        let mut snapshot = Snapshot {
            tree: ui.snapshot.tree.clone(),
            sections: ui.snapshot.sections.clone(),
            ..Snapshot::default()
        };
        snapshot.workspace.apply = Apply {
            mode: "focus".into(),
            selected: "a".into(),
            sizes: None,
            ..Apply::default()
        };
        ui.update(snapshot);
        assert_eq!((ui.view, ui.focus_target.as_str()), (View::Focus, "a"));
        assert!(
            ui.embed.is_none(),
            "restoring a view never connects to a terminal"
        );
    }
    #[test]
    fn sizes_stay_within_limits_and_follow_the_pointer() {
        let mut sizes = Sizes::default();
        assert!(sizes.set(Split::Details, 500));
        assert_eq!(sizes.details, 80);
        assert!(!sizes.set(Split::Details, 90), "already at the limit");
        assert!(sizes.reset(Split::Details));
        assert_eq!(sizes, Sizes::default());
        // A sidebar too narrow to read folds to its icon rail.
        assert!(sizes.set(Split::Sidebar, 5));
        assert_eq!(sizes.sidebar, 0);
        sizes.set(Split::Sidebar, 13);
        assert_eq!(sizes.sidebar, 16);
        sizes.reset(Split::Sidebar);
        let wild = Sizes {
            sidebar: 99,
            details: 1000,
            screen: 0,
            tiles: 50,
        };
        assert_eq!(
            wild.clamped(),
            Sizes {
                sidebar: 40,
                details: 80,
                screen: 20,
                tiles: 100
            }
        );
        let handle = |split, origin, length| Handle {
            area: ratatui::layout::Rect::default(),
            split,
            origin,
            length,
        };
        // The sidebar's border lands on the pointer's column.
        assert_eq!(handle(Split::Sidebar, 0, 160).value(29, 3), 30);
        // Details is measured from the right edge of a 100-column page.
        assert_eq!(handle(Split::Details, 150, 100).value(90, 3), 60);
        // The screen grows down from its label row.
        assert_eq!(handle(Split::Screen, 10, 40).value(0, 30), 50);
    }
    #[test]
    fn saved_sizes_are_applied_within_limits() {
        let mut ui = tiles_fixture();
        let mut snapshot = Snapshot {
            tree: ui.snapshot.tree.clone(),
            sections: ui.snapshot.sections.clone(),
            ..Snapshot::default()
        };
        snapshot.workspace.apply = serde_json::from_str(
            r#"{"mode": "tree", "selected": "", "sizes": {"sidebar": 30, "details": 95, "screen": 70, "tiles": 150}}"#,
        )
        .unwrap();
        ui.update(snapshot);
        assert_eq!(
            ui.sizes,
            Sizes {
                sidebar: 30,
                details: 80,
                screen: 70,
                tiles: 150
            }
        );
        // A view without sizes keeps the current ones.
        let mut snapshot = Snapshot {
            tree: ui.snapshot.tree.clone(),
            sections: ui.snapshot.sections.clone(),
            ..Snapshot::default()
        };
        snapshot.workspace.apply =
            serde_json::from_str(r#"{"mode": "tiles", "sizes": null}"#).unwrap();
        ui.update(snapshot);
        assert_eq!(ui.sizes.sidebar, 30);
    }
    #[test]
    fn oversized_previews_keep_their_newest_lines() {
        let line = format!("\x1b[32m{}\x1b[0m", "x".repeat(1000));
        let text = (0..400)
            .map(|i| format!("{i:03}{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.len() > Preview::BYTES);
        let preview = Preview::new(&text, "");
        let screen = preview.screen.screen();
        let last = screen
            .contents()
            .lines()
            .last()
            .unwrap_or_default()
            .to_owned();
        assert!(last.starts_with("399"), "{}", &last[..10.min(last.len())]);
        // The first kept line starts cleanly: no half of a color code.
        assert!(
            screen
                .contents()
                .lines()
                .next()
                .unwrap()
                .starts_with(char::is_numeric)
        );
    }
    #[test]
    fn facts_parse_with_or_without_a_tone() {
        let node: Node = serde_json::from_str(
            r#"{"fields": [["Server", "demo"], ["State", "idle", "state"], []]}"#,
        )
        .unwrap();
        assert_eq!(node.fields[0], Field::new("Server", "demo", ""));
        assert_eq!(node.fields[1], Field::new("State", "idle", "state"));
        assert_eq!(node.fields[2], Field::default());
    }
    #[test]
    fn preview_text_is_parsed_once_and_bottom_lines_are_known() {
        let preview = Preview::new("one\n\x1b[31mtwo\x1b[0m\nthree", "");
        assert_eq!(preview.lines, 3);
        assert_eq!(preview.screen.screen().contents(), "one\ntwo\nthree");
        assert_eq!(
            preview.screen.screen().cell(1, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
    }
    #[test]
    fn focus_reveals_and_selects_item_and_catalog_is_retained() {
        let mut ui = fixture();
        ui.collapsed.insert("server".into());
        ui.filter = "unrelated".into();
        let snapshot = Snapshot {
            tree: ui.snapshot.tree.clone(),
            sections: ui.snapshot.sections.clone(),
            focus: "pane".into(),
            ..Snapshot::default()
        };
        ui.update(snapshot);
        assert!(ui.filter.is_empty());
        assert!(!ui.collapsed.contains("server"));
        assert_eq!(ui.selected_row().unwrap().id, "pane");
        assert_eq!(ui.snapshot.catalog.len(), 1);
    }
    #[test]
    fn focus_resize_geometry_handles_horizontal_and_vertical_edges_and_bounds() {
        let a = LayoutPane {
            id: "a".into(),
            x: 0,
            y: 0,
            w: 60,
            h: 20,
            focused: false,
        };
        let b = LayoutPane {
            id: "b".into(),
            x: 60,
            y: 0,
            w: 40,
            h: 20,
            focused: false,
        };
        let mut layout = Layout {
            width: 100,
            height: 20,
            zoomed: false,
            panes: vec![a, b],
        };
        let screen = ratatui::layout::Rect::new(5, 3, 100, 20);
        let edges = layout.dividers(screen);
        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert!(edge.horizontal);
        assert_eq!((&*edge.leading, &*edge.trailing), ("a", "b"));
        assert_eq!(edge.cells((60, 10), (65, 10)), 5);
        assert_eq!(
            edge.cells((60, 10), (65535, 10)),
            36,
            "retain at least four columns in the neighbor"
        );
        layout.panes[0].w = 100;
        layout.panes[0].h = 10;
        layout.panes[1].x = 0;
        layout.panes[1].y = 10;
        layout.panes[1].w = 100;
        layout.panes[1].h = 10;
        let edges = layout.dividers(screen);
        assert_eq!(edges.len(), 1);
        assert!(!edges[0].horizontal);
        assert_eq!(
            edges[0].cells((10, 10), (10, 65535)),
            7,
            "retain at least three rows in the neighbor"
        );
        layout.zoomed = true;
        assert!(layout.dividers(screen).is_empty());
    }
}
