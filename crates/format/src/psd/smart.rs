//! Smart objects: each placed file becomes a component on the Components page and each placed
//! layer an instance of it.
//!
//! A placed layer's `SoLd` descriptor names its file (`Idnt`, the uuid of an item in the file's
//! `lnk2` / `lnk3` / `lnkD` / `lnkE` blocks), the corners it is placed at (`Trnf`) and the size of
//! the placed content (`Sz  `). An item holds the file itself (`liFD`, embedded) or names it
//! (`liFE`, linked: its name, full and relative path). The component holds:
//!
//! * a placed PSD's own layers (an embedded one, or a linked one the caller finds);
//! * a placed picture (PNG, JPEG, …) as an image;
//! * a vector smart object (an Illustrator file saved with PDF content) its paths as vector
//!   layers, with the pixels Photoshop rendered for it above them (to compare, hide or delete);
//!   a colour overlay recolours the vectors, one component per colour;
//! * otherwise (a missing link, artwork the vectors can't hold) the pixels Photoshop rendered for
//!   the layer. Instances turned differently get a component of their own then.

use std::rc::Rc;

use figcraft_doc::VectorNetwork;
use figcraft_doc::geom::{Affine, Rect, Shape};
use figcraft_psd::{LayerRecord, PsdFile};
use serde_json::{Value, json};

use super::effects::Fx;
use super::layers::{self, Built, FileCx};
use super::util::{self, desc, num, text};
use super::{LinkedRef, MAX_SMART_DEPTH, Shared};

/// Room between components on the Components page, and how wide a row of them gets.
const GAP: f64 = 100.0;
const ROW: f64 = 8000.0;

/// A file a smart object places: embedded (`data`: where its bytes are in which global block) or
/// linked (`linked`: how the PSD names it).
pub(crate) struct Link {
    pub uuid: String,
    pub name: String,
    pub data: Option<(usize, std::ops::Range<usize>)>,
    pub linked: Option<LinkedRef>,
}

/// A component's main: its node id, its content's size and where the content comes from.
pub(crate) struct Main {
    pub id: String,
    pub w: f64,
    pub h: f64,
    source: Source,
}

#[derive(Clone)]
enum Source {
    /// A PSD's layers (its file and the id keying its pictures).
    Psd(Rc<PsdFile>, usize),
    /// A picture.
    Image(String),
    /// Vector artwork, recoloured by a colour overlay, with Photoshop's picture of it above
    /// (its `imageRef` and box in the artwork's space).
    Vector { art: Rc<super::vector::Art>, recolor: Option<[f64; 3]>, render: Option<(String, Rect)> },
}

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
}

/// The files a PSD's smart objects place.
pub(crate) fn links(file: &PsdFile) -> Vec<Link> {
    let mut out = Vec::new();
    for (bi, block) in file.global_blocks.iter().enumerate() {
        if !matches!(&block.key, b"lnk2" | b"lnk3" | b"lnkD" | b"lnkE") {
            continue;
        }
        let mut r = Rd { d: &block.data, p: 0 };
        while let Some(len) = r.u64().and_then(|n| usize::try_from(n).ok()) {
            let start = r.p;
            if r.take(len).is_none() {
                break;
            }
            if let Some(l) = item(&block.data, start, len, bi) {
                out.push(l);
            }
            if out.len() > 100_000 {
                break;
            }
            r.p = start + len;
            r.p = r.p.saturating_add(3) & !3;
        }
    }
    out
}

