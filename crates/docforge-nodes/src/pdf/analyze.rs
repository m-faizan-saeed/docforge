//! Finds the bounding box of everything visibly painted on a PDF page.
//!
//! This extends the approach of the reference `pdf-art-grid.rs`: walk the
//! page's content stream, track the current transformation matrix (CTM)
//! through `q`/`Q`/`cm`, collect path points into a pending box, and commit
//! that box only when the path is painted (`S`, `f`, `B`, …) — `n` discards
//! it. On top of that it handles what real-world files need:
//!
//! * Form XObjects (`Do`) are entered recursively with their `/Matrix`,
//!   `/BBox` clip and own resources — design tools wrap artwork in them.
//! * Images (XObject and inline) cover the unit square mapped by the CTM.
//! * Curves (`c`, `v`, `y`) contribute their exact extrema, not just the
//!   end point; strokes are widened by half the line width.
//! * Text is measured from the font's glyph widths and ascent/descent.
//! * Clipping paths (`W`/`W*`) and the page's CropBox limit what counts.
//! * Plain-white fills can be ignored (e.g. full-page background rectangles).

use std::{collections::HashMap, rc::Rc};

use docforge_core::{PdfImageInfo, PdfPageInfo};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, content::Content};

use super::{
    fonts::FontMetrics,
    geometry::{Bbox, Matrix, cubic_bounds},
};

/// Nesting limit for Form XObjects (guards against malicious cycles).
const MAX_FORM_DEPTH: usize = 24;

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Ignore fill-only paths painted in pure white.
    pub ignore_white_fills: bool,
}

/// Counts of what was found, for summaries and debugging.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub paths: usize,
    pub images: usize,
    pub text_runs: usize,
    pub forms: usize,
    pub shadings: usize,
    pub white_fills_ignored: usize,
    pub warnings: Vec<String>,
}

pub fn num(o: &Object) -> Option<f64> {
    match o {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(*r as f64),
        _ => None,
    }
}

/// Follows a reference (one level of indirection is all PDF allows here,
/// but chains are tolerated).
pub fn deref<'a>(doc: &'a Document, o: &'a Object) -> &'a Object {
    doc.dereference(o).map(|(_, o)| o).unwrap_or(o)
}

/// Looks up an inheritable page attribute (MediaBox, CropBox, Rotate,
/// Resources) on the page or its ancestors.
pub(crate) fn inherited<'a>(doc: &'a Document, page: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut id = page;
    for _ in 0..64 {
        let dict = doc.get_dictionary(id).ok()?;
        if let Ok(v) = dict.get(key) {
            return Some(deref(doc, v));
        }
        id = dict.get(b"Parent").and_then(Object::as_reference).ok()?;
    }
    None
}

pub(crate) fn rect(doc: &Document, o: &Object) -> Option<Bbox> {
    let a = o.as_array().ok()?;
    let v: Vec<f64> = a.iter().filter_map(|x| num(deref(doc, x))).collect();
    (v.len() == 4).then(|| Bbox::from_rect([v[0], v[1], v[2], v[3]]))
}

/// Analyses one page. `page` is the page object's id.
pub fn analyze_page(doc: &Document, page: ObjectId, opts: Options) -> (PdfPageInfo, Stats) {
    let media = inherited(doc, page, b"MediaBox")
        .and_then(|o| rect(doc, o))
        .unwrap_or(Bbox::from_rect([0.0, 0.0, 612.0, 792.0]));
    let crop = inherited(doc, page, b"CropBox")
        .and_then(|o| rect(doc, o))
        .and_then(|c| c.intersect(&media))
        .unwrap_or(media);
    let rotation = inherited(doc, page, b"Rotate")
        .and_then(num)
        .map_or(0, |r| (r as i64).rem_euclid(360) as u16 / 90 * 90);

    let resources: Vec<&Dictionary> = inherited(doc, page, b"Resources")
        .and_then(|o| o.as_dict().ok())
        .into_iter()
        .collect();

    let mut interp = Interp {
        doc,
        opts,
        bbox: None,
        images: Vec::new(),
        stats: Stats::default(),
        fonts: HashMap::new(),
        form_stack: Vec::new(),
    };
    let content = doc.get_page_content(page);
    interp.run(
        &content,
        &resources,
        GState::new(Matrix::IDENTITY, Clip::Region(crop)),
    );

    let content_box = interp.bbox.and_then(|b| b.intersect(&crop));
    let info = PdfPageInfo {
        media_box: media.to_array(),
        crop_box: crop.to_array(),
        rotation,
        content_box: content_box.map(Bbox::to_array),
        images: interp.images,
    };
    (info, interp.stats)
}

