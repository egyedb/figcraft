//! XD artboards and layers → REST API file JSON nodes.
//!
//! Layers carry a `transform` (`a`, `b`, `c`, `d`, `tx`, `ty`) relative to their parent group.
//! Inside an artboard the layers are placed on the canvas, not on the artboard: the artboard's
//! own place comes from the resources' `artboards` table. Groups have no size of their own; a
//! group here is sized to what it holds. A group's `meta.ux.clipPathResources` is its mask, a
//! shape in the group's own space. A `syncRef` layer stands for the component layer its
//! `syncSourceGuid` names in the resources' `symbols`; it is copied in as a plain layer.

use std::collections::{BTreeSet, HashMap, HashSet};

use figcraft_doc::geom::{Affine, BezPath, Point, Rect, Shape};
use figcraft_doc::{VectorNetwork, WindingRule};
use serde_json::{Map as JsonMap, Value, json};

use super::style::{self, clip};
use super::{Archive, Fonts};

/// Most distinct notes kept.
const MAX_NOTES: usize = 200;
/// Deepest layer nesting followed (deeper layers are skipped with a warning).
const MAX_DEPTH: usize = 200;
/// Most component layers copied in for `syncRef` layers, counted in layers.
const MAX_SYNC_WORK: usize = 2_000_000;
/// Most layers indexed among the component definitions.
const MAX_SYMBOL_INDEX: usize = 2_000_000;
/// Largest size and farthest position kept (the REST importer's own bounds).
const MAX_SIZE: f64 = 1e6;
const MAX_COORD: f64 = 1e7;
/// Longest path data read from one layer.
const MAX_PATH_BYTES: usize = 4 * 1024 * 1024;

/// The REST JSON of an XD file, with what the mapping noticed.
pub(crate) struct Map {
    pub json: Value,
    pub warnings: Vec<String>,
    /// Vector networks the JSON references by `vectorNetworkRef`.
    pub networks: Vec<Option<VectorNetwork>>,
}

/// The mapping's state while walking the file.
pub(crate) struct Cx<'a> {
    /// Component layers by id, for `syncRef` layers.
    symbols: HashMap<&'a str, &'a Value>,
    /// Layers copied in for `syncRef` layers so far.
    sync_work: usize,
    pub warnings: Vec<String>,
    seen: BTreeSet<String>,
    pub networks: Vec<Option<VectorNetwork>>,
    /// Ids given so far (every node gets one of its own).
    ids: HashSet<String>,
    /// Layers mapped so far, and the most the file may give.
    nodes: usize,
    max_nodes: usize,
    pub fonts: Fonts,
    /// What `fonts` said so far, by PostScript name and by family and style.
    pub face_cache: HashMap<String, Option<(String, String)>>,
    pub metrics_cache: HashMap<(String, String), Option<(f64, f64)>>,
}

impl Cx<'_> {
    pub(crate) fn warn(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if self.seen.len() > MAX_NOTES || !self.seen.insert(msg.clone()) {
            return;
        }
        if self.seen.len() > MAX_NOTES {
            self.warnings.push("more notes were left out".into());
        } else {
            self.warnings.push(msg);
        }
    }

    /// Count one more layer; false (with a warning) once the file's budget is spent.
    fn count(&mut self) -> bool {
        if self.nodes >= self.max_nodes {
            self.warn(format!("the file has more than {} layers; the rest were skipped", self.max_nodes));
            return false;
        }
        self.nodes += 1;
        true
    }

    /// A node id: the layer's own when it has one not given yet, else a new one.
    fn id(&mut self, own: Option<&str>) -> String {
        if let Some(id) = own.filter(|i| !i.is_empty() && i.len() <= 128)
            && self.ids.insert(id.to_string())
        {
            return id.to_string();
        }
        loop {
            let id = format!("xd:{}", self.ids.len());
            if self.ids.insert(id.clone()) {
                return id;
            }
        }
    }
}

