//! PDF Overlay: layout maths, and the stacked result checked by rendering.

use docforge_core::{EvalContext, Graph, PdfDocument};
use lopdf::{Dictionary, Document, Object, Stream, dictionary};

use super::*;
use crate::{PdfEdit, PdfInput, PrintLayout, pdf::test_support::*, pdf_edit, pdf_input, print};

/// Loads `bytes` through PDF Input.
fn load(bytes: &[u8]) -> PdfDocument {
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "in.pdf", bytes);
    PdfInput::new(&path).load(&EvalContext::new()).unwrap()
}

/// Loads `bytes` and crops it to its content with PDF Edit.
fn cropped(bytes: &[u8]) -> PdfDocument {
    PdfEdit::default().apply(&load(bytes)).unwrap()
}

#[track_caller]
fn assert_rgb(actual: [u8; 3], expected: [u8; 3]) {
    assert!(
        actual
            .iter()
            .zip(expected)
            .all(|(a, e)| (*a as i32 - e as i32).abs() <= 3),
        "rendered {actual:?}, expected {expected:?}"
    );
}

fn page(crop: [f32; 4]) -> PageBox {
    PageBox { crop, rotation: 0 }
}

/// A PDF whose pages (200 × 200 pt) draw `contents`.
fn pages(contents: &[&str]) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let kids: Vec<Object> = contents
        .iter()
        .map(|c| {
            let content = doc.add_object(Stream::new(Dictionary::new(), c.as_bytes().to_vec()));
            doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
                "Contents" => content,
                "Resources" => Dictionary::new(),
            })
            .into()
        })
        .collect();
    doc.objects.insert(
        pages_id,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Count" => kids.len() as i64, "Kids" => kids },
        ),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

// ----- layout -----------------------------------------------------------------

#[test]
fn original_alignment_keeps_page_coordinates_on_a_union_page() {
    let overlay = PdfOverlay::default();
    let layout = overlay.layout(&[
        vec![page([20.0, 20.0, 80.0, 80.0])],
        vec![page([50.0, 50.0, 110.0, 110.0])],
    ]);
    assert_eq!(layout.len(), 1);
    assert_eq!(layout[0].size, [90.0, 90.0]);
    let rects: Vec<[f32; 4]> = layout[0].layers.iter().map(|l| l.rect).collect();
    assert_eq!(rects, [[0.0, 0.0, 60.0, 60.0], [30.0, 30.0, 90.0, 90.0]]);
    // The matrix moves page coordinates by the union's corner.
    assert_eq!(layout[0].layers[1].matrix.apply(50.0, 50.0), (30.0, 30.0));

    // The bottom layer's page instead: the top layer sticks out (clipped).
    let bottom = PdfOverlay {
        page_size: PageSize::Bottom,
        ..PdfOverlay::default()
    }
    .layout(&[
        vec![page([20.0, 20.0, 80.0, 80.0])],
        vec![page([50.0, 50.0, 110.0, 110.0])],
    ]);
    assert_eq!(bottom[0].size, [60.0, 60.0]);
    assert_eq!(bottom[0].layers[1].rect, [30.0, 30.0, 90.0, 90.0]);
}

#[test]
fn centre_top_left_and_offsets() {
    let boxes = [
        vec![page([0.0, 0.0, 200.0, 200.0])],
        vec![page([0.0, 0.0, 100.0, 100.0])],
    ];
    let rect = |overlay: PdfOverlay| overlay.layout(&boxes)[0].layers[1].rect;
    let center = PdfOverlay {
        align: Align::Center,
        ..PdfOverlay::default()
    };
    assert_eq!(rect(center.clone()), [50.0, 50.0, 150.0, 150.0]);
    let top_left = PdfOverlay {
        align: Align::TopLeft,
        ..PdfOverlay::default()
    };
    assert_eq!(rect(top_left), [0.0, 100.0, 100.0, 200.0]);

    // 10 mm right and 5 mm down.
    let mut nudged = center;
    nudged.layer_mut(1).offset_mm = [10.0, 5.0];
    let [x0, y0, ..] = rect(nudged);
    assert!((x0 - (50.0 + 10.0 * PT_PER_MM)).abs() < 0.01, "{x0}");
    assert!((y0 - (50.0 - 5.0 * PT_PER_MM)).abs() < 0.01, "{y0}");
}