#[derive(Clone)]
struct TextParams {
    font: Rc<FontMetrics>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
    rise: f64,
    render_mode: i64,
}

/// The current clipping region, approximated by its bounding box.
#[derive(Clone, Copy, Debug)]
enum Clip {
    Region(Bbox),
    /// Clipped to nothing: painting has no visible effect.
    Empty,
}

impl Clip {
    fn and(self, area: Bbox) -> Clip {
        match self {
            Clip::Region(c) => c.intersect(&area).map_or(Clip::Empty, Clip::Region),
            Clip::Empty => Clip::Empty,
        }
    }

    /// The part of `area` that stays visible.
    fn visible(self, area: Bbox) -> Option<Bbox> {
        match self {
            Clip::Region(c) => area.intersect(&c),
            Clip::Empty => None,
        }
    }
}

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    line_width: f64,
    clip: Clip,
    fill_is_white: bool,
    text: TextParams,
}

impl GState {
    fn new(ctm: Matrix, clip: Clip) -> Self {
        Self {
            ctm,
            line_width: 1.0,
            clip,
            fill_is_white: false,
            text: TextParams {
                font: Rc::new(FontMetrics::default()),
                size: 0.0,
                char_spacing: 0.0,
                word_spacing: 0.0,
                h_scale: 1.0,
                leading: 0.0,
                rise: 0.0,
                render_mode: 0,
            },
        }
    }
}

struct Interp<'a> {
    doc: &'a Document,
    opts: Options,
    bbox: Option<Bbox>,
    images: Vec<PdfImageInfo>,
    stats: Stats,
    fonts: HashMap<Vec<u8>, Rc<FontMetrics>>,
    /// Forms being executed, to stop cycles.
    form_stack: Vec<ObjectId>,
}

impl<'a> Interp<'a> {
    /// Adds a painted area, limited by the current clip.
    fn commit(&mut self, area: Bbox, clip: Clip) {
        if let Some(v) = clip.visible(area) {
            self.bbox = Some(self.bbox.map_or(v, |b| b.union(&v)));
        }
    }

    fn warn(&mut self, msg: String) {
        if !self.stats.warnings.contains(&msg) {
            self.stats.warnings.push(msg);
        }
    }

