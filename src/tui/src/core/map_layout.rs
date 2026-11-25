//! Collision-free BSP presentation geometry, independent of terminal runtime.
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    pub id: String,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
impl Card {
    pub fn valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 4096
            && !self.id.chars().any(char::is_control)
            && (1..=1000).contains(&self.width)
            && (1..=1000).contains(&self.height)
            && u32::from(self.x) + u32::from(self.width) <= 1000
            && u32::from(self.y) + u32::from(self.height) <= 1000
    }
}
pub fn validate(cards: &[Card]) -> Result<(), String> {
    if cards.len() > 512
        || cards
            .iter()
            .enumerate()
            .any(|(i, c)| !c.valid() || cards[..i].iter().any(|p| p.id == c.id))
    {
        return Err("Invalid map geometry or duplicate terminal; at most 512 saved panes".into());
    }
    Ok(())
}
/// Per-tab geometry fingerprints let saved presentation layouts distinguish a
/// local edit from a later native-server change, including after a restart.
pub fn native_baselines(
    value: &serde_json::Value,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let entries = value.as_object().ok_or("Invalid native Tetris baselines")?;
    if entries.len() > 512 {
        return Err("Too many native Tetris baselines".into());
    }
    entries
        .iter()
        .map(|(key, value)| {
            let hash = value.as_str().ok_or("Invalid native Tetris baseline")?;
            if key.is_empty()
                || key.len() > 4096
                || key.chars().any(char::is_control)
                || hash.len() != 64
                || !hash.bytes().all(|c| c.is_ascii_hexdigit())
            {
                return Err("Invalid native Tetris baseline".into());
            }
            Ok((key.clone(), hash.to_owned()))
        })
        .collect()
}
#[derive(Clone, Debug, PartialEq)]
pub enum Tiling {
    Pane(String),
    Split {
        horizontal: bool,
        ratio: f64,
        first: Box<Self>,
        second: Box<Self>,
    },
}
#[derive(Clone, Debug)]
pub struct Boundary {
    pub path: Vec<bool>,
    pub area: Rect,
    pub horizontal: bool,
    pub position: u16,
}
impl Boundary {
    pub fn hit(&self, x: u16, y: u16) -> bool {
        if self.horizontal {
            x.abs_diff(self.position) <= 1
                && y > self.area.y
                && y < self.area.bottom().saturating_sub(1)
        } else {
            y.abs_diff(self.position) <= 1
                && x > self.area.x
                && x < self.area.right().saturating_sub(1)
        }
    }
}
#[derive(Default)]
pub struct Projection {
    pub panes: Vec<(String, Rect)>,
    pub boundaries: Vec<Boundary>,
}
impl Tiling {
    /// Recover the split hierarchy from a complete, nonoverlapping backend layout.
    pub fn from_rects(area: Rect, panes: &[(String, Rect)]) -> Option<Self> {
        if panes.is_empty()
            || panes.len() > 512
            || !super::geometry::unique_ids(panes.iter().map(|(id, _)| id.as_str()))
        {
            return None;
        }
        let relative = panes
            .iter()
            .map(|(_, r)| {
                Some(super::geometry::Rect {
                    x: r.x.checked_sub(area.x)?,
                    y: r.y.checked_sub(area.y)?,
                    w: r.width,
                    h: r.height,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        super::geometry::Geometry::new(area.width, area.height, relative)?;
        if panes
            .iter()
            .map(|(_, r)| u32::from(r.width) * u32::from(r.height))
            .sum::<u32>()
            != u32::from(area.width) * u32::from(area.height)
        {
            return None;
        }
        Self::partition(area, panes)
    }
    fn partition(area: Rect, panes: &[(String, Rect)]) -> Option<Self> {
        if panes.len() == 1 {
            return (panes[0].1 == area).then(|| Self::Pane(panes[0].0.clone()));
        }
        // Prefer a balanced legal cut; every child remains an exact partition.
        let mut candidates = Vec::new();
        for horizontal in [true, false] {
            for (_, r) in panes {
                let cut = if horizontal { r.right() } else { r.bottom() };
                let (start, end) = if horizontal {
                    (area.x, area.right())
                } else {
                    (area.y, area.bottom())
                };
                if cut <= start || cut >= end {
                    continue;
                }
                let mut left = 0usize;
                let mut right = 0usize;
                let mut crossed = false;
                for (_, pane) in panes {
                    let (a, b) = if horizontal {
                        (pane.x, pane.right())
                    } else {
                        (pane.y, pane.bottom())
                    };
                    if b <= cut {
                        left += 1;
                    } else if a >= cut {
                        right += 1;
                    } else {
                        crossed = true;
                        break;
                    }
                }
                if !crossed && left > 0 && right > 0 {
                    candidates.push((left.abs_diff(right), horizontal, cut));
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        for (_, horizontal, cut) in candidates {
            let (left, right): (Vec<_>, Vec<_>) = panes.iter().cloned().partition(|(_, r)| {
                if horizontal {
                    r.right() <= cut
                } else {
                    r.bottom() <= cut
                }
            });
            let (a, b) = split_rect(area, horizontal, cut);
            if let Some((first, second)) = Self::partition(a, &left).zip(Self::partition(b, &right))
            {
                let (start, length) = if horizontal {
                    (area.x, area.width)
                } else {
                    (area.y, area.height)
                };
                return Some(Self::Split {
                    horizontal,
                    ratio: f64::from(cut - start) / f64::from(length),
                    first: Box::new(first),
                    second: Box::new(second),
                });
            }
        }
        None
    }
    pub fn from_cards(cards: &[Card]) -> Option<Self> {
        validate(cards).ok()?;
        let panes = cards
            .iter()
            .map(|c| (c.id.clone(), Rect::new(c.x, c.y, c.width, c.height)))
            .collect::<Vec<_>>();
        Self::from_rects(Rect::new(0, 0, 1000, 1000), &panes)
    }
    /// Fallback when the backend supplies no layout. Fit every identity exactly once.
    pub fn balanced(ids: &[String], area: Rect) -> Option<Self> {
        if ids.is_empty() || ids.len() > usize::from(area.width) * usize::from(area.height) {
            return None;
        }
        if ids.len() == 1 {
            return Some(Self::Pane(ids[0].clone()));
        }
        let horizontal = (u32::from(area.width) >= u32::from(area.height) * 2 && area.width > 1)
            || area.height == 1;
        let length = if horizontal { area.width } else { area.height };
        let cut = length / 2;
        let (a, b) = split_rect(
            area,
            horizontal,
            if horizontal {
                area.x + cut
            } else {
                area.y + cut
            },
        );
        let capacity = |r: Rect| usize::from(r.width) * usize::from(r.height);
        let count = (ids.len() / 2).clamp(
            ids.len().saturating_sub(capacity(b)).max(1),
            capacity(a).min(ids.len() - 1),
        );
        Some(Self::Split {
            horizontal,
            ratio: f64::from(cut) / f64::from(length),
            first: Box::new(Self::balanced(&ids[..count], a)?),
            second: Box::new(Self::balanced(&ids[count..], b)?),
        })
    }
    fn minimum(&self, leaf: (u16, u16)) -> (u16, u16) {
        match self {
            Self::Pane(_) => leaf,
            Self::Split {
                horizontal,
                first,
                second,
                ..
            } => {
                let a = first.minimum(leaf);
                let b = second.minimum(leaf);
                if *horizontal {
                    (a.0.saturating_add(b.0), a.1.max(b.1))
                } else {
                    (a.0.max(b.0), a.1.saturating_add(b.1))
                }
            }
        }
    }
    pub fn project(&self, area: Rect, preferred: (u16, u16)) -> Option<Projection> {
        let fits = |minimum: (u16, u16)| minimum.0 <= area.width && minimum.1 <= area.height;
        let leaf = if fits(self.minimum(preferred)) {
            preferred
        } else {
            (1, 1)
        };
        if !fits(self.minimum(leaf)) {
            return None;
        }
        let mut out = Projection::default();
        self.project_into(area, leaf, &mut Vec::new(), &mut out);
        Some(out)
    }
    fn project_into(
        &self,
        area: Rect,
        leaf: (u16, u16),
        path: &mut Vec<bool>,
        out: &mut Projection,
    ) {
        match self {
            Self::Pane(id) => out.panes.push((id.clone(), area)),
            Self::Split {
                horizontal,
                ratio,
                first,
                second,
            } => {
                let a = first.minimum(leaf);
                let b = second.minimum(leaf);
                let (start, length, min_a, min_b) = if *horizontal {
                    (area.x, area.width, a.0, b.0)
                } else {
                    (area.y, area.height, a.1, b.1)
                };
                let offset = (f64::from(length) * ratio).round() as u16;
                let position = start + offset.clamp(min_a, length - min_b);
                out.boundaries.push(Boundary {
                    path: path.clone(),
                    area,
                    horizontal: *horizontal,
                    position,
                });
                let (a, b) = split_rect(area, *horizontal, position);
                path.push(false);
                first.project_into(a, leaf, path, out);
                path.pop();
                path.push(true);
                second.project_into(b, leaf, path, out);
                path.pop();
            }
        }
    }
    pub fn resize(&mut self, path: &[bool], ratio: f64) -> bool {
        if let Some((&second, rest)) = path.split_first() {
            if let Self::Split {
                first,
                second: right,
                ..
            } = self
            {
                return if second { right } else { first }.resize(rest, ratio);
            }
            return false;
        }
        if let Self::Split { ratio: value, .. } = self {
            *value = ratio.clamp(0.01, 0.99);
            true
        } else {
            false
        }
    }
    pub fn swap(&mut self, a: &str, b: &str) {
        match self {
            Self::Pane(id) => {
                if id == a {
                    *id = b.into()
                } else if id == b {
                    *id = a.into()
                }
            }
            Self::Split { first, second, .. } => {
                first.swap(a, b);
                second.swap(a, b)
            }
        }
    }
    pub fn cards(&self) -> Vec<Card> {
        self.project(Rect::new(0, 0, 1000, 1000), (1, 1))
            .map(|p| {
                p.panes
                    .into_iter()
                    .map(|(id, r)| Card {
                        id,
                        x: r.x,
                        y: r.y,
                        width: r.width,
                        height: r.height,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}
fn split_rect(r: Rect, horizontal: bool, at: u16) -> (Rect, Rect) {
    if horizontal {
        (
            Rect::new(r.x, r.y, at - r.x, r.height),
            Rect::new(at, r.y, r.right() - at, r.height),
        )
    } else {
        (
            Rect::new(r.x, r.y, r.width, at - r.y),
            Rect::new(r.x, at, r.width, r.bottom() - at),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fitted(p: &Projection, area: Rect, count: usize) {
        assert_eq!(p.panes.len(), count);
        assert!(Tiling::from_rects(area, &p.panes).is_some());
    }
    #[test]
    fn native_baselines_are_bounded_and_round_trip_as_view_preferences() {
        let good = serde_json::json!({"tab":"a".repeat(64)});
        let view: crate::core::layouts::View =
            serde_json::from_value(serde_json::json!({"mode":"map","map_native":good})).unwrap();
        view.validate().unwrap();
        let stored = serde_json::to_value(view).unwrap();
        assert_eq!(
            native_baselines(&stored["map_native"]).unwrap()["tab"],
            "a".repeat(64)
        );
        for bad in [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({"":"a".repeat(64)}),
            serde_json::json!({"tab":"short"}),
            serde_json::json!({"tab":"g".repeat(64)}),
            serde_json::json!({"tab":0}),
        ] {
            assert!(native_baselines(&bad).is_err());
        }
        let entries = (0..513)
            .map(|i| (i.to_string(), serde_json::json!("a".repeat(64))))
            .collect::<serde_json::Map<_, _>>();
        assert!(native_baselines(&entries.into()).is_err());
    }
    #[test]
    fn every_pane_fits_without_overlap_for_dense_and_small_layouts() {
        for (w, h) in [(311, 74), (120, 36), (40, 12), (3, 3)] {
            for n in 1..=100.min(usize::from(w) * usize::from(h)) {
                let ids = (0..n).map(|i| format!("pane-{i}")).collect::<Vec<_>>();
                let area = Rect::new(2, 3, w, h);
                let tree = Tiling::balanced(&ids, area).unwrap();
                fitted(&tree.project(area, (4, 3)).unwrap(), area, n);
            }
        }
    }
    #[test]
    fn native_nested_layout_resizes_and_swaps_without_hiding_identities() {
        let panes = vec![
            ("a".into(), Rect::new(0, 0, 20, 60)),
            ("b".into(), Rect::new(20, 0, 80, 1)),
            ("c".into(), Rect::new(20, 1, 80, 19)),
            ("d".into(), Rect::new(20, 20, 80, 40)),
        ];
        let mut tree = Tiling::from_rects(Rect::new(0, 0, 100, 60), &panes).unwrap();
        let area = Rect::new(0, 0, 311, 74);
        let before = tree.project(area, (4, 3)).unwrap();
        fitted(&before, area, 4);
        tree.resize(&before.boundaries[0].path, 0.99);
        fitted(&tree.project(area, (4, 3)).unwrap(), area, 4);
        tree.swap("a", "d");
        fitted(&tree.project(area, (4, 3)).unwrap(), area, 4);
        fitted(
            &Tiling::from_cards(&tree.cards())
                .unwrap()
                .project(area, (4, 3))
                .unwrap(),
            area,
            4,
        );
    }
    #[test]
    fn legacy_overlapping_cards_are_rejected_for_migration() {
        let cards = (0..20)
            .map(|i| Card {
                id: format!("pane-{i}"),
                x: (i % 3) * 300,
                y: (i / 3 % 3) * 300,
                width: 400,
                height: 400,
            })
            .collect::<Vec<_>>();
        assert!(validate(&cards).is_ok());
        assert!(Tiling::from_cards(&cards).is_none());
    }
}
