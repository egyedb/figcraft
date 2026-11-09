//! XD artboards and layers → REST API file JSON nodes.
//!
//! Layers carry a `transform` (`a`, `b`, `c`, `d`, `tx`, `ty`) relative to their parent group.
//! Inside an artboard the layers are placed on the canvas, not on the artboard: the artboard's
//! own place comes from the resources' `artboards` table. Groups have no size of their own; a
//! group here is sized to what it holds. A group's `meta.ux.clipPathResources` is its mask, a
//! shape in the group's own space. A `syncRef` layer stands for the component layer its
//! `syncSourceGuid` names in the resources' `symbols`; it is copied in.
//!
//! Components: a group with `meta.ux.symbolId` is a component's main when `isMaster` says so,
//! else an instance of it. The main becomes a COMPONENT, each instance an INSTANCE naming it and
//! holding the layers XD shows for it (its overrides included); the engine links them by
//! structure and keeps what differs as overrides. A component whose main isn't on the canvas
//! (linked from a library) gets one made from its definition, on a page of its own. A component
//! with states (`meta.ux.states` on its definition) becomes a component set on that page: its
//! definition is the `State=Default` variant, each other state a variant beside it; its main on
//! the canvas becomes an instance of the default one, and instances name the variant of the state
//! they show (`meta.ux.stateId`).

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
    /// Component definitions by `symbolId`.
    defs: HashMap<&'a str, &'a Value>,
    /// The node id of each component's main, by `symbolId`.
    mains: HashMap<String, String>,
    /// Ids kept for mains (no other layer takes them).
    reserved: HashSet<String>,
    /// Components whose main is made from the definition, in the order instances asked for them.
    needed: Vec<String>,
    /// Stacks and padded groups met, and those laid out with auto layout.
    stacks: (usize, usize),
    /// The styles made from the document's assets, which layers link to.
    pub styles: super::assets::Styles,
    /// Prototype interactions by the id of the layer they start on.
    pub protos: HashMap<&'a str, &'a Value>,
    /// The canvas place of the artboard being mapped.
    pub artboard_at: Option<(f64, f64)>,
    /// How overlays sit, by the artboard they show (`TOP_LEFT` or `MANUAL`).
    pub overlays: HashMap<String, &'static str>,
    /// Where prototypes start: artboard ids and flow names.
    flows: Vec<(String, String)>,
    /// The node id of each other state's variant, by state id; the default state's is its main's.
    pub states: HashMap<String, String>,
    /// Each state's component, by state id.
    state_of: HashMap<String, String>,
    /// How many components or instances the layer being mapped is inside.
    component_depth: usize,
    /// Repeat grids met, and those laid out as grids.
    grids: (usize, usize),
}

