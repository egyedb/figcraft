//! Vector smart objects: an embedded Illustrator file saved with PDF content (or a PDF) read as
//! vector shapes through craft-pdf's interpreter.
//!
//! The first page is interpreted with a device that keeps filled and stroked paths with solid
//! colours (in page points, y down), applying transparency group opacity and clip paths to fills.
//! Anything a plain vector layer can't hold the same way (gradients and patterns, images, text,
//! soft masks, blend modes, clipped strokes) makes the read give up: the component then keeps
//! only the pixels Photoshop rendered.

use craft_pdf::interpret::color::Color as PdfColor;
use craft_pdf::interpret::font::Glyph;
use craft_pdf::interpret::{
    BlendMode, ClipPath, Context, Device, FillRule, GlyphDrawMode, Image, InterpreterCache, InterpreterSettings, Paint, PathDrawMode, SoftMask,
    TransformExt,
};
use figcraft_doc::geom::kurbo::{self, Cap, Join, Shape};
use figcraft_doc::geom::{Affine, BezPath};
use figcraft_doc::{BoolOp, WindingRule};

/// Most shapes kept from one file.
const MAX_SHAPES: usize = 5_000;
/// Most path elements kept from one file.
const MAX_ELEMENTS: usize = 500_000;
/// Largest file read.
const MAX_BYTES: usize = 64 << 20;

/// A stroke: colour (RGBA), width, cap, join and dash lengths.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stroke {
    pub color: [f64; 4],
    pub width: f64,
    pub cap: &'static str,
    pub join: &'static str,
    pub dashes: Vec<f64>,
}

/// One painted path in page points (y down).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Painted {
    pub path: BezPath,
    pub fill: Option<([f64; 4], WindingRule)>,
    pub stroke: Option<Stroke>,
}

/// A file's artwork: its page size and shapes, bottom to top.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Art {
    pub w: f64,
    pub h: f64,
    pub shapes: Vec<Painted>,
}

/// Whether `bytes` hold PDF content (an Illustrator file saved with PDF content starts so too).
pub(crate) fn is_pdf(bytes: &[u8]) -> bool {
    bytes.get(..1024).unwrap_or(bytes).windows(5).any(|w| w == b"%PDF-")
}

/// The artwork of an Illustrator or PDF file, or why it can't be kept as vectors.
pub(crate) fn read(bytes: &[u8]) -> Result<Art, &'static str> {
    if bytes.len() > MAX_BYTES {
        return Err("too large");
    }
    let pdf = craft_pdf::open_slice(bytes, None).map_err(|_| "unreadable")?;
    let page = pdf.pages().first().ok_or("no page")?;
    let (w, h) = page.render_dimensions();
    let (w, h) = (f64::from(w), f64::from(h));
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 && w < 1e6 && h < 1e6) {
        return Err("no area");
    }
    let cache = InterpreterCache::new();
    let mut ctx =
        Context::new(page.initial_transform(true).to_kurbo(), kurbo::Rect::new(0.0, 0.0, w, h), &cache, page.xref(), InterpreterSettings::default());
    let mut dev = Collect::default();
    craft_pdf::interpret_page(page, &mut ctx, &mut dev).map_err(|_| "interpretation failed")?;
    if let Some(why) = dev.unsupported {
        return Err(why);
    }
    if dev.shapes.is_empty() {
        return Err("nothing drawn");
    }
    Ok(Art { w, h, shapes: dev.shapes })
}

/// The device collecting shapes.
#[derive(Default)]
struct Collect {
    shapes: Vec<Painted>,
    elements: usize,
    /// Each clip pushed: the clip in effect from then on (`None`: one that changes nothing).
    clips: Vec<Option<BezPath>>,
    /// Each transparency group's opacity.
    groups: Vec<f64>,
    soft_mask: bool,
    blend: bool,
    in_annotations: bool,
    unsupported: Option<&'static str>,
}

impl Collect {
    fn give_up(&mut self, why: &'static str) {
        self.unsupported.get_or_insert(why);
    }

    fn clip(&self) -> Option<&BezPath> {
        self.clips.iter().rev().find_map(Option::as_ref)
    }

    fn opacity(&self) -> f64 {
        self.groups.iter().product()
    }

    fn color(&self, c: &PdfColor) -> [f64; 4] {
        let [r, g, b, a] = c.to_rgba().components();
        [f64::from(r).clamp(0.0, 1.0), f64::from(g).clamp(0.0, 1.0), f64::from(b).clamp(0.0, 1.0), (f64::from(a) * self.opacity()).clamp(0.0, 1.0)]
    }
}

impl<'a> Device<'a> for Collect {
    fn set_soft_mask(&mut self, mask: Option<SoftMask<'a>>) {
        self.soft_mask = mask.is_some();
    }

    fn set_blend_mode(&mut self, blend_mode: BlendMode) {
        self.blend = blend_mode != BlendMode::Normal;
    }

