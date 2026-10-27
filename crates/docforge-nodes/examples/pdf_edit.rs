//! Lists the vector colours of a PDF and saves a copy cropped to its content
//! and recoloured with a PCB preset.
//!
//!     cargo run --release -p docforge-nodes --example pdf_edit -- in.pdf [out.pdf] [top|bottom|gray|none]

use docforge_core::EvalContext;
use docforge_nodes::{
    PdfEdit, PdfInput,
    pdf_edit::{Recolor, RecolorMode, palette},
};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let input = args
        .next()
        .expect("usage: pdf_edit <in.pdf> [out.pdf] [top|bottom|gray|none]");
    let output = args.next();
    let preset = args.next().map(|p| p.to_string_lossy().into_owned());

    let loaded = PdfInput::new(input)
        .load(&EvalContext::new())
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1)
        });
    println!("colours used (fills / strokes):");
    for c in palette(&loaded.bytes).iter().take(12) {
        println!(
            "  #{:02x}{:02x}{:02x}  {:>6} / {:<6}",
            c.rgb[0], c.rgb[1], c.rgb[2], c.fills, c.strokes
        );
    }
    let Some(output) = output else { return };
    let recolor = match preset.as_deref() {
        Some("top") | None => Recolor::pcb_top(),
        Some("bottom") => Recolor::pcb_bottom(),
        Some("gray") => Recolor {
            mode: RecolorMode::Grayscale,
            ..Recolor::default()
        },
        _ => Recolor::default(),
    };
    let edit = PdfEdit {
        recolor,
        ..PdfEdit::default()
    };
    let doc = edit.apply(&loaded).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    std::fs::write(&output, &doc.bytes).expect("write output");
    for line in &doc.summary {
        println!("{line}");
    }
    println!("saved {}", output.to_string_lossy());
}
