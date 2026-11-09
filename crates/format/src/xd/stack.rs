//! XD stacks and padding → REST auto layout.
//!
//! A stack is a group with `meta.ux.contentStackType` (`horizontal` or `vertical`): XD lays its
//! layers out in a row or column, each followed by its own gap (`contentStackMarginAfter`). A
//! group with `contentPadding` and `hasContentBackground` keeps its first layer, a background, that
//! much larger than the rest on each side. FigCraft's auto layout has one gap and one alignment
//! for all items, so a stack becomes an auto layout frame only when that reproduces where XD put
//! every layer: evenly spaced, aligned on one edge or the centre across, the padding what the
//! background shows. Others keep their layers where XD drew them, with a note. The background
//! becomes an absolutely placed layer that stretches with the frame.

use serde_json::{Map as JsonMap, Value, json};

use super::tree::Built;

/// A group's layers with their place in it.
type Layers = Vec<(usize, Built)>;

/// How far apart (px) places may be and still count as the same.
const TOLERANCE: f64 = 0.75;

/// What a group asks for.
pub(crate) struct Ask {
    /// `Some(true)`: a row, `Some(false)`: a column, `None`: padding only.
    pub row: Option<bool>,
    /// `[left, top, right, bottom]` when the group has a padded background.
    pub padding: Option<[f64; 4]>,
    /// Each layer's gap after it (`contentStackMarginAfter`), by its place in the group.
    pub gaps: Vec<Option<f64>>,
}

/// What group `l` asks for, or `None` when it is neither a stack nor padded.
pub(crate) fn ask(l: &Value) -> Option<Ask> {
    let ux = l.get("meta").and_then(|m| m.get("ux"))?;
    let row = match ux.get("contentStackType").and_then(Value::as_str) {
        Some("horizontal") => Some(true),
        Some("vertical") => Some(false),
        _ => None,
    };
    let padded = ux.get("hasContentBackground").and_then(Value::as_bool) == Some(true);
    let padding = ux.get("contentPadding").filter(|_| padded).map(|p| {
        let n = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
        [n("left"), n("top"), n("right"), n("bottom")]
    });
    (row.is_some() || padding.is_some()).then_some(Ask { row, padding, gaps: Vec::new() })
}

/// Lay out `built` (the group's layers with their index in the group, boxes in the group's
/// space) as auto layout items when that keeps them where they are: the frame's auto layout
/// fields, with `built` put in flow order (the background first). `None` leaves everything as it
/// was.
pub(crate) fn layout(ask: &Ask, built: &mut Vec<(usize, Built)>) -> Option<JsonMap<String, Value>> {
    let shown = |b: &Built| b.node.get("visible").and_then(Value::as_bool) != Some(false);
    // The background is the group's first layer; hidden layers take no room.
    let bg = if ask.padding.is_some() {
        let i = built.iter().position(|(i, b)| *i == 0 && shown(b))?;
        Some(built.remove(i).1)
    } else {
        None
    };
    let (mut items, hidden): (Layers, Layers) = std::mem::take(built).into_iter().partition(|(_, b)| shown(b));
    let restore = |built: &mut Layers, bg: Option<Built>, items: Layers, hidden: Layers| {
        built.extend(bg.map(|b| (0, b)));
        built.extend(items);
        built.extend(hidden);
        built.sort_by_key(|(i, _)| *i);
    };
    // Padding alone: one item, laid out as a column.
    let row = match ask.row {
        Some(r) => r,
        None if items.len() == 1 => false,
        None => {
            restore(built, bg, items, hidden);
            return None;
        }
    };
    if items.is_empty() {
        restore(built, bg, items, hidden);
        return None;
    }
    let start = |b: &Built| if row { b.bbox.x0 } else { b.bbox.y0 };
    items.sort_by(|a, b| start(&a.1).total_cmp(&start(&b.1)));
    // The gap XD keeps after each item but the last, when they are all the same: XD's own spacing,
    // which the layers' boxes can't always show (a text's width is only known once measured).
    let given: Vec<f64> = items.iter().filter_map(|(i, _)| ask.gaps.get(*i).copied().flatten()).collect();
    let uniform = given.first().copied().filter(|g| given.len() + 1 >= items.len() && given.iter().all(|x| (x - g).abs() <= TOLERANCE));
    let fits = fit(row, &items, bg.as_ref(), ask.padding, uniform);
    let Some((gap, align, pad)) = fits else {
        restore(built, bg, items, hidden);
        return None;
    };
    let mut frame = JsonMap::new();
    frame.insert("layoutMode".into(), json!(if row { "HORIZONTAL" } else { "VERTICAL" }));
    frame.insert("itemSpacing".into(), json!(gap));
    frame.insert("primaryAxisAlignItems".into(), json!("MIN"));
    frame.insert("counterAxisAlignItems".into(), json!(align));
    frame.insert("primaryAxisSizingMode".into(), json!("AUTO"));
    frame.insert("counterAxisSizingMode".into(), json!("AUTO"));
    for (k, v) in ["paddingLeft", "paddingTop", "paddingRight", "paddingBottom"].into_iter().zip(pad) {
        frame.insert(k.into(), json!(v));
    }
    if let Some(mut b) = bg {
        b.node.insert("layoutPositioning".into(), json!("ABSOLUTE"));
        b.node.insert("constraints".into(), json!({"horizontal": "LEFT_RIGHT", "vertical": "TOP_BOTTOM"}));
        built.push((0, b));
    }
    built.extend(items);
    built.extend(hidden);
    Some(frame)
}

