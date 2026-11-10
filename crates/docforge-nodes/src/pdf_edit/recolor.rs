//! Changes the colours of vector graphics (paths and text) by rewriting the
//! colour operators of PDF content streams. The drawing stays vector; images
//! and gradients (shadings) are left as they are.
//!
//! Every colour-setting operator (`g`, `rg`, `k`, `cs`+`sc`/`scn`, and their
//! stroking twins) is converted to RGB, passed through the mapping, and —
//! when the mapping changes it — replaced by an `rg`/`RG` with the new
//! colour. Colours the mapping leaves alone are written back unchanged, in
//! their original colour space (spot colours stay spot colours). Since a
//! replacement switches the stream to DeviceRGB, the original colour space is
//! re-selected before any later untouched `sc`/`scn` that depends on it.
//!
//! A colour can also be *removed*: shapes painted with it are not drawn at
//! all (their painting operator becomes `n`, or the other half of a
//! fill-and-stroke), and text in it becomes invisible (render mode 3), so
//! whatever lies underneath shows through — e.g. a white page background
//! that would hide the layer below in an overlay.

use std::collections::{HashMap, HashSet};

use lopdf::{
    Dictionary, Document, Object, ObjectId,
    content::{Content, Operation},
};
use serde::{Deserialize, Serialize};

use crate::pdf::analyze::{deref, inherited, num};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecolorMode {
    #[default]
    Off,
    /// Every vector colour becomes `color` (optionally keeping white).
    Single,
    /// Colours matching a rule's `from` become its `to`.
    Replace,
    /// Every vector colour becomes its gray equivalent.
    Grayscale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorRule {
    pub from: [u8; 3],
    pub to: [u8; 3],
    /// Instead of `to`: don't draw what is painted in this colour.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remove: bool,
}

/// What a colour becomes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    Color([f32; 3]),
    /// Not drawn (transparent).
    Remove,
}

/// Settings for changing vector colours.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recolor {
    pub mode: RecolorMode,
    /// The colour for `single`.
    pub color: [u8; 3],
    /// In `single` mode, leave near-white colours alone so white details
    /// (pads, holes, knock-outs) stay white.
    pub keep_white: bool,
    /// In `single` mode, don't draw near-white shapes at all (e.g. a white
    /// background); takes precedence over `keep_white`.
    pub remove_white: bool,
    /// Rules for `replace`, tried in order.
    pub rules: Vec<ColorRule>,
    /// How far (per channel, 0–255) a colour may differ from a rule's `from`.
    pub tolerance: u8,
    /// Change outline (stroke) colours.
    pub strokes: bool,
    /// Change fill colours (this includes most text).
    pub fills: bool,
}

impl Default for Recolor {
    fn default() -> Self {
        Self {
            mode: RecolorMode::Off,
            color: [0, 0, 0],
            keep_white: true,
            remove_white: false,
            rules: Vec::new(),
            tolerance: 24,
            strokes: true,
            fills: true,
        }
    }
}

impl Recolor {
    /// PCB assembly guide, top layer: everything black, white kept.
    pub fn pcb_top() -> Self {
        Self {
            mode: RecolorMode::Single,
            color: [0, 0, 0],
            ..Self::default()
        }
    }

    /// PCB assembly guide, bottom layer: everything mid gray, white kept.
    pub fn pcb_bottom() -> Self {
        Self {
            mode: RecolorMode::Single,
            color: [160, 160, 160],
            ..Self::default()
        }
    }

    /// Removes white shapes (a page background, white boxes behind the
    /// art) and leaves every other colour as it is.
    pub fn remove_white() -> Self {
        Self {
            mode: RecolorMode::Replace,
            rules: vec![ColorRule {
                from: [255, 255, 255],
                to: [255, 255, 255],
                remove: true,
            }],
            ..Self::default()
        }
    }

    pub fn is_active(&self) -> bool {
        self.mode != RecolorMode::Off && (self.strokes || self.fills)
    }

