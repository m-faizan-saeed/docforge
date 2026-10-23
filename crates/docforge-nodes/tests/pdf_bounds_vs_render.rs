//! Checks PDF Input's vector bounding boxes against where ink actually lands
//! when the page is rendered (with `hayro`), on PDFs exported by real tools:
//!
//! * `badge.pdf` — Inkscape (from `badge.svg`): transformed group, thick
//!   strokes, curves, embedded-font text, a clip path, white background.
//! * `cards.pdf` — Cairo: rotated + scaled embedded photo, dashed circle,
//!   text, and a blank third page.
//! * `simple-smps.pdf` — a PCB layout printed through Foxit's PDF printer
//!   (2,300+ paths), the user's reference input for the grid tool.

use std::path::Path;

use docforge_core::{EvalContext, PdfDocument};
use docforge_nodes::PdfInput;
use hayro::{
    PixmapSettings, RenderCache, RenderSettings, hayro_interpret::InterpreterSettings,
    hayro_syntax::Pdf, vello_cpu::color::palette::css::WHITE,
};

/// Render scale: pixels per point.
const SCALE: f32 = 3.0;

fn load(name: &str) -> PdfDocument {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    PdfInput::new(path).load(&EvalContext::new()).unwrap()
}

/// Bounds of non-white pixels, in page points, or `None` for a blank page.
fn ink_box(doc: &PdfDocument, page: usize) -> Option<[f32; 4]> {
    let pdf = Pdf::new(doc.bytes.clone()).unwrap();
    let pixmap = hayro::render(
        &pdf.pages()[page],
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: SCALE,
            y_scale: SCALE,
            bg_color: WHITE,
        },
    );
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    let data = pixmap.data_as_u8_slice();
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let px = &data[(y * w + x) * 4..][..3];
            if px.iter().any(|&c| c < 248) {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    if x0 == usize::MAX {
        return None;
    }
    let crop = doc.page_info[page].crop_box;
    Some([
        crop[0] + x0 as f32 / SCALE,
        crop[3] - (y1 + 1) as f32 / SCALE,
        crop[0] + (x1 + 1) as f32 / SCALE,
        crop[3] - y0 as f32 / SCALE,
    ])
}

/// The detected box must contain all ink (to within a pixel) and exceed it
/// by no more than `slack` points on any side.
#[track_caller]
fn check(name: &str, page: usize, slack: [f32; 4]) {
    let doc = load(name);
    let detected = doc.page_info[page].content_box;
    let ink = ink_box(&doc, page);
    match (detected, ink) {
        (None, None) => {}
        (Some(d), Some(i)) => {
            let px = 1.0 / SCALE + 0.05;
            // Outward distance of each detected side beyond the ink.
            let over = [i[0] - d[0], i[1] - d[1], d[2] - i[2], d[3] - i[3]];
            for (side, (o, s)) in ["left", "bottom", "right", "top"]
                .iter()
                .zip(over.iter().zip(slack))
            {
                assert!(
                    *o >= -px,
                    "{name} p{}: ink sticks out {:.2} pt on the {side}\n detected {d:?}\n ink      {i:?}",
                    page + 1,
                    -o
                );
                assert!(
                    *o <= s,
                    "{name} p{}: {side} is {o:.2} pt looser than the ink (allowed {s})\n detected {d:?}\n ink      {i:?}",
                    page + 1
                );
            }
        }
        (d, i) => panic!("{name} p{}: detected {d:?} but ink {i:?}", page + 1),
    }
}

#[test]
fn inkscape_badge_matches_rendering() {
    // The bottom edge is set by text without descenders: the font's descent
    // makes the box up to ~7 pt roomier there, never tighter.
    check("badge.pdf", 0, [1.0, 8.0, 1.0, 1.0]);
}

#[test]
fn cairo_pages_match_rendering() {
    check("cards.pdf", 0, [1.0, 1.0, 1.0, 1.0]);
    check("cards.pdf", 1, [1.0, 1.0, 1.0, 1.0]);
    check("cards.pdf", 2, [0.0; 4]); // blank
}

#[test]
fn embedded_photo_is_listed() {
    let doc = load("cards.pdf");
    let images = &doc.page_info[0].images;
    assert_eq!(images.len(), 1);
    assert_eq!((images[0].width_px, images[0].height_px), (120, 90));
    assert_eq!(doc.page_info.len(), 3);
}

#[test]
fn pcb_layout_matches_rendering() {
    check("simple-smps.pdf", 0, [1.0, 1.0, 1.0, 1.0]);
}

/// After cropping, the page is the artwork: ink reaches every edge (no white
/// margin left) and nothing is cut off.
#[test]
fn cropped_output_is_exactly_the_artwork() {
    for name in ["simple-smps.pdf", "badge.pdf", "cards.pdf"] {
        let doc = load(name);
        let page = &doc.page_info[0];
        assert_eq!(
            Some(page.crop_box),
            page.content_box,
            "{name}: page cropped"
        );
        let ink = ink_box(&doc, 0).unwrap();
        let [x0, y0, x1, y1] = page.crop_box;
        // Text-bound edges keep the font's descent below the baseline.
        let bottom_slack = if name == "badge.pdf" { 8.0 } else { 1.0 };
        let gaps = [ink[0] - x0, ink[1] - y0, x1 - ink[2], y1 - ink[3]];
        for (side, (gap, slack)) in ["left", "bottom", "right", "top"]
            .iter()
            .zip(gaps.iter().zip([1.0, bottom_slack, 1.0, 1.0]))
        {
            assert!(
                *gap <= slack,
                "{name}: {gap:.2} pt of white margin on the {side}"
            );
        }
    }
    // The PCB art from the reference: 99.6 × 54.8 mm.
    let [w, h] = load("simple-smps.pdf").pages_mm[0];
    assert!(
        (w - 99.62).abs() < 0.05 && (h - 54.82).abs() < 0.05,
        "{w} × {h} mm"
    );
}

#[test]
fn cairo_photo_is_extracted_with_its_pixels_and_print_size() {
    let doc = load("cards.pdf");
    let entries = docforge_nodes::pdf_input::images::list(&doc);
    assert_eq!(entries.len(), 1);
    let key = entries[0].key.as_str();
    let img = docforge_nodes::pdf_input::images::extract(&doc, &[key], None)
        .remove(0)
        .unwrap();
    // The source surface: 120 × 90, orange→blue gradient with a white disc.
    assert_eq!(img.dimensions(), (120, 90));
    let centre = img.get_pixel(60, 45).unwrap();
    assert!(centre.iter().all(|&c| c > 245), "{centre:?}");
    let corner = img.get_pixel(1, 1).unwrap();
    assert!(
        corner[0] > 200 && corner[2] < 90,
        "orange corner: {corner:?}"
    );
    // Drawn at 2× (240 pt wide, rotated 10°): 120 px / 3.33 in = 36 dpi.
    assert!((img.dpi().unwrap() - 36.0).abs() < 0.5, "{:?}", img.dpi());
}
