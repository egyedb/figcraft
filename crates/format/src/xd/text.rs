//! XD text layers → REST text nodes.
//!
//! A text layer's `text` holds the `rawText`, its `frame` (`positioned`: grows from a point on
//! the first baseline; `autoHeight`: a fixed width that grows down; `area`: a fixed box) and the
//! `paragraphs` XD laid out: lines of runs with their `x` and baseline `y` relative to the layer.
//! The `style` holds the base font, size, colour and `textAttributes` (line height, letter
//! spacing in thousandths of an em, paragraph alignment and spacing); `meta.ux.rangedStyles`
//! splits the text into styled runs by `length` (UTF-16 code units).
//!
//! FigCraft centres a font's ascent and descent in each line, while XD puts the baselines where
//! the paragraphs say: the layer is placed so its first baseline lands on XD's.

use std::collections::HashMap;

use serde_json::{Map as JsonMap, Value, json};

use super::style::{self, clip};
use super::tree::{Cx, num, str_field};

/// Most characters read from one text layer (the REST importer's own limit).
const MAX_CHARS: usize = 1 << 20;
/// Most styled runs read from one text layer.
const MAX_RUNS: usize = 10_000;
/// Ascent and descent (shares of the size) assumed when the font's own aren't known.
pub(crate) const FALLBACK_METRICS: (f64, f64) = (0.93, 0.24);
/// Average advance (share of the size) assumed to guess the width of left-aligned point text,
/// which the engine measures again.
const AVERAGE_ADVANCE: f64 = 0.55;

/// A text layer mapped: its TEXT node (without its place), its box relative to the layer
/// (`x`, `y`, `width`, `height`), and the size of the area that clips it when XD's fixed box
/// shows only part of the text.
pub(crate) struct Laid {
    pub node: JsonMap<String, Value>,
    pub rect: (f64, f64, f64, f64),
    pub clip: Option<(f64, f64)>,
}

