//! Vector artwork from PDF pages, placed as Form XObjects.
//!
//! The reference grid tool copied the page's whole content stream into every
//! grid cell. Here each source page becomes one Form XObject — its content,
//! resources (fonts, images, nested forms, graphics states) copied into the
//! output document once — and every copy is a single `Do`. The art stays
//! vector, the file stays small however many copies are printed, and the
//! form's `/BBox` clips anything outside the content box.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};

use super::pack::Rect;
use crate::pdf::{analyze::inherited, geometry::Matrix};

/// One page of a source PDF, to be printed as a piece.
pub struct PdfArt<'a> {
    pub doc: &'a Document,
    pub page: ObjectId,
    /// The artwork's box in the page's own coordinates, points.
    pub bbox: [f32; 4],
    /// Clockwise display rotation of the source page (0/90/180/270).
    pub rotation: u16,
}

impl PdfArt<'_> {
    /// Width and height as displayed (rotation applied), in points.
    pub fn display_size_pt(&self) -> [f32; 2] {
        let [x0, y0, x1, y1] = self.bbox;
        let (w, h) = (x1 - x0, y1 - y0);
        if self.rotation % 180 == 90 {
            [h, w]
        } else {
            [w, h]
        }
    }

    /// Transform from the page's coordinates onto `slot` (output page points).
    /// `rotated` turns the piece 90° counter-clockwise, as for images.
    pub fn placement(&self, slot: &Rect, rotated: bool) -> Matrix {
        let [x0, y0, x1, y1] = self.bbox.map(f64::from);
        let (w, h) = (x1 - x0, y1 - y0);
        // 1. Move the art's corner to the origin.
        let mut m = Matrix::translate(-x0, -y0);
        // 2. Show it the way the source page is displayed (/Rotate is clockwise).
        let rotate = match self.rotation {
            90 => Matrix::new([0.0, -1.0, 1.0, 0.0, 0.0, w]),
            180 => Matrix::new([-1.0, 0.0, 0.0, -1.0, w, h]),
            270 => Matrix::new([0.0, 1.0, -1.0, 0.0, h, 0.0]),
            _ => Matrix::IDENTITY,
        };
        m = m.then(&rotate);
        // 3. Scale to the piece size (unrotated slot dimensions).
        let [dw, dh] = self.display_size_pt().map(f64::from);
        let (sw, sh) = (slot.w as f64, slot.h as f64);
        let (pw, ph) = if rotated { (sh, sw) } else { (sw, sh) };
        m = m.then(&Matrix::new([pw / dw, 0.0, 0.0, ph / dh, 0.0, 0.0]));
        // 4. Turn it into the slot if the packer rotated it.
        if rotated {
            m = m.then(&Matrix::new([0.0, 1.0, -1.0, 0.0, sw, 0.0]));
        }
        m.then(&Matrix::translate(slot.x as f64, slot.y as f64))
    }
}

/// Objects already copied into the output, keyed by source document and id,
/// so pages of one PDF share fonts and images.
#[derive(Default)]
pub struct Imported {
    map: HashMap<(usize, ObjectId), ObjectId>,
}

/// Copies the page into `dst` as a Form XObject and returns its id.
pub fn import_page_as_form(
    dst: &mut Document,
    art: &PdfArt<'_>,
    imported: &mut Imported,
) -> ObjectId {
    let src = art.doc;
    let key = src as *const Document as usize;
    let content = src.get_page_content(art.page);
    let resources = inherited(src, art.page, b"Resources")
        .map(|r| copy(src, dst, key, r, imported))
        .unwrap_or_else(|| Object::Dictionary(Dictionary::new()));

    let [x0, y0, x1, y1] = art.bbox;
    let mut dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
        "FormType" => 1,
        "BBox" => vec![Object::Real(x0), Object::Real(y0), Object::Real(x1), Object::Real(y1)],
        "Resources" => resources,
    };
    // Keep the page's transparency group so blending looks the same.
    if let Ok(group) = src.get_dictionary(art.page).and_then(|p| p.get(b"Group")) {
        let group = copy(src, dst, key, group, imported);
        dict.set("Group", group);
    }
    dst.add_object(Stream::new(dict, content))
}

/// Copies object `id` from `src` into `dst`, with everything it references
/// (e.g. an image's soft mask or colour space), and returns its new id.
pub(crate) fn import_object(
    dst: &mut Document,
    src: &Document,
    id: ObjectId,
    imported: &mut Imported,
) -> ObjectId {
    copy_ref(src, dst, src as *const Document as usize, id, imported)
}

