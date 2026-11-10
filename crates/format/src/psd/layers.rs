//! The layer tree → REST API file JSON nodes.
//!
//! Every node is built with its place on the page (`world`: its own box `0..w × 0..h` → page) and
//! made relative to its parent when the parent is built. The same layers can be built more than
//! once at different places (a component's main and each of its instances); building is
//! deterministic and the pictures it makes are cached, so the copies match.

use std::rc::Rc;

use figcraft_doc::geom::{Affine, Rect};
use figcraft_psd::tagged::{BlockData, TaggedBlock};
use figcraft_psd::{LayerNode, LayerRecord, PsdFile};
use serde_json::{Map as JsonMap, Value, json};

use super::effects::{self, Fx};
use super::util::{self, MAX_SIZE, bbox, desc, num, rel};
use super::{Shared, fills, shape, smart, text};

/// Deepest group nesting followed.
const MAX_DEPTH: usize = 200;
/// Room between the import and the preview of Photoshop's own picture.
const PREVIEW_GAP: f64 = 200.0;

/// Adjustment layer blocks: FigCraft has no adjustment layers.
const ADJUSTMENTS: [&[u8; 4]; 18] = [
    b"levl", b"curv", b"brit", b"hue2", b"hue ", b"blnc", b"selc", b"mixr", b"grdm", b"phfl", b"expA", b"vibA", b"nvrt", b"post", b"thrs", b"clrL",
    b"blwh", b"CgEd",
];

/// The file being mapped (the opened PSD, or a file a smart object places).
pub(crate) struct FileCx {
    pub file: Rc<PsdFile>,
    /// Keys this file's pictures in the image cache.
    pub id: usize,
    pub w: f64,
    pub h: f64,
    /// The global light angle in degrees (effects that use it).
    pub angle: f64,
    /// The files its smart objects place.
    pub links: Vec<smart::Link>,
    /// The document's text engine data (`Txt2`), where named styles live.
    pub txt2: Rc<Option<figcraft_psd::engine_data::Value>>,
}

impl FileCx {
    pub fn new(sh: &mut Shared, file: Rc<PsdFile>) -> Self {
        let id = sh.next_file;
        sh.next_file += 1;
        Self::with_id(sh, file, id)
    }

    /// The context of `file`, its pictures keyed by `id`.
    pub fn with_id(sh: &mut Shared, file: Rc<PsdFile>, id: usize) -> Self {
        let txt2 = sh
            .txt2_cache
            .entry(id)
            .or_insert_with(|| {
                Rc::new(file.global_blocks.iter().find(|b| &b.key == b"Txt2").and_then(|b| {
                    let mut w = Vec::with_capacity(b.data.len() + 4);
                    w.extend_from_slice(b"<<");
                    w.extend_from_slice(&b.data);
                    w.extend_from_slice(b">>");
                    figcraft_psd::engine_data::parse(&w).ok()
                }))
            })
            .clone();
        let angle = file
            .resource(1037)
            .and_then(|r| r.data.get(..4))
            .map_or(120.0, |b| f64::from(i32::from_be_bytes([b[0], b[1], b[2], b[3]])))
            .clamp(-360.0, 360.0);
        let links = smart::links(&file);
        FileCx { w: f64::from(file.header.width), h: f64::from(file.header.height), file, id, angle, links, txt2 }
    }

    pub fn record(&self, index: usize) -> Option<&LayerRecord> {
        self.file.layers().get(index)
    }
}

/// A node being built: its REST fields and its box's place on the page.
pub(crate) struct Built {
    pub node: JsonMap<String, Value>,
    pub world: Affine,
    pub w: f64,
    pub h: f64,
}

impl Built {
    pub fn new(sh: &mut Shared, ty: &str, name: &str, world: Affine, w: f64, h: f64) -> Self {
        let mut node = JsonMap::new();
        node.insert("type".into(), json!(ty));
        node.insert("id".into(), json!(sh.id()));
        node.insert("name".into(), json!(clip_name(name)));
        Built { node, world, w: w.clamp(0.0, MAX_SIZE), h: h.clamp(0.0, MAX_SIZE) }
    }