#[test]
fn rotated_pages_are_placed_as_displayed() {
    // 200 × 100 landscape page shown turned clockwise: 100 × 200 portrait.
    let overlay = PdfOverlay::default();
    let layout = overlay.layout(&[vec![PageBox {
        crop: [0.0, 0.0, 200.0, 100.0],
        rotation: 90,
    }]]);
    assert_eq!(layout[0].size, [100.0, 200.0]);
    // The page's left edge is at the top when displayed.
    let (x, y) = layout[0].layers[0].matrix.apply(0.0, 50.0);
    assert!(
        (x - 50.0).abs() < 1e-6 && (y - 200.0).abs() < 1e-6,
        "{x} {y}"
    );
}

#[test]
fn hidden_layers_and_page_counts() {
    let mut overlay = PdfOverlay::default();
    overlay.layer_mut(0).visible = false;
    let layout = overlay.layout(&[
        vec![page([0.0, 0.0, 300.0, 300.0])],
        vec![page([0.0, 0.0, 100.0, 100.0]), page([0.0, 0.0, 50.0, 50.0])],
    ]);
    assert_eq!(layout.len(), 2);
    // A hidden bottom layer does not size the page.
    assert_eq!(layout[0].size, [100.0, 100.0]);
    assert_eq!(layout[0].layers.len(), 1);
    assert_eq!(layout[0].layers[0].layer, 1);
    assert_eq!(layout[1].size, [50.0, 50.0]);
}

// ----- the stacked PDF ------------------------------------------------------------

/// Gray square on the bottom, black square on a white background on top.
fn pcb_layers() -> (Vec<u8>, Vec<u8>) {
    (
        pdf("0.5 g 20 20 60 60 re f"),
        pdf("1 g 0 0 200 200 re f 0 g 50 50 60 60 re f"),
    )
}

#[test]
fn cropped_layers_stay_in_register_and_vector() {
    let (bottom, top) = pcb_layers();
    // Each layer is cropped to its own content (white ignored).
    let (bottom, top) = (cropped(&bottom), cropped(&top));
    assert_box(bottom.page_info[0].content_box, [20.0, 20.0, 80.0, 80.0]);
    assert_box(top.page_info[0].content_box, [50.0, 50.0, 110.0, 110.0]);

    let out = PdfOverlay::default().apply(&[&bottom, &top]).unwrap();
    assert_eq!(out.page_info.len(), 1);
    assert_box(out.page_info[0].content_box, [0.0, 0.0, 90.0, 90.0]);
    let mm = 90.0 / PT_PER_MM;
    assert!((out.pages_mm[0][0] - mm).abs() < 0.01);
    assert!(
        out.summary[0].starts_with("2 layers on 1 page"),
        "{:?}",
        out.summary
    );

    // Both layers are forms on the page: still vector.
    let (doc, id) = page_of(&out.bytes);
    let (res, _) = doc.get_page_resources(id).unwrap();
    let xobjects = res.unwrap().get(b"XObject").unwrap().as_dict().unwrap();
    assert_eq!(xobjects.len(), 2);

    let px = render(&out.bytes, 2.0);
    assert_rgb(px(5.0, 5.0), [128, 128, 128]); // bottom only
    assert_rgb(px(85.0, 85.0), [0, 0, 0]); // top only
    assert_rgb(px(40.0, 40.0), [0, 0, 0]); // top over bottom
}

