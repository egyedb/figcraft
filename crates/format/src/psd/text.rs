//! Type layers (`TySh`): the transform, and the text engine data (`EngineData`) with the text,
//! its style runs, paragraph settings and whether it is point or box text.
//!
//! EngineData sizes are in text space, which the transform maps to document pixels. A style run
//! stores what differs from the document's normal style sheet; fonts are indexes into the
//! resource dictionary's font set (PostScript names).

use figcraft_doc::geom::Affine;
use figcraft_psd::LayerRecord;
use figcraft_psd::descriptor::{Value as D, VersionedDescriptor};
use figcraft_psd::engine_data::{self, Value as E};
use serde_json::{Map as JsonMap, Value, json};

use super::Shared;
use super::effects::{self, Fx};
use super::layers::{Built, FileCx};
use super::util::{self, desc, enumv, num};

/// Most characters kept in one text.
const MAX_CHARS: usize = 200_000;
/// Most style runs read from one text.
const MAX_RUNS: usize = 10_000;

/// A style run: its length in UTF-16 units and its settings, named as in EngineData.
type StyleRun = (usize, E);

/// A type layer as text (`None` when it should keep Photoshop's pixels instead).
pub(crate) fn build(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord, base: Affine, fxs: &Fx) -> Option<Built> {
    let data = &rec.block(b"TySh")?.data;
    let mut m = [0.0f64; 6];
    for (i, v) in m.iter_mut().enumerate() {
        let b = data.get(2 + i * 8..10 + i * 8)?;
        *v = f64::from_be_bytes(b.try_into().ok()?);
    }
    let t = Affine::new(m);
    if !util::sane(t) {
        return None;
    }
    let (text_desc, used) = VersionedDescriptor::parse_prefix(data.get(52..)?).ok()?;
    let d = text_desc.descriptor;
    let warp = data.get(52 + used + 2..).and_then(|w| VersionedDescriptor::parse_prefix(w).ok()).map(|(w, _)| w.descriptor);
    if warp.as_ref().and_then(|w| enumv(w, "warpStyle")).is_some_and(|s| s != "warpNone") {
        sh.note("warped type layers kept the pixels Photoshop rendered for them");
        return None;
    }
    if enumv(&d, "Ornt").as_deref() == Some("Vrtc") {
        sh.note("vertical type layers kept the pixels Photoshop rendered for them");
        return None;
    }
    let e = match d.get("EngineData") {
        Some(D::RawData(raw)) => engine_data::parse(raw).ok()?,
        _ => return None,
    };
    let mut chars: String = e.path(&["EngineDict", "Editor", "Text"]).and_then(E::as_str).unwrap_or("").chars().take(MAX_CHARS).collect();
    if chars.ends_with('\r') {
        chars.pop();
    }
    let chars = chars.replace(['\r', '\u{3}'], "\n");
    let (placed, sx, sy) = util::split_scale(t)?;
    if (sx - sy).abs() > 0.01 * sx.max(sy) {
        sh.note("type layers stretched wider or taller than their font kept their font's own proportions");
    }
    let res = e.get("ResourceDict").or_else(|| e.get("DocumentResources"));
    let fonts: Vec<String> = res
        .and_then(|r| r.get("FontSet"))
        .and_then(E::as_array)
        .unwrap_or(&[])
        .iter()
        .map(|f| f.get("Name").and_then(E::as_str).unwrap_or("").to_string())
        .collect();
    let normal = sheet(res, "StyleSheetSet", "TheNormalStyleSheet", "StyleSheetData");
    let normal_para = sheet(res, "ParagraphSheetSet", "TheNormalParagraphSheet", "Properties");
    let para = e.path(&["EngineDict", "ParagraphRun", "RunArray"]).and_then(E::as_array).and_then(|a| a.first());
    let para_prop = |k: &str| para.and_then(|p| p.path(&["ParagraphSheet", "Properties", k])).or_else(|| normal_para.and_then(|n| n.get(k)));
    let align = match para_prop("Justification").and_then(E::as_i64) {
        Some(1) => "RIGHT",
        Some(2) => "CENTER",
        Some(3..=6) => "JUSTIFIED",
        _ => "LEFT",
    };
    let auto_leading = para_prop("AutoLeading").and_then(E::as_f64).filter(|l| *l > 0.0 && *l < 10.0).unwrap_or(1.2);
    let fill_opacity = f64::from(rec.fill_opacity()) / 255.0;

    // Style runs: lengths in UTF-16 units and each run's style.
    let runs_v = e.path(&["EngineDict", "StyleRun", "RunArray"]).and_then(E::as_array).unwrap_or(&[]);
    let lens = e.path(&["EngineDict", "StyleRun", "RunLengthArray"]).and_then(E::as_array).unwrap_or(&[]);
    let mut styles: Vec<(usize, JsonMap<String, Value>)> = Vec::new();
    let index = match d.get("TextIndex") {
        Some(D::Integer(i)) => usize::try_from(*i).ok(),
        _ => None,
    };
    let from_txt2 = (*fx.txt2).as_ref().zip(index).and_then(|(t, i)| txt2_runs(t, i, &chars));
    if let Some((t2_fonts, runs)) = from_txt2 {
        // Named paragraph and character styles: Photoshop keeps what they set only in `Txt2`.
        for (len, st) in runs.iter().take(MAX_RUNS) {
            let get = |k: &str| st.get(k);
            styles.push((*len, run_style(sh, &get, &t2_fonts, sy, auto_leading, fxs, fill_opacity)));
        }
    } else {
        for (r, len) in runs_v.iter().zip(lens).take(MAX_RUNS) {
            let data = r.path(&["StyleSheet", "StyleSheetData"]);
            let get = |k: &str| data.and_then(|d| d.get(k)).or_else(|| normal.and_then(|n| n.get(k)));
            let len = len.as_i64().and_then(|l| usize::try_from(l).ok()).unwrap_or(0);
            styles.push((len, run_style(sh, &get, &fonts, sy, auto_leading, fxs, fill_opacity)));
        }
    }
    let Some((_, first)) = styles.first() else {
        sh.note("type layers without style runs kept the pixels Photoshop rendered for them");
        return None;
    };
    let mut style = first.clone();
    style.insert("textAlignHorizontal".into(), json!(align));
    let space = para_prop("SpaceBefore").and_then(E::as_f64).unwrap_or(0.0) + para_prop("SpaceAfter").and_then(E::as_f64).unwrap_or(0.0);
    if space > 0.0 {
        style.insert("paragraphSpacing".into(), json!((space * sy).clamp(0.0, 10_000.0)));
    }
    let font_size = style.get("fontSize").and_then(Value::as_f64).unwrap_or(12.0);
    let line_h = style.get("lineHeightPx").and_then(Value::as_f64).unwrap_or(font_size * auto_leading);

    // Point or box text, and where its box sits in text space.
    let shape = e.path(&["EngineDict", "Rendered", "Shapes", "Children"]).and_then(E::as_array).and_then(|a| a.first());
    let shape_type = shape.and_then(|s| s.get("ShapeType")).and_then(E::as_i64).unwrap_or(0);
    let box_bounds: Option<[f64; 4]> = shape
        .and_then(|s| s.path(&["Cookie", "Photoshop", "BoxBounds"]))
        .and_then(E::as_array)
        .and_then(|a| {
            let v: Vec<f64> = a.iter().filter_map(E::as_f64).collect();
            <[f64; 4]>::try_from(v).ok()
        })
        .filter(|b| b[2] > b[0] && b[3] > b[1]);
    let lines = chars.split('\n').count().max(1) as f64;
    let (local, w, h, resize) = match box_bounds {
        Some([l, t, r, b]) if shape_type == 1 => (Affine::translate((l * sx, t * sy)), (r - l) * sx, (b - t) * sy, "NONE"),
        _ => {
            // Point text: the first baseline is at the origin; the box is as wide as the ink.
            let bounds = desc(&d, "bounds");
            let (left, right) = (bounds.and_then(|b| num(b, "Left")).unwrap_or(0.0), bounds.and_then(|b| num(b, "Rght")).unwrap_or(0.0));
            let (asc, dsc) = metrics(sh, &style);
            let baseline = (line_h - (asc + dsc) * font_size) / 2.0 + asc * font_size;
            let w = ((right - left) * sx).max(1.0);
            (Affine::translate((left * sx, -baseline)), w, line_h * lines, "WIDTH_AND_HEIGHT")
        }
    };
    style.insert("textAutoResize".into(), json!(resize));
    // A left indent shared by the paragraphs moves the box in; a first-line indent stays one.
    let indent = (para_prop("StartIndent").and_then(E::as_f64).unwrap_or(0.0) * sx).clamp(0.0, w.max(0.0) * 0.9);
    let first_indent = para_prop("FirstLineIndent").and_then(E::as_f64).unwrap_or(0.0) * sx;
    if first_indent > 0.0 {
        style.insert("paragraphIndent".into(), json!(first_indent.min(10_000.0)));
    }
    let (local, w) = if indent > 0.0 && align != "RIGHT" { (local * Affine::translate((indent, 0.0)), w - indent) } else { (local, w) };
    let mut b = Built::new(sh, "TEXT", &rec.name(), base * placed * local, w, h);
    b.node.insert("characters".into(), json!(chars));
    // Runs that differ from the first become style overrides, per UTF-16 unit.
    if styles.iter().any(|(_, s)| s != first) {
        let total: usize = chars.encode_utf16().count();
        let mut table = JsonMap::new();
        let mut ids: Vec<Value> = Vec::with_capacity(total);
        let mut distinct: Vec<JsonMap<String, Value>> = vec![first.clone()];
        for (len, s) in &styles {
            let id = match distinct.iter().position(|x| x == s) {
                Some(i) => i,
                None => {
                    distinct.push(s.clone());
                    table.insert((distinct.len() - 1).to_string(), Value::Object(s.clone()));
                    distinct.len() - 1
                }
            };
            for _ in 0..*len {
                if ids.len() < total {
                    ids.push(json!(id));
                }
            }
        }
        b.node.insert("characterStyleOverrides".into(), Value::Array(ids));
        b.node.insert("styleOverrideTable".into(), Value::Object(table));
    }
    b.node.insert("style".into(), Value::Object(style));
    // Gradient and pattern overlays paint the type over its own colour.
    let extra = super::fills::overlay_paints(sh, fx, rec, fxs, figcraft_doc::geom::Rect::new(0.0, 0.0, w.max(1.0), h.max(1.0)));
    if !extra.is_empty() {
        let add = |st: &mut Value| {
            if let Some(Value::Array(f)) = st.get_mut("fills") {
                f.extend(extra.iter().cloned());
            }
        };
        if let Some(st) = b.node.get_mut("style") {
            add(st);
        }
        if let Some(Value::Object(t)) = b.node.get_mut("styleOverrideTable") {
            t.values_mut().for_each(add);
        }
    }
    if let Some(s) = &fxs.stroke {
        effects::apply_stroke(&mut b.node, s);
    }
    Some(b)
}

