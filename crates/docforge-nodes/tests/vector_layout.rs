//! End-to-end: the user's PCB layout (`simple-smps.pdf`, the reference grid
//! tool's input) through PDF Input → Print Layout with "fill sheet", checked
//! by rendering the result with `hayro`.

use std::path::Path;

use docforge_core::{EvalContext, Graph, PdfDocument};
use docforge_nodes::{
    PdfInput, PrintLayout, pdf_input,
    print::{self, CutMarks, PrintItem},
};
use hayro::{
    PixmapSettings, RenderCache, RenderSettings, hayro_interpret::InterpreterSettings,
    hayro_syntax::Pdf, vello_cpu::color::palette::css::WHITE,
};
use lopdf::{Document, content::Content};

const SCALE: f32 = 3.0; // pixels per point

fn layout_smps(layout: PrintLayout) -> PdfDocument {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/simple-smps.pdf");
    let mut g = Graph::new();
    let input = g.add(PdfInput::new(path));
    let print = g.add(layout);
    g.connect(input, pdf_input::OUTPUT, print, print::PDF_INPUT)
        .unwrap();
    let report = g.evaluate(&EvalContext::new());
    assert!(report.is_ok(), "{:?}", report.failed);
    g.output(print, print::OUTPUT)
        .unwrap()
        .as_document()
        .unwrap()
        .as_ref()
        .clone()
}

/// Where each `cm … /Fm Do` puts its form's BBox, in page points.
fn placements(doc: &PdfDocument) -> Vec<[f32; 4]> {
    let pdf = Document::load_mem(&doc.bytes).unwrap();
    let page = *pdf.get_pages().values().next().unwrap();
    let ops = Content::decode(&pdf.get_page_content(page))
        .unwrap()
        .operations;
    let (res, _) = pdf.get_page_resources(page).unwrap();
    let xobjects = res.unwrap().get(b"XObject").unwrap().as_dict().unwrap();
    ops.windows(2)
        .filter(|w| w[0].operator == "cm" && w[1].operator == "Do")
        .map(|w| {
            let m: Vec<f32> = w[0]
                .operands
                .iter()
                .map(|o| o.as_float().unwrap())
                .collect();
            let id = xobjects
                .get(w[1].operands[0].as_name().unwrap())
                .unwrap()
                .as_reference()
                .unwrap();
            let b: Vec<f32> = pdf
                .get_object(id)
                .unwrap()
                .as_stream()
                .unwrap()
                .dict
                .get(b"BBox")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o.as_float().unwrap())
                .collect();
            let pts = [(b[0], b[1]), (b[2], b[1]), (b[0], b[3]), (b[2], b[3])]
                .map(|(x, y)| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]));
            let xs = pts.map(|p| p.0);
            let ys = pts.map(|p| p.1);
            [
                xs.iter().copied().fold(f32::MAX, f32::min),
                ys.iter().copied().fold(f32::MAX, f32::min),
                xs.iter().copied().fold(f32::MIN, f32::max),
                ys.iter().copied().fold(f32::MIN, f32::max),
            ]
        })
        .collect()
}

/// Renders page 1; returns (width, height, ink?) per pixel.
fn render(doc: &PdfDocument) -> (usize, usize, Vec<bool>, f32) {
    let pdf = Pdf::new(doc.bytes.clone()).unwrap();
    let pixmap = hayro::render(
        &pdf.pages()[0],
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
    let ink = pixmap
        .data_as_u8_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[..3].iter().any(|&c| c < 248))
        .collect();
    let page_h_pt = h as f32 / SCALE;
    (w, h, ink, page_h_pt)
}

#[test]
fn smps_art_fills_an_a4_sheet_exactly_where_it_is_placed() {
    let mut layout = PrintLayout::default();
    layout.pdf_items = vec![PrintItem {
        fill: true,
        ..PrintItem::default()
    }];
    layout.cut_marks = CutMarks::None;
    let doc = layout_smps(layout);
    assert_eq!(doc.pages_mm.len(), 1, "{:?}", doc.summary);
    let rects = placements(&doc);
    // The reference tool fits 6 on A4; packing may squeeze in more.
    assert!(
        rects.len() >= 6,
        "{} copies: {:?}",
        rects.len(),
        doc.summary
    );
    for r in &rects {
        let (w, h) = ((r[2] - r[0]) * 25.4 / 72.0, (r[3] - r[1]) * 25.4 / 72.0);
        let (w, h) = (w.max(h), w.min(h));
        assert!(
            (w - 99.62).abs() < 0.05 && (h - 54.82).abs() < 0.05,
            "printed at {w} × {h} mm"
        );
    }

    let (w, h, ink, page_h) = render(&doc);
    let to_px = |r: &[f32; 4]| {
        // Page points (origin bottom-left) → pixel rows/cols.
        let x0 = (r[0] * SCALE).floor().max(0.0) as usize;
        let x1 = ((r[2] * SCALE).ceil() as usize).min(w);
        let y0 = (((page_h - r[3]) * SCALE).floor().max(0.0)) as usize;
        let y1 = ((((page_h - r[1]) * SCALE).ceil()) as usize).min(h);
        (x0, x1, y0, y1)
    };
    let mut inside = vec![false; w * h];
    for r in &rects {
        let (x0, x1, y0, y1) = to_px(r);
        let band = (1.5 * SCALE) as usize; // the green outline sits on the edge
        let any = |xa: usize, xb: usize, ya: usize, yb: usize| {
            (ya..yb).any(|y| (xa..xb).any(|x| ink[y * w + x]))
        };
        assert!(
            any(x0, x0 + band, y0, y1),
            "no art at the left edge of {r:?}"
        );
        assert!(
            any(x1 - band, x1, y0, y1),
            "no art at the right edge of {r:?}"
        );
        assert!(
            any(x0, x1, y0, y0 + band),
            "no art at the top edge of {r:?}"
        );
        assert!(
            any(x0, x1, y1 - band, y1),
            "no art at the bottom edge of {r:?}"
        );
        for y in y0.saturating_sub(1)..(y1 + 1).min(h) {
            for x in x0.saturating_sub(1)..(x1 + 1).min(w) {
                inside[y * w + x] = true;
            }
        }
    }
    let stray = (0..w * h).filter(|&i| ink[i] && !inside[i]).count();
    assert_eq!(stray, 0, "ink outside the placed pieces");
}

#[test]
fn smps_vector_art_is_stored_once() {
    let mut layout = PrintLayout::default();
    layout.pdf_items = vec![PrintItem {
        copies: 6,
        ..PrintItem::default()
    }];
    let doc = layout_smps(layout);
    let source =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/simple-smps.pdf"))
            .unwrap();
    // Six copies cost about as much as one: far less than 6× the source.
    assert!(
        doc.bytes.len() < source.len() * 2,
        "{} bytes for 6 copies of a {}-byte page",
        doc.bytes.len(),
        source.len()
    );
}