    fn run(&mut self, content: &[u8], resources: &[&'a Dictionary], mut gs: GState) {
        let ops = match Content::decode(content) {
            Ok(c) => c.operations,
            Err(e) => {
                self.warn(format!(
                    "part of the page could not be read ({e}); its size may be underestimated"
                ));
                return;
            }
        };
        let mut stack: Vec<GState> = Vec::new();
        let mut path: Option<Bbox> = None;
        let mut current = (0.0, 0.0); // current point, page space
        let mut clip_pending = false;
        let mut tm = Matrix::IDENTITY;
        let mut tlm = Matrix::IDENTITY;

        for op in &ops {
            let o = &op.operands;
            let n = |i: usize| o.get(i).and_then(num);
            match op.operator.as_str() {
                // --- graphics state ---
                "q" => stack.push(gs.clone()),
                "Q" => {
                    if let Some(saved) = stack.pop() {
                        gs = saved;
                    }
                }
                "cm" => {
                    if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) =
                        (n(0), n(1), n(2), n(3), n(4), n(5))
                    {
                        gs.ctm = Matrix::new([a, b, c, d, e, f]).then(&gs.ctm);
                    }
                }
                "w" => gs.line_width = n(0).unwrap_or(1.0).max(0.0),
                "g" => gs.fill_is_white = n(0).is_some_and(|v| v >= 0.999),
                "rg" => gs.fill_is_white = (0..3).all(|i| n(i).is_some_and(|v| v >= 0.999)),
                "k" => gs.fill_is_white = (0..4).all(|i| n(i).is_some_and(|v| v <= 0.001)),
                "sc" | "scn" => {
                    let comps: Vec<f64> = o.iter().filter_map(num).collect();
                    gs.fill_is_white = comps.len() == o.len()
                        && match comps.len() {
                            1 | 3 => comps.iter().all(|v| *v >= 0.999),
                            4 => comps.iter().all(|v| *v <= 0.001),
                            _ => false,
                        };
                }
                "cs" => gs.fill_is_white = false,

                // --- path construction (page space) ---
                "m" | "l" => {
                    if let (Some(x), Some(y)) = (n(0), n(1)) {
                        current = gs.ctm.apply(x, y);
                        extend(&mut path, Bbox::point(current.0, current.1));
                    }
                }
                "c" | "v" | "y" => {
                    let pts: Vec<f64> = o.iter().filter_map(num).collect();
                    let p = |i: usize| gs.ctm.apply(pts[i], pts[i + 1]);
                    let curve = match (op.operator.as_str(), pts.len()) {
                        ("c", 6) => Some([current, p(0), p(2), p(4)]),
                        ("v", 4) => Some([current, current, p(0), p(2)]),
                        ("y", 4) => Some([current, p(0), p(2), p(2)]),
                        _ => None,
                    };
                    if let Some(curve) = curve {
                        extend(&mut path, cubic_bounds(curve));
                        current = curve[3];
                    }
                }
                "re" => {
                    if let (Some(x), Some(y), Some(w), Some(h)) = (n(0), n(1), n(2), n(3)) {
                        let r = Bbox::transformed(&Bbox::from_rect([x, y, x + w, y + h]), &gs.ctm);
                        extend(&mut path, r);
                        current = gs.ctm.apply(x, y);
                    }
                }
                "W" | "W*" => clip_pending = true,

                // --- path painting ---
                op_name @ ("S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n") => {
                    if let Some(shape) = path.take() {
                        let stroke = matches!(op_name, "S" | "s" | "B" | "B*" | "b" | "b*");
                        let fill = matches!(op_name, "f" | "F" | "f*" | "B" | "B*" | "b" | "b*");
                        if fill && !stroke && gs.fill_is_white && self.opts.ignore_white_fills {
                            self.stats.white_fills_ignored += 1;
                        } else if stroke || fill {
                            let area = if stroke {
                                shape.expand(gs.line_width / 2.0 * gs.ctm.max_scale())
                            } else {
                                shape
                            };
                            self.commit(area, gs.clip);
                            self.stats.paths += 1;
                        }
                        // The clip takes effect after painting.
                        if clip_pending {
                            gs.clip = gs.clip.and(shape);
                        }
                    }
                    clip_pending = false;
                }

                // --- images, forms, shadings ---
                "Do" => {
                    if let Some(name) = o.first().and_then(|n| n.as_name().ok()) {
                        self.do_xobject(name, resources, &gs);
                    }
                }
                "BI" => {
                    if let Some(Object::Stream(img)) = o.first() {
                        let placed =
                            Bbox::transformed(&Bbox::from_rect([0.0, 0.0, 1.0, 1.0]), &gs.ctm);
                        self.commit(placed, gs.clip);
                        self.stats.images += 1;
                        let dim = |long: &[u8], short: &[u8]| {
                            img.dict
                                .get(long)
                                .or_else(|_| img.dict.get(short))
                                .ok()
                                .and_then(num)
                                .unwrap_or(0.0) as u32
                        };
                        self.images.push(PdfImageInfo {
                            key: None,
                            matrix: matrix_array(&gs.ctm),
                            name: "inline".into(),
                            object: None,
                            width_px: dim(b"Width", b"W"),
                            height_px: dim(b"Height", b"H"),
                            placed: placed.to_array(),
                            filter: filter_name(self.doc, &img.dict),
                        });
                    }
                }
                "sh" => {
                    // A shading fills the whole clip region.
                    if let Clip::Region(region) = gs.clip {
                        self.commit(region, gs.clip);
                    }
                    self.stats.shadings += 1;
                }

                // --- text ---
                "BT" => {
                    tm = Matrix::IDENTITY;
                    tlm = Matrix::IDENTITY;
                }
                "Tf" => {
                    if let Some(name) = o.first().and_then(|n| n.as_name().ok()) {
                        gs.text.font = self.font(name, resources);
                    }
                    gs.text.size = n(1).unwrap_or(gs.text.size);
                }
                "Tc" => gs.text.char_spacing = n(0).unwrap_or(0.0),
                "Tw" => gs.text.word_spacing = n(0).unwrap_or(0.0),
                "Tz" => gs.text.h_scale = n(0).unwrap_or(100.0) / 100.0,
                "TL" => gs.text.leading = n(0).unwrap_or(0.0),
                "Ts" => gs.text.rise = n(0).unwrap_or(0.0),
                "Tr" => gs.text.render_mode = n(0).unwrap_or(0.0) as i64,
                "Td" | "TD" => {
                    if let (Some(tx), Some(ty)) = (n(0), n(1)) {
                        if op.operator == "TD" {
                            gs.text.leading = -ty;
                        }
                        tlm = Matrix::translate(tx, ty).then(&tlm);
                        tm = tlm;
                    }
                }
                "Tm" => {
                    if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) =
                        (n(0), n(1), n(2), n(3), n(4), n(5))
                    {
                        tlm = Matrix::new([a, b, c, d, e, f]);
                        tm = tlm;
                    }
                }
                "T*" => {
                    tlm = Matrix::translate(0.0, -gs.text.leading).then(&tlm);
                    tm = tlm;
                }
                "Tj" | "'" | "\"" | "TJ" => {
                    if op.operator == "'" || op.operator == "\"" {
                        if op.operator == "\"" {
                            gs.text.word_spacing = n(0).unwrap_or(gs.text.word_spacing);
                            gs.text.char_spacing = n(1).unwrap_or(gs.text.char_spacing);
                        }
                        tlm = Matrix::translate(0.0, -gs.text.leading).then(&tlm);
                        tm = tlm;
                    }
                    let parts: Vec<TextPart> = match (op.operator.as_str(), o.last()) {
                        ("TJ", Some(Object::Array(items))) => items
                            .iter()
                            .filter_map(|i| match i {
                                Object::String(s, _) => Some(TextPart::Text(s)),
                                other => num(other).map(TextPart::Kern),
                            })
                            .collect(),
                        (_, Some(Object::String(s, _))) => vec![TextPart::Text(s)],
                        _ => Vec::new(),
                    };
                    tm = self.show_text(&parts, &gs, tm);
                }
                _ => {}
            }
        }
    }

    /// Measures a text show operation, commits its box and returns the text
    /// matrix advanced past it.
    fn show_text(&mut self, parts: &[TextPart<'_>], gs: &GState, tm: Matrix) -> Matrix {
        let t = &gs.text;
        let (mut x, mut min_x, mut max_x) = (0.0f64, 0.0f64, 0.0f64);
        for part in parts {
            match part {
                TextPart::Text(bytes) => {
                    for code in t.font.codes(bytes) {
                        let mut advance = t.font.advance(code) * t.size + t.char_spacing;
                        if !t.font.two_byte && code == 32 {
                            advance += t.word_spacing;
                        }
                        x += advance * t.h_scale;
                        min_x = min_x.min(x);
                        max_x = max_x.max(x);
                    }
                }
                TextPart::Kern(k) => x -= k / 1000.0 * t.size * t.h_scale,
            }
        }
        // Modes 3 (invisible) and 7 (clip only) paint nothing.
        let painted = !matches!(t.render_mode, 3 | 7);
        if painted && t.size != 0.0 && max_x > min_x {
            let text_box = Bbox::from_rect([
                min_x,
                t.rise + t.font.descent * t.size,
                max_x,
                t.rise + t.font.ascent * t.size,
            ]);
            let area = Bbox::transformed(&text_box, &tm.then(&gs.ctm));
            self.commit(area, gs.clip);
            self.stats.text_runs += 1;
        }
        Matrix::translate(x, 0.0).then(&tm)
    }

    fn font(&mut self, name: &[u8], resources: &[&'a Dictionary]) -> Rc<FontMetrics> {
        if let Some(f) = self.fonts.get(name) {
            return Rc::clone(f);
        }
        let metrics =
            match lookup(self.doc, resources, b"Font", name).and_then(|o| o.as_dict().ok()) {
                Some(dict) => FontMetrics::from_dict(self.doc, dict),
                None => {
                    self.warn(format!(
                        "font /{} is missing; text size estimated",
                        String::from_utf8_lossy(name)
                    ));
                    FontMetrics::default()
                }
            };
        let metrics = Rc::new(metrics);
        self.fonts.insert(name.to_vec(), Rc::clone(&metrics));
        metrics
    }

    fn do_xobject(&mut self, name: &[u8], resources: &[&'a Dictionary], gs: &GState) {
        let Some((id, stream)) = lookup_ref(self.doc, resources, b"XObject", name) else {
            self.warn(format!(
                "XObject /{} is missing",
                String::from_utf8_lossy(name)
            ));
            return;
        };
        let dict = &stream.dict;
        match dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .unwrap_or_default()
        {
            b"Image" => {
                let placed = Bbox::transformed(&Bbox::from_rect([0.0, 0.0, 1.0, 1.0]), &gs.ctm);
                self.commit(placed, gs.clip);
                self.stats.images += 1;
                let dim = |k: &[u8]| {
                    dict.get(k)
                        .ok()
                        .and_then(|o| num(deref(self.doc, o)))
                        .unwrap_or(0.0) as u32
                };
                self.images.push(PdfImageInfo {
                    key: None, // assigned per document by PDF Input
                    matrix: matrix_array(&gs.ctm),
                    name: String::from_utf8_lossy(name).into_owned(),
                    object: id,
                    width_px: dim(b"Width"),
                    height_px: dim(b"Height"),
                    placed: placed.to_array(),
                    filter: filter_name(self.doc, dict),
                });
            }
            b"Form" => {
                if let Some(id) = id {
                    if self.form_stack.contains(&id) {
                        self.warn("a form refers to itself; the loop was skipped".into());
                        return;
                    }
                    self.form_stack.push(id);
                }
                if self.form_stack.len() > MAX_FORM_DEPTH {
                    self.warn("forms are nested too deeply; inner content skipped".into());
                } else {
                    self.run_form(stream, resources, gs);
                }
                if id.is_some() {
                    self.form_stack.pop();
                }
            }
            _ => {} // PostScript XObjects are not rendered by viewers.
        }
    }

    fn run_form(&mut self, stream: &'a Stream, parent_resources: &[&'a Dictionary], gs: &GState) {
        self.stats.forms += 1;
        let dict = &stream.dict;
        let matrix = dict
            .get(b"Matrix")
            .ok()
            .map(|o| deref(self.doc, o))
            .and_then(|o| o.as_array().ok())
            .map(|a| {
                a.iter()
                    .filter_map(|x| num(deref(self.doc, x)))
                    .collect::<Vec<_>>()
            })
            .filter(|v| v.len() == 6)
            .map_or(Matrix::IDENTITY, |v| {
                Matrix::new([v[0], v[1], v[2], v[3], v[4], v[5]])
            });
        let mut inner = gs.clone();
        inner.ctm = matrix.then(&gs.ctm);
        if let Some(form_box) = dict
            .get(b"BBox")
            .ok()
            .and_then(|o| rect(self.doc, deref(self.doc, o)))
        {
            inner.clip = inner.clip.and(Bbox::transformed(&form_box, &inner.ctm));
        }
        let own: Option<&'a Dictionary> = dict
            .get(b"Resources")
            .ok()
            .map(|o| deref(self.doc, o))
            .and_then(|o| o.as_dict().ok());
        let resources: Vec<&'a Dictionary> = match own {
            Some(r) => vec![r],
            None => parent_resources.to_vec(),
        };
        let content = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        self.run(&content, &resources, inner);
    }
}