/// A mapped layer: its REST node and its box in its parent node's space.
pub(crate) struct Built {
    pub node: JsonMap<String, Value>,
    pub bbox: Rect,
}

/// Map the artboards and loose layers of `archive` to a REST file JSON named `name`.
pub(crate) fn to_rest(archive: &Archive, name: &str, max_nodes: usize, fonts: Fonts) -> Map {
    let mut cx = Cx {
        symbols: HashMap::new(),
        sync_work: 0,
        warnings: Vec::new(),
        seen: BTreeSet::new(),
        networks: Vec::new(),
        ids: HashSet::new(),
        nodes: 0,
        max_nodes,
        fonts,
        face_cache: HashMap::new(),
        metrics_cache: HashMap::new(),
    };
    index_symbols(&mut cx, &archive.resources);
    let mut roots = Vec::new();
    for ab in &archive.artboards {
        if let Some(b) = artboard(&mut cx, ab) {
            roots.push(Value::Object(b));
        }
    }
    if let Some(p) = &archive.pasteboard {
        for b in children(&mut cx, list(p, "children"), Affine::IDENTITY, Affine::IDENTITY, 1, true) {
            roots.push(Value::Object(b.node));
        }
    }
    if archive.interactions {
        cx.warn("prototype interactions are not supported yet and were skipped");
    }
    let canvas = json!({"type": "CANVAS", "id": "0:1", "name": "Page 1", "children": roots});
    let json = json!({"name": name, "document": {"type": "DOCUMENT", "id": "0:0", "children": [canvas]}});
    Map { json, warnings: cx.warnings, networks: cx.networks }
}

/// Index the layers of the component definitions by id.
fn index_symbols<'a>(cx: &mut Cx<'a>, resources: &'a Value) {
    let symbols = resources.get("resources").and_then(|r| r.get("meta")).and_then(|m| m.get("ux")).and_then(|u| u.get("symbols"));
    let mut stack: Vec<&Value> = symbols.and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    while let Some(n) = stack.pop() {
        if cx.symbols.len() >= MAX_SYMBOL_INDEX {
            cx.warn("the file's components hold more layers than the import reads; some component copies were skipped");
            break;
        }
        if let Some(id) = n.get("id").and_then(Value::as_str) {
            cx.symbols.insert(id, n);
        }
        stack.extend(kids(n));
    }
}

/// The children of a group or artboard layer.
fn kids(n: &Value) -> &[Value] {
    let inner = n.get("group").or_else(|| n.get("artboard"));
    inner.map_or(&[], |g| list(g, "children"))
}

fn list<'v>(v: &'v Value, key: &str) -> &'v [Value] {
    v.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
}

pub(crate) fn str_field<'v>(v: &'v Value, key: &str) -> Option<&'v str> {
    v.get(key).and_then(Value::as_str)
}

/// A finite number field, within the import's coordinate bounds.
pub(crate) fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|x| x.is_finite()).map(|x| x.clamp(-MAX_COORD, MAX_COORD))
}

/// A layer's `transform` (identity when missing or not finite).
fn transform(cx: &mut Cx, n: &Value) -> Affine {
    let Some(t) = n.get("transform") else { return Affine::IDENTITY };
    let k = |key: &str| t.get(key).and_then(Value::as_f64);
    match (k("a"), k("b"), k("c"), k("d"), k("tx"), k("ty")) {
        (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f))
            if [a, b, c, d].iter().all(|v| v.is_finite() && v.abs() <= 1e4) && [e, f].iter().all(|v| v.is_finite() && v.abs() <= MAX_COORD) =>
        {
            Affine::new([a, b, c, d, e, f])
        }
        (None, None, None, None, None, None) => Affine::IDENTITY,
        _ => {
            cx.warn("layers with an invalid transform were placed without it");
            Affine::IDENTITY
        }
    }
}

