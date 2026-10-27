//! Just enough font information to measure where text lands: glyph advance
//! widths and the vertical extent of the font.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object};

use super::analyze::{deref, num};

#[derive(Debug, Clone)]
pub struct FontMetrics {
    /// Codes are two bytes wide (Type0 fonts with the usual Identity CMaps).
    pub two_byte: bool,
    widths: HashMap<u32, f64>,
    default_width: f64,
    /// Glyph space → text space for widths (1/1000 except for Type3 fonts).
    width_scale: f64,
    /// Vertical extent in text space units per unit font size.
    pub ascent: f64,
    pub descent: f64,
}

impl Default for FontMetrics {
    /// Used when a font cannot be found: an average sans-serif.
    fn default() -> Self {
        Self {
            two_byte: false,
            widths: HashMap::new(),
            default_width: 550.0,
            width_scale: 0.001,
            ascent: 0.8,
            descent: -0.2,
        }
    }
}

impl FontMetrics {
    /// Advance of `code` in text space units per unit font size.
    pub fn advance(&self, code: u32) -> f64 {
        self.widths
            .get(&code)
            .copied()
            .unwrap_or(self.default_width)
            * self.width_scale
    }

    /// Splits a string operand into character codes.
    pub fn codes<'a>(&self, bytes: &'a [u8]) -> impl Iterator<Item = u32> + 'a {
        let step = if self.two_byte { 2 } else { 1 };
        bytes
            .chunks(step)
            .map(|c| c.iter().fold(0u32, |acc, &b| (acc << 8) | b as u32))
    }

    pub fn from_dict(doc: &Document, font: &Dictionary) -> Self {
        let mut m = Self::default();
        let subtype = font
            .get(b"Subtype")
            .and_then(Object::as_name)
            .unwrap_or_default();
        let base = font
            .get(b"BaseFont")
            .and_then(Object::as_name)
            .unwrap_or_default();
        if base.windows(7).any(|w| w == b"Courier") {
            m.default_width = 600.0;
        }

        let descriptor_source = if subtype == b"Type0" {
            m.two_byte = true;
            m.default_width = 1000.0;
            let Some(cid_font) = font
                .get(b"DescendantFonts")
                .ok()
                .map(|o| deref(doc, o))
                .and_then(|o| o.as_array().ok())
                .and_then(|a| a.first())
                .map(|o| deref(doc, o))
                .and_then(|o| o.as_dict().ok())
            else {
                return m;
            };
            if let Some(dw) = cid_font.get(b"DW").ok().and_then(|o| num(deref(doc, o))) {
                m.default_width = dw;
            }
            if let Some(w) = cid_font
                .get(b"W")
                .ok()
                .map(|o| deref(doc, o))
                .and_then(|o| o.as_array().ok())
            {
                read_cid_widths(doc, w, &mut m.widths);
            }
            cid_font
        } else {
            if subtype == b"Type3" {
                m.width_scale = font
                    .get(b"FontMatrix")
                    .ok()
                    .map(|o| deref(doc, o))
                    .and_then(|o| o.as_array().ok())
                    .and_then(|a| a.first())
                    .and_then(|o| num(deref(doc, o)))
                    .unwrap_or(0.001);
            }
            let first = font
                .get(b"FirstChar")
                .ok()
                .and_then(|o| num(deref(doc, o)))
                .unwrap_or(0.0) as u32;
            if let Some(widths) = font
                .get(b"Widths")
                .ok()
                .map(|o| deref(doc, o))
                .and_then(|o| o.as_array().ok())
            {
                for (i, w) in widths.iter().enumerate() {
                    if let Some(w) = num(deref(doc, w)) {
                        m.widths.insert(first + i as u32, w);
                    }
                }
            }
            font
        };

        if let Some(desc) = descriptor_source
            .get(b"FontDescriptor")
            .ok()
            .map(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())
        {
            let get = |k: &[u8]| desc.get(k).ok().and_then(|o| num(deref(doc, o)));
            if let (Some(a), Some(d)) = (get(b"Ascent"), get(b"Descent"))
                && a > d
                && a != 0.0
            {
                m.ascent = a / 1000.0;
                m.descent = d.min(0.0) / 1000.0;
            }
            if let Some(mw) = get(b"MissingWidth").filter(|w| *w > 0.0) {
                m.default_width = mw;
            }
        }
        m
    }
}

/// CID width arrays mix two forms: `c [w1 w2 …]` and `c_first c_last w`.
fn read_cid_widths(doc: &Document, w: &[Object], out: &mut HashMap<u32, f64>) {
    let mut i = 0;
    while i < w.len() {
        let Some(first) = num(deref(doc, &w[i])) else {
            break;
        };
        match w.get(i + 1).map(|o| deref(doc, o)) {
            Some(Object::Array(list)) => {
                for (k, v) in list.iter().enumerate() {
                    if let Some(v) = num(deref(doc, v)) {
                        out.insert(first as u32 + k as u32, v);
                    }
                }
                i += 2;
            }
            Some(o) => {
                let (Some(last), Some(width)) =
                    (num(o), w.get(i + 2).and_then(|x| num(deref(doc, x))))
                else {
                    break;
                };
                for code in first as u32..=last as u32 {
                    out.insert(code, width);
                }
                i += 3;
            }
            None => break,
        }
    }
}
