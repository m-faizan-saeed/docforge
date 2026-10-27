//! Shared PDF reading: a content-stream interpreter that measures what a
//! page paints ([`analyze`]), the font metrics it needs, geometry, and page
//! helpers used by PDF Input, PDF Edit and Print Layout.

pub mod analyze;
mod fonts;
pub mod geometry;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

use docforge_core::{PdfDocument, PdfPageInfo};

use self::analyze::{inherited, num, rect};

/// PDF points per millimetre.
pub const PT_PER_MM: f32 = 72.0 / 25.4;

/// Displayed page sizes (rotation applied), in mm.
pub fn page_sizes_mm(pages: &[PdfPageInfo]) -> Vec<[f32; 2]> {
    pages
        .iter()
        .map(|p| {
            let [x0, y0, x1, y1] = p.crop_box;
            let (w, h) = ((x1 - x0) / PT_PER_MM, (y1 - y0) / PT_PER_MM);
            if p.rotation % 180 == 90 {
                [h, w]
            } else {
                [w, h]
            }
        })
        .collect()
}

/// Writes a document to bytes. lopdf decrypts while loading, so a file that
/// was encrypted is written as a plain copy rather than one whose /Encrypt
/// dictionary no longer matches its contents.
pub fn save(doc: &mut lopdf::Document) -> Result<Vec<u8>, String> {
    if doc.trailer.remove(b"Encrypt").is_some() {
        doc.encryption_state = None;
    }
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| format!("cannot write the PDF ({e})"))?;
    Ok(out)
}

/// The visible area and display rotation of a page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBox {
    /// CropBox clipped to the MediaBox, `[x0, y0, x1, y1]` in points.
    pub crop: [f32; 4],
    /// Clockwise display rotation: 0, 90, 180 or 270.
    pub rotation: u16,
}

/// Visible area and rotation of every page, from the analysis attached to the
/// document or, for generated PDFs without one, read from the file.
pub fn page_boxes(doc: &PdfDocument) -> Vec<PageBox> {
    if !doc.page_info.is_empty() {
        return doc
            .page_info
            .iter()
            .map(|p| PageBox {
                crop: p.crop_box,
                rotation: p.rotation,
            })
            .collect();
    }
    let Ok(parsed) = lopdf::Document::load_mem(&doc.bytes) else {
        return Vec::new();
    };
    parsed
        .get_pages()
        .values()
        .map(|&page| {
            let media = inherited(&parsed, page, b"MediaBox")
                .and_then(|o| rect(&parsed, o))
                .unwrap_or(geometry::Bbox::from_rect([0.0, 0.0, 612.0, 792.0]));
            let crop = inherited(&parsed, page, b"CropBox")
                .and_then(|o| rect(&parsed, o))
                .and_then(|c| c.intersect(&media))
                .unwrap_or(media);
            let rotation = inherited(&parsed, page, b"Rotate")
                .and_then(num)
                .map_or(0, |r| (r as i64).rem_euclid(360) as u16 / 90 * 90);
            PageBox {
                crop: crop.to_array(),
                rotation,
            }
        })
        .collect()
}

/// Maps a rectangle in *displayed* page coordinates — fractions of the page
/// as shown (rotation applied), origin top-left, y down; what the preview's
/// handles use — to page points `[x0, y0, x1, y1]`.
pub fn displayed_to_page(norm: [f32; 4], page: PageBox) -> [f32; 4] {
    let [cx0, cy0, cx1, cy1] = page.crop;
    let (w, h) = (cx1 - cx0, cy1 - cy0);
    let point = |u: f32, v: f32| -> (f32, f32) {
        match page.rotation {
            // Shown turned clockwise: the page's left edge is at the top.
            90 => (cx0 + v * w, cy0 + u * h),
            180 => (cx1 - u * w, cy0 + v * h),
            270 => (cx1 - v * w, cy1 - u * h),
            _ => (cx0 + u * w, cy1 - v * h),
        }
    };
    let (a, b) = (point(norm[0], norm[1]), point(norm[2], norm[3]));
    [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]
}

/// Inverse of [`displayed_to_page`].
pub fn page_to_displayed(b: [f32; 4], page: PageBox) -> [f32; 4] {
    let [cx0, cy0, cx1, cy1] = page.crop;
    let (w, h) = ((cx1 - cx0).max(1e-6), (cy1 - cy0).max(1e-6));
    let norm = |x: f32, y: f32| -> (f32, f32) {
        match page.rotation {
            90 => ((y - cy0) / h, (x - cx0) / w),
            180 => ((cx1 - x) / w, (y - cy0) / h),
            270 => ((cy1 - y) / h, (cx1 - x) / w),
            _ => ((x - cx0) / w, (cy1 - y) / h),
        }
    };
    let (a, c) = (norm(b[0], b[1]), norm(b[2], b[3]));
    [a.0.min(c.0), a.1.min(c.1), a.0.max(c.0), a.1.max(c.1)]
}
