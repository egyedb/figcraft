//! XD repeat grids → REST grid auto layout.
//!
//! A repeat grid is a group with `meta.ux.repeatGrid`: `columns` × `rows` cells of `cellWidth` ×
//! `cellHeight`, `paddingX` and `paddingY` apart (negative when cells overlap). Its layers are the
//! cells, row by row, each a copy of the first moved by whole steps. It becomes a grid auto layout
//! frame of fixed tracks, each cell a frame of the cell's size holding the cell's layers where XD
//! draws them. When the cells are copies of one another (XD keeps them so; their texts and images
//! may differ), the first cell is a component and the others instances of it, as editing one cell
//! of a repeat grid edits them all; inside a component (or an instance) they stay frames, since a
//! component can't hold another's main.

use figcraft_doc::geom::{Affine, Rect};
use serde_json::{Map as JsonMap, Value, json};

use super::tree::{Built, Cx, num, shift, str_field};

/// A group's layers with their place in it.
type Layers = Vec<(usize, Built)>;

/// Most cells laid out as one grid.
const MAX_CELLS: usize = 100_000;

/// A repeat grid's layout.
pub(crate) struct Grid {
    columns: usize,
    rows: usize,
    cell: (f64, f64),
    padding: (f64, f64),
}

/// The repeat grid layer `l` is, if it is one with a usable layout.
pub(crate) fn ask(l: &Value) -> Option<Grid> {
    let g = l.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("repeatGrid"))?;
    let count = |k: &str| g.get(k).and_then(Value::as_u64).filter(|n| *n >= 1).and_then(|n| usize::try_from(n).ok());
    let (columns, rows) = (count("columns")?, count("rows")?);
    if columns.saturating_mul(rows) > MAX_CELLS {
        return None;
    }
    let cell = (num(g, "cellWidth").filter(|w| *w > 0.0)?, num(g, "cellHeight").filter(|h| *h > 0.0)?);
    Some(Grid { columns, rows, cell, padding: (num(g, "paddingX").unwrap_or(0.0), num(g, "paddingY").unwrap_or(0.0)) })
}

/// Lay out `cells` (built from `kids`, with their place among them, boxes in the grid's space,
/// which sits at `world`) as grid items: the frame's auto layout fields and the cells as items.
/// `components`: the first cell may become a component. The cells come back unchanged when they
/// aren't the grid's whole set.
pub(crate) fn layout(
    cx: &mut Cx,
    g: &Grid,
    cells: Layers,
    kids: &[Value],
    world: Affine,
    name: &str,
    components: bool,
) -> Result<(JsonMap<String, Value>, Layers), Layers> {
    let total = g.columns * g.rows;
    if cells.len() != total || kids.len() != total || cells.iter().enumerate().any(|(i, (k, _))| *k != i) {
        return Err(cells);
    }
    // A step is a cell and its gap; overlapping cells (a negative gap) step by less than a cell,
    // which the grid shows as tracks of the step with no gap, the cells' layers overflowing them.
    let step = (g.cell.0 + g.padding.0, g.cell.1 + g.padding.1);
    if !(step.0 > 0.0 && step.1 > 0.0) {
        return Err(cells);
    }
    let track = (if g.padding.0 < 0.0 { step.0 } else { g.cell.0 }, if g.padding.1 < 0.0 { step.1 } else { g.cell.1 });
    let gap = (g.padding.0.max(0.0), g.padding.1.max(0.0));
    // The grid starts where its first cell's layers do.
    let Some(start) = cells.first().map(|(_, b)| (b.bbox.x0, b.bbox.y0)) else { return Err(cells) };
    let same = kids.windows(2).all(|w| w.first().map(|a| shape_of(a, 0)) == w.get(1).map(|b| shape_of(b, 0)));
    let main = (components && same && total >= 2).then(|| cx.id(None));
    let label: String = format!("{} cell", if name.is_empty() { "Grid" } else { name }).chars().take(200).collect();
    let mut out = Vec::with_capacity(total);
    for (i, (k, cell)) in cells.into_iter().enumerate() {
        let (col, row) = (i % g.columns, i / g.columns);
        let x = start.0 + col as f64 * step.0;
        let y = start.1 + row as f64 * step.1;
        let cbox = Rect::new(x, y, x + track.0, y + track.1);
        let mut node = JsonMap::new();
        let (ty, id) = match (&main, i) {
            (Some(m), 0) => ("COMPONENT", m.clone()),
            (Some(_), _) => ("INSTANCE", cx.id(None)),
            (None, _) => ("FRAME", cx.id(None)),
        };
        node.insert("type".into(), json!(ty));
        node.insert("id".into(), json!(id));
        node.insert("name".into(), json!(label));
        if ty == "INSTANCE"
            && let Some(m) = &main
        {
            node.insert("componentId".into(), json!(m));
        }
        node.insert("visible".into(), json!(true));
        node.insert("size".into(), json!({"x": track.0, "y": track.1}));
        node.insert("relativeTransform".into(), json!([[1.0, 0.0, x], [0.0, 1.0, y]]));
        let bb = world.transform_rect_bbox(cbox);
        node.insert("absoluteBoundingBox".into(), json!({"x": bb.x0, "y": bb.y0, "width": bb.width(), "height": bb.height()}));
        node.insert("fills".into(), json!([]));
        node.insert("clipsContent".into(), json!(false));
        // The cell's layers stay where XD draws them, now inside the cell.
        let inner = shift(vec![cell], x, y);
        node.insert("children".into(), Value::Array(inner.into_iter().map(|b| Value::Object(b.node)).collect()));
        out.push((k, Built { node, bbox: cbox }));
    }
    let mut frame = JsonMap::new();
    frame.insert("layoutMode".into(), json!("GRID"));
    frame.insert("gridColumnCount".into(), json!(g.columns));
    frame.insert("gridRowCount".into(), json!(g.rows));
    frame.insert("gridColumnGap".into(), json!(gap.0));
    frame.insert("gridRowGap".into(), json!(gap.1));
    frame.insert("gridColumnsSizing".into(), json!(format!("repeat({}, {}px)", g.columns, track.0)));
    frame.insert("gridRowsSizing".into(), json!(format!("repeat({}, {}px)", g.rows, track.1)));
    Ok((frame, out))
}

/// The structure of a cell (layer kinds, shapes and the components referenced, nested): copies of
/// one another have the same.
fn shape_of(v: &Value, depth: usize) -> String {
    if depth > 32 {
        return String::new();
    }
    let ty = str_field(v, "type").unwrap_or("");
    let mut s = String::from(ty);
    if let Some(sh) = v.get("shape").and_then(|s| str_field(s, "type")) {
        s.push(':');
        s.push_str(sh);
    }
    if let Some(src) = str_field(v, "syncSourceGuid") {
        s.push('@');
        s.push_str(src);
    }
    let kids = v.get("group").and_then(|g| g.get("children")).and_then(Value::as_array);
    if let Some(kids) = kids {
        s.push('(');
        for k in kids.iter().take(10_000) {
            s.push_str(&shape_of(k, depth + 1));
            s.push(',');
        }
        s.push(')');
    }
    s
}
