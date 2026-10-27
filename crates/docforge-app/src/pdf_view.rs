//! Zoomable PDF preview rendered with `hayro` (pure Rust, CPU).
//!
//! Pages are rendered on a background thread. The resolution follows the
//! zoom level, so zooming in re-renders sharper pages while the page keeps
//! its on-screen size.

use std::sync::{Arc, mpsc};

use docforge_core::{PdfDocument, RasterImage};
use eframe::egui::{self, RichText};
use hayro::{
    PixmapSettings, RenderCache, RenderSettings, hayro_interpret::InterpreterSettings,
    hayro_syntax::Pdf, vello_cpu::color::palette::css::WHITE,
};

use crate::viewport::{Overlay, Viewport};

/// Logical units per inch: at 100% zoom a page appears roughly life-size.
const LOGICAL_DPI: f32 = 96.0;
/// Render resolutions to choose from (fewer distinct values, fewer re-renders).
const DPI_STEPS: [f32; 7] = [72.0, 110.0, 150.0, 220.0, 300.0, 450.0, 600.0];

struct Request {
    doc: Arc<PdfDocument>,
    page: usize,
    dpi: f32,
}

struct Rendered {
    doc: Arc<PdfDocument>,
    page: usize,
    dpi: f32,
    image: Result<Arc<RasterImage>, String>,
}

pub struct PdfView {
    page: usize,
    viewport: Viewport,
    requests: mpsc::Sender<Request>,
    results: mpsc::Receiver<Rendered>,
    shown: Option<Rendered>,
    /// (document, page, dpi) of the render in flight.
    requested: Option<(usize, usize, u32)>,
}

impl PdfView {
    pub fn new(ctx: egui::Context) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<Request>();
        let (res_tx, res_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("docforge-pdf-render".into())
            .spawn(move || {
                while let Ok(mut req) = req_rx.recv() {
                    while let Ok(newer) = req_rx.try_recv() {
                        req = newer;
                    }
                    let image = render_page(&req.doc, req.page, req.dpi).map(Arc::new);
                    let done = Rendered {
                        doc: req.doc,
                        page: req.page,
                        dpi: req.dpi,
                        image,
                    };
                    if res_tx.send(done).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("failed to start the PDF render thread");
        Self {
            page: 0,
            viewport: Viewport::default(),
            requests: req_tx,
            results: res_rx,
            shown: None,
            requested: None,
        }
    }

    /// A render is in flight.
    pub fn is_rendering(&self) -> bool {
        self.requested.is_some()
    }

    /// Fit the page to the view again on the next frame.
    pub fn request_fit(&mut self) {
        self.viewport.request_fit();
    }

    /// The page being shown (0-based).
    pub fn page(&self) -> usize {
        self.page
    }

    pub fn show(&mut self, ui: &mut egui::Ui, doc: &Arc<PdfDocument>) {
        self.show_inner(ui, doc, None);
    }

    /// Like [`PdfView::show`], with interactive handles drawn over the page
    /// (the frame covers the page as displayed, rotation applied).
    pub fn show_with_overlay(
        &mut self,
        ui: &mut egui::Ui,
        doc: &Arc<PdfDocument>,
        overlay: Overlay<'_>,
    ) {
        self.show_inner(ui, doc, Some(overlay));
    }

    fn show_inner(
        &mut self,
        ui: &mut egui::Ui,
        doc: &Arc<PdfDocument>,
        overlay: Option<Overlay<'_>>,
    ) {
        if let Some(latest) = self.results.try_iter().last() {
            self.shown = Some(latest);
            self.requested = None;
        }
        let pages = doc.pages_mm.len().max(1);
        self.page = self.page.min(pages - 1);

        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.page > 0, egui::Button::new("◀"))
                .clicked()
            {
                self.page -= 1;
            }
            ui.label(format!("Page {} of {pages}", self.page + 1));
            if ui
                .add_enabled(self.page + 1 < pages, egui::Button::new("▶"))
                .clicked()
            {
                self.page += 1;
            }
            ui.separator();
            if let Some(first) = doc.summary.first() {
                ui.label(first);
            }
        });
        for note in doc.summary.iter().skip(1) {
            ui.label(RichText::new(note).color(egui::Color32::from_rgb(230, 170, 70)));
        }

        // Ask for a render matching the current zoom on this screen.
        let wanted_dpi = {
            let px_per_inch = self.viewport.zoom() * LOGICAL_DPI * ui.ctx().pixels_per_point();
            DPI_STEPS
                .into_iter()
                .find(|&d| d >= px_per_inch)
                .unwrap_or(DPI_STEPS[DPI_STEPS.len() - 1])
        };
        let key = (Arc::as_ptr(doc) as usize, self.page, wanted_dpi as u32);
        let shown_key = self
            .shown
            .as_ref()
            .map(|s| (Arc::as_ptr(&s.doc) as usize, s.page, s.dpi as u32));
        if shown_key != Some(key) && self.requested != Some(key) {
            let _ = self.requests.send(Request {
                doc: Arc::clone(doc),
                page: self.page,
                dpi: wanted_dpi,
            });
            self.requested = Some(key);
        }

        // Show the latest render (possibly of an older revision) until the
        // requested one arrives, so live edits never flash empty.
        match &self.shown {
            Some(Rendered {
                image: Ok(image),
                dpi,
                page,
                ..
            }) => {
                let [w, h] = doc.pages_mm.get(*page).copied().unwrap_or_default();
                let facts = format!("{w:.0} × {h:.0} mm · rendered at {dpi:.0} dpi");
                let image = Arc::clone(image);
                self.viewport.show_scaled_with_overlay(
                    ui,
                    Some(&image),
                    LOGICAL_DPI / dpi,
                    Some(facts),
                    "",
                    overlay,
                );
            }
            Some(Rendered {
                image: Err(err), ..
            }) => {
                self.viewport
                    .show(ui, None, &format!("Could not render the PDF:\n{err}"));
            }
            None => self.viewport.show(ui, None, "Rendering…"),
        }
    }
}

fn render_page(doc: &Arc<PdfDocument>, page: usize, dpi: f32) -> Result<RasterImage, String> {
    let pdf = Pdf::new(Arc::clone(doc)).map_err(|e| format!("{e:?}"))?;
    let pages = pdf.pages();
    let page = pages.get(page).ok_or("the page does not exist")?;
    let scale = dpi / 72.0;
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    // Rendered on opaque white, so premultiplied and straight alpha agree.
    RasterImage::from_rgba8(
        pixmap.width() as u32,
        pixmap.height() as u32,
        pixmap.data_as_u8_slice().to_vec(),
    )
    .map(|img| img.with_dpi(Some(dpi)))
    .map_err(|e| e.to_string())
}