/// The data of the normal sheet in the resource dictionary's `set` (named by `which`).
fn sheet<'a>(res: Option<&'a E>, set: &str, which: &str, data: &str) -> Option<&'a E> {
    let res = res?;
    let i = res.get(which).and_then(E::as_i64).and_then(|i| usize::try_from(i).ok()).unwrap_or(0);
    res.get(set)?.as_array()?.get(i)?.get(data)
}

/// A style run as a REST type style.
fn run_style<'a>(
    sh: &mut Shared,
    get: &dyn Fn(&str) -> Option<&'a E>,
    fonts: &[String],
    scale: f64,
    auto_leading: f64,
    fxs: &Fx,
    fill_opacity: f64,
) -> JsonMap<String, Value> {
    let mut s = JsonMap::new();
    let ps = get("Font").and_then(E::as_i64).and_then(|i| usize::try_from(i).ok()).and_then(|i| fonts.get(i)).cloned().unwrap_or_default();
    let (family, style) = face(sh, &ps);
    s.insert("fontFamily".into(), json!(family));
    s.insert("fontStyle".into(), json!(style));
    if !ps.is_empty() {
        s.insert("fontPostScriptName".into(), json!(ps));
    }
    let size = (get("FontSize").and_then(E::as_f64).unwrap_or(12.0) * scale).clamp(0.5, 10_000.0);
    s.insert("fontSize".into(), json!(size));
    if get("AutoLeading").and_then(E::as_bool).unwrap_or(true) {
        s.insert("lineHeightPercentFontSize".into(), json!(auto_leading * 100.0));
        s.insert("lineHeightUnit".into(), json!("FONT_SIZE_%"));
    } else if let Some(l) = get("Leading").and_then(E::as_f64).filter(|l| *l > 0.0) {
        s.insert("lineHeightPx".into(), json!((l * scale).clamp(0.0, 100_000.0)));
        s.insert("lineHeightUnit".into(), json!("PIXELS"));
    }
    // Every field is given, so a run's style overrides the first run's in full.
    let tracking = get("Tracking").and_then(E::as_f64).unwrap_or(0.0);
    s.insert("letterSpacing".into(), json!(tracking / 1000.0 * size));
    let case = match get("FontCaps").and_then(E::as_i64) {
        Some(1) => "SMALL_CAPS",
        Some(2) => "UPPER",
        _ => "ORIGINAL",
    };
    s.insert("textCase".into(), json!(case));
    let decoration = if get("Underline").and_then(E::as_bool) == Some(true) {
        "UNDERLINE"
    } else if get("Strikethrough").and_then(E::as_bool) == Some(true) {
        "STRIKETHROUGH"
    } else {
        "NONE"
    };
    s.insert("textDecoration".into(), json!(decoration));
    let filled = get("FillFlag").and_then(E::as_bool).unwrap_or(true);
    let color = fxs.overlay.map(|(c, _)| (c, 1.0)).or_else(|| {
        let v: Vec<f64> = get("FillColor").and_then(|c| c.get("Values")).and_then(E::as_array)?.iter().filter_map(E::as_f64).collect();
        match v.as_slice() {
            [a, r, g, b] => Some(([r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)], a.clamp(0.0, 1.0))),
            _ => None,
        }
    });
    let fills = match color {
        Some((c, a)) if filled => json!([{"type": "SOLID", "color": util::rgba(c, a * fill_opacity)}]),
        None if filled => json!([{"type": "SOLID", "color": util::rgba([0.0; 3], fill_opacity)}]),
        _ => json!([]),
    };
    s.insert("fills".into(), fills);
    s
}