    /// What `rgb` (0..1) becomes, or `None` to leave it unchanged.
    pub fn map(&self, rgb: [f32; 3], stroke: bool) -> Option<Paint> {
        if (stroke && !self.strokes) || (!stroke && !self.fills) {
            return None;
        }
        let to_f = |c: [u8; 3]| c.map(|v| v as f32 / 255.0);
        match self.mode {
            RecolorMode::Off => None,
            RecolorMode::Single => {
                let white = rgb.iter().all(|&c| c >= 0.94);
                if white && self.remove_white {
                    Some(Paint::Remove)
                } else if white && self.keep_white {
                    None
                } else {
                    Some(Paint::Color(to_f(self.color)))
                }
            }
            RecolorMode::Replace => {
                let tol = self.tolerance as f32 / 255.0 + 1e-4;
                self.rules
                    .iter()
                    .find(|r| (0..3).all(|i| (rgb[i] - r.from[i] as f32 / 255.0).abs() <= tol))
                    .map(|r| {
                        if r.remove {
                            Paint::Remove
                        } else {
                            Paint::Color(to_f(r.to))
                        }
                    })
            }
            RecolorMode::Grayscale => {
                let l = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
                Some(Paint::Color([l, l, l]))
            }
        }
    }
}

/// What a recolouring pass did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecolorStats {
    /// Colour settings replaced.
    pub changed: usize,
    /// Shapes and text runs no longer drawn because their colour is removed.
    pub removed: usize,
    /// Colour settings left alone by the mapping.
    pub kept: usize,
    /// Gradient fills (`sh`) and pattern colours, which are not recoloured.
    pub untouched_shadings: usize,
    pub unreadable_streams: usize,
}

/// One colour found in a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaletteEntry {
    pub rgb: [u8; 3],
    pub fills: usize,
    pub strokes: usize,
}

/// Rewrites every page (and the forms and coloured patterns they use).
pub fn recolor_document(doc: &mut Document, recolor: &Recolor) -> RecolorStats {
    let mut stats = RecolorStats::default();
    let mut map = |rgb: [f32; 3], stroke: bool| recolor.map(rgb, stroke);
    walk(doc, &mut map, &mut stats, true);
    stats
}

/// The distinct vector colours a document paints with, most used first.
pub fn palette(bytes: &[u8]) -> Vec<PaletteEntry> {
    let Ok(mut doc) = Document::load_mem(bytes) else {
        return Vec::new();
    };
    let mut counts: HashMap<[u8; 3], (usize, usize)> = HashMap::new();
    let mut record = |rgb: [f32; 3], stroke: bool| {
        let key = rgb.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        let entry = counts.entry(key).or_default();
        if stroke {
            entry.1 += 1
        } else {
            entry.0 += 1
        }
        None
    };
    walk(&mut doc, &mut record, &mut RecolorStats::default(), false);
    let mut out: Vec<PaletteEntry> = counts
        .into_iter()
        .map(|(rgb, (fills, strokes))| PaletteEntry {
            rgb,
            fills,
            strokes,
        })
        .collect();
    out.sort_by(|a, b| {
        (b.fills + b.strokes)
            .cmp(&(a.fills + a.strokes))
            .then(a.rgb.cmp(&b.rgb))
    });
    out
}

type Mapper<'a> = dyn FnMut([f32; 3], bool) -> Option<Paint> + 'a;

/// Visits every content stream reachable from the pages; with `write`,
/// stores the rewritten streams back.
fn walk(doc: &mut Document, map: &mut Mapper<'_>, stats: &mut RecolorStats, write: bool) {
    let pages: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    let mut queue: Vec<(ObjectId, Dictionary)> = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();

    for page in pages {
        let resources = inherited(doc, page, b"Resources")
            .and_then(|o| o.as_dict().ok())
            .cloned()
            .unwrap_or_default();
        let content = doc.get_page_content(page);
        match rewrite(doc, &content, &resources, map, stats) {
            Some(new) if write => {
                let _ = doc.change_page_content(page, new);
            }
            Some(_) => {}
            None => stats.unreadable_streams += 1,
        }
        enqueue_children(doc, &resources, &mut queue, &mut seen);
    }

    // Forms and coloured tiling patterns, each rewritten once.
    while let Some((id, parent_resources)) = queue.pop() {
        let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else {
            continue;
        };
        let resources = stream
            .dict
            .get(b"Resources")
            .ok()
            .map(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())
            .cloned()
            .unwrap_or(parent_resources);
        let content = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        let rewritten = rewrite(doc, &content, &resources, map, stats);
        enqueue_children(doc, &resources, &mut queue, &mut seen);
        match rewritten {
            Some(new) if write => {
                if let Ok(Object::Stream(s)) = doc.get_object_mut(id) {
                    s.set_plain_content(new);
                    let _ = s.compress();
                }
            }
            Some(_) => {}
            None => stats.unreadable_streams += 1,
        }
    }
}

