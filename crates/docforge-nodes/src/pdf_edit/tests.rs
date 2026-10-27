//! PDF Edit: cropping and vector recolouring. Colours are checked by
//! rendering the result, so the rewritten drawing must really look right.

use docforge_core::{EvalContext, Graph, PdfDocument};
use lopdf::{Dictionary, Document, Object, Stream, dictionary};

use super::*;
use crate::{PdfInput, PrintLayout, pdf::test_support::*, print};

/// Loads `bytes` through PDF Input (unchanged) and applies `edit`.
fn edit(bytes: &[u8], edit: &PdfEdit) -> PdfDocument {
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "in.pdf", bytes);
    let src = PdfInput::new(&path).load(&EvalContext::new()).unwrap();
    edit.apply(&src).unwrap()
}

fn crop_only(crop: CropMode) -> PdfEdit {
    PdfEdit {
        crop,
        ..PdfEdit::default()
    }
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

// ----- cropping ----------------------------------------------------------------

#[test]
fn crops_to_detected_content_without_touching_the_drawing() {
    let original = pdf_with(b"0 g 40 50 30 20 re f", |_, _, page, _| {
        page.set("TrimBox", vec![0.into(), 0.into(), 200.into(), 200.into()]);
        page.set("Rotate", 90);
    });
    let doc = edit(&original, &PdfEdit::default());
    let (out, page) = page_of(&doc.bytes);
    assert_eq!(
        rect_of(&out, page, b"MediaBox"),
        Some([40.0, 50.0, 70.0, 70.0])
    );
    assert_eq!(
        rect_of(&out, page, b"CropBox"),
        Some([40.0, 50.0, 70.0, 70.0])
    );
    assert_eq!(
        rect_of(&out, page, b"TrimBox"),
        None,
        "boxes outside the new page are dropped"
    );
    assert_eq!(
        out.get_dictionary(page)
            .unwrap()
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap(),
        90
    );
    let (orig, orig_page) = page_of(&original);
    assert_eq!(
        out.get_page_content(page),
        orig.get_page_content(orig_page),
        "drawing untouched"
    );
    // Rotation swaps the displayed size: 30 × 20 pt shown as 20 × 30.
    let [w, h] = doc.pages_mm[0];
    assert!((w - 20.0 / PT_PER_MM).abs() < 0.01 && (h - 30.0 / PT_PER_MM).abs() < 0.01);
    assert_eq!(doc.page_info[0].content_box, Some([40.0, 50.0, 70.0, 70.0]));
    assert!(
        doc.summary[0].contains("cropped to content"),
        "{:?}",
        doc.summary
    );

    // Cropping the cropped output again changes nothing: the box is stable.
    let again = edit(&doc.bytes, &PdfEdit::default());
    assert_box(again.page_info[0].content_box, [40.0, 50.0, 70.0, 70.0]);
}

#[test]
fn padding_white_fills_and_blank_pages() {
    let art = pdf("1 g 0 0 200 200 re f 0 g 72 72 72 36 re f");
    // 72 pt of padding, but never beyond the 200 pt page.
    let padded = PdfEdit {
        padding_mm: 25.4,
        ..PdfEdit::default()
    };
    assert_box(
        edit(&art, &padded).page_info[0].content_box,
        [0.0, 0.0, 200.0, 180.0],
    );
    // Counting the white background makes the whole page content.
    let with_white = PdfEdit {
        ignore_white_fills: false,
        ..PdfEdit::default()
    };
    assert_box(
        edit(&art, &with_white).page_info[0].content_box,
        [0.0, 0.0, 200.0, 200.0],
    );

    let blank = pdf("1 g 0 0 200 200 re f");
    let doc = edit(&blank, &PdfEdit::default());
    assert_eq!(doc.page_info[0].content_box, None, "nothing to print");
    assert_eq!(doc.bytes, blank, "left as it is");
    assert!(
        doc.summary
            .iter()
            .any(|s| s.contains("No visible content on page 1"))
    );
}

#[test]
fn manual_box_is_relative_to_the_page_as_displayed() {
    let art = pdf("0 g 0 0 200 200 re f");
    let manual = |rotate: Option<i64>| {
        let bytes = pdf_with(b"0 g 0 0 200 200 re f", |_, _, page, _| {
            page.set("MediaBox", vec![0.into(), 0.into(), 200.into(), 100.into()]);
            if let Some(r) = rotate {
                page.set("Rotate", r);
            }
        });
        let e = PdfEdit {
            crop: CropMode::Manual,
            manual_box: [0.0, 0.0, 0.5, 0.5], // top-left quarter as shown
            ..PdfEdit::default()
        };
        edit(&bytes, &e).page_info[0].content_box
    };
    assert_box(manual(None), [0.0, 50.0, 100.0, 100.0]);
    assert_box(manual(Some(90)), [0.0, 0.0, 100.0, 50.0]);

    let bad = PdfEdit {
        crop: CropMode::Manual,
        manual_box: [0.5, 0.5, 0.2, 0.9],
        ..PdfEdit::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let src = PdfInput::new(write_pdf(dir.path(), "a.pdf", &art))
        .load(&EvalContext::new())
        .unwrap();
    assert!(matches!(
        bad.apply(&src),
        Err(NodeError::InvalidParam {
            name: "manual_box",
            ..
        })
    ));
}

#[test]
fn no_crop_and_no_recolour_passes_the_pdf_through() {
    let art = pdf("0 g 10 10 20 20 re f");
    let doc = edit(&art, &crop_only(CropMode::None));
    assert_eq!(doc.bytes, art);
    assert_eq!(doc.page_info[0].content_box, Some([0.0, 0.0, 200.0, 200.0]));
}

#[test]
fn encrypted_pdfs_with_an_empty_password_are_cropped_into_a_readable_file() {
    use lopdf::{EncryptionState, EncryptionVersion, Permissions};
    let mut doc = Document::load_mem(&pdf("0 g 10 20 30 40 re f")).unwrap();
    doc.trailer.set(
        "ID",
        vec![
            Object::string_literal("0123456789abcdef"),
            Object::string_literal("0123456789abcdef"),
        ],
    );
    let state = EncryptionState::try_from(EncryptionVersion::V2 {
        document: &doc,
        owner_password: "owner",
        user_password: "",
        key_length: 128,
        permissions: Permissions::PRINTABLE,
    })
    .unwrap();
    doc.encrypt(&state).unwrap();
    let mut encrypted = Vec::new();
    doc.save_to(&mut encrypted).unwrap();
    let has_encrypt = |bytes: &[u8]| bytes.windows(8).any(|w| w == b"/Encrypt");
    assert!(has_encrypt(&encrypted), "fixture really is encrypted");

    let out = edit(&encrypted, &PdfEdit::default());
    assert_box(out.page_info[0].content_box, [10.0, 20.0, 40.0, 60.0]);
    assert!(!has_encrypt(&out.bytes), "written decrypted");
    let (reloaded, page) = page_of(&out.bytes);
    assert_eq!(
        rect_of(&reloaded, page, b"MediaBox"),
        Some([10.0, 20.0, 40.0, 60.0])
    );
}

// ----- recolouring -----------------------------------------------------------

/// Red filled square (20..60), blue stroked line along y = 150, a white
/// square (120..160, 20..60) and black text-like fill (120..160, 120..160).
const SCENE: &str = "1 0 0 rg 20 20 40 40 re f \
                     0 0 1 RG 6 w 20 150 m 100 150 l S \
                     1 g 120 20 40 40 re f \
                     0 g 120 120 40 40 re f";

fn recolor_only(recolor: Recolor) -> PdfEdit {
    PdfEdit {
        crop: CropMode::None,
        recolor,
        ..PdfEdit::default()
    }
}

#[test]
fn pcb_presets_turn_everything_black_or_gray_but_keep_white() {
    let top = edit(&pdf(SCENE), &recolor_only(Recolor::pcb_top()));
    let px = render(&top.bytes, 1.0);
    assert_rgb(px(40.0, 40.0), [0, 0, 0]); // was red
    assert_rgb(px(60.0, 150.0), [0, 0, 0]); // was blue
    assert_rgb(px(140.0, 40.0), [255, 255, 255]); // white kept
    assert!(
        top.summary[0].contains("colours changed"),
        "{:?}",
        top.summary
    );

    let bottom = edit(&pdf(SCENE), &recolor_only(Recolor::pcb_bottom()));
    let px = render(&bottom.bytes, 1.0);
    assert_rgb(px(40.0, 40.0), [160, 160, 160]);
    assert_rgb(px(140.0, 140.0), [160, 160, 160]); // was black
    assert_rgb(px(140.0, 40.0), [255, 255, 255]);

    // Without keep_white, white turns gray too.
    let all = Recolor {
        keep_white: false,
        ..Recolor::pcb_bottom()
    };
    let px = render(&edit(&pdf(SCENE), &recolor_only(all)).bytes, 1.0);
    assert_rgb(px(140.0, 40.0), [160, 160, 160]);
}

#[test]
fn replace_rules_strokes_only_and_grayscale() {
    let rules = Recolor {
        mode: RecolorMode::Replace,
        rules: vec![ColorRule {
            from: [250, 5, 5],
            to: [0, 128, 0],
        }],
        tolerance: 10,
        ..Recolor::default()
    };
    let px = render(&edit(&pdf(SCENE), &recolor_only(rules)).bytes, 1.0);
    assert_rgb(px(40.0, 40.0), [0, 128, 0]); // red within tolerance → green
    assert_rgb(px(60.0, 150.0), [0, 0, 255]); // blue: no rule
    assert_rgb(px(140.0, 140.0), [0, 0, 0]);

    let strokes = Recolor {
        fills: false,
        ..Recolor::pcb_bottom()
    };
    let px = render(&edit(&pdf(SCENE), &recolor_only(strokes)).bytes, 1.0);
    assert_rgb(px(40.0, 40.0), [255, 0, 0]); // fill untouched
    assert_rgb(px(60.0, 150.0), [160, 160, 160]); // stroke changed

    let gray = Recolor {
        mode: RecolorMode::Grayscale,
        ..Recolor::default()
    };
    let px = render(&edit(&pdf(SCENE), &recolor_only(gray)).bytes, 1.0);
    assert_rgb(px(40.0, 40.0), [76, 76, 76]); // luma of red
}

#[test]
fn colour_spaces_are_restored_for_colours_left_alone() {
    // Indexed palette: 0 = red, 1 = blue. Index 0 is replaced, index 1 is
    // not — after the replacement switched to RGB, the second `scn` must be
    // preceded by the original colour space again or it would render wrong.
    let content = b"/CS0 cs 0 scn 20 20 40 40 re f 1 scn 120 20 40 40 re f";
    let bytes = pdf_with(content, |_, res, _, _| {
        let palette = Object::Array(vec![
            "Indexed".into(),
            "DeviceRGB".into(),
            1.into(),
            Object::String(vec![255, 0, 0, 0, 0, 255], lopdf::StringFormat::Hexadecimal),
        ]);
        res.set("ColorSpace", dictionary! { "CS0" => palette });
    });
    let rules = Recolor {
        mode: RecolorMode::Replace,
        rules: vec![ColorRule {
            from: [255, 0, 0],
            to: [0, 0, 0],
        }],
        ..Recolor::default()
    };
    let px = render(&edit(&bytes, &recolor_only(rules)).bytes, 1.0);
    assert_rgb(px(40.0, 40.0), [0, 0, 0]);
    assert_rgb(px(140.0, 40.0), [0, 0, 255]);
}

#[test]
fn colours_inside_forms_and_cmyk_are_changed_once() {
    let bytes = pdf_with(
        b"0 1 1 0 k 20 20 40 40 re f /Fm0 Do q 1 0 0 1 100 0 cm /Fm0 Do Q",
        |doc, res, _, _| {
            let form = Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Form",
                    "BBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
                },
                b"0 0 1 rg 20 120 40 40 re f".to_vec(),
            );
            let id = doc.add_object(form);
            res.set("XObject", dictionary! { "Fm0" => id });
        },
    );
    let doc = edit(&bytes, &recolor_only(Recolor::pcb_top()));
    let px = render(&doc.bytes, 1.0);
    assert_rgb(px(40.0, 40.0), [0, 0, 0]); // CMYK red
    assert_rgb(px(40.0, 140.0), [0, 0, 0]); // form, first use
    assert_rgb(px(140.0, 140.0), [0, 0, 0]); // form, second use
    // The form is shared, rewritten once: 1 page colour + 1 form colour.
    assert!(
        doc.summary[0].contains("2 colours changed"),
        "{:?}",
        doc.summary
    );
}

