//! Drawing on top of the image: text watermarks, lines and rectangles.
//!
//! Positions are fractions of the image size and lengths are fractions of the
//! shorter side, so overlays keep their look whatever the resolution or
//! earlier crops. All drawing is anti-aliased.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use docforge_core::{NodeError, RasterImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watermark {
    /// May contain line breaks.
    pub text: String,
    /// Text height as a fraction of the image's shorter side.
    pub size: f32,
    /// RGBA; the alpha channel sets the opacity.
    pub color: [u8; 4],
    /// Counter-clockwise rotation in degrees.
    pub angle: f32,
    /// Centre of the text as fractions of width and height.
    pub position: [f32; 2],
    /// Repeat the text across the whole page.
    pub tiled: bool,
    /// TrueType/OpenType font file; the built-in sans-serif when empty.
    pub font: Option<PathBuf>,
}

impl Default for Watermark {
    fn default() -> Self {
        Self {
            text: "DRAFT".into(),
            size: 0.15,
            color: [220, 30, 30, 90],
            angle: 30.0,
            position: [0.5, 0.5],
            tiled: false,
            font: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Line {
    pub from: [f32; 2],
    pub to: [f32; 2],
    /// Stroke width as a fraction of the shorter side.
    pub width: f32,
    pub color: [u8; 4],
}

impl Default for Line {
    fn default() -> Self {
        Self {
            from: [0.05, 0.95],
            to: [0.95, 0.95],
            width: 0.004,
            color: [0, 0, 0, 255],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rectangle {
    pub min: [f32; 2],
    pub max: [f32; 2],
    /// Stroke width as a fraction of the shorter side; 0 for no outline.
    pub stroke_width: f32,
    pub stroke: [u8; 4],
    pub fill: Option<[u8; 4]>,
}

impl Default for Rectangle {
    fn default() -> Self {
        Self {
            min: [0.1, 0.1],
            max: [0.9, 0.9],
            stroke_width: 0.004,
            stroke: [0, 90, 200, 255],
            fill: None,
        }
    }
}

/// "Source over" compositing of `color` with extra `coverage` (0..1).
#[inline]
fn blend(px: &mut [u8; 4], color: [u8; 4], coverage: f32) {
    let a = coverage * color[3] as f32 / 255.0;
    if a <= 0.0 {
        return;
    }
    for c in 0..3 {
        px[c] = (px[c] as f32 + (color[c] as f32 - px[c] as f32) * a).round() as u8;
    }
    px[3] = (px[3] as f32 + (255.0 - px[3] as f32) * a).round() as u8;
}

/// Runs `shade(x, y) -> Option<(color, coverage)>` for every pixel centre in
/// the clipped box, in parallel rows.
fn paint(
    img: &mut RasterImage,
    [x0, y0, x1, y1]: [f32; 4],
    shade: impl Fn(f32, f32, &mut [u8; 4]) + Sync,
) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let clamp = |v: f32, n: usize| (v.max(0.0) as usize).min(n);
    let (x0, x1) = (clamp(x0.floor(), w), clamp(x1.ceil(), w));
    let (y0, y1) = (clamp(y0.floor(), h), clamp(y1.ceil(), h));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    img.pixels_mut()
        .par_chunks_mut(w)
        .enumerate()
        .skip(y0)
        .take(y1 - y0)
        .for_each(|(y, row)| {
            for (x, px) in row.iter_mut().enumerate().take(x1).skip(x0) {
                shade(x as f32 + 0.5, y as f32 + 0.5, px);
            }
        });
}

fn short_side(img: &RasterImage) -> f32 {
    img.width().min(img.height()) as f32
}

pub(crate) fn draw_line(img: &mut RasterImage, line: &Line) {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let a = [line.from[0] * w, line.from[1] * h];
    let b = [line.to[0] * w, line.to[1] * h];
    let half = (line.width * short_side(img) / 2.0).max(0.5);
    let bounds = [
        a[0].min(b[0]) - half - 1.0,
        a[1].min(b[1]) - half - 1.0,
        a[0].max(b[0]) + half + 1.0,
        a[1].max(b[1]) + half + 1.0,
    ];
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = (dx * dx + dy * dy).max(1e-6);
    paint(img, bounds, |x, y, px| {
        let t = (((x - a[0]) * dx + (y - a[1]) * dy) / len2).clamp(0.0, 1.0);
        let d = (x - a[0] - t * dx).hypot(y - a[1] - t * dy);
        blend(px, line.color, (half - d + 0.5).clamp(0.0, 1.0));
    });
}

pub(crate) fn draw_rectangle(img: &mut RasterImage, rect: &Rectangle) {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (x0, x1) = (
        rect.min[0].min(rect.max[0]) * w,
        rect.min[0].max(rect.max[0]) * w,
    );
    let (y0, y1) = (
        rect.min[1].min(rect.max[1]) * h,
        rect.min[1].max(rect.max[1]) * h,
    );
    let half = rect.stroke_width.max(0.0) * short_side(img) / 2.0;
    let (cx, cy, bx, by) = (
        (x0 + x1) / 2.0,
        (y0 + y1) / 2.0,
        (x1 - x0) / 2.0,
        (y1 - y0) / 2.0,
    );
    let pad = half + 1.0;
    paint(img, [x0 - pad, y0 - pad, x1 + pad, y1 + pad], |x, y, px| {
        // Signed distance to the rectangle outline (negative inside).
        let (qx, qy) = ((x - cx).abs() - bx, (y - cy).abs() - by);
        let outside = qx.max(0.0).hypot(qy.max(0.0));
        let sd = outside + qx.max(qy).min(0.0);
        if let Some(fill) = rect.fill {
            blend(px, fill, (0.5 - sd).clamp(0.0, 1.0));
        }
        if half > 0.0 {
            blend(px, rect.stroke, (half - sd.abs() + 0.5).clamp(0.0, 1.0));
        }
    });
}

fn default_font() -> &'static FontArc {
    static FONT: OnceLock<FontArc> = OnceLock::new();
    FONT.get_or_init(|| {
        FontArc::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).expect("bundled font is valid")
    })
}

fn load_font(path: Option<&PathBuf>) -> Result<FontArc, NodeError> {
    let Some(path) = path.filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(default_font().clone());
    };
    let bytes = docforge_core::files::read(path).map_err(|source| NodeError::Io {
        path: path.clone(),
        source,
    })?;
    FontArc::try_from_vec(bytes).map_err(|_| NodeError::InvalidParam {
        name: "font",
        reason: format!("`{}` is not a TrueType/OpenType font", path.display()),
    })
}

/// Coverage mask of `text` (multi-line, centred), one f32 per pixel.
struct Mask {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

impl Mask {
    fn render(font: &FontArc, text: &str, px: f32) -> Self {
        let scaled = font.as_scaled(PxScale::from(px));
        let line_h = scaled.ascent() - scaled.descent() + scaled.line_gap();
        let lines: Vec<(f32, Vec<ab_glyph::Glyph>)> = text
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let (mut caret, mut prev, mut glyphs) = (0.0, None, Vec::new());
                for ch in line.chars() {
                    let id = scaled.glyph_id(ch);
                    if let Some(p) = prev {
                        caret += scaled.kern(p, id);
                    }
                    let baseline = scaled.ascent() + i as f32 * line_h;
                    glyphs.push(id.with_scale_and_position(px, point(caret, baseline)));
                    caret += scaled.h_advance(id);
                    prev = Some(id);
                }
                (caret, glyphs)
            })
            .collect();
        let text_w = lines.iter().map(|(w, _)| *w).fold(0.0, f32::max);
        let n_lines = lines.len().max(1) as f32;
        let (w, h) = (
            (text_w.ceil() as usize + 2).max(1),
            ((line_h * n_lines).ceil() as usize + 2).max(1),
        );
        let mut data = vec![0.0f32; w * h];
        for (line_w, glyphs) in &lines {
            let indent = 1.0 + (text_w - line_w) / 2.0;
            for mut glyph in glyphs.iter().cloned() {
                glyph.position.x += indent;
                glyph.position.y += 1.0;
                let Some(outline) = font.outline_glyph(glyph) else {
                    continue;
                };
                let b = outline.px_bounds();
                outline.draw(|gx, gy, c| {
                    let (x, y) = (b.min.x as i64 + gx as i64, b.min.y as i64 + gy as i64);
                    if (0..w as i64).contains(&x) && (0..h as i64).contains(&y) {
                        let i = y as usize * w + x as usize;
                        data[i] = (data[i] + c).min(1.0);
                    }
                });
            }
        }
        Self { w, h, data }
    }

    fn sample(&self, x: f32, y: f32) -> f32 {
        let (x, y) = (x - 0.5, y - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (tx, ty) = (x - x0, y - y0);
        let at = |x: f32, y: f32| {
            if x < 0.0 || y < 0.0 || x >= self.w as f32 || y >= self.h as f32 {
                0.0
            } else {
                self.data[y as usize * self.w + x as usize]
            }
        };
        let top = at(x0, y0) + (at(x0 + 1.0, y0) - at(x0, y0)) * tx;
        let bottom = at(x0, y0 + 1.0) + (at(x0 + 1.0, y0 + 1.0) - at(x0, y0 + 1.0)) * tx;
        top + (bottom - top) * ty
    }
}

pub(crate) fn draw_watermark(img: &mut RasterImage, wm: &Watermark) -> Result<(), NodeError> {
    if wm.text.trim().is_empty() || img.is_empty() {
        return Ok(());
    }
    if !(wm.size.is_finite() && wm.size > 0.0) {
        return Err(NodeError::InvalidParam {
            name: "size",
            reason: "must be positive".into(),
        });
    }
    let font = load_font(wm.font.as_ref())?;
    let px = (wm.size * short_side(img)).clamp(4.0, 4096.0);
    let mask = Arc::new(Mask::render(&font, &wm.text, px));
    let (mw, mh) = (mask.w as f32, mask.h as f32);

    let (w, h) = (img.width() as f32, img.height() as f32);
    let (cx, cy) = (wm.position[0] * w, wm.position[1] * h);
    let (sin, cos) = wm.angle.to_radians().sin_cos();
    // Screen → text space (inverse of a counter-clockwise rotation in y-down
    // coordinates), with the mask centred on the anchor.
    let to_text = move |x: f32, y: f32| {
        let (dx, dy) = (x - cx, y - cy);
        (
            cos * dx - sin * dy + mw / 2.0,
            sin * dx + cos * dy + mh / 2.0,
        )
    };

    if wm.tiled {
        let (tile_w, tile_h) = (mw + mh * 1.5, mh * 2.5);
        paint(img, [0.0, 0.0, w, h], |x, y, px| {
            let (tx, ty) = to_text(x, y);
            let row = (ty / tile_h).floor();
            let shift = if row.rem_euclid(2.0) == 1.0 {
                tile_w / 2.0
            } else {
                0.0
            };
            let (u, v) = ((tx + shift).rem_euclid(tile_w), ty.rem_euclid(tile_h));
            blend(px, wm.color, mask.sample(u, v));
        });
    } else {
        // Bounding box of the rotated mask.
        let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(sx, sy)| {
            let (x, y) = (sx * mw / 2.0, sy * mh / 2.0);
            (cx + cos * x + sin * y, cy - sin * x + cos * y)
        });
        let xs = corners.map(|c| c.0);
        let ys = corners.map(|c| c.1);
        let bounds = [
            xs.iter().copied().fold(f32::MAX, f32::min) - 1.0,
            ys.iter().copied().fold(f32::MAX, f32::min) - 1.0,
            xs.iter().copied().fold(f32::MIN, f32::max) + 1.0,
            ys.iter().copied().fold(f32::MIN, f32::max) + 1.0,
        ];
        paint(img, bounds, |x, y, px| {
            let (u, v) = to_text(x, y);
            blend(px, wm.color, mask.sample(u, v));
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(w: u32, h: u32) -> RasterImage {
        RasterImage::new(w, h, [255; 4])
    }

    fn inked(img: &RasterImage) -> usize {
        img.pixels().iter().filter(|p| p[0] < 200).count()
    }

    #[test]
    fn watermark_draws_inside_the_image() {
        let mut img = white(400, 300);
        let wm = Watermark {
            color: [0, 0, 0, 255],
            angle: 0.0,
            ..Watermark::default()
        };
        draw_watermark(&mut img, &wm).unwrap();
        let ink = inked(&img);
        assert!(ink > 500, "{ink}");
        // Centred: the corners stay clean.
        assert_eq!(img.get_pixel(0, 0).unwrap(), [255; 4]);
        assert_eq!(img.get_pixel(399, 299).unwrap(), [255; 4]);
    }

    #[test]
    fn tiled_watermark_covers_more_than_one() {
        let base = Watermark {
            color: [0, 0, 0, 255],
            size: 0.08,
            ..Watermark::default()
        };
        let (mut one, mut many) = (white(400, 400), white(400, 400));
        draw_watermark(&mut one, &base).unwrap();
        draw_watermark(
            &mut many,
            &Watermark {
                tiled: true,
                ..base
            },
        )
        .unwrap();
        assert!(inked(&many) > 3 * inked(&one));
    }

    #[test]
    fn translucent_colour_blends() {
        let mut img = white(100, 100);
        draw_rectangle(
            &mut img,
            &Rectangle {
                fill: Some([0, 0, 0, 128]),
                stroke_width: 0.0,
                ..Rectangle::default()
            },
        );
        let mid = img.get_pixel(50, 50).unwrap();
        assert!((120..=135).contains(&mid[0]), "{mid:?}");
        assert_eq!(img.get_pixel(2, 2).unwrap(), [255; 4]);
    }

    #[test]
    fn line_has_requested_width() {
        let mut img = white(200, 100);
        draw_line(
            &mut img,
            &Line {
                from: [0.0, 0.5],
                to: [1.0, 0.5],
                width: 0.1, // 10 px on a 100 px short side
                color: [0, 0, 0, 255],
            },
        );
        let column: Vec<u8> = (0..100)
            .map(|y| img.get_pixel(100, y).unwrap()[0])
            .collect();
        let dark = column.iter().filter(|&&v| v < 128).count();
        assert!((9..=11).contains(&dark), "{dark}");
    }

    #[test]
    fn bad_font_path_is_reported() {
        let mut img = white(50, 50);
        let wm = Watermark {
            font: Some("/no/such/font.ttf".into()),
            ..Watermark::default()
        };
        assert!(matches!(
            draw_watermark(&mut img, &wm),
            Err(NodeError::Io { .. })
        ));
    }
}