    fn draw_path(&mut self, path: &BezPath, transform: Affine, paint: &Paint<'a>, draw_mode: &PathDrawMode) {
        if self.unsupported.is_some() || self.in_annotations {
            return;
        }
        if self.soft_mask {
            return self.give_up("soft masks");
        }
        if self.blend {
            return self.give_up("blend modes");
        }
        let Paint::Color(c) = paint else { return self.give_up("gradients or patterns") };
        let color = self.color(c);
        if self.shapes.len() >= MAX_SHAPES {
            return self.give_up("too many shapes");
        }
        let placed = transform * path.clone();
        self.elements += placed.elements().len();
        if self.elements > MAX_ELEMENTS {
            return self.give_up("too many path points");
        }
        match draw_mode {
            PathDrawMode::Fill(rule) => {
                let winding = match rule {
                    FillRule::NonZero => WindingRule::NonZero,
                    FillRule::EvenOdd => WindingRule::EvenOdd,
                };
                let path = match self.clip() {
                    Some(clip) => {
                        // Even-odd paths are made non-zero first so the intersection keeps their holes.
                        let src = if winding == WindingRule::EvenOdd {
                            match figcraft_pathops::simplify_with_rule(&placed, true) {
                                Ok(p) => p,
                                Err(_) => return self.give_up("clip paths"),
                            }
                        } else {
                            placed
                        };
                        match figcraft_pathops::boolean_step(&src, clip, BoolOp::Intersect) {
                            Ok(p) => p,
                            Err(_) => return self.give_up("clip paths"),
                        }
                    }
                    None => placed,
                };
                if path.elements().is_empty() {
                    return;
                }
                let winding = if self.clip().is_some() { WindingRule::NonZero } else { winding };
                self.shapes.push(Painted { path, fill: Some((color, winding)), stroke: None });
            }
            PathDrawMode::Stroke(s) => {
                let [a, b, c2, d, ..] = transform.as_coeffs();
                let (sx, sy) = (a.hypot(b), c2.hypot(d));
                if (sx - sy).abs() > 0.01 * sx.max(sy) {
                    return self.give_up("stretched strokes");
                }
                let width = f64::from(s.line_width) * sx;
                if let Some(clip) = self.clip() {
                    // A clip that leaves the whole stroke alone changes nothing.
                    let bb = placed.bounding_box().inflate(width, width);
                    if !(is_rect(clip) && clip.bounding_box().contains_rect(bb)) {
                        return self.give_up("clipped strokes");
                    }
                }
                let stroke = Stroke {
                    color,
                    width: width.clamp(0.0, 10_000.0),
                    cap: match s.line_cap {
                        Cap::Round => "ROUND",
                        Cap::Square => "SQUARE",
                        Cap::Butt => "NONE",
                    },
                    join: match s.line_join {
                        Join::Round => "ROUND",
                        Join::Bevel => "BEVEL",
                        Join::Miter => "MITER",
                    },
                    dashes: s.dash_array.iter().map(|v| (f64::from(*v) * sx).clamp(0.0, 10_000.0)).collect(),
                };
                self.shapes.push(Painted { path: placed, fill: None, stroke: Some(stroke) });
            }
        }
    }

    fn push_clip_path(&mut self, clip_path: &ClipPath) {
        let even_odd = clip_path.fill == FillRule::EvenOdd;
        let path = match figcraft_pathops::simplify_with_rule(&clip_path.path, even_odd) {
            Ok(p) => p,
            Err(_) => {
                self.give_up("clip paths");
                self.clips.push(None);
                return;
            }
        };
        let combined = match self.clip() {
            Some(prev) => figcraft_pathops::boolean_step(prev, &path, BoolOp::Intersect).ok(),
            None => Some(path),
        };
        if combined.is_none() {
            self.give_up("clip paths");
        }
        self.clips.push(combined);
    }

    fn push_transparency_group(&mut self, opacity: f32, mask: Option<SoftMask<'a>>, blend_mode: BlendMode) {
        if mask.is_some() {
            self.give_up("soft masks");
        }
        if blend_mode != BlendMode::Normal {
            self.give_up("blend modes");
        }
        self.groups.push(f64::from(opacity).clamp(0.0, 1.0));
    }

    fn draw_glyph(&mut self, _glyph: &Glyph<'a>, _transform: Affine, _glyph_transform: Affine, _paint: &Paint<'a>, draw_mode: &GlyphDrawMode) {
        if !matches!(draw_mode, GlyphDrawMode::Invisible) && !self.in_annotations {
            self.give_up("text");
        }
    }

    fn draw_image(&mut self, _image: Image<'a, '_>, _transform: Affine) {
        if !self.in_annotations {
            self.give_up("images");
        }
    }

    fn pop_clip_path(&mut self) {
        let _ = self.clips.pop();
    }

    fn pop_transparency_group(&mut self) {
        let _ = self.groups.pop();
    }

    fn begin_annotations(&mut self) {
        self.in_annotations = true;
    }

    fn end_annotations(&mut self) {
        self.in_annotations = false;
    }
}

/// Whether `p` is one axis-aligned rectangle.
fn is_rect(p: &BezPath) -> bool {
    let area = p.area().abs();
    let bb = p.bounding_box();
    bb.area() > 0.0 && (area - bb.area()).abs() <= 1e-3 * bb.area()
}