impl Cx<'_> {
    /// The component node a state id names: its variant, or for a component's own id (its default
    /// state) its main.
    pub(crate) fn state_node(&self, id: &str) -> Option<String> {
        self.states.get(id).cloned().or_else(|| self.mains.get(id).cloned())
    }
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
    pub(crate) fn id(&mut self, own: Option<&str>) -> String {
        if let Some(id) = own.filter(|i| !i.is_empty() && i.len() <= 128 && !self.reserved.contains(*i))
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
        defs: HashMap::new(),
        mains: HashMap::new(),
        reserved: HashSet::new(),
        needed: Vec::new(),
        stacks: (0, 0),
        styles: super::assets::Styles::default(),
        protos: HashMap::new(),
        artboard_at: None,
        overlays: HashMap::new(),
        flows: Vec::new(),
        states: HashMap::new(),
        state_of: HashMap::new(),
        component_depth: 0,
        grids: (0, 0),
    };
    for (id, list) in archive.protos.as_object().into_iter().flatten() {
        cx.protos.insert(id.as_str(), list);
    }
    index_symbols(&mut cx, &archive.resources);
    find_mains(&mut cx, archive);
    let (defs, styles) = super::assets::definitions(&mut cx, &archive.resources);
    cx.styles = styles;
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
    // Artboards shown as overlays say how they sit.
    for r in &mut roots {
        if let Some(pos) = r.get("id").and_then(Value::as_str).and_then(|id| cx.overlays.get(id).copied()) {
            r["overlayPositionType"] = json!(pos);
        }
    }
    let mut starts: Vec<Value> = Vec::new();
    if let Some(home) = &archive.home
        && let Some(ab) = archive.artboards.iter().find(|a| &a.id == home)
    {
        starts.push(json!({"nodeId": home, "name": ab.name}));
    }
    for (id, name) in std::mem::take(&mut cx.flows) {
        if archive.home.as_deref() != Some(id.as_str()) {
            starts.push(json!({"nodeId": id, "name": name}));
        }
    }
    let mut page = json!({"type": "CANVAS", "id": "0:1", "name": "Page 1", "children": roots});
    if !starts.is_empty() {
        page["flowStartingPoints"] = Value::Array(starts);
    }
    let mut canvases = vec![page];
    let made = made_mains(&mut cx);
    let (grids, as_grids) = cx.grids;
    if as_grids < grids {
        cx.warn(format!("{} of {grids} repeat grids kept their cells at fixed places (their cells didn't make a whole grid)", grids - as_grids));
    }
    let (stacks, laid_out) = cx.stacks;
    if laid_out < stacks {
        cx.warn(format!(
            "{} of {stacks} stacks and padded groups kept their layers at fixed places (their spacing or alignment varies, or they are masked); the rest became auto layout",
            stacks - laid_out
        ));
    }
    if !made.is_empty() {
        cx.warn("components linked from libraries got a main, and components with states a component set of their states, on the page Components");
        canvases.push(json!({"type": "CANVAS", "id": "0:2", "name": "Components", "children": made}));
    }
    let json = json!({"name": name, "document": {"type": "DOCUMENT", "id": "0:0", "children": canvases}, "styleDefinitions": defs});
    Map { json, warnings: cx.warnings, networks: cx.networks }
}

/// Index the layers of the component definitions by id, their states' and masks' too.
fn index_symbols<'a>(cx: &mut Cx<'a>, resources: &'a Value) {
    let symbols = resources.get("resources").and_then(|r| r.get("meta")).and_then(|m| m.get("ux")).and_then(|u| u.get("symbols"));
    let mut stack: Vec<&Value> = symbols.and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    // Other states come after every main definition: an id in both is the main one's.
    let mut states: Vec<&Value> = Vec::new();
    while let Some(n) = stack.pop().or_else(|| states.pop()) {
        if cx.symbols.len() >= MAX_SYMBOL_INDEX {
            cx.warn("the file's components hold more layers than the import reads; some component copies were skipped");
            break;
        }
        if let Some(id) = n.get("id").and_then(Value::as_str) {
            cx.symbols.entry(id).or_insert(n);
        }
        stack.extend(kids(n));
        // A group's mask and a component's other states hold layers references name too.
        let ux = n.get("meta").and_then(|m| m.get("ux"));
        stack.extend(ux.and_then(|u| u.get("clipPathResources")).map_or(&[][..], |c| list(c, "children")));
        states.extend(ux.and_then(|u| u.get("states")).and_then(Value::as_array).into_iter().flatten());
    }
}