/// Queues the Form XObjects and coloured tiling patterns in `resources`.
fn enqueue_children(
    doc: &Document,
    resources: &Dictionary,
    queue: &mut Vec<(ObjectId, Dictionary)>,
    seen: &mut HashSet<ObjectId>,
) {
    for (category, is_wanted) in [
        (
            &b"XObject"[..],
            (|d: &Dictionary| {
                d.get(b"Subtype")
                    .and_then(Object::as_name)
                    .is_ok_and(|n| n == b"Form")
            }) as fn(&Dictionary) -> bool,
        ),
        // PaintType 1: the pattern carries its own colours.
        (&b"Pattern"[..], |d: &Dictionary| {
            d.get(b"PatternType").ok().and_then(num) == Some(1.0)
                && d.get(b"PaintType").ok().and_then(num) == Some(1.0)
        }),
    ] {
        let Some(dict) = resources
            .get(category)
            .ok()
            .map(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())
        else {
            continue;
        };
        for (_, entry) in dict.iter() {
            let Ok(id) = entry.as_reference() else {
                continue;
            };
            let wanted = doc
                .get_object(id)
                .and_then(Object::as_stream)
                .is_ok_and(|s| is_wanted(&s.dict));
            if wanted && seen.insert(id) {
                queue.push((id, resources.clone()));
            }
        }
    }
}

/// A colour space, as far as converting its colours to RGB goes.
#[derive(Clone, Debug)]
enum Space {
    Gray,
    Rgb,
    Cmyk,
    Lab,
    Indexed {
        base: Box<Space>,
        table: Vec<u8>,
    },
    /// Separation / DeviceN: approximated as gray from the tint.
    Tint,
    Pattern,
    Unknown,
}

impl Space {
    fn to_rgb(&self, c: &[f64]) -> Option<[f32; 3]> {
        let g = |i: usize| c.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0) as f32;
        Some(match self {
            Space::Gray => [g(0); 3],
            Space::Rgb => [g(0), g(1), g(2)],
            Space::Cmyk => {
                let k = 1.0 - g(3);
                [(1.0 - g(0)) * k, (1.0 - g(1)) * k, (1.0 - g(2)) * k]
            }
            Space::Lab => [(c.first().copied().unwrap_or(0.0) / 100.0).clamp(0.0, 1.0) as f32; 3],
            Space::Indexed { base, table } => {
                let n = base.components();
                let i = c.first().copied().unwrap_or(0.0).max(0.0) as usize;
                let entry = table.get(i * n..(i + 1) * n)?;
                let comps: Vec<f64> = entry.iter().map(|&v| v as f64 / 255.0).collect();
                return base.to_rgb(&comps);
            }
            Space::Tint => {
                let t = c.iter().copied().sum::<f64>() / c.len().max(1) as f64;
                [(1.0 - t).clamp(0.0, 1.0) as f32; 3]
            }
            Space::Pattern | Space::Unknown => return None,
        })
    }

    fn components(&self) -> usize {
        match self {
            Space::Gray | Space::Indexed { .. } | Space::Tint | Space::Pattern | Space::Unknown => {
                1
            }
            Space::Rgb | Space::Lab => 3,
            Space::Cmyk => 4,
        }
    }

    /// The colour a space starts with after `cs` (black, or full tint).
    fn initial(&self) -> Vec<f64> {
        match self {
            Space::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            Space::Tint => vec![1.0],
            other => vec![0.0; other.components()],
        }
    }
}