    /// Its page bounds.
    pub fn bounds(&self) -> Rect {
        bbox(self.world, self.w, self.h)
    }

    fn take(&mut self, key: &str) -> Option<Value> {
        self.node.remove(key)
    }
}

pub(crate) fn clip_name(name: &str) -> String {
    name.chars().take(256).collect()
}

/// `kids` as the REST children of a node placed at `parent` on the page.
pub(crate) fn attach(sh: &mut Shared, parent: Affine, kids: Vec<Built>) -> Value {
    let det = parent.determinant();
    let inv = if det.abs() > 1e-12 { parent.inverse() } else { Affine::IDENTITY };
    Value::Array(
        kids.into_iter()
            .map(|mut k| {
                unscale(sh, &mut k);
                let b = k.bounds();
                k.node.insert("relativeTransform".into(), rel(inv * k.world));
                k.node.insert("size".into(), json!({"x": k.w, "y": k.h}));
                k.node.insert("absoluteBoundingBox".into(), json!({"x": b.x0, "y": b.y0, "width": b.width(), "height": b.height()}));
                Value::Object(k.node)
            })
            .collect(),
    )
}

/// A layer placed with a scale (inside a scaled instance) keeps its look at scale 1: its size,
/// path, type sizes, strokes, corners and effects take the scale instead.
fn unscale(sh: &mut Shared, k: &mut Built) {
    let Some((placed, sx, sy)) = util::split_scale(k.world) else { return };
    if (sx - 1.0).abs() < 1e-9 && (sy - 1.0).abs() < 1e-9 {
        return;
    }
    k.world = placed;
    k.w = (k.w * sx).clamp(0.0, MAX_SIZE);
    k.h = (k.h * sy).clamp(0.0, MAX_SIZE);
    let s = (sx * sy).sqrt();
    if let Some(i) = k.node.get("vectorNetworkRef").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok())
        && let Some(Some(net)) = sh.networks.get_mut(i)
    {
        net.scale(sx, sy);
    }
    let times = |v: &mut Value, f: f64| {
        if let Some(x) = v.as_f64() {
            *v = json!(x * f);
        }
    };
    for key in ["strokeWeight", "cornerRadius"] {
        if let Some(v) = k.node.get_mut(key) {
            times(v, s);
        }
    }
    for key in ["rectangleCornerRadii", "strokeDashes"] {
        if let Some(Value::Array(a)) = k.node.get_mut(key) {
            a.iter_mut().for_each(|v| times(v, s));
        }
    }
    if let Some(Value::Array(fx)) = k.node.get_mut("effects") {
        for e in fx {
            for key in ["radius", "spread"] {
                if let Some(v) = e.get_mut(key) {
                    times(v, s);
                }
            }
            if let Some(o) = e.get_mut("offset") {
                if let Some(v) = o.get_mut("x") {
                    times(v, sx);
                }
                if let Some(v) = o.get_mut("y") {
                    times(v, sy);
                }
            }
        }
    }
    // Type scales with the height.
    let text_scale = |st: &mut Value| {
        for key in ["fontSize", "lineHeightPx", "letterSpacing"] {
            if let Some(v) = st.get_mut(key) {
                times(v, sy);
            }
        }
    };
    if let Some(st) = k.node.get_mut("style") {
        text_scale(st);
    }
    if let Some(Value::Object(t)) = k.node.get_mut("styleOverrideTable") {
        t.values_mut().for_each(text_scale);
    }
}

/// A group of `kids` (`None` when there are none), sized to what it holds.
pub(crate) fn group(sh: &mut Shared, name: &str, kids: Vec<Built>) -> Option<Built> {
    let b = kids.iter().map(Built::bounds).reduce(|a, b| a.union(b))?;
    let world = Affine::translate((b.x0, b.y0));
    let mut g = Built::new(sh, "GROUP", name, world, b.width(), b.height());
    g.node.insert("children".into(), attach(sh, world, kids));
    Some(g)
}