/// One artboard as a top-level frame.
fn artboard(cx: &mut Cx, ab: &super::Artboard) -> Option<JsonMap<String, Value>> {
    let root = list(&ab.content, "children").iter().find(|c| str_field(c, "type") == Some("artboard"));
    let Some(root) = root else {
        cx.warn("artboards without content were skipped");
        return None;
    };
    if !cx.count() {
        return None;
    }
    let origin = Affine::translate((ab.x, ab.y));
    // The layers sit on the canvas: their parent transform is none.
    let built = children(cx, kids(root), Affine::IDENTITY, origin, 1, false);
    let (w, h, origin, built) = if ab.width > 0.0 && ab.height > 0.0 {
        (ab.width.min(MAX_SIZE), ab.height.min(MAX_SIZE), origin, built)
    } else {
        // No size in the resources: the frame fits its content.
        let b = union(built.iter().map(|b| b.bbox)).unwrap_or(Rect::ZERO);
        let built = shift(built, b.x0, b.y0);
        (b.width().min(MAX_SIZE), b.height().min(MAX_SIZE), origin * Affine::translate((b.x0, b.y0)), built)
    };
    let mut node = JsonMap::new();
    node.insert("type".into(), json!("FRAME"));
    let id = cx.id(Some(&ab.id));
    node.insert("id".into(), json!(id));
    node.insert("name".into(), json!(ab.name));
    node.insert("visible".into(), json!(root.get("visible").and_then(Value::as_bool).unwrap_or(true)));
    node.insert("size".into(), json!({"x": w, "y": h}));
    node.insert("relativeTransform".into(), rel(origin));
    node.insert("clipsContent".into(), json!(true));
    let fill = root.get("style").and_then(|s| s.get("fill"));
    let fills = match fill {
        Some(f) => style::fills(cx, Some(f), w, h),
        None => vec![json!({"type": "SOLID", "color": {"r": 1.0, "g": 1.0, "b": 1.0, "a": 1.0}})],
    };
    node.insert("fills".into(), Value::Array(fills));
    unsupported(cx, root);
    node.insert("children".into(), Value::Array(built.into_iter().map(|b| Value::Object(b.node)).collect()));
    Some(node)
}

/// A transform as REST `relativeTransform`.
fn rel(t: Affine) -> Value {
    let [a, b, c, d, e, f] = t.as_coeffs();
    json!([[a, c, e], [b, d, f]])
}

fn union(rects: impl Iterator<Item = Rect>) -> Option<Rect> {
    rects.reduce(|a, b| a.union(b))
}

/// Move built nodes by `-dx`, `-dy` in their parent's space (their parent moved by that much).
fn shift(built: Vec<Built>, dx: f64, dy: f64) -> Vec<Built> {
    built
        .into_iter()
        .map(|mut b| {
            if let Some(rt) = b.node.get_mut("relativeTransform").and_then(Value::as_array_mut) {
                for (row, d) in rt.iter_mut().zip([dx, dy]) {
                    if let Some(e) = row.get_mut(2) {
                        *e = json!(e.as_f64().unwrap_or(0.0) - d);
                    }
                }
            }
            b.bbox = b.bbox - figcraft_doc::geom::Vec2::new(dx, dy);
            b
        })
        .collect()
}

/// Map `layers`: their parent transform on the canvas is `xd_parent`, and the node they go into
/// is placed at `origin`. `root`: they go onto the page itself.
pub(crate) fn children(cx: &mut Cx, layers: &[Value], xd_parent: Affine, origin: Affine, depth: usize, root: bool) -> Vec<Built> {
    layers.iter().filter_map(|l| layer(cx, l, xd_parent, origin, depth, root)).collect()
}

