//! Extracts the images embedded in a PDF as [`RasterImage`]s.
//!
//! Instead of re-implementing every PDF image encoding (JPEG, Flate, LZW,
//! CCITT and JBIG2 for scans, JPEG 2000; ICC, Indexed, CMYK, Separation and
//! DeviceN colour; 16-bit samples, `/Decode` arrays, soft masks), each image
//! object is copied into a one-image PDF whose page is exactly its pixel
//! size, and rendered with `hayro` at one pixel per sample. That yields the
//! stored pixels for every format `hayro` supports.
//!
//! The result is then turned to the orientation it has on the page (its
//! placement turn/flip and the page's `/Rotate`) and given the DPI at which
//! it is printed there, so it keeps its real size in a layout.

use std::{collections::HashMap, sync::Arc};

use docforge_core::{PdfDocument, PdfImageInfo, RasterImage};
use hayro::{
    PixmapSettings, RenderCache, RenderSettings, hayro_interpret::InterpreterSettings,
    hayro_syntax::Pdf, vello_cpu::color::palette::css::TRANSPARENT,
};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};

use crate::print::art::{Imported, import_object};

/// One distinct image of a document.
#[derive(Clone, Debug)]
pub struct ImageEntry {
    pub key: String,
    /// 1-based page where it first appears.
    pub page: usize,
    pub info: PdfImageInfo,
    /// `/Rotate` of that page.
    pub page_rotation: u16,
}

/// Assigns selection keys (`p{page} {name}`) to the images of freshly
/// analysed pages: one key per image object, document order, unique.
pub(crate) fn assign_keys(pages: &mut [docforge_core::PdfPageInfo]) {
    let mut by_object: HashMap<(u32, u16), String> = HashMap::new();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (p, page) in pages.iter_mut().enumerate() {
        for img in &mut page.images {
            let Some(object) = img.object else { continue }; // inline: not selectable
            let key = by_object.entry(object).or_insert_with(|| {
                let mut key = format!("p{} {}", p + 1, img.name);
                if used.contains(&key) {
                    key = format!("{key}.{}", object.0);
                }
                used.insert(key.clone());
                key
            });
            img.key = Some(key.clone());
        }
    }
}

/// Every distinct (selectable) image, in document order.
pub fn list(doc: &PdfDocument) -> Vec<ImageEntry> {
    list_pages(&doc.page_info)
}

/// [`list`] for page analyses not (yet) wrapped in a document.
pub fn list_pages(pages: &[docforge_core::PdfPageInfo]) -> Vec<ImageEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (p, page) in pages.iter().enumerate() {
        for info in &page.images {
            if let Some(key) = &info.key
                && seen.insert(key.clone())
            {
                out.push(ImageEntry {
                    key: key.clone(),
                    page: p + 1,
                    info: info.clone(),
                    page_rotation: page.rotation,
                });
            }
        }
    }
    out
}

/// Number of inline images (which cannot be extracted).
pub fn inline_count(doc: &PdfDocument) -> usize {
    doc.page_info
        .iter()
        .flat_map(|p| &p.images)
        .filter(|i| i.key.is_none())
        .count()
}

/// Decodes the images with the given keys. `max_side` downsizes (for
/// thumbnails); `None` extracts at full resolution. Results are in the order
/// of `keys`.
pub fn extract(
    doc: &PdfDocument,
    keys: &[&str],
    max_side: Option<u32>,
) -> Vec<Result<RasterImage, String>> {
    let entries = list(doc);
    let parsed = match lopdf::Document::load_mem(&doc.bytes) {
        Ok(d) => d,
        Err(e) => {
            return keys
                .iter()
                .map(|_| Err(format!("the PDF cannot be read ({e})")))
                .collect();
        }
    };
    keys.iter()
        .map(|key| {
            let entry = entries
                .iter()
                .find(|e| e.key == *key)
                .ok_or_else(|| format!("image `{key}` is not in this PDF"))?;
            let id = entry
                .info
                .object
                .ok_or("inline images cannot be extracted")?;
            let stored = decode(&parsed, id, max_side)?;
            Ok(orient(stored, entry))
        })
        .collect()
}