/// The family and style of an installed face for a PostScript name, else a guess from the name.
fn face(sh: &mut Shared, ps: &str) -> (String, String) {
    if ps.is_empty() {
        return ("Arial".into(), "Regular".into());
    }
    if let Some(found) = sh.face_cache.get(ps) {
        return found.clone().unwrap_or_else(|| guess_face(ps));
    }
    let found = sh.fonts.face.as_ref().and_then(|f| f(ps));
    sh.face_cache.insert(ps.to_string(), found.clone());
    found.unwrap_or_else(|| guess_face(ps))
}

/// A family and style guessed from a PostScript name: `OpenSans-SemiBoldItalic` → `Open Sans`,
/// `Semi Bold Italic`; `ArialMT` → `Arial`, `Regular`.
pub(crate) fn guess_face(ps: &str) -> (String, String) {
    let (fam, sty) = ps.split_once('-').unwrap_or((ps, ""));
    let fam = fam.strip_suffix("MT").or_else(|| fam.strip_suffix("PSMT")).unwrap_or(fam);
    let fam = fam.strip_suffix("PS").unwrap_or(fam);
    let spaced = |s: &str| {
        let mut out = String::new();
        let mut prev: Option<char> = None;
        for c in s.chars() {
            if c.is_uppercase() && prev.is_some_and(|p| p.is_lowercase()) {
                out.push(' ');
            }
            out.push(c);
            prev = Some(c);
        }
        out
    };
    let sty = sty.strip_suffix("MT").unwrap_or(sty);
    let style = if sty.is_empty() { "Regular".to_string() } else { spaced(sty).replace("Semibold", "Semi Bold").replace("Extrabold", "Extra Bold") };
    (spaced(fam), style)
}