/// Map a whole Photoshop file to a REST file JSON named `name`.
pub(crate) fn to_rest(sh: &mut Shared, file: Rc<PsdFile>, name: &str) -> Value {
    let fx = FileCx::new(sh, file);
    let tree = fx.file.layer_tree_max_depth(MAX_DEPTH);
    let guides = guides(&fx.file);
    let doc_rect = Rect::new(0.0, 0.0, fx.w, fx.h);
    let mut roots: Vec<Built> = Vec::new();
    let artboards = tree.iter().any(|n| n.is_group() && fx.record(n.index()).is_some_and(|r| artboard_rect(r).is_some()));
    if artboards {
        let mut loose = Vec::new();
        for n in &tree {
            match fx.record(n.index()).and_then(artboard_rect) {
                Some(r) if n.is_group() => roots.extend(artboard(sh, &fx, n, r, &guides)),
                _ => loose.extend(build_list(sh, &fx, std::slice::from_ref(n), Affine::IDENTITY, 1)),
            }
        }
        roots.extend(loose);
    } else {
        let mut kids = build_list(sh, &fx, &tree, Affine::IDENTITY, 1);
        if tree.is_empty() {
            // A flattened file: its picture is all there is.
            kids.extend(composite(sh, &fx, Affine::IDENTITY, name));
        }
        let mut frame = Built::new(sh, "FRAME", name, Affine::IDENTITY, fx.w, fx.h);
        frame.node.insert("fills".into(), json!([{"type": "SOLID", "color": {"r": 1.0, "g": 1.0, "b": 1.0, "a": 1.0}}]));
        frame.node.insert("clipsContent".into(), json!(true));
        frame.node.insert("children".into(), attach(sh, Affine::IDENTITY, kids));
        if !guides.is_empty() {
            frame.node.insert("guides".into(), guide_json(&guides, 0.0, 0.0, None));
        }
        roots.push(frame);
    }
    if sh.preview && !tree.is_empty() {
        let right = roots.iter().map(Built::bounds).fold(doc_rect, |a, b| a.union(b)).x1;
        let at = Affine::translate((right + PREVIEW_GAP, 0.0));
        let kids: Vec<Built> = composite(sh, &fx, at, "Photoshop picture").into_iter().collect();
        if kids.is_empty() {
            sh.warn("the file has no flattened picture (saved without Maximize Compatibility), so no preview was put beside the import");
        } else {
            let mut frame = Built::new(sh, "FRAME", &format!("{name} (Photoshop preview)"), at, fx.w, fx.h);
            frame.node.insert("fills".into(), json!([]));
            frame.node.insert("locked".into(), json!(true));
            frame.node.insert("children".into(), attach(sh, at, kids));
            roots.push(frame);
        }
    }
    let mut canvases = vec![json!({"type": "CANVAS", "id": "0:1", "name": "Page 1", "children": attach(sh, Affine::IDENTITY, roots)})];
    if !sh.mains.is_empty() {
        canvases.push(json!({"type": "CANVAS", "id": "0:2", "name": "Components", "children": std::mem::take(&mut sh.mains)}));
    }
    json!({"name": name, "document": {"type": "DOCUMENT", "id": "0:0", "children": canvases}})
}

/// The flattened picture of the file as an image layer placed by `base`.
pub(crate) fn composite(sh: &mut Shared, fx: &FileCx, base: Affine, name: &str) -> Option<Built> {
    let key = format!("{}:composite", fx.id);
    let r = cached_image(sh, key, || {
        let img = fx.file.composite_rgba8().ok()?;
        util::png(img.width, img.height, img.data)
    })?;
    let mut b = Built::new(sh, "RECTANGLE", name, base, fx.w, fx.h);
    b.node.insert("fills".into(), json!([{"type": "IMAGE", "scaleMode": "FILL", "imageRef": r}]));
    Some(b)
}

/// An image made once per `key`: its `imageRef`.
pub(crate) fn cached_image(sh: &mut Shared, key: String, make: impl FnOnce() -> Option<Vec<u8>>) -> Option<String> {
    if let Some(r) = sh.image_cache.get(&key) {
        return r.clone();
    }
    let r = make().and_then(|png| sh.add_image(png));
    sh.image_cache.insert(key, r.clone());
    r
}

