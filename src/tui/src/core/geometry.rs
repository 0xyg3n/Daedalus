//! Checked cell geometry shared by projection, divider rendering and resize admission.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}
impl Rect {
    pub fn right(self) -> u32 {
        u32::from(self.x) + u32::from(self.w)
    }
    pub fn bottom(self) -> u32 {
        u32::from(self.y) + u32::from(self.h)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub leading: usize,
    pub trailing: usize,
    pub horizontal: bool,
    pub start: u16,
    pub end: u16,
}
#[derive(Clone, Debug)]
pub struct Geometry {
    rects: Vec<Rect>,
}
#[derive(Clone, Copy)]
struct Interval {
    start: u16,
    end: u16,
    index: usize,
}
impl Geometry {
    pub fn new(width: u16, height: u16, rects: Vec<Rect>) -> Option<Self> {
        if width == 0 || height == 0 || rects.is_empty() {
            return None;
        }
        let mut events = Vec::with_capacity(rects.len() * 2);
        for (index, r) in rects.iter().copied().enumerate() {
            if r.w == 0
                || r.h == 0
                || r.right() > u32::from(width)
                || r.bottom() > u32::from(height)
            {
                return None;
            }
            // Ends precede starts at the same column: touching is valid.
            events.push((u32::from(r.x), true, index));
            events.push((r.right(), false, index));
        }
        events.sort_unstable();
        let mut active: BTreeMap<u16, (u32, usize)> = BTreeMap::new();
        for (_, start, index) in events {
            let r = rects[index];
            if !start {
                active.remove(&r.y);
                continue;
            }
            if active
                .range(..=r.y)
                .next_back()
                .is_some_and(|(_, (end, _))| *end > u32::from(r.y))
                || active
                    .range(r.y..)
                    .next()
                    .is_some_and(|(begin, _)| u32::from(*begin) < r.bottom())
            {
                return None;
            }
            active.insert(r.y, (r.bottom(), index));
        }
        Some(Self { rects })
    }
    /// Merge disjoint intervals at matching borders, rather than every pane pair.
    /// A one-cell gap occupied by another pane is never a shared divider.
    pub fn edges(&self) -> Vec<Edge> {
        let mut result = Vec::new();
        for horizontal in [true, false] {
            let mut ends: HashMap<u16, Vec<Interval>> = HashMap::new();
            let mut starts: HashMap<u16, Vec<Interval>> = HashMap::new();
            let mut occupied: HashMap<u16, Vec<Interval>> = HashMap::new();
            for (index, r) in self.rects.iter().copied().enumerate() {
                let (begin, end, from, to, length) = if horizontal {
                    (r.x, r.right() as u16, r.y, r.bottom() as u16, r.w)
                } else {
                    (r.y, r.bottom() as u16, r.x, r.right() as u16, r.h)
                };
                let item = Interval {
                    start: from,
                    end: to,
                    index,
                };
                starts.entry(begin).or_default().push(item);
                ends.entry(end).or_default().push(item);
                if length == 1 {
                    occupied.entry(begin).or_default().push(item);
                }
            }
            for list in ends
                .values_mut()
                .chain(starts.values_mut())
                .chain(occupied.values_mut())
            {
                list.sort_unstable_by_key(|r| (r.start, r.end, r.index));
            }
            for (at, left) in &ends {
                for next in [Some(*at), at.checked_add(1)].into_iter().flatten() {
                    let Some(right) = starts.get(&next) else {
                        continue;
                    };
                    let mut a = 0;
                    let mut b = 0;
                    while a < left.len() && b < right.len() {
                        let l = left[a];
                        let r = right[b];
                        let start = l.start.max(r.start);
                        let end = l.end.min(r.end);
                        let blocked = next != *at
                            && occupied.get(at).is_some_and(|rows| {
                                let index = rows.partition_point(|p| p.end <= start);
                                rows.get(index).is_some_and(|p| p.start < end)
                            });
                        if start < end && !blocked {
                            result.push(Edge {
                                leading: l.index,
                                trailing: r.index,
                                horizontal,
                                start,
                                end,
                            });
                        }
                        if l.end <= r.end {
                            a += 1;
                        } else {
                            b += 1;
                        }
                    }
                }
            }
        }
        // Preserve the old renderer's leading/trailing/axis order.
        result.sort_unstable_by_key(|e| (e.leading, e.trailing, !e.horizontal));
        result
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ResizePolicy {
    pub pane_limit: u64,
    pub max_cells: u64,
    pub min_cols: u64,
    pub min_rows: u64,
}
impl Default for ResizePolicy {
    fn default() -> Self {
        Self {
            pane_limit: 128,
            max_cells: 4096,
            min_cols: 4,
            min_rows: 3,
        }
    }
}
impl ResizePolicy {
    pub fn validate(&self) -> bool {
        (2..=65536).contains(&self.pane_limit)
            && (1..=65535).contains(&self.max_cells)
            && (1..=32767).contains(&self.min_cols)
            && (1..=32767).contains(&self.min_rows)
    }
    pub fn minimum(&self, horizontal: bool) -> i32 {
        if horizontal {
            self.min_cols as i32
        } else {
            self.min_rows as i32
        }
    }
}

/// Identity uniqueness belongs to callers; this checks their exact encoded IDs.
pub fn unique_ids<'a>(ids: impl IntoIterator<Item = &'a str>) -> bool {
    let mut seen = BTreeSet::new();
    ids.into_iter().all(|id| !id.is_empty() && seen.insert(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }
    #[test]
    fn checked_extents_overlap_and_zero_dimensions_refuse() {
        assert!(Geometry::new(65535, 1, vec![rect(0, 0, 65535, 1)]).is_some());
        assert!(Geometry::new(65535, 1, vec![rect(1, 0, 65535, 1)]).is_none());
        assert!(Geometry::new(20, 20, vec![rect(0, 0, 0, 1)]).is_none());
        assert!(Geometry::new(20, 20, vec![rect(0, 0, 11, 10), rect(10, 0, 10, 10)]).is_none());
        assert!(Geometry::new(20, 20, vec![rect(0, 0, 10, 10), rect(10, 0, 10, 10)]).is_some());
        assert!(!unique_ids(["pane", "pane"]));
        assert!(!unique_ids([""]));
    }
    #[test]
    fn indexed_edges_match_independent_pair_checks_on_valid_tilings() {
        for gap in [0, 1] {
            let rects: Vec<_> = (0..8)
                .flat_map(|y| (0..9).map(move |x| rect(x * (5 + gap), y * (4 + gap), 5, 4)))
                .collect();
            let geometry = Geometry::new(100, 100, rects.clone()).unwrap();
            let actual = geometry.edges();
            let mut expected = Vec::new();
            for (i, a) in rects.iter().enumerate() {
                for (j, b) in rects.iter().enumerate() {
                    for horizontal in [true, false] {
                        let (end, start, from, to) = if horizontal {
                            (
                                a.right(),
                                u32::from(b.x),
                                a.y.max(b.y),
                                (a.bottom().min(b.bottom())) as u16,
                            )
                        } else {
                            (
                                a.bottom(),
                                u32::from(b.y),
                                a.x.max(b.x),
                                (a.right().min(b.right())) as u16,
                            )
                        };
                        if start >= end && start - end <= 1 && from < to {
                            expected.push(Edge {
                                leading: i,
                                trailing: j,
                                horizontal,
                                start: from,
                                end: to,
                            });
                        }
                    }
                }
            }
            assert_eq!(actual, expected);
        }
    }
    #[test]
    fn one_cell_terminal_cannot_be_skipped_when_finding_a_divider() {
        let geometry = Geometry::new(
            21,
            10,
            vec![rect(0, 0, 10, 10), rect(10, 0, 1, 10), rect(11, 0, 10, 10)],
        )
        .unwrap();
        assert!(
            !geometry
                .edges()
                .iter()
                .any(|e| e.leading == 0 && e.trailing == 2)
        );
        assert_eq!(geometry.edges().len(), 2);
    }
    #[test]
    fn long_shared_edge_keeps_each_actual_neighbor_segment() {
        let geometry = Geometry::new(
            20,
            20,
            vec![rect(0, 0, 10, 20), rect(11, 0, 9, 9), rect(11, 10, 9, 10)],
        )
        .unwrap();
        assert_eq!(
            geometry.edges(),
            vec![
                Edge {
                    leading: 0,
                    trailing: 1,
                    horizontal: true,
                    start: 0,
                    end: 9
                },
                Edge {
                    leading: 0,
                    trailing: 2,
                    horizontal: true,
                    start: 10,
                    end: 20
                },
                Edge {
                    leading: 1,
                    trailing: 2,
                    horizontal: false,
                    start: 11,
                    end: 20
                }
            ]
        );
    }
}