/// A text layer `l` as a TEXT node.
pub(crate) fn node(cx: &mut Cx, l: &Value) -> Option<Laid> {
    let t = l.get("text")?;
    let raw = str_field(t, "rawText").unwrap_or("");
    // XD keeps control characters (U+0003 where a line was once broken) that it doesn't draw or
    // break at: a word joiner, one UTF-16 unit like them, keeps the runs' ranges.
    let characters: String = raw.chars().take(MAX_CHARS).map(|c| if c.is_control() && c != '\n' && c != '\t' { '\u{2060}' } else { c }).collect();
    if raw.chars().nth(MAX_CHARS).is_some() {
        cx.warn("very long text was cut to 1,048,576 characters");
    }
    let st = l.get("style");
    let font = st.and_then(|s| s.get("font"));
    let attrs = st.and_then(|s| s.get("textAttributes"));
    let size = font.and_then(|f| num(f, "size")).filter(|s| *s > 0.0).map_or(12.0, |s| s.min(10_000.0));
    let segments = segments(t);
    let faces = faces(&segments);
    let base_ps = font.and_then(|f| str_field(f, "postscriptName")).unwrap_or("");
    let (family, style_name) =
        resolve(cx, base_ps, font.and_then(|f| str_field(f, "family")).unwrap_or(""), font.and_then(|f| str_field(f, "style")).unwrap_or(""), &faces);
    let line_height = attrs.and_then(|a| num(a, "lineHeight")).filter(|v| *v > 0.0).map(|v| v.min(1e5));
    let tracking = attrs.and_then(|a| num(a, "letterSpacing")).unwrap_or(0.0);
    let frame = t.get("frame");
    let kind = frame.and_then(|f| str_field(f, "type")).unwrap_or("positioned");

    let mut node = JsonMap::new();
    style::apply(cx, st, 0.0, 0.0, true, &mut node);
    // A text layer's stroke outlines its glyphs: kept, as FigCraft draws text strokes.
    let mut base = json!({"fontFamily": family, "fontStyle": style_name, "fontSize": size, "letterSpacing": tracking * size / 1000.0});
    let align = match attrs.and_then(|a| str_field(a, "paragraphAlign")) {
        Some("center") => "CENTER",
        Some("right") => "RIGHT",
        Some(a) if a.starts_with("justify") => "JUSTIFIED",
        _ => "LEFT",
    };
    let resize = match kind {
        "area" => "NONE",
        "autoHeight" => "HEIGHT",
        _ => "WIDTH_AND_HEIGHT",
    };
    let (ascent, descent) = metrics(cx, &family, &style_name);
    // Without a line height, XD's lines are as far apart as its layout put them.
    let line_height = line_height.or_else(|| line_step(t));
    if let Some(m) = base.as_object_mut() {
        m.insert("textAlignHorizontal".into(), json!(align));
        m.insert("textAlignVertical".into(), json!("TOP"));
        m.insert("textAutoResize".into(), json!(resize));
        if let Some(lh) = line_height {
            m.insert("lineHeightPx".into(), json!(lh));
            m.insert("lineHeightUnit".into(), json!("PIXELS"));
        }
        if let Some(sp) = attrs.and_then(|a| num(a, "paragraphAfterSpacing")).filter(|v| *v > 0.0) {
            m.insert("paragraphSpacing".into(), json!(sp.min(1e4)));
        }
    }
    runs(cx, l, &characters, &base, &faces, &mut node);
    node.insert("characters".into(), json!(characters));
    node.insert("style".into(), base);

    // Where the box goes so the first baseline lands on XD's.
    let line = line_height.unwrap_or((ascent + descent) * size);
    let first_baseline = (line - (ascent + descent) * size) / 2.0 + ascent * size;
    let ys: Vec<f64> = segments.iter().filter(|s| s.first_in_line).map(|s| s.y).collect();
    let y0 = ys.first().copied().unwrap_or(if kind == "positioned" { 0.0 } else { ascent * size });
    let y_last = ys.last().copied().unwrap_or(y0);
    let dy = y0 - first_baseline;
    let content_h = (y_last - y0).max(0.0) + line;
    let fw = frame.and_then(|f| num(f, "width")).filter(|w| *w > 0.0);
    let fh = frame.and_then(|f| num(f, "height")).filter(|h| *h > 0.0);
    let (dx, w, h) = match (kind, fw) {
        ("area", Some(w)) => (0.0, w, fh.map_or(content_h, |h| (h - dy).max(content_h))),
        ("autoHeight", Some(w)) => (0.0, w, content_h),
        _ => {
            // A point text: lines are laid out about the point by the alignment.
            let starts: Vec<f64> = segments.iter().filter(|s| s.first_in_line).map(|s| s.x).collect();
            let w = match align {
                "CENTER" => starts.iter().map(|x| -2.0 * x).fold(0.0, f64::max),
                "RIGHT" => starts.iter().map(|x| -x).fold(0.0, f64::max),
                _ => longest_line(&characters) as f64 * size * AVERAGE_ADVANCE,
            };
            let dx = match align {
                "CENTER" => -w / 2.0,
                "RIGHT" => -w,
                _ => starts.iter().copied().fold(0.0, f64::min),
            };
            (dx, w.max(1.0), content_h)
        }
    };
    // A fixed box XD laid out only part of the text in: the lines that don't fit are hidden.
    let laid = segments.iter().map(|s| s.to).max().unwrap_or(0);
    let clip = match (kind, fw, fh) {
        ("area", Some(w), Some(h)) if !segments.is_empty() && laid < characters.trim_end().encode_utf16().count() => {
            // XD hides whole lines: the box ends below the last line it laid out.
            Some((w, h.min(dy + content_h).max(0.0)))
        }
        _ => None,
    };
    Some(Laid { node, rect: (dx, dy, w, h), clip })
}

/// One laid-out run of a line.
struct Segment {
    x: f64,
    y: f64,
    /// The first run of its line.
    first_in_line: bool,
    /// Where it ends in the text (UTF-16 units).
    to: usize,
    /// The face XD used: PostScript name, family and style.
    face: Option<(String, String, String)>,
}