/// The ruler guides of image resource 1032: `(vertical, position in pixels)`.
pub(crate) fn guides(file: &PsdFile) -> Vec<(bool, f64)> {
    let Some(d) = file.resource(1032).map(|r| r.data.as_slice()) else { return Vec::new() };
    let u32_at = |at: usize| d.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let Some(count) = u32_at(12).and_then(|c| usize::try_from(c).ok()) else { return Vec::new() };
    (0..count.min(10_000))
        .map_while(|i| {
            let at = 16 + i * 5;
            let g = d.get(at..at + 5)?;
            let pos = f64::from(i32::from_be_bytes([g[0], g[1], g[2], g[3]])) / 32.0;
            Some((g[4] == 0, pos))
        })
        .collect()
}

/// Guides as a REST `guides` list, moved by (`dx`, `dy`); `within`: only those crossing it.
fn guide_json(guides: &[(bool, f64)], dx: f64, dy: f64, within: Option<Rect>) -> Value {
    Value::Array(
        guides
            .iter()
            .filter(|(v, p)| within.is_none_or(|r| if *v { (r.x0..=r.x1).contains(p) } else { (r.y0..=r.y1).contains(p) }))
            .map(|(v, p)| json!({"axis": if *v { "X" } else { "Y" }, "offset": if *v { p - dx } else { p - dy }}))
            .collect(),
    )
}

/// An artboard's rectangle (`artb`), in document pixels.
fn artboard_rect(rec: &LayerRecord) -> Option<Rect> {
    let d = rec.block(b"artb").and_then(|b| util::versioned(&b.data))?;
    let r = desc(&d, "artboardRect")?;
    let (t, l, b, rt) = (num(r, "Top ")?, num(r, "Left")?, num(r, "Btom")?, num(r, "Rght")?);
    (rt > l && b > t).then(|| Rect::new(l, t, rt, b))
}

/// An artboard group as a frame.
fn artboard(sh: &mut Shared, fx: &FileCx, node: &LayerNode, r: Rect, guides: &[(bool, f64)]) -> Option<Built> {
    let rec = fx.record(node.index())?;
    if !sh.count() {
        return None;
    }
    let kids = build_list(sh, fx, node.children(), Affine::IDENTITY, 2);
    let world = Affine::translate((r.x0, r.y0));
    let mut f = Built::new(sh, "FRAME", &rec.name(), world, r.width(), r.height());
    let d = rec.block(b"artb").and_then(|b| util::versioned(&b.data));
    let fill = match d.as_ref().and_then(|d| num(d, "artboardBackgroundType")).map(|t| t as i64) {
        Some(2) => json!([{"type": "SOLID", "color": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}]),
        Some(3) => json!([]),
        Some(4) => match d.as_ref().and_then(|d| desc(d, "Clr ")).and_then(util::color) {
            Some(c) => json!([{"type": "SOLID", "color": util::rgba(c, 1.0)}]),
            None => json!([]),
        },
        _ => json!([{"type": "SOLID", "color": {"r": 1.0, "g": 1.0, "b": 1.0, "a": 1.0}}]),
    };
    f.node.insert("fills".into(), fill);
    f.node.insert("clipsContent".into(), json!(true));
    f.node.insert("visible".into(), json!(rec.is_visible()));
    f.node.insert("children".into(), attach(sh, world, kids));
    let g = guide_json(guides, r.x0, r.y0, Some(r));
    if g.as_array().is_some_and(|a| !a.is_empty()) {
        f.node.insert("guides".into(), g);
    }
    Some(f)
}