/// A face's ascent and descent as shares of its size (a common guess when it isn't installed).
fn metrics(sh: &mut Shared, style: &JsonMap<String, Value>) -> (f64, f64) {
    let family = style.get("fontFamily").and_then(Value::as_str).unwrap_or("").to_string();
    let st = style.get("fontStyle").and_then(Value::as_str).unwrap_or("").to_string();
    let key = (family.clone(), st.clone());
    let found = match sh.metrics_cache.get(&key) {
        Some(m) => *m,
        None => {
            let m = sh.fonts.metrics.as_ref().and_then(|f| f(&family, &st));
            sh.metrics_cache.insert(key, m);
            m
        }
    };
    found.filter(|(a, d)| a.is_finite() && d.is_finite() && *a > 0.0 && *d >= 0.0).unwrap_or((0.9, 0.22))
}

/// The style runs of text object `index` in the document's `Txt2`, resolved through its named
/// paragraph and character styles, as EngineData-style dictionaries (with the `Txt2` font list
/// they index); `None` when `Txt2` doesn't hold this text (an edit elsewhere leaves it stale).
///
/// Observed layout (numbered keys): `/1 /1` lists the text objects; an object's `/0` is its model:
/// `/0` the text, `/5 /0` its paragraph runs and `/6 /0` its character runs, each
/// `<< /0 << /0 <<run>> >> /1 utf16-length >>`. A paragraph run names its paragraph sheet (`/6`)
/// and overrides (`/5`, with the character defaults at `/5 /32`); a character run names its
/// character sheet (`/5`) and overrides (`/6`). Sheets: `/0 /5 /0` character sheets (`/0 /6`
/// their settings), `/0 /6 /0` paragraph sheets (`/0 /5`), fonts at `/0 /1 /0` (`/0 /0 /0` the
/// PostScript name). Character settings by number: 0 font, 1 size, 4 auto leading, 5 leading,
/// 6 horizontal scale, 8 tracking, 12 caps, 15 strikethrough, 16 underline, 53 fill paint,
/// 56 fill on.
fn txt2_runs(t: &E, index: usize, chars: &str) -> Option<(Vec<String>, Vec<StyleRun>)> {
    let model = t.path(&["1", "1"])?.as_array()?.get(index)?.get("0")?;
    let text = model.get("0")?.as_str()?;
    let clean = |s: &str| s.trim_end_matches(['\r', '\n']).replace(['\r', '\u{3}'], "\n");
    if clean(text) != clean(chars) {
        return None;
    }
    let fonts: Vec<String> = t
        .path(&["0", "1", "0"])
        .and_then(E::as_array)
        .unwrap_or(&[])
        .iter()
        .map(|f| f.path(&["0", "0", "0"]).and_then(E::as_str).unwrap_or("").to_string())
        .collect();
    let char_sheets = t.path(&["0", "5", "0"]).and_then(E::as_array).unwrap_or(&[]);
    let para_sheets = t.path(&["0", "6", "0"]).and_then(E::as_array).unwrap_or(&[]);
    let runs = |key: &str| -> Vec<(usize, &E)> {
        model
            .path(&[key, "0"])
            .and_then(E::as_array)
            .unwrap_or(&[])
            .iter()
            .take(MAX_RUNS)
            .filter_map(|r| Some((usize::try_from(r.get("1")?.as_i64()?).ok()?, r.path(&["0", "0"])?)))
            .collect()
    };
    let empty = E::Dict(Vec::new());
    let (paras, mut char_runs) = (runs("5"), runs("6"));
    if char_runs.is_empty() {
        // No character runs: the paragraph styles say it all.
        if paras.is_empty() {
            return None;
        }
        char_runs.push((usize::MAX / 2, &empty));
    }
    // Merge settings: later ones win.
    let merge = |into: &mut Vec<(String, E)>, from: Option<&E>| {
        if let Some(E::Dict(items)) = from {
            for (k, v) in items {
                match into.iter_mut().find(|(h, _)| h == k) {
                    Some(e) => e.1 = v.clone(),
                    None => into.push((k.clone(), v.clone())),
                }
            }
        }
    };
    let sheet_idx = |v: Option<&E>| v.and_then(E::as_i64).and_then(|i| usize::try_from(i).ok());
    // Walk both run lists together, cutting at every boundary.
    let total: usize = clean(chars).encode_utf16().count();
    let (mut pi, mut ci) = (0usize, 0usize);
    let mut pend = paras.first().map_or(usize::MAX, |p| p.0);
    let mut cend = char_runs.first().map_or(0, |c| c.0);
    let mut at = 0usize;
    let mut out: Vec<StyleRun> = Vec::new();
    let mut guard = 0usize;
    while at < total && ci < char_runs.len() && guard < 4 * MAX_RUNS {
        guard += 1;
        let end = pend.min(cend).min(total);
        if end > at {
            let mut s: Vec<(String, E)> = Vec::new();
            merge(&mut s, char_sheets.first().and_then(|c| c.path(&["0", "6"])));
            if let Some((_, p)) = paras.get(pi) {
                merge(&mut s, sheet_idx(p.get("6")).and_then(|i| para_sheets.get(i)).and_then(|ps| ps.path(&["0", "5", "32"])));
                merge(&mut s, p.path(&["5", "32"]));
            }
            if let Some((_, c)) = char_runs.get(ci) {
                if let Some(i) = sheet_idx(c.get("5")).filter(|i| *i != 0) {
                    merge(&mut s, char_sheets.get(i).and_then(|cs| cs.path(&["0", "6"])));
                }
                merge(&mut s, c.get("6"));
            }
            let style = named(&s);
            match out.last_mut() {
                Some((len, last)) if *last == style => *len += end - at,
                _ => out.push((end - at, style)),
            }
            at = end;
        }
        if at >= cend {
            ci += 1;
            cend = cend.saturating_add(char_runs.get(ci).map_or(0, |c| c.0));
        }
        if at >= pend {
            pi += 1;
            pend = pend.saturating_add(paras.get(pi).map_or(usize::MAX, |p| p.0));
        }
    }
    (!out.is_empty()).then_some((fonts, out))
}

/// Numbered `Txt2` character settings as EngineData's named ones.
fn named(s: &[(String, E)]) -> E {
    let get = |k: &str| s.iter().find(|(h, _)| h == k).map(|(_, v)| v.clone());
    let mut items: Vec<(String, E)> = Vec::new();
    for (num, name) in [
        ("0", "Font"),
        ("1", "FontSize"),
        ("4", "AutoLeading"),
        ("5", "Leading"),
        ("6", "HorizontalScale"),
        ("8", "Tracking"),
        ("12", "FontCaps"),
        ("56", "FillFlag"),
    ] {
        if let Some(v) = get(num) {
            items.push((name.to_string(), v));
        }
    }
    let on = |k: &str| get(k).and_then(|v| v.as_i64().map(|i| i != 0).or_else(|| v.as_bool()));
    items.push(("Underline".into(), E::Bool(on("16").unwrap_or(false))));
    items.push(("Strikethrough".into(), E::Bool(on("15").unwrap_or(false))));
    if let Some(values) = get("53").and_then(|p| p.path(&["0", "1"]).cloned()) {
        items.push(("FillColor".into(), E::Dict(vec![("Type".into(), E::Int(1)), ("Values".into(), values)])));
    }
    E::Dict(items)
}