#[test]
fn palette_lists_the_colours_used() {
    let colors = palette(&pdf(SCENE));
    let rgbs: Vec<[u8; 3]> = colors.iter().map(|c| c.rgb).collect();
    for expected in [[255, 0, 0], [0, 0, 255], [255, 255, 255], [0, 0, 0]] {
        assert!(rgbs.contains(&expected), "{rgbs:?}");
    }
    let blue = colors.iter().find(|c| c.rgb == [0, 0, 255]).unwrap();
    assert_eq!((blue.fills, blue.strokes), (0, 1));
}

// ----- graph & old projects -----------------------------------------------------

#[test]
fn pdf_input_edit_layout_chain_and_old_projects() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "art.pdf", &pdf("0 g 30 40 72 36 re f"));

    // An old project: PDF Input cropping (its default) wired to Print Layout.
    let old = serde_json::json!({
        "version": 1,
        "nodes": [
            {"id": 0, "kind": "pdf_input", "params": {
                "path": path, "crop_to_content": true, "bounds": "content",
                "padding_mm": 0.0, "ignore_white_fills": true, "selected_images": []}},
            {"id": 1, "kind": "print_layout", "params": {"pdf_items": [{"copies": 2}]}}
        ],
        "edges": [{"from": {"node": 0, "port": "pdf"}, "to": {"node": 1, "port": "pdfs"}}]
    });
    let mut g = Graph::from_json(&old.to_string(), &crate::registry()).unwrap();
    let edit_id = g
        .nodes()
        .find(|(_, n)| n.kind() == PdfEdit::KIND)
        .map(|(id, _)| id)
        .expect("PDF Edit inserted");
    assert!(
        g.edges()
            .iter()
            .any(|e| e.from.node.0 == 0 && e.to.node == edit_id)
    );
    assert!(
        g.edges()
            .iter()
            .any(|e| e.from.node == edit_id && e.to.port == print::PDF_INPUT)
    );

    let report = g.evaluate(&EvalContext::new());
    assert!(report.is_ok(), "{:?}", report.failed);
    let cropped = g.output(edit_id, OUTPUT).unwrap().as_document().unwrap();
    assert_box(cropped.page_info[0].content_box, [30.0, 40.0, 102.0, 76.0]);
    let sheet = g
        .output(docforge_core::NodeId(1), print::OUTPUT)
        .unwrap()
        .as_document()
        .unwrap();
    assert!(
        sheet.summary[0].starts_with("2 pieces"),
        "{:?}",
        sheet.summary
    );

    // Saving and loading again does not insert another node.
    let again = Graph::from_json(&g.to_json().unwrap(), &crate::registry()).unwrap();
    assert_eq!(again.len(), 3);
    assert_eq!(
        again.node_as::<PdfEdit>(edit_id),
        g.node_as::<PdfEdit>(edit_id)
    );

    // An old PDF Input that did not crop just loses the settings.
    let mut doc: docforge_core::GraphDoc = serde_json::from_value(serde_json::json!({
        "version": 1,
        "nodes": [{"id": 0, "kind": "pdf_input", "params": {"path": "x.pdf", "crop_to_content": false}}]
    }))
    .unwrap();
    migrate_pdf_input_crop(&mut doc);
    assert_eq!(doc.nodes.len(), 1);
    assert!(doc.nodes[0].params.get("crop_to_content").is_none());
    let _ = (PrintLayout::default(), Dictionary::new());
}