/// The layers `nodes` (bottom to top) placed by `base` (file pixels → page). Layers clipped to
/// the one below them become a masked group with it.
pub(crate) fn build_list(sh: &mut Shared, fx: &FileCx, nodes: &[LayerNode], base: Affine, depth: usize) -> Vec<Built> {
    let mut out = Vec::new();
    let clipped = |n: &LayerNode| fx.record(n.index()).is_some_and(|r| r.clipping != 0);
    let mut i = 0;
    while i < nodes.len() {
        let Some(first) = nodes.get(i) else { break };
        let mut j = i + 1;
        while nodes.get(j).is_some_and(clipped) {
            j += 1;
        }
        let built = build_node(sh, fx, first, base, depth);
        let rest = nodes.get(i + 1..j).unwrap_or(&[]);
        match built {
            Some(b) if !rest.is_empty() => out.extend(clip_group(sh, fx, first, b, rest, base, depth)),
            Some(b) => out.push(b),
            None if !rest.is_empty() => {
                // Clipped to an empty layer: Photoshop shows nothing of them either.
                sh.note("layers clipped to an empty layer were left out");
            }
            None => {}
        }
        i = j;
    }
    out
}

/// A clipping mask: the base layer, a copy of it masking the layers clipped to it, in a group.
fn clip_group(sh: &mut Shared, fx: &FileCx, base_node: &LayerNode, base: Built, rest: &[LayerNode], at: Affine, depth: usize) -> Option<Built> {
    let name = base.node.get("name").and_then(Value::as_str).unwrap_or("Clipping mask").to_string();
    let hidden = base.node.get("visible").and_then(Value::as_bool) == Some(false);
    let mut kids = Vec::new();
    if let Some(mut mask) = build_node(sh, fx, base_node, at, depth) {
        for k in ["effects", "opacity", "blendMode", "visible", "locked"] {
            mask.node.remove(k);
        }
        mask.node.insert("isMask".into(), json!(true));
        mask.node.insert("name".into(), json!(format!("{} (clipping mask)", clip_name(&name))));
        kids.push(mask);
    }
    kids.push(base);
    kids.extend(rest.iter().filter_map(|n| build_node(sh, fx, n, at, depth)));
    let mut g = group(sh, &name, kids)?;
    if hidden {
        g.node.insert("visible".into(), json!(false));
    }
    Some(g)
}

/// One layer or group.
fn build_node(sh: &mut Shared, fx: &FileCx, node: &LayerNode, base: Affine, depth: usize) -> Option<Built> {
    let index = node.index();
    let rec = fx.record(index)?;
    if depth > MAX_DEPTH || !sh.count() {
        return None;
    }
    let fxs = effects::parse(sh, fx, rec);
    let (mut b, mask_baked) = match node {
        LayerNode::Group { children, .. } => {
            let kids = build_list(sh, fx, children, base, depth + 1);
            let mut g = group(sh, &rec.name(), kids)?;
            g.node.insert("blendMode".into(), json!(util::blend(rec.blend_mode)));
            (g, false)
        }
        LayerNode::Layer { .. } => layer(sh, fx, rec, index, base, &fxs)?,
    };
    common(rec, &mut b, node.is_group());
    if !fxs.effects.is_empty() {
        b.node.insert("effects".into(), Value::Array(fxs.effects.clone()));
    }
    Some(masks(sh, fx, rec, b, base, mask_baked, node.is_group()))
}

/// A layer that isn't a group; also whether its raster mask is baked into its pixels.
fn layer(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord, index: usize, base: Affine, fxs: &Fx) -> Option<(Built, bool)> {
    if ADJUSTMENTS.iter().any(|k| rec.block(k).is_some()) {
        sh.note("adjustment layers were left out (FigCraft has no adjustment layers; Photoshop's preview shows their effect)");
        return None;
    }
    let fill_key = [b"SoCo", b"GdFl", b"PtFl"].into_iter().find(|k| rec.block(k).is_some());
    let vector = rec.block(b"vsms").or_else(|| rec.block(b"vmsk")).is_some();
    if rec.block(b"TySh").is_some() {
        if let Some(t) = text::build(sh, fx, rec, base, fxs) {
            return Some((t, false));
        }
        return raster(sh, fx, rec, index, base, fxs).map(|r| (r, true));
    }
    if [b"SoLd", b"SoLE", b"PlLd"].into_iter().any(|k| rec.block(k).is_some()) {
        if let Some(s) = smart::build(sh, fx, rec, index, base, fxs) {
            return Some((s, false));
        }
        return raster(sh, fx, rec, index, base, fxs).map(|r| (r, true));
    }
    if vector && (fill_key.is_some() || rec.block(b"vstk").is_some()) {
        if let Some(s) = shape::build(sh, fx, rec, base, fxs) {
            return Some((s, false));
        }
        return raster(sh, fx, rec, index, base, fxs).map(|r| (r, true));
    }
    if fill_key.is_some() {
        if let Some(f) = fill_layer(sh, fx, rec, base, fxs) {
            return Some((f, false));
        }
        sh.note("fill layers that couldn't be read kept the pixels Photoshop rendered for them");
    }
    raster(sh, fx, rec, index, base, fxs).map(|r| (r, true))
}