enum TextPart<'b> {
    Text(&'b [u8]),
    Kern(f64),
}

fn matrix_array(m: &Matrix) -> [f32; 6] {
    [m.a, m.b, m.c, m.d, m.e, m.f].map(|v| v as f32)
}

fn extend(path: &mut Option<Bbox>, b: Bbox) {
    *path = Some(path.map_or(b, |p| p.union(&b)));
}

fn filter_name(doc: &Document, dict: &Dictionary) -> Option<String> {
    let f = deref(doc, dict.get(b"Filter").or_else(|_| dict.get(b"F")).ok()?);
    let name = match f {
        Object::Array(a) => a.last().and_then(|o| o.as_name().ok()),
        o => o.as_name().ok(),
    }?;
    Some(String::from_utf8_lossy(name).into_owned())
}

/// Finds `name` in the `category` dictionary (Font, XObject, …) of the
/// first resource dictionary that has it.
fn lookup<'a>(
    doc: &'a Document,
    resources: &[&'a Dictionary],
    category: &[u8],
    name: &[u8],
) -> Option<&'a Object> {
    resources.iter().find_map(|res| {
        let cat = deref(doc, res.get(category).ok()?).as_dict().ok()?;
        Some(deref(doc, cat.get(name).ok()?))
    })
}

/// Like [`lookup`] for streams, also returning the object id when indirect.
fn lookup_ref<'a>(
    doc: &'a Document,
    resources: &[&'a Dictionary],
    category: &[u8],
    name: &[u8],
) -> Option<(Option<ObjectId>, &'a Stream)> {
    resources.iter().find_map(|res| {
        let cat = deref(doc, res.get(category).ok()?).as_dict().ok()?;
        let entry = cat.get(name).ok()?;
        let id = entry.as_reference().ok();
        let stream = deref(doc, entry).as_stream().ok()?;
        Some((id, stream))
    })
}