/// One layer as a REST node with its children, or `None` when it is skipped.
fn layer(cx: &mut Cx, l: &Value, xd_parent: Affine, origin: Affine, depth: usize, root: bool) -> Option<Built> {
    if depth > MAX_DEPTH {
        cx.warn(format!("layers nested deeper than {MAX_DEPTH} levels were skipped"));
        return None;
    }
    let ty = str_field(l, "type")?;
    if ty == "syncRef" {
        return sync_ref(cx, l, xd_parent, origin, depth, root);
    }
    if !cx.count() {
        return None;
    }
    unsupported(cx, l);
    let world = xd_parent * transform(cx, l);
    let mut built = match ty {
        "group" | "artboard" => group(cx, l, world, origin, depth, root)?,
        "shape" => shape(cx, l, world, origin, root)?,
        "text" => {
            let (mut node, (dx, dy, w, h)) = super::text::node(cx, l)?;
            let bbox = place(cx, l, "TEXT", world * Affine::translate((dx, dy)), w, h, origin, root, &mut node);
            Built { node, bbox }
        }
        other => {
            cx.warn(format!("XD {} layers are not supported yet and were skipped", clip(other)));
            return None;
        }
    };
    if l.get("visible").and_then(Value::as_bool) == Some(false) {
        built.node.insert("visible".into(), json!(false));
    }
    Some(built)
}

/// A `syncRef` layer: a plain copy of the component layer it stands for. It takes the reference's
/// own id, since copies of one component layer appear many times.
fn sync_ref(cx: &mut Cx, l: &Value, xd_parent: Affine, origin: Affine, depth: usize, root: bool) -> Option<Built> {
    let Some(src) = str_field(l, "syncSourceGuid").and_then(|g| cx.symbols.get(g).copied()) else {
        cx.warn("component layers whose definition is missing from the file were skipped");
        return None;
    };
    let weight = weight(src);
    if cx.sync_work.saturating_add(weight) > MAX_SYNC_WORK {
        cx.warn("the file's components copy in more layers than the import handles; the copies past that were skipped");
        return None;
    }
    cx.sync_work += weight;
    let mut built = layer(cx, src, xd_parent, origin, depth + 1, root)?;
    if let Some(guid) = str_field(l, "guid") {
        let id = cx.id(Some(guid));
        built.node.insert("id".into(), json!(id));
    }
    Some(built)
}

/// How many layers `l` holds, itself included (stops counting past [`MAX_SYNC_WORK`]).
fn weight(l: &Value) -> usize {
    let mut n = 0usize;
    let mut stack = vec![l];
    while let Some(v) = stack.pop() {
        n += 1;
        if n > MAX_SYNC_WORK {
            break;
        }
        stack.extend(kids(v));
    }
    n
}

/// Notes about what layer `l` uses that the import doesn't support yet. What it shows is
/// imported all the same, as static layers.
fn unsupported(cx: &mut Cx, l: &Value) {
    let Some(ux) = l.get("meta").and_then(|m| m.get("ux")) else { return };
    let has = |k: &str| ux.get(k).is_some_and(|v| !v.is_null());
    if has("symbolId") {
        cx.warn("components are not supported yet: instances and main components were imported as plain groups");
    }
    if has("repeatGrid") {
        cx.warn("repeat grids are not supported yet: they were imported as plain groups of their cells");
    }
    if has("states") || has("stateId") {
        cx.warn("component states are not supported yet: only the state shown was imported");
    }
    if has("contentStackType") || has("contentPadding") || has("hasContentBackground") {
        cx.warn("stacks and padding are not supported yet: their layers were imported at fixed places");
    }
    if has("scrollingType") || ux.get("fixed").and_then(Value::as_bool) == Some(true) {
        cx.warn("scroll groups and fixed-position layers are not supported yet: they were imported as plain layers");
    }
    if has("interactions") {
        cx.warn("prototype interactions are not supported yet and were skipped");
    }
    if has("linkedElementRef") {
        cx.warn("linked library assets were imported as plain copies");
    }
}