/// Visibility, opacity, blend mode and lock.
fn common(rec: &LayerRecord, b: &mut Built, is_group: bool) {
    b.node.insert("visible".into(), json!(rec.is_visible()));
    if rec.opacity < 255 {
        b.node.insert("opacity".into(), json!(f64::from(rec.opacity) / 255.0));
    }
    if !is_group {
        let mode = util::blend(rec.blend_mode);
        b.node.insert("blendMode".into(), json!(if mode == "PASS_THROUGH" { "NORMAL" } else { mode }));
    }
    let locked = match rec.block(b"lspf").and_then(TaggedBlock::parsed) {
        Some(Ok(BlockData::Protection(v))) => v & 0x8000_0000 != 0 || v & 4 != 0,
        _ => false,
    };
    if locked {
        b.node.insert("locked".into(), json!(true));
    }
}

/// The layer's user mask, when it has one that is on.
fn user_mask(rec: &LayerRecord) -> Option<&figcraft_psd::LayerMask> {
    rec.layer_mask().filter(|m| !m.disabled())
}

/// A layer's pixels as an image rectangle (with its raster mask and any colour overlay baked in).
pub(crate) fn raster(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord, index: usize, base: Affine, fxs: &Fx) -> Option<Built> {
    if rec.rect.is_empty() {
        return None;
    }
    let look = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}{:?}{:?}", fxs.overlay, fxs.gradient, fxs.pattern).hash(&mut h);
        h.finish()
    };
    let key = format!("{}:{index}:{look:x}", fx.id);
    let file = &fx.file;
    let has_mask = user_mask(rec).is_some();
    let tile = fxs.pattern.as_ref().and_then(|p| fills::tile(sh, fx, &p.id));
    if fxs.pattern.is_some() && tile.is_none() {
        sh.note("pattern overlays whose pattern isn't in the file were left out");
    }
    let canvas = Rect::new(0.0, 0.0, fx.w, fx.h);
    let r = cached_image(sh, key, || {
        let layer = file.layer(index)?;
        let mut img = layer.rgba8().ok()?;
        if has_mask && let Some(Ok(mask)) = layer.user_mask() {
            bake_mask(&mut img, &mask);
        }
        // Photoshop stacks the overlays pattern, gradient, colour (top).
        let origin = (f64::from(img.left), f64::from(img.top));
        if let (Some(p), Some(t)) = (&fxs.pattern, &tile) {
            fills::bake_pattern(t, p, &mut img.data, img.width, origin, fills::layer_box(rec));
        }
        if let Some(g) = &fxs.gradient {
            fills::bake_gradient(g, fills::reference(g, fills::layer_box(rec), canvas), &mut img.data, img.width, origin);
        }
        if let Some((c, a)) = fxs.overlay {
            overlay(&mut img.data, c, a);
        }
        util::png(img.width, img.height, img.data)
    });
    let Some(r) = r else {
        sh.note("layers whose pixels couldn't be read were left out");
        return None;
    };
    let (w, h) = (rec.rect.width() as f64, rec.rect.height() as f64);
    let world = base * Affine::translate((f64::from(rec.rect.left), f64::from(rec.rect.top)));
    let mut b = Built::new(sh, "RECTANGLE", &rec.name(), world, w, h);
    let mut paint = json!({"type": "IMAGE", "scaleMode": "FILL", "imageRef": r});
    if rec.fill_opacity() < 255 {
        paint["opacity"] = json!(f64::from(rec.fill_opacity()) / 255.0);
    }
    b.node.insert("fills".into(), json!([paint]));
    if fxs.stroke.is_some() {
        sh.note("strokes on pixel layers were left out");
    }
    Some(b)
}

