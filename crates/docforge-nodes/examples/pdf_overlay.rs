//! Makes a PCB assembly guide: the bottom PDF in gray under the top PDF in
//! black (multiplied, so white is see-through), each cropped to its content
//! and kept in register.
//!
//!     cargo run --release -p docforge-nodes --example pdf_overlay -- bottom.pdf top.pdf out.pdf
//!
//! Pass the same file twice to stack one design's two colourings.

use docforge_core::EvalContext;
use docforge_nodes::{PdfEdit, PdfInput, PdfOverlay, pdf_edit::Recolor, pdf_overlay::Blend};

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [bottom, top, output] = &args[..] else {
        eprintln!("usage: pdf_overlay <bottom.pdf> <top.pdf> <out.pdf>");
        std::process::exit(2)
    };
    let fail = |e: docforge_core::NodeError| -> ! {
        eprintln!("{e}");
        std::process::exit(1)
    };
    let layer = |path, recolor| {
        let loaded = PdfInput::new(path)
            .load(&EvalContext::new())
            .unwrap_or_else(|e| fail(e));
        PdfEdit {
            recolor,
            ..PdfEdit::default()
        }
        .apply(&loaded)
        .unwrap_or_else(|e| fail(e))
    };
    let bottom = layer(bottom, Recolor::pcb_bottom());
    let top = layer(top, Recolor::pcb_top());

    let mut overlay = PdfOverlay::default();
    overlay.layer_mut(1).blend = Blend::Multiply;
    let doc = overlay.apply(&[&bottom, &top]).unwrap_or_else(|e| fail(e));
    std::fs::write(output, &doc.bytes).expect("write output");
    for line in &doc.summary {
        println!("{line}");
    }
    println!("saved {}", output.to_string_lossy());
}
