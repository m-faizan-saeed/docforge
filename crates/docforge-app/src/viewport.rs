//! Zoomable, pannable image preview.

use std::sync::Arc;

use docforge_core::RasterImage;

use crate::handles::ImageFrame;
use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Pos2, Rect, Sense, TextureFilter, TextureHandle,
    TextureOptions, TextureWrapMode, Vec2, pos2, vec2,
};

/// Draws and handles interactive handles on top of the shown image.
pub type Overlay<'a> = &'a mut dyn FnMut(&mut egui::Ui, &ImageFrame);

const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 32.0;

pub struct Viewport {
    shown: Option<Shown>,
    checker: Option<TextureHandle>,
    zoom: f32,
    /// Offset of the image centre from the view centre, in screen points.
    pan: Vec2,
    /// Keep fitting the image to the view until the user zooms or pans.
    fit: bool,
    /// Pixel under the cursor during the last frame (the toolbar showing it
    /// is laid out before the image).
    hover_px: Option<(u32, u32)>,
}

struct Shown {
    /// Held so the pointer identity used for change detection stays valid.
    image: Arc<RasterImage>,
    texture: TextureHandle,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            shown: None,
            checker: None,
            zoom: 1.0,
            pan: Vec2::ZERO,
            fit: true,
            hover_px: None,
        }
    }
}

impl Viewport {
    pub fn request_fit(&mut self) {
        self.fit = true;
    }

    /// Current zoom: screen points per logical unit (an image pixel, unless
    /// shown scaled).
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn show(&mut self, ui: &mut egui::Ui, image: Option<&Arc<RasterImage>>, placeholder: &str) {
        self.show_scaled(ui, image, 1.0, None, placeholder);
    }

    /// Shows `image` at `scale` logical units per pixel, so the same content
    /// rendered at different resolutions keeps its on-screen size. `facts`
    /// replaces the pixel-size description in the toolbar.
    pub fn show_scaled(
        &mut self,
        ui: &mut egui::Ui,
        image: Option<&Arc<RasterImage>>,
        scale: f32,
        facts: Option<String>,
        placeholder: &str,
    ) {
        self.show_inner(ui, image, scale, facts, placeholder, None);
    }

    /// Like [`Viewport::show`], then lets `overlay` draw and handle
    /// interactive handles on top of the image (see [`crate::handles`]).
    /// Handles take priority over panning where they overlap.
    pub fn show_with_overlay(
        &mut self,
        ui: &mut egui::Ui,
        image: Option<&Arc<RasterImage>>,
        placeholder: &str,
        overlay: Overlay<'_>,
    ) {
        self.show_inner(ui, image, 1.0, None, placeholder, Some(overlay));
    }

    fn show_inner(
        &mut self,
        ui: &mut egui::Ui,
        image: Option<&Arc<RasterImage>>,
        scale: f32,
        facts: Option<String>,
        placeholder: &str,
        overlay: Option<Overlay<'_>>,
    ) {
        let Some(image) = image else {
            self.shown = None;
            let rect = ui.available_rect_before_wrap();
            ui.allocate_rect(rect, Sense::hover());
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                placeholder,
                FontId::proportional(15.0),
                ui.visuals().weak_text_color(),
            );
            return;
        };
        self.upload_if_changed(ui.ctx(), image);
        let img_size = vec2(image.width() as f32, image.height() as f32) * scale;