fn resolve_space(doc: &Document, obj: &Object, resources: &Dictionary, depth: u8) -> Space {
    let obj = deref(doc, obj);
    if depth > 4 {
        return Space::Unknown;
    }
    if let Ok(name) = obj.as_name() {
        return match name {
            b"DeviceGray" | b"G" | b"CalGray" => Space::Gray,
            b"DeviceRGB" | b"RGB" | b"CalRGB" => Space::Rgb,
            b"DeviceCMYK" | b"CMYK" => Space::Cmyk,
            b"Pattern" => Space::Pattern,
            other => {
                let named = resources
                    .get(b"ColorSpace")
                    .ok()
                    .map(|o| deref(doc, o))
                    .and_then(|o| o.as_dict().ok())
                    .and_then(|d| d.get(other).ok());
                match named {
                    Some(o) => resolve_space(doc, o, resources, depth + 1),
                    None => Space::Unknown,
                }
            }
        };
    }
    let Ok(arr) = obj.as_array() else {
        return Space::Unknown;
    };
    let family = arr
        .first()
        .and_then(|o| o.as_name().ok())
        .unwrap_or_default();
    match family {
        b"ICCBased" => {
            let n = arr
                .get(1)
                .map(|o| deref(doc, o))
                .and_then(|o| o.as_stream().ok())
                .and_then(|s| s.dict.get(b"N").ok().and_then(num))
                .unwrap_or(3.0);
            match n as u32 {
                1 => Space::Gray,
                4 => Space::Cmyk,
                _ => Space::Rgb,
            }
        }
        b"CalGray" => Space::Gray,
        b"CalRGB" => Space::Rgb,
        b"Lab" => Space::Lab,
        b"Separation" | b"DeviceN" => Space::Tint,
        b"Pattern" => Space::Pattern,
        b"Indexed" | b"I" => {
            let base = arr
                .get(1)
                .map(|o| resolve_space(doc, o, resources, depth + 1))
                .unwrap_or(Space::Unknown);
            let table = match arr.get(3).map(|o| deref(doc, o)) {
                Some(Object::String(bytes, _)) => bytes.clone(),
                Some(Object::Stream(s)) => s
                    .decompressed_content()
                    .unwrap_or_else(|_| s.content.clone()),
                _ => Vec::new(),
            };
            Space::Indexed {
                base: Box::new(base),
                table,
            }
        }
        _ => Space::Unknown,
    }
}

/// Colour state for one side (fill or stroke).
#[derive(Clone)]
struct Side {
    space: Space,
    /// The original operator that selected `space` (re-emitted when needed).
    select: Operation,
    /// Whether the output stream's current colour space is `space`.
    in_sync: bool,
    /// The current colour is removed: what this side paints is not drawn.
    hidden: bool,
}

impl Side {
    fn new(stroke: bool) -> Self {
        Self {
            space: Space::Gray,
            select: Operation::new(
                if stroke { "CS" } else { "cs" },
                vec![Object::Name(b"DeviceGray".to_vec())],
            ),
            in_sync: true,
            hidden: false,
        }
    }
}

/// The painting operator `op` with the removed half (fill and/or stroke)
/// left out, or `None` when nothing it paints is removed.
fn without_hidden(op: &str, fill_hidden: bool, stroke_hidden: bool) -> Option<Vec<&'static str>> {
    let (fills, strokes) = match op {
        "f" | "F" | "f*" => (true, false),
        "S" | "s" => (false, true),
        "B" | "B*" | "b" | "b*" => (true, true),
        _ => return None,
    };
    let fill_gone = fills && fill_hidden;
    let stroke_gone = strokes && stroke_hidden;
    if !fill_gone && !stroke_gone {
        return None;
    }
    let even_odd = op.ends_with('*');
    let closes = matches!(op, "s" | "b" | "b*");
    Some(match (fills && !fill_gone, strokes && !stroke_gone) {
        // Only the fill stays.
        (true, false) => {
            let fill = if even_odd { "f*" } else { "f" };
            if closes { vec!["h", fill] } else { vec![fill] }
        }
        // Only the outline stays.
        (false, true) => vec![if closes { "s" } else { "S" }],
        // Nothing is drawn; the path is still ended (a pending clip, `W`,
        // still applies).
        _ => vec!["n"],
    })
}