/// The gap, cross alignment and padding (`[left, top, right, bottom]`) that put `items` (in flow
/// order) where they are, or `None` when no single gap and alignment does.
fn fit(
    row: bool,
    items: &[(usize, Built)],
    bg: Option<&Built>,
    padding: Option<[f64; 4]>,
    given: Option<f64>,
) -> Option<(f64, &'static str, [f64; 4])> {
    let near = |a: f64, b: f64| (a - b).abs() <= TOLERANCE;
    let cross = |b: &Built| if row { (b.bbox.y0, b.bbox.y1) } else { (b.bbox.x0, b.bbox.x1) };
    let gap = match given {
        Some(g) => g,
        None => {
            let gaps: Vec<f64> = items.windows(2).filter_map(|w| Some(start_of(row, &w.get(1)?.1) - end_of(row, &w.first()?.1))).collect();
            let gap = gaps.first().copied().unwrap_or(0.0);
            if !gaps.iter().all(|g| near(*g, gap)) {
                return None;
            }
            gap
        }
    };
    if gap < -TOLERANCE {
        return None;
    }
    let lows: Vec<f64> = items.iter().map(|(_, b)| cross(b).0).collect();
    let highs: Vec<f64> = items.iter().map(|(_, b)| cross(b).1).collect();
    let all_near = |v: &[f64]| v.first().is_some_and(|f| v.iter().all(|x| near(*x, *f)));
    let centres: Vec<f64> = lows.iter().zip(&highs).map(|(a, b)| (a + b) / 2.0).collect();
    let align = if all_near(&lows) {
        "MIN"
    } else if all_near(&centres) {
        "CENTER"
    } else if all_near(&highs) {
        "MAX"
    } else {
        return None;
    };
    let content = items.iter().map(|(_, b)| b.bbox).reduce(|a, b| a.union(b))?;
    let pad = match (bg, padding) {
        (Some(bg), Some(_)) => {
            let p = [content.x0 - bg.bbox.x0, content.y0 - bg.bbox.y0, bg.bbox.x1 - content.x1, bg.bbox.y1 - content.y1];
            if p.iter().any(|v| *v < -TOLERANCE) {
                return None;
            }
            // Centred across, the padding on both sides must match for the centre to stay.
            let (a, b) = if row { (p[1], p[3]) } else { (p[0], p[2]) };
            if align == "CENTER" && !near(a, b) {
                return None;
            }
            p.map(|v| v.max(0.0))
        }
        _ => [0.0; 4],
    };
    Some((gap.max(0.0), align, pad))
}

fn start_of(row: bool, b: &Built) -> f64 {
    if row { b.bbox.x0 } else { b.bbox.y0 }
}

fn end_of(row: bool, b: &Built) -> f64 {
    if row { b.bbox.x1 } else { b.bbox.y1 }
}