/// Multiply `img`'s alpha by `mask` (both in document pixels).
fn bake_mask(img: &mut figcraft_psd::RgbaImage, mask: &figcraft_psd::GrayImage) {
    let (w, h) = (img.width as i64, img.height as i64);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (i64::from(img.left) + x, i64::from(img.top) + y);
            let (mx, my) = (dx - i64::from(mask.left), dy - i64::from(mask.top));
            let m = if mx >= 0 && my >= 0 && mx < i64::from(mask.width) && my < i64::from(mask.height) {
                mask.data.get((my * i64::from(mask.width) + mx) as usize).copied().unwrap_or(mask.default_value)
            } else {
                mask.default_value
            };
            if let Some(a) = img.data.get_mut(((y * w + x) * 4 + 3) as usize) {
                *a = ((u16::from(*a) * u16::from(m) + 127) / 255) as u8;
            }
        }
    }
}

/// Paint `color` over the pixels (keeping their alpha) at `opacity`.
fn overlay(data: &mut [u8], color: [f64; 3], opacity: f64) {
    let o = opacity.clamp(0.0, 1.0);
    for px in data.chunks_exact_mut(4) {
        for (v, c) in px.iter_mut().zip(color) {
            *v = (f64::from(*v) * (1.0 - o) + c * 255.0 * o).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// A fill layer (solid colour, gradient or pattern): a rectangle over the canvas (its masks
/// shape it).
fn fill_layer(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord, base: Affine, fxs: &Fx) -> Option<Built> {
    // A mask that hides everything outside its box needs nothing beyond it.
    let r = match user_mask(rec) {
        Some(m) if m.default_color == 0 && !m.rect.is_empty() => {
            Rect::new(f64::from(m.rect.left), f64::from(m.rect.top), f64::from(m.rect.right), f64::from(m.rect.bottom))
        }
        _ => Rect::new(0.0, 0.0, fx.w, fx.h),
    };
    let a = f64::from(rec.fill_opacity()) / 255.0;
    let canvas = Rect::new(0.0, 0.0, fx.w, fx.h);
    let d = |key: &[u8; 4]| rec.block(key).and_then(|b| util::versioned(&b.data));
    let mut paints = if let Some(d) = d(b"SoCo") {
        let c = desc(&d, "Clr ").and_then(util::color)?;
        vec![json!({"type": "SOLID", "color": util::rgba(c, 1.0), "opacity": a})]
    } else if let Some(d) = d(b"GdFl") {
        let g = fills::gradient(&d, a)?;
        vec![fills::paint(&g, fills::reference(&g, r, canvas), r)]
    } else {
        let p = fills::pattern(&d(b"PtFl")?, a)?;
        vec![fills::pattern_paint(sh, fx, &p)?]
    };
    paints.extend(fills::overlay_paints(sh, fx, rec, fxs, r));
    if let Some((c, o)) = fxs.overlay {
        paints.push(json!({"type": "SOLID", "color": util::rgba(c, 1.0), "opacity": o}));
    }
    let mut b = Built::new(sh, "RECTANGLE", &rec.name(), base * Affine::translate((r.x0, r.y0)), r.width(), r.height());
    b.node.insert("fills".into(), Value::Array(paints));
    Some(b)
}

/// The layer's vector and raster masks as mask layers below it in a group (effects, opacity and
/// blend mode move to the group, as Photoshop applies them to the masked layer).
fn masks(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord, b: Built, base: Affine, mask_baked: bool, is_group: bool) -> Built {
    let mut mask_nodes = Vec::new();
    let is_shape = !is_group && [b"SoCo", b"GdFl", b"PtFl", b"vstk"].into_iter().any(|k| rec.block(k).is_some());
    if !is_shape && let Some(m) = shape::vector_mask(sh, fx, rec, base) {
        mask_nodes.push(m);
    }
    if !mask_baked && let Some(m) = raster_mask(sh, fx, rec, &b, base) {
        mask_nodes.push(m);
    }
    if mask_nodes.is_empty() {
        return b;
    }
    let name = rec.name();
    let mut content = b;
    let moved: Vec<(&str, Value)> =
        ["effects", "opacity", "blendMode", "visible", "locked"].into_iter().filter_map(|k| content.take(k).map(|v| (k, v))).collect();
    if is_group && let Some(m) = moved.iter().find(|(k, _)| *k == "blendMode").map(|(_, v)| v.clone()) {
        // The group keeps passing through; the masked group takes its mode.
        content.node.insert("blendMode".into(), m);
    }
    // A mask masks the layers above it up to the next mask: each mask gets a group of its own,
    // the outer one (the vector mask) holding the inner one.
    let mut inner = content;
    let count = mask_nodes.len();
    for (i, m) in mask_nodes.into_iter().rev().enumerate() {
        let label = if i + 1 == count { name.clone() } else { format!("{name} (masked)") };
        match group(sh, &label, vec![m, inner]) {
            Some(g) => inner = g,
            None => return Built::new(sh, "GROUP", &name, base, 0.0, 0.0),
        }
    }
    let mut g = inner;
    for (k, v) in moved {
        g.node.insert(k.into(), v);
    }
    g
}

/// A raster user mask as an image mask layer covering the layer.
fn raster_mask(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord, content: &Built, base: Affine) -> Option<Built> {
    let m = user_mask(rec)?;
    let index = fx.file.layers().iter().position(|r| std::ptr::eq(r, rec))?;
    // The mask covers its own box, and the layer's when outside the box shows.
    let mr = Rect::new(f64::from(m.rect.left), f64::from(m.rect.top), f64::from(m.rect.right), f64::from(m.rect.bottom));
    let inv = if base.determinant().abs() > 1e-12 { base.inverse() } else { Affine::IDENTITY };
    let cover = if m.default_color == 0 { mr } else { mr.union(util::bbox(inv * content.world, content.w, content.h)) };
    let area = Rect::new(cover.x0.floor(), cover.y0.floor(), cover.x1.ceil(), cover.y1.ceil());
    if area.width() < 1.0 || area.height() < 1.0 {
        return None;
    }
    let file = &fx.file;
    let key = format!("{}:{index}:mask:{},{},{},{}", fx.id, area.x0, area.y0, area.x1, area.y1);
    let r = cached_image(sh, key, || {
        let mask = file.layer(index)?.user_mask()?.ok()?;
        let (w, h) = (area.width() as u32, area.height() as u32);
        if u64::from(w) * u64::from(h) > 400_000_000 {
            return None;
        }
        let mut data = vec![0u8; (w as usize) * (h as usize) * 4];
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let (mx, my) = (area.x0 as i64 + x - i64::from(mask.left), area.y0 as i64 + y - i64::from(mask.top));
                let v = if mx >= 0 && my >= 0 && mx < i64::from(mask.width) && my < i64::from(mask.height) {
                    mask.data.get((my * i64::from(mask.width) + mx) as usize).copied().unwrap_or(mask.default_value)
                } else {
                    mask.default_value
                };
                if let Some(a) = data.get_mut(((y * i64::from(w) + x) * 4 + 3) as usize) {
                    *a = v;
                }
            }
        }
        util::png(w, h, data)
    })?;
    let mut b = Built::new(sh, "RECTANGLE", "Layer mask", base * Affine::translate((area.x0, area.y0)), area.width(), area.height());
    b.node.insert("fills".into(), json!([{"type": "IMAGE", "scaleMode": "FILL", "imageRef": r}]));
    b.node.insert("isMask".into(), json!(true));
    Some(b)
}