/// Note the main of each component on the canvas (the first, when a file has more), and every
/// component definition.
fn find_mains<'a>(cx: &mut Cx<'a>, archive: &'a Archive) {
    let symbols = archive.resources.get("resources").and_then(|r| r.get("meta")).and_then(|m| m.get("ux")).and_then(|u| u.get("symbols"));
    for d in symbols.and_then(Value::as_array).into_iter().flatten() {
        if let Some(sid) = symbol_id(d) {
            cx.defs.entry(sid).or_insert(d);
            for st in states(d) {
                if let Some(id) = str_field(st, "id").filter(|i| !i.is_empty() && *i != sid && i.len() <= 128) {
                    let node = format!("xd-state:{id}");
                    cx.reserved.insert(node.clone());
                    cx.states.entry(id.to_string()).or_insert(node);
                    cx.state_of.entry(id.to_string()).or_insert_with(|| sid.to_string());
                }
            }
        }
    }
    let mut stack: Vec<&Value> = archive.artboards.iter().flat_map(|a| list(&a.content, "children")).collect();
    stack.extend(archive.pasteboard.iter().flat_map(|p| list(p, "children")));
    let mut seen = 0usize;
    while let Some(n) = stack.pop() {
        seen += 1;
        if seen > MAX_SYMBOL_INDEX {
            break;
        }
        // A component with states gets its set on the Components page, where its variants have
        // room: its main on the canvas becomes an instance of the default one.
        if let (Some(sid), Some(id), true) = (symbol_id(n), str_field(n, "id"), is_master(n))
            && !cx.mains.contains_key(sid)
            && cx.defs.get(sid).is_none_or(|d| states(d).is_empty())
        {
            cx.mains.insert(sid.to_string(), id.to_string());
            cx.reserved.insert(id.to_string());
        }
        stack.extend(kids(n));
    }
}

/// A layer's `meta.ux.symbolId`: it is a component's main or instance.
fn symbol_id(n: &Value) -> Option<&str> {
    n.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("symbolId")).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// A component definition's other states.
fn states(d: &Value) -> &[Value] {
    d.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("states")).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn is_master(n: &Value) -> bool {
    n.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("isMaster")).and_then(Value::as_bool) == Some(true)
}

/// What a group is: a component's main (`COMPONENT`, with the id its instances name), an
/// instance (`INSTANCE`, with its main's id) or a plain group.
fn group_kind(cx: &mut Cx, l: &Value) -> (&'static str, Option<String>) {
    let Some(sid) = symbol_id(l) else { return ("GROUP", None) };
    let main = cx.mains.get(sid).cloned();
    if is_master(l) && main.is_some() && main.as_deref() == str_field(l, "id") {
        return ("COMPONENT", main);
    }
    // An instance showing another state names that state's variant.
    let shown = l.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("stateId")).and_then(Value::as_str).filter(|s| *s != sid);
    let variant = shown.filter(|s| cx.state_of.get(*s).is_some_and(|c| c == sid)).and_then(|s| cx.states.get(s).cloned());
    if let Some(m) = main {
        return ("INSTANCE", Some(variant.unwrap_or(m)));
    }
    if cx.defs.contains_key(sid) {
        // A main made from the definition, on the Components page.
        let id = format!("xd-main:{}", sid.chars().take(100).collect::<String>());
        cx.reserved.insert(id.clone());
        cx.mains.insert(sid.to_string(), id.clone());
        cx.needed.push(sid.to_string());
        return ("INSTANCE", Some(variant.unwrap_or(id)));
    }
    cx.warn("instances of components whose definition is missing from the file were imported as plain groups");
    ("GROUP", None)
}