/// The runs XD laid out, line by line.
fn segments(t: &Value) -> Vec<Segment> {
    let mut out = Vec::new();
    let paragraphs = t.get("paragraphs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    for p in paragraphs.iter().take(100_000) {
        for line in p.get("lines").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default().iter().take(100_000) {
            for (i, seg) in line.as_array().map(Vec::as_slice).unwrap_or_default().iter().take(MAX_RUNS).enumerate() {
                let f = seg.get("style").and_then(|s| s.get("font"));
                let face = f.and_then(|f| {
                    Some((
                        str_field(f, "postscriptName")?.to_string(),
                        str_field(f, "family")?.to_string(),
                        str_field(f, "style").unwrap_or("").to_string(),
                    ))
                });
                out.push(Segment {
                    x: num(seg, "x").unwrap_or(0.0),
                    y: num(seg, "y").unwrap_or(0.0),
                    first_in_line: i == 0,
                    to: seg.get("to").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).unwrap_or(0),
                    face,
                });
                if out.len() > 4 * MAX_RUNS {
                    return out;
                }
            }
        }
    }
    out
}

/// The spacing of a text's first two lines when they are in one paragraph, else `None`.
fn line_step(t: &Value) -> Option<f64> {
    let p = t.get("paragraphs")?.as_array()?.first()?;
    let lines = p.get("lines")?.as_array()?;
    let y = |i: usize| lines.get(i).and_then(|l| l.as_array()).and_then(|l| l.first()).and_then(|s| num(s, "y"));
    let step = y(1)? - y(0)?;
    (step > 0.0 && step < 1e5).then_some(step)
}

/// The families and styles XD resolved, by PostScript name.
fn faces(segments: &[Segment]) -> HashMap<String, (String, String)> {
    segments
        .iter()
        .filter_map(|s| s.face.clone())
        .filter(|(ps, f, st)| !ps.is_empty() && !f.is_empty() && !st.is_empty())
        .map(|(ps, f, st)| (ps, (f, st)))
        .collect()
}

/// The family and style of a font XD names by PostScript name `ps`, `family` and `style`: the
/// installed face of `ps`, else what XD's layout resolved `ps` to, else the names given (XD often
/// gives the PostScript name as the family, with no style), else what `ps` reads as.
fn resolve(cx: &mut Cx, ps: &str, family: &str, style: &str, faces: &HashMap<String, (String, String)>) -> (String, String) {
    if !ps.is_empty() {
        if let Some(found) = installed(cx, ps) {
            return found;
        }
        if let Some(found) = faces.get(ps) {
            return found.clone();
        }
    }
    if !family.is_empty() && !style.is_empty() && family != ps {
        return (family.chars().take(256).collect(), style.chars().take(256).collect());
    }
    let name = if ps.is_empty() { family } else { ps };
    if name.is_empty() {
        cx.warn("text without a font uses Helvetica");
    }
    crate::sketch::text::font_name(name)
}

/// The installed family and style of PostScript name `ps`, when the import can ask.
fn installed(cx: &mut Cx, ps: &str) -> Option<(String, String)> {
    if let Some(found) = cx.face_cache.get(ps) {
        return found.clone();
    }
    let found = cx.fonts.face.as_ref().and_then(|f| f(ps));
    // A file names a few hundred fonts at most; junk ones don't grow the cache past that.
    if cx.face_cache.len() < 4096 {
        cx.face_cache.insert(ps.to_string(), found.clone());
    }
    found
}

/// A face's ascent and descent as shares of its size.
fn metrics(cx: &mut Cx, family: &str, style: &str) -> (f64, f64) {
    let key = (family.to_string(), style.to_string());
    if let Some(found) = cx.metrics_cache.get(&key) {
        return found.unwrap_or(FALLBACK_METRICS);
    }
    let found = cx
        .fonts
        .metrics
        .as_ref()
        .and_then(|f| f(family, style))
        .filter(|(a, d)| a.is_finite() && d.is_finite() && *a > 0.0 && *d >= 0.0 && a + d < 4.0);
    if cx.metrics_cache.len() < 4096 {
        cx.metrics_cache.insert(key, found);
    }
    found.unwrap_or(FALLBACK_METRICS)
}