        self.toolbar(ui, image, facts);
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);

        if self.fit || response.double_clicked() {
            self.zoom = fit_zoom(rect.size(), img_size);
            self.pan = Vec2::ZERO;
            self.fit = true;
        }
        if response.dragged() {
            self.pan += response.drag_delta();
            self.fit = false;
        }
        if let Some(pointer) = response.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y + (i.zoom_delta() - 1.0) * 200.0);
            if scroll != 0.0 {
                let new_zoom = (self.zoom * (scroll * 0.0025).exp()).clamp(MIN_ZOOM, MAX_ZOOM);
                // Keep the image point under the cursor fixed.
                let anchor = pointer - rect.center() - self.pan;
                self.pan -= anchor * (new_zoom / self.zoom - 1.0);
                self.zoom = new_zoom;
                self.fit = false;
            }
        }

        let image_rect = Rect::from_center_size(rect.center() + self.pan, img_size * self.zoom);
        let checker = self.checker(ui.ctx());
        let tiles = image_rect.size() / 16.0;
        painter.image(
            checker.id(),
            image_rect,
            Rect::from_min_max(Pos2::ZERO, pos2(tiles.x, tiles.y)),
            Color32::WHITE,
        );
        if let Some(shown) = &self.shown {
            painter.image(
                shown.texture.id(),
                image_rect,
                Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        if let Some(overlay) = overlay {
            overlay(
                ui,
                &ImageFrame {
                    rect: image_rect,
                    clip: rect,
                },
            );
        }

        self.hover_px = response.hover_pos().and_then(|pointer| {
            let p = (pointer - image_rect.min) / (self.zoom * scale);
            (p.x >= 0.0 && p.y >= 0.0 && p.x < image.width() as f32 && p.y < image.height() as f32)
                .then_some((p.x as u32, p.y as u32))
        });
    }

    /// Zoom controls, image facts and the hovered pixel.
    fn toolbar(&mut self, ui: &mut egui::Ui, image: &RasterImage, facts: Option<String>) {
        ui.horizontal(|ui| {
            if ui
                .button("Fit")
                .on_hover_text("Fit to view (double-click the image)")
                .clicked()
            {
                self.fit = true;
            }
            if ui.button("100%").clicked() {
                self.zoom = 1.0;
                self.pan = Vec2::ZERO;
                self.fit = false;
            }
            ui.label(format!("{:.0}%", self.zoom * 100.0));
            ui.separator();
            let facts = facts.unwrap_or_else(|| {
                let mut facts = format!("{} × {} px", image.width(), image.height());
                if let (Some(dpi), Some((w, h))) = (image.dpi(), image.size_mm()) {
                    facts += &format!(" · {dpi:.0} dpi · {w:.1} × {h:.1} mm");
                }
                facts
            });
            ui.label(facts);
            if let Some((x, y)) = self.hover_px
                && let Some([r, g, b, a]) = image.get_pixel(x, y)
            {
                ui.separator();
                ui.label(format!("({x}, {y})  rgba({r}, {g}, {b}, {a})"));
            }
        });
    }

    fn upload_if_changed(&mut self, ctx: &egui::Context, image: &Arc<RasterImage>) {
        if self
            .shown
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(&s.image, image))
        {
            return;
        }
        let max_side = ctx.input(|i| i.max_texture_side).max(1);
        let color_image = to_color_image(image, max_side);
        let options = TextureOptions {
            magnification: TextureFilter::Nearest, // crisp pixels when zoomed in
            minification: TextureFilter::Linear,
            wrap_mode: TextureWrapMode::ClampToEdge,
            mipmap_mode: Some(TextureFilter::Linear), // smooth when zoomed out
        };
        let texture = match self.shown.take() {
            Some(mut shown) => {
                shown.texture.set(color_image, options);
                shown.texture
            }
            None => ctx.load_texture("preview", color_image, options),
        };
        self.shown = Some(Shown {
            image: Arc::clone(image),
            texture,
        });
    }

    fn checker(&mut self, ctx: &egui::Context) -> TextureHandle {
        self.checker
            .get_or_insert_with(|| {
                let (a, b) = (Color32::from_gray(200), Color32::from_gray(160));
                ctx.load_texture(
                    "checker",
                    ColorImage::new([2, 2], vec![a, b, b, a]),
                    TextureOptions::NEAREST_REPEAT,
                )
            })
            .clone()
    }
}

fn fit_zoom(view: Vec2, image: Vec2) -> f32 {
    if image.x <= 0.0 || image.y <= 0.0 {
        return 1.0;
    }
    ((view.x / image.x).min(view.y / image.y) * 0.96).clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Converts for display, box-downscaling if the GPU cannot hold the full size.
fn to_color_image(image: &RasterImage, max_side: usize) -> ColorImage {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let factor = w.max(h).div_ceil(max_side).max(1);
    if factor == 1 {
        return ColorImage::from_rgba_unmultiplied([w, h], image.as_raw());
    }
    let (sw, sh) = (w / factor, h / factor);
    let mut out = Vec::with_capacity(sw * sh * 4);
    for sy in 0..sh {
        for sx in 0..sw {
            let mut acc = [0u32; 4];
            for y in sy * factor..(sy + 1) * factor {
                for px in &image.row(y as u32)[sx * factor..(sx + 1) * factor] {
                    (0..4).for_each(|c| acc[c] += px[c] as u32);
                }
            }
            let n = (factor * factor) as u32;
            out.extend(acc.map(|v| (v / n) as u8));
        }
    }
    ColorImage::from_rgba_unmultiplied([sw, sh], &out)
}