/// The node properties every layer has: type, id, name, visibility, size and place. `world`:
/// the node's place on the canvas, `origin`: its parent node's. Returns the node's box in its
/// parent's space.
#[allow(clippy::too_many_arguments)]
fn place(cx: &mut Cx, l: &Value, ty: &str, world: Affine, w: f64, h: f64, origin: Affine, root: bool, node: &mut JsonMap<String, Value>) -> Rect {
    let (w, h) = (w.clamp(0.0, MAX_SIZE), h.clamp(0.0, MAX_SIZE));
    node.insert("type".into(), json!(ty));
    let id = cx.id(str_field(l, "id"));
    node.insert("id".into(), json!(id));
    let default = match ty {
        "TEXT" => "Text",
        "GROUP" => "Group",
        _ => "Shape",
    };
    let name: String = str_field(l, "name").filter(|n| !n.is_empty()).unwrap_or(default).chars().take(512).collect();
    node.insert("name".into(), json!(name));
    node.insert("visible".into(), json!(true));
    node.insert("size".into(), json!({"x": w, "y": h}));
    let local = origin.inverse() * world;
    node.insert("relativeTransform".into(), rel(local));
    // The page places top-level layers by their transform; deeper ones also say where they end up,
    // which tells the importer their transform is relative to their parent.
    if !root {
        let bb = world.transform_rect_bbox(Rect::new(0.0, 0.0, w, h));
        node.insert("absoluteBoundingBox".into(), json!({"x": bb.x0, "y": bb.y0, "width": bb.width(), "height": bb.height()}));
    }
    local.transform_rect_bbox(Rect::new(0.0, 0.0, w, h))
}

/// A group (or an artboard inside a group, which XD doesn't make): sized to its children, its
/// mask first.
fn group(cx: &mut Cx, l: &Value, world: Affine, origin: Affine, depth: usize, root: bool) -> Option<Built> {
    let mut built = Vec::new();
    let clip_path = l.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("clipPathResources"));
    if let Some(cp) = clip_path {
        for m in children(cx, list(cp, "children"), world, world, depth + 1, false) {
            built.push(as_mask(m));
        }
    }
    built.extend(children(cx, kids(l), world, world, depth + 1, false));
    let Some(b) = union(built.iter().map(|b| b.bbox)) else {
        // An empty group shows nothing.
        return None;
    };
    let built = shift(built, b.x0, b.y0);
    let mut node = JsonMap::new();
    let bbox = place(cx, l, "GROUP", world * Affine::translate((b.x0, b.y0)), b.width(), b.height(), origin, root, &mut node);
    style::apply(cx, l.get("style"), b.width(), b.height(), false, &mut node);
    node.remove("fills");
    node.remove("strokes");
    node.insert("children".into(), Value::Array(built.into_iter().map(|b| Value::Object(b.node)).collect()));
    Some(Built { node, bbox })
}

/// A mask shape: it clips the layers after it and isn't drawn.
fn as_mask(mut b: Built) -> Built {
    for k in ["strokes", "effects", "opacity", "blendMode"] {
        b.node.remove(k);
    }
    b.node.insert("fills".into(), json!([{"type": "SOLID", "color": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}]));
    b.node.insert("isMask".into(), json!(true));
    b.node.insert("maskType".into(), json!("VECTOR"));
    b.node.insert("visible".into(), json!(true));
    b
}