/// The mains made from definitions, in a row on their own page. Making one can ask for more
/// (components nested in it).
fn made_mains(cx: &mut Cx) -> Vec<Value> {
    let mut out = Vec::new();
    let mut x = 0.0;
    let mut i = 0;
    while let Some(sid) = cx.needed.get(i).cloned() {
        i += 1;
        let (Some(def), Some(id)) = (cx.defs.get(sid.as_str()).copied(), cx.mains.get(&sid).cloned()) else { continue };
        if !cx.count() {
            break;
        }
        let world = transform(cx, def);
        let Some(b) = group(cx, def, world, Affine::IDENTITY, 1, true, "COMPONENT", Some(id)) else { continue };
        let mut b = with_states(cx, def, &sid, b, world, Affine::IDENTITY, 1, true);
        // In a row, 100 px apart, tops at 0.
        let (dx, dy) = (x - b.bbox.x0, -b.bbox.y0);
        x += b.bbox.width() + 100.0;
        move_by(&mut b, dx, dy, Affine::IDENTITY);
        out.push(Value::Object(b.node));
    }
    out
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
    cx.artboard_at = Some((ab.x, ab.y));
    let built = children(cx, kids(root), Affine::IDENTITY, origin, 1, false);
    cx.artboard_at = None;
    if let Some(flow) = root.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("flowName")).and_then(Value::as_str) {
        cx.flows.push((ab.id.clone(), flow.chars().take(200).collect()));
    }
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
    let ints = super::proto::interactions(cx, &ab.id, Some((ab.x, ab.y)));
    if !ints.is_empty() {
        node.insert("interactions".into(), Value::Array(ints));
    }
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
pub(crate) fn shift(built: Vec<Built>, dx: f64, dy: f64) -> Vec<Built> {
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
        "group" | "artboard" => {
            let (ty, main) = group_kind(cx, l);
            let b = group(cx, l, world, origin, depth, root, ty, main)?;
            match symbol_id(l).filter(|_| ty == "COMPONENT") {
                Some(sid) => with_states(cx, l, sid, b, world, origin, depth, root),
                None => b,
            }
        }
        "shape" => shape(cx, l, world, origin, root)?,
        "text" => {
            let super::text::Laid { mut node, rect, clip } = super::text::node(cx, l)?;
            match clip {
                None => {
                    let (dx, dy, w, h) = rect;
                    let bbox = place(cx, l, "TEXT", world * Affine::translate((dx, dy)), w, h, origin, root, &mut node);
                    Built { node, bbox }
                }
                Some(size) => clipped_text(cx, l, node, world, rect, size, origin, root),
            }
        }
        other => {
            cx.warn(format!("XD {} layers are not supported yet and were skipped", clip(other)));
            return None;
        }
    };
    if l.get("visible").and_then(Value::as_bool) == Some(false) {
        built.node.insert("visible".into(), json!(false));
    }
    if let Some(id) = str_field(l, "id") {
        link_protos(cx, id, &mut built);
    }
    Some(built)
}

/// The prototype interactions that start on layer `id`, onto its node.
fn link_protos(cx: &mut Cx, id: &str, built: &mut Built) {
    if !cx.protos.contains_key(id) {
        return;
    }
    let bb = built.node.get("absoluteBoundingBox");
    let at = bb.and_then(|b| Some((b.get("x")?.as_f64()?, b.get("y")?.as_f64()?)));
    let ints = super::proto::interactions(cx, id, at);
    if !ints.is_empty() {
        built.node.insert("interactions".into(), Value::Array(ints));
    }
}

