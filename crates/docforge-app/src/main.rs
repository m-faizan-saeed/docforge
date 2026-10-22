//! docforge desktop app.
//!
//!     cargo run --release -p docforge-app [project.json | image]

mod app;
mod canvas;
mod inspector;
mod pdf_view;
mod viewport;
mod worker;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    let open = std::env::args_os().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("docforge")
            .with_inner_size([1440.0, 920.0])
            .with_min_inner_size([960.0, 600.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "docforge",
        options,
        Box::new(move |cc| Ok(Box::new(app::DocforgeApp::new(cc, open)))),
    )
}