/// A shape layer: rectangle, circle, ellipse, line, path, compound path or polygon.
fn shape(cx: &mut Cx, l: &Value, world: Affine, origin: Affine, root: bool) -> Option<Built> {
    let s = l.get("shape")?;
    let n = |k: &str| num(s, k).unwrap_or(0.0);
    let mut node = JsonMap::new();
    let (ty, local, w, h) = match str_field(s, "type") {
        Some("rect") => {
            corner_radii(s, &mut node);
            ("RECTANGLE", Affine::translate((n("x"), n("y"))), n("width"), n("height"))
        }
        Some("circle") => {
            let r = n("r").abs();
            ("ELLIPSE", Affine::translate((n("cx") - r, n("cy") - r)), 2.0 * r, 2.0 * r)
        }
        Some("ellipse") => {
            let (rx, ry) = (n("rx").abs(), n("ry").abs());
            ("ELLIPSE", Affine::translate((n("cx") - rx, n("cy") - ry)), 2.0 * rx, 2.0 * ry)
        }
        Some("line") => {
            let (x1, y1, x2, y2) = (n("x1"), n("y1"), n("x2"), n("y2"));
            let len = (x2 - x1).hypot(y2 - y1);
            ("LINE", Affine::translate((x1, y1)) * Affine::rotate((y2 - y1).atan2(x2 - x1)), len, 0.0)
        }
        Some(kind @ ("path" | "compound" | "polygon")) => {
            let path = match kind {
                "polygon" => polygon(cx, s)?,
                _ => path_data(cx, s)?,
            };
            let winding = match str_field(s, "winding") {
                Some("evenodd") => WindingRule::EvenOdd,
                Some(_) => WindingRule::NonZero,
                // A compound shape's path is the result of its operation; exclusions overlap.
                None if str_field(s, "operation") == Some("exclude") => WindingRule::EvenOdd,
                None => WindingRule::NonZero,
            };
            let bb = path.bounding_box();
            let moved = Affine::translate((-bb.x0, -bb.y0)) * path;
            let mut net = VectorNetwork::from_path(&moved);
            for r in &mut net.regions {
                r.winding = winding;
            }
            node.insert("vectorNetworkRef".into(), json!(cx.networks.len()));
            cx.networks.push(Some(net));
            ("VECTOR", Affine::translate((bb.x0, bb.y0)), bb.width(), bb.height())
        }
        Some(other) => {
            cx.warn(format!("XD {} shapes are not supported yet and were skipped", clip(other)));
            return None;
        }
        None => return None,
    };
    let bbox = place(cx, l, ty, world * local, w, h, origin, root, &mut node);
    style::apply(cx, l.get("style"), w, h, false, &mut node);
    if ty == "LINE" {
        node.remove("fills");
    }
    Some(Built { node, bbox })
}

/// A rectangle's corner radii (`r`: top left, top right, bottom right, bottom left).
fn corner_radii(s: &Value, node: &mut JsonMap<String, Value>) {
    let radii: Vec<f64> = list(s, "r").iter().take(4).map(|r| r.as_f64().filter(|r| r.is_finite()).map_or(0.0, |r| r.clamp(0.0, MAX_SIZE))).collect();
    match radii.as_slice() {
        [] => {}
        [r] => {
            node.insert("cornerRadius".into(), json!(r));
        }
        [a, rest @ ..] if rest.iter().all(|r| r == a) => {
            node.insert("cornerRadius".into(), json!(a));
        }
        more => {
            let all: Vec<f64> = (0..4).map(|i| more.get(i).copied().unwrap_or(0.0)).collect();
            node.insert("rectangleCornerRadii".into(), json!(all));
        }
    }
}

/// A path or compound shape's `path` (SVG path data).
fn path_data(cx: &mut Cx, s: &Value) -> Option<BezPath> {
    let d = str_field(s, "path")?;
    if d.len() > MAX_PATH_BYTES {
        cx.warn("very long paths were skipped");
        return None;
    }
    match figcraft_doc::geom::svg_path(d) {
        Ok(p) if !p.elements().is_empty() && p.bounding_box().is_finite() => Some(p),
        Ok(_) => None,
        Err(_) => {
            cx.warn("paths with invalid path data were skipped");
            None
        }
    }
}

/// A polygon's corner `points`, closed (rounded corners are left square).
fn polygon(cx: &mut Cx, s: &Value) -> Option<BezPath> {
    if num(s, "uxdesign#cornerRadius").is_some_and(|r| r > 0.0) {
        cx.warn("rounded polygon corners were imported square");
    }
    let pts: Vec<Point> = list(s, "points").iter().take(10_000).filter_map(|p| Some(Point::new(num(p, "x")?, num(p, "y")?))).collect();
    let (first, rest) = pts.split_first()?;
    let mut path = BezPath::new();
    path.move_to(*first);
    for p in rest {
        path.line_to(*p);
    }
    path.close_path();
    Some(path)
}