/// Text render mode `mode` with the removed halves made invisible.
fn text_mode_without_hidden(mode: i64, fill_hidden: bool, stroke_hidden: bool) -> i64 {
    let (fills, strokes, clips) = match mode {
        0 => (true, false, false),
        1 => (false, true, false),
        2 => (true, true, false),
        4 => (true, false, true),
        5 => (false, true, true),
        6 => (true, true, true),
        7 => (false, false, true),
        _ => (false, false, false),
    };
    let (f, s) = (fills && !fill_hidden, strokes && !stroke_hidden);
    let visible = match (f, s) {
        (true, false) => 0,
        (false, true) => 1,
        (true, true) => 2,
        (false, false) => 3,
    };
    match (clips, visible) {
        (false, v) => v,
        (true, 3) => 7,
        (true, v) => v + 4,
    }
}

/// Rewrites one content stream; `None` if it cannot be parsed.
fn rewrite(
    doc: &Document,
    content: &[u8],
    resources: &Dictionary,
    map: &mut Mapper<'_>,
    stats: &mut RecolorStats,
) -> Option<Vec<u8>> {
    let ops = Content::decode(content).ok()?.operations;
    let mut out: Vec<Operation> = Vec::with_capacity(ops.len());
    let (mut fill, mut stroke) = (Side::new(false), Side::new(true));
    // Text render mode (Tr), part of the graphics state like the colours.
    let mut text_mode: i64 = 0;
    let mut stack: Vec<(Side, Side, i64)> = Vec::new();

    for op in ops {
        let name = op.operator.as_str();
        let is_stroke = matches!(name, "G" | "RG" | "K" | "CS" | "SC" | "SCN");
        let comps: Vec<f64> = op.operands.iter().filter_map(num).collect();
        match name {
            "q" => {
                stack.push((fill.clone(), stroke.clone(), text_mode));
                out.push(op);
            }
            "Q" => {
                if let Some((f, s, t)) = stack.pop() {
                    fill = f;
                    stroke = s;
                    text_mode = t;
                }
                out.push(op);
            }
            "f" | "F" | "f*" | "S" | "s" | "B" | "B*" | "b" | "b*" => {
                match without_hidden(name, fill.hidden, stroke.hidden) {
                    Some(ops) => {
                        stats.removed += 1;
                        out.extend(ops.into_iter().map(|o| Operation::new(o, vec![])));
                    }
                    None => out.push(op),
                }
            }
            "Tr" => {
                text_mode = comps.first().map_or(0, |&m| m as i64);
                out.push(op);
            }
            "Tj" | "TJ" | "'" | "\"" => {
                let mode = text_mode_without_hidden(text_mode, fill.hidden, stroke.hidden);
                if mode == text_mode {
                    out.push(op);
                } else {
                    stats.removed += 1;
                    let tr = |m: i64| Operation::new("Tr", vec![Object::Integer(m)]);
                    out.extend([tr(mode), op, tr(text_mode)]);
                }
            }
            "g" | "G" | "rg" | "RG" | "k" | "K" => {
                let side = if is_stroke { &mut stroke } else { &mut fill };
                side.space = match name {
                    "g" | "G" => Space::Gray,
                    "rg" | "RG" => Space::Rgb,
                    _ => Space::Cmyk,
                };
                let device = match &side.space {
                    Space::Gray => &b"DeviceGray"[..],
                    Space::Rgb => b"DeviceRGB",
                    _ => b"DeviceCMYK",
                };
                side.select = Operation::new(
                    if is_stroke { "CS" } else { "cs" },
                    vec![Object::Name(device.to_vec())],
                );
                emit_color(&mut out, side, &comps, op, is_stroke, map, stats);
            }
            "cs" | "CS" => {
                let side = if is_stroke { &mut stroke } else { &mut fill };
                side.space = op
                    .operands
                    .first()
                    .map(|o| resolve_space(doc, o, resources, 0))
                    .unwrap_or(Space::Unknown);
                side.select = op.clone();
                if matches!(side.space, Space::Pattern | Space::Unknown) {
                    side.in_sync = true;
                    side.hidden = false;
                    out.push(op);
                } else {
                    // `cs` also sets the colour to the space's initial value.
                    let initial = side.space.initial();
                    emit_color(&mut out, side, &initial, op, is_stroke, map, stats);
                }
            }
            "sc" | "scn" | "SC" | "SCN" => {
                let side = if is_stroke { &mut stroke } else { &mut fill };
                if matches!(side.space, Space::Pattern | Space::Unknown)
                    || comps.len() != op.operands.len()
                {
                    if matches!(side.space, Space::Pattern) {
                        stats.untouched_shadings += 1;
                    }
                    if !side.in_sync {
                        out.push(side.select.clone());
                        side.in_sync = true;
                    }
                    side.hidden = false;
                    out.push(op);
                } else {
                    emit_color(&mut out, side, &comps, op, is_stroke, map, stats);
                }
            }
            "sh" => {
                stats.untouched_shadings += 1;
                out.push(op);
            }
            _ => out.push(op),
        }
    }
    Content { operations: out }.encode().ok()
}