/// A `syncRef` layer: a plain copy of the component layer it stands for. It takes the reference's
/// own id, since copies of one component layer appear many times.
fn sync_ref(cx: &mut Cx, l: &Value, xd_parent: Affine, origin: Affine, depth: usize, root: bool) -> Option<Built> {
    let Some(src) = str_field(l, "syncSourceGuid").and_then(|g| cx.symbols.get(g).copied()) else {
        cx.warn("component layers whose definition is missing from the file were skipped");
        return None;
    };
    // A reference that lists children of its own keeps them (they carry the instance's
    // overrides, an image or a text changed); only the layer's own properties come from the
    // definition.
    let own = l.get("group").filter(|g| g.get("children").is_some());
    let merged;
    let src = match own {
        Some(group) => {
            let mut m: JsonMap<String, Value> = src
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, _)| k.as_str() != "group" && k.as_str() != "artboard")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            m.insert("group".into(), group.clone());
            merged = Value::Object(m);
            &merged
        }
        None => src,
    };
    // Children of its own are counted as the walk meets them; a whole definition's up front.
    let weight = if own.is_some() { 1 } else { weight(src) };
    if cx.sync_work.saturating_add(weight) > MAX_SYNC_WORK {
        cx.warn("the file's components copy in more layers than the import handles; the copies past that were skipped");
        return None;
    }
    cx.sync_work += weight;
    let mut built = layer(cx, src, xd_parent, origin, depth + 1, root)?;
    if let Some(guid) = str_field(l, "guid") {
        let id = cx.id(Some(guid));
        built.node.insert("id".into(), json!(id));
        link_protos(cx, guid, &mut built);
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
    if has("scrollingType") || ux.get("fixed").and_then(Value::as_bool) == Some(true) {
        cx.warn("scroll groups and fixed-position layers are not supported yet: they were imported as plain layers");
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
    let name: String =
        str_field(l, "name").filter(|n| !n.is_empty()).unwrap_or(default).chars().filter(|c| !c.is_control() || *c == '\n').take(512).collect();
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

/// A text whose fixed box shows only part of it, in a frame of that box that clips the rest
/// (XD hides the lines that don't fit; FigCraft text would show them).
#[allow(clippy::too_many_arguments)]
fn clipped_text(
    cx: &mut Cx,
    l: &Value,
    mut text: JsonMap<String, Value>,
    world: Affine,
    (dx, dy, w, h): (f64, f64, f64, f64),
    (cw, ch): (f64, f64),
    origin: Affine,
    root: bool,
) -> Built {
    cx.warn("text that overflows its fixed box was put in a clipping frame of that box, as XD hides what doesn't fit");
    let mut frame = JsonMap::new();
    let bbox = place(cx, &json!({"name": str_field(l, "name").unwrap_or("Text")}), "FRAME", world, cw, ch, origin, root, &mut frame);
    frame.insert("clipsContent".into(), json!(true));
    frame.insert("fills".into(), json!([]));
    place(cx, l, "TEXT", world * Affine::translate((dx, dy)), w, h, world, false, &mut text);
    frame.insert("children".into(), json!([Value::Object(text)]));
    Built { node: frame, bbox }
}

/// A group (or an artboard inside a group, which XD doesn't make): sized to its children, its
/// mask first.
/// `ty`: `GROUP`, or `COMPONENT` / `INSTANCE` with `main` the main's id.
#[allow(clippy::too_many_arguments)]
fn group(cx: &mut Cx, l: &Value, world: Affine, origin: Affine, depth: usize, root: bool, ty: &str, main: Option<String>) -> Option<Built> {
    let mut built = Vec::new();
    // Inside a component or instance, layers can't become components of their own.
    let inside = matches!(ty, "COMPONENT" | "INSTANCE");
    if inside {
        cx.component_depth += 1;
    }
    let clip_path = l.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("clipPathResources"));
    if let Some(cp) = clip_path {
        for m in children(cx, list(cp, "children"), world, world, depth + 1, false) {
            built.push(as_mask(m));
        }
    }
    let masked = !built.is_empty();
    // The layers with their place in the group (a stack's background is its first).
    let mut own: Vec<(usize, Built)> =
        kids(l).iter().enumerate().filter_map(|(i, k)| layer(cx, k, world, world, depth + 1, false).map(|b| (i, b))).collect();
    if inside {
        cx.component_depth -= 1;
    }
    let mut auto = None;
    // A repeat grid: grid auto layout of its cells.
    if let Some(g) = super::grid::ask(l) {
        cx.grids.0 += 1;
        if !masked {
            let name = str_field(l, "name").unwrap_or("");
            let components = cx.component_depth == 0 && !inside;
            match super::grid::layout(cx, &g, std::mem::take(&mut own), kids(l), world, name, components) {
                Ok((props, cells)) => {
                    own = cells;
                    auto = Some(props);
                    cx.grids.1 += 1;
                }
                Err(cells) => own = cells,
            }
        }
    } else if l.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("repeatGrid")).is_some() {
        cx.grids.0 += 1;
    }
    // A stack or padded group: auto layout when that keeps every layer where XD put it.
    if auto.is_none()
        && let Some(mut ask) = super::stack::ask(l)
    {
        cx.stacks.0 += 1;
        // A reference's gap is its definition's.
        ask.gaps = kids(l)
            .iter()
            .map(|k| {
                let k = if str_field(k, "type") == Some("syncRef") {
                    str_field(k, "syncSourceGuid").and_then(|g| cx.symbols.get(g).copied()).unwrap_or(k)
                } else {
                    k
                };
                k.get("meta")
                    .and_then(|m| m.get("ux"))
                    .and_then(|u| u.get("contentStackMarginAfter"))
                    .and_then(Value::as_f64)
                    .filter(|v| v.is_finite())
            })
            .collect();
        if !masked {
            auto = super::stack::layout(&ask, &mut own);
        }
        if auto.is_some() {
            cx.stacks.1 += 1;
        }
    }
    built.extend(own.into_iter().map(|(_, b)| b));
    let Some(b) = union(built.iter().map(|b| b.bbox)) else {
        // An empty group shows nothing.
        return None;
    };
    let built = shift(built, b.x0, b.y0);
    let mut node = JsonMap::new();
    // Auto layout needs a frame.
    let ty = if auto.is_some() && ty == "GROUP" { "FRAME" } else { ty };
    let bbox = place(cx, l, ty, world * Affine::translate((b.x0, b.y0)), b.width(), b.height(), origin, root, &mut node);
    style::apply(cx, l.get("style"), b.width(), b.height(), false, &mut node);
    node.remove("fills");
    node.remove("strokes");
    node.remove("styles");
    match (ty, main) {
        ("COMPONENT", Some(id)) => {
            node.insert("id".into(), json!(id));
            node.insert("fills".into(), json!([]));
            node.insert("clipsContent".into(), json!(false));
        }
        ("INSTANCE", Some(id)) => {
            node.insert("componentId".into(), json!(id));
            node.insert("fills".into(), json!([]));
            node.insert("clipsContent".into(), json!(false));
        }
        ("FRAME", _) => {
            node.insert("fills".into(), json!([]));
            node.insert("clipsContent".into(), json!(false));
        }
        _ => {}
    }
    if let Some(a) = auto {
        node.extend(a);
    }
    node.insert("children".into(), Value::Array(built.into_iter().map(|b| Value::Object(b.node)).collect()));
    Some(Built { node, bbox })
}

