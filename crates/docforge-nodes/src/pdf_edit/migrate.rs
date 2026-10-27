//! Keeps projects saved before the PDF Input / PDF Edit split working.

use docforge_core::{Edge, GraphDoc, NodeDoc, NodeId, NodeMeta, PortRef};

/// PDF Input used to crop its pages to the detected content (on by default).
/// That moved to PDF Edit: for every old PDF Input that cropped, insert a PDF
/// Edit with the same settings after it, so the project produces the same
/// result. Also removes the old settings, which makes it idempotent.
pub fn migrate_pdf_input_crop(doc: &mut GraphDoc) {
    const OLD_KEYS: [&str; 4] = [
        "crop_to_content",
        "bounds",
        "padding_mm",
        "ignore_white_fills",
    ];
    let mut next_id = doc.nodes.iter().map(|n| n.id.0 + 1).max().unwrap_or(0);
    let mut inserted = Vec::new();
    for node in &mut doc.nodes {
        if node.kind != "pdf_input" {
            continue;
        }
        let Some(params) = node.params.as_object_mut() else {
            continue;
        };
        if !OLD_KEYS.iter().any(|k| params.contains_key(*k)) {
            continue;
        }
        // Defaults of the old version: crop on, to the detected content.
        let crop = params
            .get("crop_to_content")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let bounds = params
            .get("bounds")
            .and_then(|v| v.as_str())
            .unwrap_or("content")
            .to_owned();
        let padding = params
            .get("padding_mm")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let ignore_white = params
            .get("ignore_white_fills")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        for key in OLD_KEYS {
            params.remove(key);
        }
        // Cropping to the CropBox/MediaBox changed nothing visible.
        if crop && bounds == "content" {
            let edit = NodeId(next_id);
            next_id += 1;
            inserted.push((node.id, edit, node.meta.position, padding, ignore_white));
        }
    }
    for (input, edit, position, padding, ignore_white) in inserted {
        doc.nodes.push(NodeDoc {
            id: edit,
            kind: "pdf_edit".into(),
            params: serde_json::json!({
                "crop": "content",
                "padding_mm": padding,
                "ignore_white_fills": ignore_white,
            }),
            meta: NodeMeta {
                label: Some("Crop (was in PDF Input)".into()),
                position: [position[0] + 30.0, position[1] + 110.0],
            },
        });
        // Everything that read the PDF Input's document now reads the crop.
        for e in &mut doc.edges {
            if e.from.node == input && e.from.port == "pdf" {
                e.from.node = edit;
            }
        }
        doc.edges.push(Edge {
            from: PortRef::new(input, "pdf"),
            to: PortRef::new(edit, "pdf"),
        });
    }
}
