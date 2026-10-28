//! docforge app: a desktop program, or a web page when built for wasm32.
//!
//!     cargo run --release -p docforge-app [project.json | image]
//!     trunk serve --release crates/docforge-app/index.html   # in a browser

mod app;
mod background;
mod bounds;
mod canvas;
mod dialogs;
mod handles;
mod inspector;
mod pdf_view;
mod thumbs;
mod viewport;
#[cfg(target_arch = "wasm32")]
mod web;
mod worker;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    use std::path::PathBuf;

    use eframe::egui;

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

/// Web entry point: runs the app in the page's `docforge_canvas` canvas.
#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast;

    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .expect("running in a web page");
        let canvas = document
            .get_element_by_id("docforge_canvas")
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("the page has a <canvas id=\"docforge_canvas\">");
        let started = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(app::DocforgeApp::new(cc, None)))),
            )
            .await;
        // Replace the "Loading…" text with the app, or with what went wrong.
        if let Some(loading) = document.get_element_by_id("loading") {
            match started {
                Ok(()) => loading.remove(),
                Err(err) => loading.set_inner_html(&format!(
                    "<p>docforge could not start:</p><pre>{err:?}</pre>"
                )),
            }
        }
    });
}