/// Main `main` (built from `l`, component `sid`, placed at `world`) as a component set when its
/// definition has other states: the main as `State=Default`, each other state a variant in a row
/// to its right, 40 px apart. Other mains as they are.
#[allow(clippy::too_many_arguments)]
fn with_states(cx: &mut Cx, l: &Value, sid: &str, mut main: Built, world: Affine, origin: Affine, depth: usize, root: bool) -> Built {
    let Some(def) = cx.defs.get(sid).copied() else { return main };
    let others = states(def);
    if others.is_empty() {
        return main;
    }
    let mut used = std::collections::HashSet::new();
    let mut name_of = |raw: &str| {
        // A variant's name holds `Property=Value` pairs separated by commas.
        let spaced: String = raw.chars().map(|c| if c == ',' || c == '=' { ' ' } else { c }).collect();
        let clean: String = spaced.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(100).collect();
        let base = if clean.is_empty() { "State".to_string() } else { clean };
        let mut name = base.clone();
        let mut n = 2;
        while !used.insert(name.to_lowercase()) {
            name = format!("{base} {n}");
            n += 1;
        }
        format!("State={name}")
    };
    main.node.insert("name".into(), json!(name_of("Default")));
    let mut variants = vec![main];
    for st in others.iter().take(256) {
        let Some(id) = str_field(st, "id").and_then(|i| cx.states.get(i).cloned()) else { continue };
        let name = name_of(str_field(st, "name").unwrap_or("State"));
        // A state is laid out like its component's main.
        let Some(mut v) = group(cx, st, world, origin, depth, false, "COMPONENT", Some(id)) else { continue };
        v.node.insert("name".into(), json!(name));
        variants.push(v);
    }
    // In a row from the main's left, tops level with it.
    let mut x = variants.first().map_or(0.0, |v| v.bbox.x1 + 40.0);
    let top = variants.first().map_or(0.0, |v| v.bbox.y0);
    for v in variants.iter_mut().skip(1) {
        let (dx, dy) = (x - v.bbox.x0, top - v.bbox.y0);
        x += v.bbox.width() + 40.0;
        move_by(v, dx, dy, origin);
    }
    // Inside the set every variant says where it ends up (the main may have been a top-level layer).
    for v in &mut variants {
        let bb = origin.transform_rect_bbox(v.bbox);
        v.node.insert("absoluteBoundingBox".into(), json!({"x": bb.x0, "y": bb.y0, "width": bb.width(), "height": bb.height()}));
    }
    let Some(b) = union(variants.iter().map(|v| v.bbox)) else { return variants.remove(0) };
    let variants = shift(variants, b.x0, b.y0);
    let mut set = JsonMap::new();
    let bbox = place(cx, l, "COMPONENT_SET", origin * Affine::translate((b.x0, b.y0)), b.width(), b.height(), origin, root, &mut set);
    set.insert("id".into(), json!(format!("xd-set:{}", sid.chars().take(100).collect::<String>())));
    set.insert("fills".into(), json!([]));
    set.insert("clipsContent".into(), json!(false));
    set.insert("children".into(), Value::Array(variants.into_iter().map(|v| Value::Object(v.node)).collect()));
    let _ = depth;
    Built { node: set, bbox }
}