fn copy(
    src: &Document,
    dst: &mut Document,
    key: usize,
    obj: &Object,
    imported: &mut Imported,
) -> Object {
    match obj {
        Object::Reference(id) => Object::Reference(copy_ref(src, dst, key, *id, imported)),
        Object::Array(items) => Object::Array(
            items
                .iter()
                .map(|o| copy(src, dst, key, o, imported))
                .collect(),
        ),
        Object::Dictionary(d) => Object::Dictionary(copy_dict(src, dst, key, d, imported)),
        Object::Stream(s) => {
            // Bytes stay exactly as stored (still encoded per /Filter).
            let mut stream = Stream::new(
                copy_dict(src, dst, key, &s.dict, imported),
                s.content.clone(),
            );
            stream.allows_compression = s.allows_compression;
            Object::Stream(stream)
        }
        other => other.clone(),
    }
}

fn copy_dict(
    src: &Document,
    dst: &mut Document,
    key: usize,
    d: &Dictionary,
    imported: &mut Imported,
) -> Dictionary {
    let mut out = Dictionary::new();
    for (k, v) in d.iter() {
        // Back-links into the source page tree would drag in whole pages.
        if k == b"Parent" {
            continue;
        }
        out.set(k.clone(), copy(src, dst, key, v, imported));
    }
    out
}

fn copy_ref(
    src: &Document,
    dst: &mut Document,
    key: usize,
    id: ObjectId,
    imported: &mut Imported,
) -> ObjectId {
    if let Some(&done) = imported.map.get(&(key, id)) {
        return done;
    }
    // Reserve the id first so cycles resolve to it.
    let new_id = dst.new_object_id();
    imported.map.insert((key, id), new_id);
    let obj = src.get_object(id).cloned().unwrap_or(Object::Null);
    let copied = copy(src, dst, key, &obj, imported);
    dst.objects.insert(new_id, copied);
    new_id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn art(rotation: u16) -> PdfArt<'static> {
        static DOC: std::sync::OnceLock<Document> = std::sync::OnceLock::new();
        PdfArt {
            doc: DOC.get_or_init(Document::new),
            page: (1, 0),
            bbox: [100.0, 200.0, 140.0, 220.0], // 40 × 20 pt
            rotation,
        }
    }

    fn corners(m: &Matrix, a: &PdfArt<'_>) -> Vec<(i32, i32)> {
        let [x0, y0, x1, y1] = a.bbox.map(f64::from);
        let mut v: Vec<_> = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            .iter()
            .map(|&(x, y)| {
                let (px, py) = m.apply(x, y);
                (px.round() as i32, py.round() as i32)
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn art_maps_onto_its_slot() {
        let slot = Rect {
            x: 10.0,
            y: 20.0,
            w: 80.0,
            h: 40.0,
        }; // 2× scale
        let a = art(0);
        let m = a.placement(&slot, false);
        assert_eq!(
            corners(&m, &a),
            vec![(10, 20), (10, 60), (90, 20), (90, 60)]
        );
        // Bottom-left of the art lands bottom-left of the slot.
        assert_eq!(m.apply(100.0, 200.0), (10.0, 20.0));
    }

    #[test]
    fn rotated_slots_and_rotated_pages_fill_the_slot_exactly() {
        let slot = Rect {
            x: 0.0,
            y: 0.0,
            w: 20.0,
            h: 40.0,
        };
        let a = art(0);
        let m = a.placement(&slot, true);
        assert_eq!(corners(&m, &a), vec![(0, 0), (0, 40), (20, 0), (20, 40)]);

        // A page shown rotated 90° is 20 × 40 as displayed.
        let r = art(90);
        assert_eq!(r.display_size_pt(), [20.0, 40.0]);
        let m = r.placement(&slot, false);
        assert_eq!(corners(&m, &r), vec![(0, 0), (0, 40), (20, 0), (20, 40)]);
        // /Rotate 90 is clockwise: the art's top-left corner ends top-right.
        let (x, y) = m.apply(100.0, 220.0);
        assert_eq!((x.round(), y.round()), (20.0, 40.0));

        for rotation in [180, 270] {
            let r = art(rotation);
            let size = r.display_size_pt();
            let slot = Rect {
                x: 5.0,
                y: 5.0,
                w: size[0],
                h: size[1],
            };
            let m = r.placement(&slot, false);
            let c = corners(&m, &r);
            assert_eq!(
                (c[0], c[3]),
                ((5, 5), (5 + size[0] as i32, 5 + size[1] as i32)),
                "{rotation}"
            );
        }
    }
}
