//! Native split ratios are relative to the ancestor area, not adjacent leaves.
use super::runtime_settings::Settings;
use crate::core::geometry::Rect;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

type Result<T> = std::result::Result<T, String>;
const INVALID: &str =
    "Native split metadata is incomplete or inconsistent; refresh before resizing";
const LIMIT: &str = "Resize exceeds native split ratio or saved minimum pane sizes";

pub struct Plan<'a> {
    pub amount: String,
    pub affected: Vec<&'a str>,
    area: Rect,
    expected: HashMap<&'a str, Rect>,
    before: HashMap<&'a str, Rect>,
}
impl Plan<'_> {
    pub fn confirms(&self, layout: &Value) -> bool {
        self.matches(layout, &self.expected)
    }
    pub fn unchanged(&self, layout: &Value) -> bool {
        self.matches(layout, &self.before)
    }
    fn matches(&self, layout: &Value, expected: &HashMap<&str, Rect>) -> bool {
        let Some(panes) = layout["panes"].as_array() else {
            return false;
        };
        if layout["zoomed"] != false
            || rect(&layout["area"]) != Some(self.area)
            || panes.len() != expected.len()
        {
            return false;
        }
        let mut seen = HashSet::new();
        panes.iter().all(|pane| {
            let Some(id) = pane["pane_id"].as_str() else {
                return false;
            };
            seen.insert(id)
                && expected
                    .get(id)
                    .is_some_and(|r| rect(&pane["rect"]) == Some(*r))
        })
    }
}
#[derive(Clone, Copy)]
enum Link {
    Split(usize),
    Pane(usize),
}
struct Split {
    area: Rect,
    horizontal: bool,
    ratio: f32,
    children: Option<[Link; 2]>,
}
fn key(r: Rect) -> [u16; 4] {
    [r.x, r.y, r.w, r.h]
}
fn rect(value: &Value) -> Option<Rect> {
    let field = |name| u16::try_from(value.get(name)?.as_u64()?).ok();
    let r = Rect {
        x: field("x")?,
        y: field("y")?,
        w: field("width")?,
        h: field("height")?,
    };
    (r.w > 0 && r.h > 0 && r.right() <= 65535 && r.bottom() <= 65535).then_some(r)
}
fn divide(area: Rect, horizontal: bool, ratio: f32) -> Option<[Rect; 2]> {
    let size = if horizontal { area.w } else { area.h };
    let first = (f32::from(size) * ratio).round() as u16;
    if first == 0 || first >= size {
        return None;
    }
    let (mut a, mut b) = (area, area);
    if horizontal {
        a.w = first;
        b.x = area.x.checked_add(first)?;
        b.w = size - first;
    } else {
        a.h = first;
        b.y = area.y.checked_add(first)?;
        b.h = size - first;
    }
    Some([a, b])
}
/// Validate the entire binary layout before calculating any native command.
pub fn prepare<'a>(
    value: &Value,
    size: [u16; 2],
    expected: &HashMap<&'a str, Rect>,
    target: &str,
    horizontal: bool,
    cells: i32,
    settings: &Settings,
) -> Result<Plan<'a>> {
    settings.validate()?;
    let panes = value["panes"].as_array().ok_or(INVALID)?;
    let rows = value["splits"].as_array().ok_or(INVALID)?;
    let area = rect(&value["area"]).ok_or(INVALID)?;
    if value["zoomed"] != false
        || area.x != 0
        || area.y != 0
        || [area.w, area.h] != size
        || panes.len() < 2
        || panes.len() != expected.len()
        || panes.len() as u64 > settings.terminal_resize_pane_limit
        || rows.len() != panes.len() - 1
        || cells == 0
        || u64::from(cells.unsigned_abs()) > settings.terminal_resize_max_cells
    {
        return Err(INVALID.into());
    }
    let mut leaves = Vec::with_capacity(panes.len());
    let mut locations = HashMap::new();
    let mut seen = HashSet::new();
    for pane in panes {
        let id = pane["pane_id"].as_str().ok_or(INVALID)?;
        let (&id, &original) = expected.get_key_value(id).ok_or(INVALID)?;
        if rect(&pane["rect"]) != Some(original)
            || !seen.insert(id)
            || locations
                .insert(key(original), Link::Pane(leaves.len()))
                .is_some()
        {
            return Err(INVALID.into());
        }
        leaves.push((id, original));
    }
    let mut splits = Vec::with_capacity(rows.len());
    let mut split_ids = HashSet::new();
    for row in rows {
        let id = row["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or(INVALID)?;
        let area = rect(&row["rect"]).ok_or(INVALID)?;
        let ratio = row["ratio"].as_f64().ok_or(INVALID)? as f32;
        let horizontal = match row["direction"].as_str() {
            Some("right") => true,
            Some("down") => false,
            _ => return Err(INVALID.into()),
        };
        if !ratio.is_finite()
            || !(0.1..=0.9).contains(&ratio)
            || !split_ids.insert(id)
            || locations
                .insert(key(area), Link::Split(splits.len()))
                .is_some()
        {
            return Err(INVALID.into());
        }
        splits.push(Split {
            area,
            ratio,
            horizontal,
            children: None,
        });
    }
    let root = *locations.get(&key(area)).ok_or(INVALID)?;
    for split in &mut splits {
        let [a, b] = divide(split.area, split.horizontal, split.ratio).ok_or(INVALID)?;
        split.children = Some([
            *locations.get(&key(a)).ok_or(INVALID)?,
            *locations.get(&key(b)).ok_or(INVALID)?,
        ]);
    }
    // Iterative traversal also handles deeply nested layouts within saved limits.
    let mut stack = vec![root];
    let mut visited = HashSet::new();
    while let Some(link) = stack.pop() {
        let tag = match link {
            Link::Split(i) => (true, i),
            Link::Pane(i) => (false, i),
        };
        if !visited.insert(tag) {
            return Err(INVALID.into());
        }
        if let Link::Split(i) = link {
            stack.extend(splits[i].children.ok_or(INVALID)?);
        }
    }
    if visited.len() != leaves.len() + splits.len() {
        return Err(INVALID.into());
    }
    let target_rect = *expected.get(target).ok_or(INVALID)?;
    let edge = if horizontal {
        if cells > 0 {
            target_rect.right()
        } else {
            u32::from(target_rect.x)
        }
    } else if cells > 0 {
        target_rect.bottom()
    } else {
        u32::from(target_rect.y)
    };
    let mut candidates = Vec::new();
    for (i, split) in splits.iter().enumerate() {
        if split.horizontal != horizontal {
            continue;
        }
        let [a, _] = divide(split.area, horizontal, split.ratio).ok_or(INVALID)?;
        let position = if horizontal { a.right() } else { a.bottom() };
        let overlap = if horizontal {
            u32::from(split.area.y) < target_rect.bottom()
                && u32::from(target_rect.y) < split.area.bottom()
        } else {
            u32::from(split.area.x) < target_rect.right()
                && u32::from(target_rect.x) < split.area.right()
        };
        let distance = position.abs_diff(edge);
        if overlap && distance <= 1 {
            candidates.push((distance, i));
        }
    }
    candidates.sort_unstable();
    let &(distance, chosen) = candidates.first().ok_or(INVALID)?;
    if distance != 0 || candidates.get(1).is_some_and(|(d, _)| *d == distance) {
        return Err(INVALID.into());
    }
    let split = &splits[chosen];
    let extent = if horizontal {
        split.area.w
    } else {
        split.area.h
    };
    let start = if horizontal {
        split.area.x
    } else {
        split.area.y
    };
    let desired = i64::from(edge) + i64::from(cells) - i64::from(start);
    if desired <= 0 || desired >= i64::from(extent) {
        return Err(LIMIT.into());
    }
    let ratio = desired as f32 / f32::from(extent);
    let delta = (ratio - split.ratio).abs();
    if !(0.1..=0.9).contains(&ratio) || delta > 0.5 {
        return Err(LIMIT.into());
    }
    let amount = format!("{delta:.9}");
    let delta: f32 = amount.parse().map_err(|_| INVALID)?;
    let ratio = if cells > 0 {
        split.ratio + delta
    } else {
        split.ratio - delta
    };
    let [first, _] = divide(split.area, horizontal, ratio).ok_or(LIMIT)?;
    let actual = if horizontal { first.w } else { first.h };
    if i64::from(actual) != desired {
        return Err(LIMIT.into());
    }
    let mut affected = Vec::new();
    let mut expected_after = HashMap::new();
    let mut stack = vec![(root, area)];
    while let Some((link, proposed)) = stack.pop() {
        match link {
            Link::Pane(i) => {
                let (id, original) = leaves[i];
                expected_after.insert(id, proposed);
                if proposed != original {
                    if u64::from(proposed.w) < settings.terminal_resize_min_cols
                        || u64::from(proposed.h) < settings.terminal_resize_min_rows
                    {
                        return Err(LIMIT.into());
                    }
                    affected.push(id);
                }
            }
            Link::Split(i) => {
                let split = &splits[i];
                let [a, b] = divide(
                    proposed,
                    split.horizontal,
                    if i == chosen { ratio } else { split.ratio },
                )
                .ok_or(LIMIT)?;
                let [first, second] = split.children.ok_or(INVALID)?;
                stack.push((second, b));
                stack.push((first, a));
            }
        }
    }
    Ok(Plan {
        amount,
        affected,
        area,
        expected: expected_after,
        before: expected.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> (Value, HashMap<&'static str, Rect>) {
        let r = |x, w| json!({"x":x,"y":0,"width":w,"height":40});
        let v = json!({"area":r(0,120),"zoomed":false,"panes":[{"pane_id":"one","rect":r(0,30)},{"pane_id":"two","rect":r(30,30)},{"pane_id":"three","rect":r(60,60)}],"splits":[{"id":"root","rect":r(0,120),"direction":"right","ratio":0.5},{"id":"nested","rect":r(0,60),"direction":"right","ratio":0.5}]});
        let expected = [
            (
                "one",
                Rect {
                    x: 0,
                    y: 0,
                    w: 30,
                    h: 40,
                },
            ),
            (
                "two",
                Rect {
                    x: 30,
                    y: 0,
                    w: 30,
                    h: 40,
                },
            ),
            (
                "three",
                Rect {
                    x: 60,
                    y: 0,
                    w: 60,
                    h: 40,
                },
            ),
        ]
        .into_iter()
        .collect();
        (v, expected)
    }
    #[test]
    fn ancestor_and_inner_dividers_use_their_own_areas_in_both_directions() {
        let (v, expected) = fixture();
        let settings = Settings::default();
        for (target, cells, extent, affected) in [
            ("two", 5, 120., 3),
            ("three", -5, 120., 3),
            ("one", 5, 60., 2),
            ("two", -5, 60., 2),
        ] {
            let p = prepare(&v, [120, 40], &expected, target, true, cells, &settings).unwrap();
            assert!((p.amount.parse::<f64>().unwrap() - 5. / extent).abs() < 0.000001);
            assert_eq!(p.affected.len(), affected);
        }
    }
    #[test]
    fn every_changed_leaf_obeys_saved_minima_including_non_neighbor_descendants() {
        let (mut v, mut expected) = fixture();
        v["splits"][1]["ratio"] = json!(0.1);
        v["panes"][0]["rect"]["width"] = json!(6);
        v["panes"][1]["rect"]["x"] = json!(6);
        v["panes"][1]["rect"]["width"] = json!(54);
        expected.get_mut("one").unwrap().w = 6;
        expected.get_mut("two").unwrap().x = 6;
        expected.get_mut("two").unwrap().w = 54;
        let mut settings = Settings {
            terminal_resize_min_cols: 6,
            ..Default::default()
        };
        // Selected right and its middle peer remain wide; the first sibling
        // shrinks from6 to5 when their shared ancestor shrinks by10.
        assert!(prepare(&v, [120, 40], &expected, "three", true, -10, &settings).is_err());
        settings.terminal_resize_min_cols = 5;
        assert!(prepare(&v, [120, 40], &expected, "three", true, -10, &settings).is_ok());
    }
    #[test]
    fn malformed_incomplete_stale_and_provider_clamped_layouts_are_refused() {
        let (v, expected) = fixture();
        let settings = Settings::default();
        for mutation in 0..5 {
            let mut bad = v.clone();
            match mutation {
                0 => bad["splits"].as_array_mut().unwrap().truncate(1),
                1 => bad["splits"][1]["ratio"] = json!(0.6),
                2 => bad["splits"][1]["id"] = json!("root"),
                3 => bad["panes"][0]["pane_id"] = json!("two"),
                _ => bad["zoomed"] = json!(true),
            }
            assert!(prepare(&bad, [120, 40], &expected, "two", true, 5, &settings).is_err());
        }
        assert!(prepare(&v, [120, 40], &expected, "two", true, 55, &settings).is_err());
    }
    #[test]
    fn vertical_ancestors_and_rounded_ratios_keep_exact_cell_movement() {
        let (mut v, expected) = fixture();
        for rect in std::iter::once(&mut v["area"]) {
            let a = rect["width"].take();
            rect["width"] = rect["height"].take();
            rect["height"] = a;
        }
        for key in ["panes", "splits"] {
            for row in v[key].as_array_mut().unwrap() {
                let rect = &mut row["rect"];
                let x = rect["x"].take();
                rect["x"] = rect["y"].take();
                rect["y"] = x;
                let w = rect["width"].take();
                rect["width"] = rect["height"].take();
                rect["height"] = w;
                if key == "splits" {
                    row["direction"] = json!("down");
                }
            }
        }
        let expected = expected
            .into_iter()
            .map(|(id, r)| {
                (
                    id,
                    Rect {
                        x: r.y,
                        y: r.x,
                        w: r.h,
                        h: r.w,
                    },
                )
            })
            .collect();
        for (id, cells) in [("two", 5), ("three", -5)] {
            let p = prepare(
                &v,
                [40, 120],
                &expected,
                id,
                false,
                cells,
                &Settings::default(),
            )
            .unwrap();
            assert!((p.amount.parse::<f64>().unwrap() - 5. / 120.).abs() < 0.000001);
            assert_eq!(p.affected.len(), 3);
        }
    }
    #[test]
    fn acknowledgement_requires_every_exact_expected_rectangle() {
        let (v, expected) = fixture();
        let p = prepare(
            &v,
            [120, 40],
            &expected,
            "two",
            true,
            5,
            &Settings::default(),
        )
        .unwrap();
        assert!(p.unchanged(&v));
        assert!(!p.confirms(&v));
        assert!(!p.confirms(&json!({})));
        let mut after = v.clone();
        after["panes"][0]["rect"]["width"] = json!(33);
        after["panes"][1]["rect"]["x"] = json!(33);
        after["panes"][1]["rect"]["width"] = json!(32);
        after["panes"][2]["rect"]["x"] = json!(65);
        after["panes"][2]["rect"]["width"] = json!(55);
        assert!(p.confirms(&after));
        assert!(!p.unchanged(&after));
        after["panes"][0]["rect"]["width"] = json!(32);
        assert!(!p.confirms(&after));
    }
}