/// Renders the image object into a pixel-exact page.
fn decode(src: &Document, id: ObjectId, max_side: Option<u32>) -> Result<RasterImage, String> {
    let dict = &src
        .get_object(id)
        .and_then(Object::as_stream)
        .map_err(|_| "the image object is missing")?
        .dict;
    let dim = |k: &[u8]| {
        dict.get(k)
            .ok()
            .and_then(|o| crate::pdf::analyze::num(crate::pdf::analyze::deref(src, o)))
            .unwrap_or(0.0)
    };
    let (w, h) = (dim(b"Width"), dim(b"Height"));
    if w < 1.0 || h < 1.0 || w > 65_000.0 || h > 65_000.0 {
        return Err(format!("unsupported image size {w} × {h}"));
    }

    let mut doc = Document::with_version("1.7");
    let image = import_object(&mut doc, src, id, &mut Imported::default());
    let pages_id = doc.new_object_id();
    let content = doc.add_object(Stream::new(
        Dictionary::new(),
        format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q").into_bytes(),
    ));
    let page = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), Object::Real(w as f32), Object::Real(h as f32)],
        "Contents" => content,
        "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![page.into()] },
        ),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).map_err(|e| e.to_string())?;

    let scale = match max_side {
        Some(m) => (m as f64 / w.max(h)).min(1.0) as f32,
        None => 1.0,
    };
    let pdf = Pdf::new(Arc::new(bytes)).map_err(|e| format!("{e:?}"))?;
    let page = pdf.pages().first().ok_or("render failed")?;
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: TRANSPARENT,
        },
    );
    // hayro produces premultiplied alpha; RasterImage is straight alpha.
    let mut data = pixmap.data_as_u8_slice().to_vec();
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    RasterImage::from_rgba8(pixmap.width() as u32, pixmap.height() as u32, data)
        .map_err(|e| e.to_string())
}

/// A unit axis direction in screen space (x right, y down).
type Axis = (i8, i8);

/// Snaps a page-space vector (y up) to the screen axis it points along,
/// after the page's clockwise `/Rotate`.
fn screen_axis(vx: f64, vy: f64, rotation: u16) -> Axis {
    let (mut x, mut y) = (vx, vy);
    for _ in 0..(rotation / 90) % 4 {
        (x, y) = (y, -x); // 90° clockwise in y-up space
    }
    let (sx, sy) = (x, -y); // to y-down screen space
    if sx.abs() >= sy.abs() {
        (sx.signum() as i8, 0)
    } else {
        (0, sy.signum() as i8)
    }
}

/// Turns the stored image to how it appears on the page and sets its DPI
/// from the size it is printed at.
fn orient(img: RasterImage, entry: &ImageEntry) -> RasterImage {
    let [a, b, c, d, ..] = entry.info.matrix.map(f64::from);
    // Image columns run along (a, b); rows run top-down, i.e. along −(c, d).
    let right = screen_axis(a, b, entry.page_rotation);
    let down = screen_axis(-c, -d, entry.page_rotation);
    let row_len = a.hypot(b);
    let col_len = c.hypot(d);

    let oriented = if right == down || (right.0 == 0) == (down.0 == 0) {
        img // degenerate (skewed/collapsed) placement: keep as stored
    } else {
        remap(&img, right, down)
    };
    // The printed width of the oriented image, in points.
    let width_pt = if right.0 != 0 || right == down {
        row_len
    } else {
        col_len
    };
    let dpi = (width_pt > 0.0).then(|| (oriented.width() as f64 / (width_pt / 72.0)) as f32);
    oriented.with_dpi(dpi)
}

/// Rearranges pixels so that source columns run along `right` and rows
/// along `down` (both screen axes).
fn remap(img: &RasterImage, right: Axis, down: Axis) -> RasterImage {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if right == (1, 0) && down == (0, 1) {
        return img.clone();
    }
    let (ow, oh) = if right.0 != 0 { (w, h) } else { (h, w) };
    // Screen position of source pixel (u, v) = u·right + v·down, shifted so
    // the smallest coordinate is 0.
    let pos = |u: i64, v: i64| {
        (
            u * right.0 as i64 + v * down.0 as i64,
            u * right.1 as i64 + v * down.1 as i64,
        )
    };
    let corners = [pos(0, 0), pos(w - 1, 0), pos(0, h - 1), pos(w - 1, h - 1)];
    let min_x = corners.iter().map(|p| p.0).min().unwrap_or(0);
    let min_y = corners.iter().map(|p| p.1).min().unwrap_or(0);
    let src = img.pixels();
    let mut out = vec![[0u8; 4]; (ow * oh) as usize];
    for v in 0..h {
        for u in 0..w {
            let (x, y) = pos(u, v);
            out[((y - min_y) * ow + (x - min_x)) as usize] = src[(v * w + u) as usize];
        }
    }
    RasterImage::from_rgba8(ow as u32, oh as u32, out.into_flattened())
        .expect("sized from the source")
}