/// Writes the colour `comps` (in `side.space`) either mapped, as `rg`/`RG`,
/// or unchanged, as `original` — re-selecting the colour space first if an
/// earlier replacement switched the output to DeviceRGB.
fn emit_color(
    out: &mut Vec<Operation>,
    side: &mut Side,
    comps: &[f64],
    original: Operation,
    stroke: bool,
    map: &mut Mapper<'_>,
    stats: &mut RecolorStats,
) {
    let mapped = side.space.to_rgb(comps).and_then(|rgb| map(rgb, stroke));
    side.hidden = mapped == Some(Paint::Remove);
    match mapped {
        Some(Paint::Color([r, g, b])) => {
            stats.changed += 1;
            out.push(Operation::new(
                if stroke { "RG" } else { "rg" },
                vec![Object::Real(r), Object::Real(g), Object::Real(b)],
            ));
            side.in_sync = matches!(side.space, Space::Rgb);
        }
        // A removed colour is still set (later operators may rely on the
        // colour space); the painting operators leave out what uses it.
        Some(Paint::Remove) | None => {
            if mapped.is_none() {
                stats.kept += 1;
            }
            let selects_space = matches!(
                original.operator.as_str(),
                "g" | "G" | "rg" | "RG" | "k" | "K" | "cs" | "CS"
            );
            if !selects_space && !side.in_sync {
                out.push(side.select.clone());
            }
            out.push(original);
            side.in_sync = true;
        }
    }
}

#[cfg(test)]
pub(super) mod tests_support {
    use super::*;

    /// Rewrites a lone content stream (no resources) with `recolor`.
    pub fn rewrite_plain(content: &[u8], recolor: &Recolor) -> Vec<u8> {
        let doc = Document::with_version("1.5");
        let mut map = |rgb: [f32; 3], stroke: bool| recolor.map(rgb, stroke);
        rewrite(
            &doc,
            content,
            &Dictionary::new(),
            &mut map,
            &mut RecolorStats::default(),
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painting_operators_drop_only_the_removed_half() {
        let cases = [
            ("f", (true, false), Some(vec!["n"])),
            ("f*", (true, true), Some(vec!["n"])),
            ("f", (false, true), None),
            ("S", (false, true), Some(vec!["n"])),
            ("s", (true, false), None),
            ("B", (true, false), Some(vec!["S"])),
            ("B*", (false, true), Some(vec!["f*"])),
            ("b", (true, false), Some(vec!["s"])),
            ("b", (false, true), Some(vec!["h", "f"])),
            ("b*", (false, true), Some(vec!["h", "f*"])),
            ("B", (true, true), Some(vec!["n"])),
            ("n", (true, true), None),
        ];
        for (op, (fill, stroke), expected) in cases {
            assert_eq!(
                without_hidden(op, fill, stroke),
                expected,
                "{op} {fill} {stroke}"
            );
        }
    }

    #[test]
    fn text_render_modes_lose_the_removed_half_and_keep_clipping() {
        assert_eq!(text_mode_without_hidden(0, true, false), 3);
        assert_eq!(text_mode_without_hidden(0, false, true), 0);
        assert_eq!(text_mode_without_hidden(2, true, false), 1);
        assert_eq!(text_mode_without_hidden(2, false, true), 0);
        assert_eq!(text_mode_without_hidden(6, true, true), 7);
        assert_eq!(text_mode_without_hidden(4, true, false), 7);
        assert_eq!(text_mode_without_hidden(3, true, true), 3);
    }
}