#[test]
fn multiply_makes_white_see_through_and_opacity_fades() {
    let (bottom, top) = pcb_layers();
    let (bottom, top) = (load(&bottom), load(&top));

    // Normal: the top layer's white background hides the gray square.
    let normal = PdfOverlay::default().apply(&[&bottom, &top]).unwrap();
    assert_rgb(render(&normal.bytes, 1.0)(30.0, 30.0), [255, 255, 255]);

    let mut overlay = PdfOverlay::default();
    overlay.layer_mut(1).blend = Blend::Multiply;
    let multiplied = overlay.apply(&[&bottom, &top]).unwrap();
    let px = render(&multiplied.bytes, 1.0);
    assert_rgb(px(30.0, 30.0), [128, 128, 128]);
    assert_rgb(px(60.0, 60.0), [0, 0, 0]);
    assert_rgb(px(150.0, 150.0), [255, 255, 255]);

    let mut overlay = PdfOverlay::default();
    overlay.layer_mut(1).opacity = 0.5;
    let faded = overlay.apply(&[&bottom, &top]).unwrap();
    let px = render(&faded.bytes, 1.0);
    // Half white over gray, half black over white.
    assert_rgb(px(30.0, 30.0), [191, 191, 191]);
    assert_rgb(px(100.0, 100.0), [128, 128, 128]);
}

#[test]
fn page_by_page_and_errors() {
    let two = load(&pages(&["0 g 0 0 10 10 re f", "0 g 0 0 10 10 re f"]));
    let one = load(&pdf("0 g 100 100 10 10 re f"));
    let out = PdfOverlay::default().apply(&[&one, &two]).unwrap();
    assert_eq!(out.page_info.len(), 2);
    assert!(out.summary[0].contains("2 pages"), "{:?}", out.summary);
    assert_eq!(Document::load_mem(&out.bytes).unwrap().get_pages().len(), 2);

    let mut hidden = PdfOverlay::default();
    hidden.layer_mut(0).visible = false;
    assert!(hidden.apply(&[&one]).is_err());
    assert!(PdfOverlay::default().apply(&[]).is_err());

    let mut bad = PdfOverlay::default();
    bad.layer_mut(0).opacity = 2.0;
    assert!(bad.apply(&[&one]).is_err());
}

// ----- graph ----------------------------------------------------------------------

#[test]
fn pcb_assembly_guide_graph() {
    let dir = tempfile::tempdir().unwrap();
    let design = write_pdf(
        dir.path(),
        "pcb.pdf",
        &pdf("1 0 0 rg 20 20 60 60 re f 0 0 1 rg 50 50 60 60 re f"),
    );
    let mut g = Graph::new();
    let input = g.add(PdfInput::new(&design));
    let bottom = g.add(PdfEdit {
        recolor: pdf_edit::Recolor::pcb_bottom(),
        ..PdfEdit::default()
    });
    let top = g.add(PdfEdit {
        recolor: pdf_edit::Recolor::pcb_top(),
        ..PdfEdit::default()
    });
    let stack = g.add(PdfOverlay::default());
    let sheet = g.add(PrintLayout::default());
    for edit in [bottom, top] {
        g.connect(input, pdf_input::OUTPUT, edit, pdf_edit::INPUT)
            .unwrap();
        g.connect(edit, pdf_edit::OUTPUT, stack, INPUT).unwrap();
    }
    g.connect(stack, OUTPUT, sheet, print::PDF_INPUT).unwrap();
    let report = g.evaluate(&EvalContext::new());
    assert!(report.is_ok(), "{:?}", report.failed);

    let out = g.output(stack, OUTPUT).unwrap().as_document().unwrap();
    // Black on top of gray: everything painted is black.
    let px = render(&out.bytes, 1.0);
    assert_rgb(px(10.0, 10.0), [0, 0, 0]);
    assert!(
        g.output(sheet, print::OUTPUT)
            .unwrap()
            .as_document()
            .unwrap()
            .summary[0]
            .starts_with("1 piece"),
    );

    // Swapping the order puts the gray layer on top.
    assert!(g.swap_inputs(stack, INPUT, 0, 1));
    assert!(g.evaluate(&EvalContext::new()).is_ok());
    let out = g.output(stack, OUTPUT).unwrap().as_document().unwrap();
    assert_rgb(render(&out.bytes, 1.0)(10.0, 10.0), [160, 160, 160]);

    // Saved and loaded with its settings.
    let again = Graph::from_json(&g.to_json().unwrap(), &crate::registry()).unwrap();
    assert_eq!(
        again.node_as::<PdfOverlay>(stack),
        g.node_as::<PdfOverlay>(stack)
    );
    assert_eq!(again.edges(), g.edges());
}