/// The most characters on one line of `s`.
fn longest_line(s: &str) -> usize {
    s.split('\n').map(|l| l.chars().count()).max().unwrap_or(0)
}

/// The styled runs (`meta.ux.rangedStyles`) that differ from `base`, as per-character style ids.
fn runs(cx: &mut Cx, l: &Value, characters: &str, base: &Value, faces: &HashMap<String, (String, String)>, node: &mut JsonMap<String, Value>) {
    let ranged =
        l.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("rangedStyles")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    if ranged.len() > MAX_RUNS {
        cx.warn(format!("text with more than {MAX_RUNS} styled runs keeps the first ones"));
    }
    let units = characters.encode_utf16().count();
    let base_fill = node.get("fills").cloned();
    let mut ids = vec![0u64; units];
    let mut table = JsonMap::new();
    let mut start = 0usize;
    let count = ranged.len().min(MAX_RUNS);
    for (i, r) in ranged.iter().take(MAX_RUNS).enumerate() {
        let len = r.get("length").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
        // The last run reaches the end (a lone run often says 0).
        let end = if i + 1 == count { units } else { start.saturating_add(len).min(units) };
        if end <= start {
            continue;
        }
        let s = run_style(cx, r, base, faces, base_fill.as_ref());
        if s != *base {
            let id = table.len() as u64 + 1;
            table.insert(id.to_string(), s);
            for slot in ids.get_mut(start..end).into_iter().flatten() {
                *slot = id;
            }
        }
        start = end;
    }
    if !table.is_empty() {
        node.insert("characterStyleOverrides".into(), json!(ids));
        node.insert("styleOverrideTable".into(), Value::Object(table));
    }
}

/// One ranged style as a REST type style over `base` (equal to `base` when it changes nothing).
fn run_style(cx: &mut Cx, r: &Value, base: &Value, faces: &HashMap<String, (String, String)>, base_fill: Option<&Value>) -> Value {
    let mut s = base.clone();
    let Some(m) = s.as_object_mut() else { return s };
    let ps = str_field(r, "postscriptName").unwrap_or("");
    let (family, style) = resolve(cx, ps, str_field(r, "fontFamily").unwrap_or(""), str_field(r, "fontStyle").unwrap_or(""), faces);
    m.insert("fontFamily".into(), json!(family));
    m.insert("fontStyle".into(), json!(style));
    let size = num(r, "fontSize").filter(|v| *v > 0.0).map(|v| v.min(10_000.0));
    if let Some(size) = size {
        m.insert("fontSize".into(), json!(size));
    }
    let size = size.or_else(|| base.get("fontSize").and_then(Value::as_f64)).unwrap_or(12.0);
    if let Some(cs) = num(r, "charSpacing") {
        m.insert("letterSpacing".into(), json!(cs * size / 1000.0));
    }
    let deco = match (r.get("underline").and_then(Value::as_bool), r.get("strikethrough").and_then(Value::as_bool)) {
        (Some(true), _) => Some("UNDERLINE"),
        (_, Some(true)) => Some("STRIKETHROUGH"),
        _ => None,
    };
    if let Some(d) = deco {
        m.insert("textDecoration".into(), json!(d));
    }
    match str_field(r, "textTransform") {
        Some("uppercase") => {
            m.insert("textCase".into(), json!("UPPER"));
        }
        Some("lowercase") => {
            m.insert("textCase".into(), json!("LOWER"));
        }
        Some("titlecase") => {
            m.insert("textCase".into(), json!("TITLE"));
        }
        Some("none") | None => {}
        Some(other) => cx.warn(format!("the {} text transform is not supported yet", clip(other))),
    }
    if matches!(str_field(r, "textScript"), Some(sc) if sc != "none") {
        cx.warn("superscript and subscript text is not supported yet and was imported on the baseline");
    }
    if let Some(c) = r.get("fill").and_then(style::color) {
        let fill = json!([{"type": "SOLID", "color": c}]);
        // A run in the layer's own colour adds nothing.
        if base_fill != Some(&fill) {
            m.insert("fills".into(), fill);
        }
    }
    s
}