/// One linked-layer item starting at `start` in block `bi`'s data.
fn item(data: &[u8], start: usize, len: usize, bi: usize) -> Option<Link> {
    let mut r = Rd { d: data.get(..start.checked_add(len)?)?, p: start };
    let kind = r.take(4)?;
    let _version = r.u32()?;
    let n = usize::from(*r.take(1)?.first()?);
    let uuid = String::from_utf8_lossy(r.take(n)?).into_owned();
    let chars = usize::try_from(r.u32()?).ok()?;
    if chars > 32_768 {
        return None;
    }
    let units: Vec<u16> = r.take(chars.checked_mul(2)?)?.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    let name = String::from_utf16_lossy(&units).trim_end_matches('\0').to_string();
    r.take(8)?; // file type and creator
    let data_len = usize::try_from(r.u64()?).ok()?;
    if *r.take(1)?.first()? != 0 {
        let (_, used) = figcraft_psd::VersionedDescriptor::parse_prefix(data.get(r.p..)?).ok()?;
        r.p += used;
    }
    match kind {
        b"liFD" => {
            let s = r.p;
            let end = s.checked_add(data_len)?;
            (end <= start + len).then_some(Link { uuid, name, data: Some((bi, s..end)), linked: None })
        }
        b"liFE" => {
            let d = data.get(r.p..start + len).and_then(util::versioned);
            let linked = LinkedRef {
                name: d.as_ref().and_then(|d| text(d, "Nm  ")).filter(|n| !n.is_empty()).unwrap_or_else(|| name.clone()),
                rel_path: d.as_ref().and_then(|d| text(d, "relPath")).filter(|p| !p.is_empty()),
                full_path: d.as_ref().and_then(|d| text(d, "fullPath")).filter(|p| !p.is_empty()),
            };
            Some(Link { uuid, name, data: None, linked: Some(linked) })
        }
        _ => None,
    }
}

/// What a `SoLd` block says: the placed file's uuid, the affine that takes the placed content's
/// box onto its corners, and that content's size.
struct Placed {
    uuid: String,
    transform: Affine,
    size: (f64, f64),
}

fn placed(sh: &mut Shared, rec: &LayerRecord) -> Option<Placed> {
    let block = rec.block(b"SoLd").or_else(|| rec.block(b"SoLE"));
    let Some(block) = block else {
        sh.note("smart objects from old Photoshop versions kept the pixels Photoshop rendered for them");
        return None;
    };
    if block.data.get(..4) != Some(b"soLD") {
        return None;
    }
    let d = block.data.get(8..).and_then(util::versioned)?;
    let uuid = text(&d, "Idnt")?;
    let sz = desc(&d, "Sz  ")?;
    let (w, h) = (num(sz, "Wdth")?, num(sz, "Hght")?);
    let corners = |key: &str| -> Option<[f64; 8]> {
        let v: Vec<f64> = util::list(&d, key).iter().filter_map(util::value_num).collect();
        <[f64; 8]>::try_from(v).ok()
    };
    let p = corners("Trnf")?;
    if let Some(q) = corners("nonAffineTransform")
        && p.iter().zip(q).any(|(a, b)| (a - b).abs() > 0.5)
    {
        sh.note("smart objects in perspective kept the pixels Photoshop rendered for them");
        return None;
    }
    if desc(&d, "warp").and_then(|w| util::enumv(w, "warpStyle")).is_some_and(|s| s != "warpNone") || desc(&d, "quiltWarp").is_some() {
        sh.note("warped smart objects kept the pixels Photoshop rendered for them");
        return None;
    }
    // A parallelogram: top left + bottom right = top right + bottom left.
    let tol = 0.5;
    if (p[0] + p[4] - p[2] - p[6]).abs() > tol || (p[1] + p[5] - p[3] - p[7]).abs() > tol || !(w > 0.0 && h > 0.0) {
        return None;
    }
    let (a, b, c, dd) = ((p[2] - p[0]) / w, (p[3] - p[1]) / w, (p[6] - p[0]) / h, (p[7] - p[1]) / h);
    let t = Affine::new([a, b, c, dd, p[0], p[1]]);
    if !util::sane(t) {
        return None;
    }
    if let Some(f) = desc(&d, "filterFX")
        && util::flag(f, "enab") != Some(false)
        && !util::list(f, "filterFXList").is_empty()
    {
        sh.note("smart filters were left out of components (the instances show the placed file without them)");
    }
    Some(Placed { uuid, transform: t, size: (w, h) })
}

/// The bytes of a placed file, embedded or found by the caller.
fn bytes_of(sh: &Shared, fx: &FileCx, link: &Link) -> Option<Vec<u8>> {
    if let Some((bi, range)) = &link.data {
        return fx.file.global_blocks.get(*bi)?.data.get(range.clone()).map(<[u8]>::to_vec);
    }
    let r = link.linked.as_ref()?;
    sh.linked.as_ref()?(r)
}