/// Move built node `b` by `dx`, `dy` in its parent's space (the canvas for a top-level one): its
/// transform, its box and the absolute boxes of everything in it, which the importer reads to tell
/// which space a group's layers use.
fn move_by(b: &mut Built, dx: f64, dy: f64, parent: Affine) {
    if let Some(rt) = b.node.get_mut("relativeTransform").and_then(Value::as_array_mut) {
        for (row, d) in rt.iter_mut().zip([dx, dy]) {
            if let Some(e) = row.get_mut(2) {
                *e = json!(e.as_f64().unwrap_or(0.0) + d);
            }
        }
    }
    b.bbox = b.bbox + figcraft_doc::geom::Vec2::new(dx, dy);
    // The same move on the canvas (`parent`: the parent's place there).
    let [a, bb, c, d, _, _] = parent.as_coeffs();
    let (dx, dy) = (a * dx + c * dy, bb * dx + d * dy);
    let mut stack: Vec<&mut Value> = Vec::new();
    if let Some(bb) = b.node.get_mut("absoluteBoundingBox") {
        shift_box(bb, dx, dy);
    }
    if let Some(Value::Array(kids)) = b.node.get_mut("children") {
        stack.extend(kids.iter_mut());
    }
    while let Some(n) = stack.pop() {
        let Some(m) = n.as_object_mut() else { continue };
        if let Some(bb) = m.get_mut("absoluteBoundingBox") {
            shift_box(bb, dx, dy);
        }
        if let Some(Value::Array(kids)) = m.get_mut("children") {
            stack.extend(kids.iter_mut());
        }
    }
}

fn shift_box(bb: &mut Value, dx: f64, dy: f64) {
    for (k, d) in [("x", dx), ("y", dy)] {
        if let Some(e) = bb.get_mut(k) {
            *e = json!(e.as_f64().unwrap_or(0.0) + d);
        }
    }
}

/// A mask shape: it clips the layers after it and isn't drawn.
fn as_mask(mut b: Built) -> Built {
    for k in ["strokes", "effects", "opacity", "blendMode", "styles"] {
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

/// A polygon's corner `points`, closed, its corners rounded by `uxdesign#cornerRadius`.
fn polygon(_cx: &mut Cx, s: &Value) -> Option<BezPath> {
    let pts: Vec<Point> = list(s, "points").iter().take(10_000).filter_map(|p| Some(Point::new(num(p, "x")?, num(p, "y")?))).collect();
    // Rounded corners: every vertex rounded by the radius (clamped so the arcs fit).
    if let Some(r) = num(s, "uxdesign#cornerRadius").filter(|r| *r > 0.0)
        && pts.len() >= 3
    {
        return Some(figcraft_doc::geom::rounded_polygon(&pts, r.min(MAX_SIZE)));
    }
    let (first, rest) = pts.split_first()?;
    let mut path = BezPath::new();
    path.move_to(*first);
    for p in rest {
        path.line_to(*p);
    }
    path.close_path();
    Some(path)
}