/// A placed layer as an instance of its file's component (`None`: keep its pixels instead).
pub(crate) fn build(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord, index: usize, base: Affine, fxs: &Fx) -> Option<Built> {
    if fxs.stroke.is_some() {
        sh.note("strokes on smart objects were left out");
    }
    let mut p = placed(sh, rec)?;
    let link = fx.links.iter().find(|l| l.uuid == p.uuid);
    // Copies of a smart object made as new smart objects embed the same file under new ids:
    // the same bytes stand for one component.
    if let Some(l) = link {
        p.uuid = canonical(sh, fx, l);
    }
    let name = link.map_or_else(|| rec.name(), |l| l.name.clone());
    // What the component holds, and its key and size. A solid colour overlay recolours vector
    // artwork (a component per colour); other overlays, and overlays on placed pictures or PSDs,
    // keep the recoloured pixels.
    let solid = fxs.overlay.filter(|(_, a)| *a >= 0.999).map(|(c, _)| c);
    let other = fxs.gradient.is_some() || fxs.pattern.is_some() || (fxs.overlay.is_some() && solid.is_none());
    let key = match solid {
        Some(c) => format!("{}|{:02x}{:02x}{:02x}", p.uuid, (c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8),
        None => p.uuid.clone(),
    };
    let found = match sh.components.get(&key) {
        _ if other => None,
        Some(m) => Some((key.clone(), m.w, m.h)),
        None => match link.and_then(|l| source(sh, fx, l)) {
            Some((Source::Vector { art, .. }, w, h)) => {
                let render = vector_render(sh, fx, rec, index, &p, (w, h), fxs);
                make_main(sh, &key, &name, Source::Vector { art, recolor: solid, render }, w, h);
                Some((key.clone(), w, h))
            }
            Some((src, w, h)) if solid.is_none() => {
                make_main(sh, &key, &name, src, w, h);
                Some((key.clone(), w, h))
            }
            _ => None,
        },
    };
    let (key, m) = match found {
        Some((key, w, h)) => (key, base * p.transform * Affine::scale_non_uniform(p.size.0 / w, p.size.1 / h)),
        None => {
            // The rendered pixels: one component per distinct picture (turned or recoloured
            // placements look different, and copies of the same picture share one).
            if rec.rect.is_empty() {
                return None;
            }
            let (rw, rh) = (rec.rect.width() as f64, rec.rect.height() as f64);
            let look = Fx { overlay: fxs.overlay, gradient: fxs.gradient.clone(), pattern: fxs.pattern.clone(), ..Fx::default() };
            let r = layers::raster(sh, fx, rec, index, Affine::IDENTITY, &look).and_then(|b| {
                b.node.get("fills").and_then(|f| f.get(0)).and_then(|f| f.get("imageRef")).and_then(Value::as_str).map(str::to_string)
            })?;
            let key = format!("pixels|{r}");
            if !sh.components.contains_key(&key) {
                if fxs.overlay.is_some() || fxs.gradient.is_some() || fxs.pattern.is_some() {
                    sh.note("recoloured smart objects became components holding their pixels with the colour overlay");
                } else if link.is_some_and(|l| l.data.is_some()) {
                    sh.note("vector smart objects became components holding the pixels Photoshop rendered for them");
                } else {
                    sh.note("smart objects whose linked file wasn't found became components holding the pixels Photoshop rendered for them");
                }
                make_main(sh, &key, &name, Source::Image(r), rw, rh);
            }
            let (w, h) = sh.components.get(&key).map(|m| (m.w, m.h))?;
            (key, base * Affine::translate((f64::from(rec.rect.left), f64::from(rec.rect.top))) * Affine::scale_non_uniform(rw / w, rh / h))
        }
    };
    let main = sh.components.get(&key)?;
    let (id, w, h, src) = (main.id.clone(), main.w, main.h, main.source.clone());
    let (world, sx, sy) = util::split_scale(m)?;
    let kids = content(sh, &src, w, h, world * Affine::scale_non_uniform(sx, sy));
    let mut b = Built::new(sh, "INSTANCE", &rec.name(), world, w * sx, h * sy);
    b.node.insert("componentId".into(), json!(id));
    b.node.insert("fills".into(), json!([]));
    b.node.insert("clipsContent".into(), json!(true));
    b.node.insert("children".into(), layers::attach(sh, world, kids));
    Some(b)
}

/// What a placed file's component holds: a PSD or a picture, with its size.
fn source(sh: &mut Shared, fx: &FileCx, link: &Link) -> Option<(Source, f64, f64)> {
    if sh.depth >= MAX_SMART_DEPTH {
        return None;
    }
    if let Some(found) = sh.vectors.get(&link.uuid) {
        return found.clone().map(|art| (Source::Vector { recolor: None, render: None, art: art.clone() }, art.w, art.h));
    }
    let bytes = bytes_of(sh, fx, link)?;
    if super::vector::is_pdf(&bytes) {
        let art = match super::vector::read(&bytes) {
            Ok(a) => Some(Rc::new(a)),
            Err(_) => {
                sh.note("vector smart objects with gradients, images, text, masks or blend modes kept only the pixels Photoshop rendered for them");
                None
            }
        };
        sh.vectors.insert(link.uuid.clone(), art.clone());
        let art = art?;
        return Some((Source::Vector { recolor: None, render: None, art: art.clone() }, art.w, art.h));
    }
    if super::is_psd(&bytes) {
        let opened = match sh.opened.get(&link.uuid) {
            Some(o) => o.clone(),
            None => {
                let f = PsdFile::from_bytes(&bytes).ok().map(Rc::new);
                if f.is_none() {
                    sh.note("placed Photoshop files that couldn't be read kept the pixels Photoshop rendered for them");
                }
                sh.opened.insert(link.uuid.clone(), f.clone());
                f
            }
        }?;
        let (w, h) = (f64::from(opened.header.width), f64::from(opened.header.height));
        let id = sh.next_file;
        sh.next_file += 1;
        return Some((Source::Psd(opened, id), w, h));
    }
    let dims = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    let r = sh.add_image(bytes)?;
    Some((Source::Image(r), f64::from(dims.0), f64::from(dims.1)))
}

/// Make the main of component `key` on the Components page.
fn make_main(sh: &mut Shared, key: &str, name: &str, src: Source, w: f64, h: f64) {
    let id = sh.id();
    let (x, y, row_h) = sh.main_cursor;
    let world = Affine::translate((x, y));
    // Its place is taken before its content is built: components made for placements inside
    // it go beside it, not on top of it.
    sh.main_cursor = if x + w + GAP > ROW { (0.0, y + row_h.max(h) + GAP, 0.0) } else { (x + w + GAP, y, row_h.max(h)) };
    // Known before its content is built, so nested placements of the same file find it.
    sh.components.insert(key.to_string(), Main { id: id.clone(), w, h, source: src.clone() });
    let kids = content(sh, &src, w, h, world);
    let mut b = Built::new(sh, "COMPONENT", name, world, w, h);
    b.node.insert("id".into(), json!(id));
    b.node.insert("fills".into(), json!([]));
    b.node.insert("clipsContent".into(), json!(true));
    b.node.insert("children".into(), layers::attach(sh, world, kids));
    if let Value::Array(a) = layers::attach(sh, Affine::IDENTITY, vec![b]) {
        sh.mains.extend(a);
    }
}

/// A component's content placed by `base` (content pixels → page), scaling with its instance.
fn content(sh: &mut Shared, src: &Source, w: f64, h: f64, base: Affine) -> Vec<Built> {
    let mut kids = match src {
        Source::Psd(file, id) => {
            let fx = FileCx::with_id(sh, file.clone(), *id);
            let tree = file.layer_tree_max_depth(200);
            sh.depth += 1;
            // A flattened file: its picture is all there is.
            let kids = if tree.is_empty() {
                layers::composite(sh, &fx, base, "Image").into_iter().collect()
            } else {
                layers::build_list(sh, &fx, &tree, base, 1)
            };
            sh.depth -= 1;
            kids
        }
        Source::Image(r) => {
            let mut b = Built::new(sh, "RECTANGLE", "Image", base, w, h);
            b.node.insert("fills".into(), json!([{"type": "IMAGE", "scaleMode": "FILL", "imageRef": r}]));
            vec![b]
        }
        Source::Vector { art, recolor, render } => {
            let shapes: Vec<Built> = art.shapes.iter().filter_map(|s| painted(sh, s, *recolor, base)).collect();
            let mut kids: Vec<Built> = layers::group(sh, "Vector", shapes).into_iter().collect();
            if let Some((r, at)) = render {
                let mut b = Built::new(sh, "RECTANGLE", "Photoshop render", base * Affine::translate((at.x0, at.y0)), at.width(), at.height());
                b.node.insert("fills".into(), json!([{"type": "IMAGE", "scaleMode": "FILL", "imageRef": r}]));
                kids.push(b);
            }
            kids
        }
    };
    for k in &mut kids {
        k.node.insert("constraints".into(), json!({"horizontal": "SCALE", "vertical": "SCALE"}));
    }
    kids
}

/// Photoshop's picture of a vector smart object in its artwork's space (`None` when the
/// placement turns or skews it, so the picture can't lie flat over the artwork).
fn vector_render(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord, index: usize, p: &Placed, art: (f64, f64), fxs: &Fx) -> Option<(String, Rect)> {
    let m = p.transform * Affine::scale_non_uniform(p.size.0 / art.0, p.size.1 / art.1);
    let [a, b, c, d, ..] = m.as_coeffs();
    if b.abs() > 1e-6 || c.abs() > 1e-6 || a <= 0.0 || d <= 0.0 || rec.rect.is_empty() {
        return None;
    }
    let look = Fx { overlay: fxs.overlay, ..Fx::default() };
    let r = layers::raster(sh, fx, rec, index, Affine::IDENTITY, &look)
        .and_then(|b| b.node.get("fills").and_then(|f| f.get(0)).and_then(|f| f.get("imageRef")).and_then(Value::as_str).map(str::to_string))?;
    let rect = Rect::new(f64::from(rec.rect.left), f64::from(rec.rect.top), f64::from(rec.rect.right), f64::from(rec.rect.bottom));
    Some((r, util::bbox(m.inverse() * Affine::translate((rect.x0, rect.y0)), rect.width(), rect.height())))
}

/// One painted path of vector artwork as a vector layer placed by `base`.
fn painted(sh: &mut Shared, s: &super::vector::Painted, recolor: Option<[f64; 3]>, base: Affine) -> Option<Built> {
    let bb = s.path.bounding_box();
    if !bb.is_finite() {
        return None;
    }
    let moved = Affine::translate((-bb.x0, -bb.y0)) * s.path.clone();
    let mut net = VectorNetwork::from_path(&moved);
    if let Some((_, winding)) = s.fill {
        for r in &mut net.regions {
            r.winding = winding;
        }
    }
    let mut b = Built::new(sh, "VECTOR", "Path", base * Affine::translate((bb.x0, bb.y0)), bb.width(), bb.height());
    b.node.insert("vectorNetworkRef".into(), json!(sh.networks.len()));
    sh.networks.push(Some(net));
    let paint = |c: [f64; 4]| {
        let rgb = recolor.unwrap_or([c[0], c[1], c[2]]);
        json!({"type": "SOLID", "color": util::rgba(rgb, c[3])})
    };
    b.node.insert(
        "fills".into(),
        match s.fill {
            Some((c, _)) => json!([paint(c)]),
            None => json!([]),
        },
    );
    if let Some(st) = &s.stroke {
        b.node.insert("strokes".into(), json!([paint(st.color)]));
        b.node.insert("strokeWeight".into(), json!(st.width));
        b.node.insert("strokeAlign".into(), json!("CENTER"));
        b.node.insert("strokeCap".into(), json!(st.cap));
        b.node.insert("strokeJoin".into(), json!(st.join));
        if st.dashes.iter().any(|d| *d > 0.0) {
            b.node.insert("strokeDashes".into(), json!(st.dashes));
        }
    }
    Some(b)
}

/// The id a placed file's component goes by: the first embedded file with the same bytes, else
/// its own uuid.
fn canonical(sh: &mut Shared, fx: &FileCx, link: &Link) -> String {
    use std::hash::{Hash, Hasher};
    let Some(bytes) = link.data.as_ref().and_then(|(bi, r)| fx.file.global_blocks.get(*bi)?.data.get(r.clone())) else { return link.uuid.clone() };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let key = (fx.id, h.finish(), bytes.len());
    sh.same_file.entry(key).or_insert_with(|| link.uuid.clone()).clone()
}
