//! Pixel filters: blur primitives, tone and colour adjustments, geometry.

use docforge_core::{NodeError, RasterImage};
use rayon::prelude::*;

/// Rec. 601 luma in 8-bit fixed point.
#[inline]
pub(crate) fn luma([r, g, b, _]: [u8; 4]) -> u8 {
    ((77 * r as u32 + 150 * g as u32 + 29 * b as u32 + 128) >> 8) as u8
}

#[inline]
pub(crate) fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

// ----- blur primitives (interleaved f32 planes with `ch` channels) -----------

/// Box radius whose three-pass repetition approximates a Gaussian of `sigma`.
pub(crate) fn gaussian_box_radius(sigma: f32) -> usize {
    if sigma <= 0.0 {
        return 0;
    }
    (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0)
        .round()
        .max(1.0) as usize
}

/// Separable box blur with clamped edges, repeated `passes` times per axis.
/// One pass is an exact local mean; three approximate a Gaussian.
pub(crate) fn box_blur(
    data: &[f32],
    w: usize,
    h: usize,
    ch: usize,
    radius: usize,
    passes: usize,
) -> Vec<f32> {
    if radius == 0 || passes == 0 || w == 0 || h == 0 {
        return data.to_vec();
    }
    let mut buf = data.to_vec();
    for _ in 0..passes {
        buf = blur_rows(&buf, w, ch, radius);
    }
    for _ in 0..passes {
        buf = blur_cols(&buf, w, h, ch, radius);
    }
    buf
}

/// Vertical box blur. Columns are processed in parallel strips with a
/// sliding window that reads whole strip-rows, which stays cache friendly
/// (a transpose-based approach is several times slower on large images).
fn blur_cols(src: &[f32], w: usize, h: usize, ch: usize, r: usize) -> Vec<f32> {
    const STRIP: usize = 64;
    let last = h as isize - 1;
    let norm = 1.0 / (2 * r + 1) as f64;
    let ri = r as isize;
    let strips: Vec<(usize, Vec<f32>)> = (0..w)
        .step_by(STRIP)
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|x0| {
            let x1 = (x0 + STRIP).min(w);
            let sw = (x1 - x0) * ch;
            let row = |y: isize| {
                let y = y.clamp(0, last) as usize;
                &src[(y * w + x0) * ch..(y * w + x1) * ch]
            };
            let mut sum = vec![0.0f64; sw];
            for y in -ri..=ri {
                sum.iter_mut()
                    .zip(row(y))
                    .for_each(|(s, &v)| *s += v as f64);
            }
            let mut out = vec![0.0f32; h * sw];
            for (y, dst) in out.chunks_exact_mut(sw).enumerate() {
                dst.iter_mut()
                    .zip(&sum)
                    .for_each(|(d, &s)| *d = (s * norm) as f32);
                let yi = y as isize;
                let (add, sub) = (row(yi + ri + 1), row(yi - ri));
                for ((s, &a), &b) in sum.iter_mut().zip(add).zip(sub) {
                    *s += a as f64 - b as f64;
                }
            }
            (x0, out)
        })
        .collect();
    let mut out = vec![0.0f32; src.len()];
    out.par_chunks_mut(w * ch).enumerate().for_each(|(y, dst)| {
        for (x0, strip) in &strips {
            let sw = strip.len() / h;
            dst[x0 * ch..x0 * ch + sw].copy_from_slice(&strip[y * sw..(y + 1) * sw]);
        }
    });
    out
}

fn blur_rows(src: &[f32], w: usize, ch: usize, r: usize) -> Vec<f32> {
    let mut out = vec![0.0; src.len()];
    out.par_chunks_mut(w * ch)
        .zip(src.par_chunks(w * ch))
        .for_each(|(o, s)| blur_line(s, o, w, ch, r));
    out
}

fn blur_line(s: &[f32], o: &mut [f32], w: usize, ch: usize, r: usize) {
    let last = w as isize - 1;
    let at = |i: isize, c: usize| s[i.clamp(0, last) as usize * ch + c] as f64;
    let norm = 1.0 / (2 * r + 1) as f64;
    let r = r as isize;
    for c in 0..ch {
        let mut sum: f64 = (-r..=r).map(|i| at(i, c)).sum();
        for x in 0..w {
            o[x * ch + c] = (sum * norm) as f32;
            let xi = x as isize;
            sum += at(xi + r + 1, c) - at(xi - r, c);
        }
    }
}

fn transpose(src: &[f32], w: usize, h: usize, ch: usize) -> Vec<f32> {
    let mut out = vec![0.0; src.len()];
    out.par_chunks_mut(h * ch).enumerate().for_each(|(x, row)| {
        for y in 0..h {
            let s = (y * w + x) * ch;
            row[y * ch..y * ch + ch].copy_from_slice(&src[s..s + ch]);
        }
    });
    out
}

/// Separable maximum filter (dilation) with clamped edges.
fn max_filter(data: &[f32], w: usize, h: usize, ch: usize, r: usize) -> Vec<f32> {
    let pass = |src: &[f32], w: usize, h: usize| {
        let mut out = vec![0.0; src.len()];
        out.par_chunks_mut(w * ch).enumerate().for_each(|(y, row)| {
            let line = &src[y * w * ch..(y + 1) * w * ch];
            for x in 0..w {
                let (lo, hi) = (x.saturating_sub(r), (x + r).min(w - 1));
                for c in 0..ch {
                    row[x * ch + c] = (lo..=hi).map(|i| line[i * ch + c]).fold(f32::MIN, f32::max);
                }
            }
        });
        let _ = h;
        out
    };
    let rows = pass(data, w, h);
    let t = transpose(&rows, w, h, ch);
    let cols = pass(&t, h, w);
    transpose(&cols, h, w, ch)
}

/// Area-average downscale by an integer factor into RGB (or luma) f32.
pub(crate) fn downscale(img: &RasterImage, factor: usize, gray: bool) -> (usize, usize, Vec<f32>) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let (sw, sh) = (w.div_ceil(factor), h.div_ceil(factor));
    let ch = if gray { 1 } else { 3 };
    let mut out = vec![0.0; sw * sh * ch];
    out.par_chunks_mut(sw * ch)
        .enumerate()
        .for_each(|(sy, row)| {
            let (y0, y1) = (sy * factor, ((sy + 1) * factor).min(h));
            for sx in 0..sw {
                let (x0, x1) = (sx * factor, ((sx + 1) * factor).min(w));
                let mut acc = [0u64; 3];
                for y in y0..y1 {
                    for px in &img.row(y as u32)[x0..x1] {
                        if gray {
                            acc[0] += luma(*px) as u64;
                        } else {
                            (0..3).for_each(|c| acc[c] += px[c] as u64);
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as f32;
                for c in 0..ch {
                    row[sx * ch + c] = acc[c] as f32 / n;
                }
            }
        });
    (sw, sh, out)
}

// ----- tone and colour -------------------------------------------------------

/// Unsharp mask on luminance: adds `(factor - 1) * (luma - blurred luma)`
/// to every channel. A factor of 1 leaves the image unchanged; 1.5 is a
/// moderate sharpen. Working on luminance only is 3× cheaper than per-channel
/// sharpening and avoids coloured halos.
pub(crate) fn sharpen(img: &RasterImage, factor: f32, radius: f32) -> RasterImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let l: Vec<f32> = img.pixels().par_iter().map(|&px| luma_f(px)).collect();
    let blurred = box_blur(&l, w, h, 1, gaussian_box_radius(radius), 3);
    let gain = factor - 1.0;
    let mut out = img.clone();
    out.pixels_mut()
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, px)| {
            let detail = gain * (l[i] - blurred[i]);
            for c in &mut px[..3] {
                *c = to_u8(*c as f32 + detail);
            }
        });
    out
}

#[inline]
fn luma_f([r, g, b, _]: [u8; 4]) -> f32 {
    0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
}

/// `brightness` and `contrast` in −1..1; 0 leaves the image unchanged.
pub(crate) fn brightness_contrast(
    img: &RasterImage,
    brightness: f32,
    contrast: f32,
) -> RasterImage {
    let contrast = contrast.clamp(-1.0, 1.0);
    let gain = if contrast >= 0.0 {
        1.0 / (1.0 - contrast * 0.99)
    } else {
        1.0 + contrast
    };
    let offset = brightness.clamp(-1.0, 1.0) * 255.0;
    let lut: [u8; 256] = std::array::from_fn(|v| to_u8((v as f32 - 127.5) * gain + 127.5 + offset));
    map_rgb(img, |px| {
        [
            lut[px[0] as usize],
            lut[px[1] as usize],
            lut[px[2] as usize],
        ]
    })
}

pub(crate) fn grayscale(img: &RasterImage) -> RasterImage {
    map_rgb(img, |px| {
        let l = luma(px);
        [l, l, l]
    })
}

fn map_rgb(img: &RasterImage, f: impl Fn([u8; 4]) -> [u8; 3] + Sync) -> RasterImage {
    let mut out = img.clone();
    out.pixels_mut().par_iter_mut().for_each(|px| {
        let [r, g, b] = f(*px);
        *px = [r, g, b, px[3]];
    });
    out
}

/// CamScanner-style "magic colour": divides out the uneven illumination
/// (shadows, yellowed paper) so the page becomes white, then deepens ink and
/// boosts colour. `strength` 0..1.
pub(crate) fn magic_color(img: &RasterImage, strength: f32) -> RasterImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 {
        return img.clone();
    }
    let strength = strength.clamp(0.0, 1.0);

    // Background estimate: shrink to ~400 px, dilate to erase dark ink, smooth.
    let factor = (w.max(h) / 400).max(1);
    let (sw, sh, small) = downscale(img, factor, false);
    // The window must be wider than the thickest ink strokes: ~3% of the page.
    let reach = ((sw.max(sh) as f32 * 0.03).ceil() as usize).max(3);
    let paper = max_filter(&small, sw, sh, 3, reach);
    let paper = box_blur(&paper, sw, sh, 3, reach / 2, 2);

    // Bilinear lookup table from full-res coordinate to background cell.
    let axis = |n: usize, sn: usize| -> Vec<(usize, usize, f32)> {
        (0..n)
            .map(|i| {
                let f = ((i as f32 + 0.5) / factor as f32 - 0.5).clamp(0.0, (sn - 1) as f32);
                let i0 = f.floor() as usize;
                (i0, (i0 + 1).min(sn - 1), f - i0 as f32)
            })
            .collect()
    };
    let (xs, ys) = (axis(w, sw), axis(h, sh));
    let gamma = 1.0 + 1.5 * strength;
    let saturation = 1.0 + 0.6 * strength;

    let mut out = img.clone();
    out.pixels_mut()
        .par_chunks_mut(w)
        .enumerate()
        .for_each(|(y, row)| {
            let (y0, y1, ty) = ys[y];
            for (x, px) in row.iter_mut().enumerate() {
                let (x0, x1, tx) = xs[x];
                let mut v = [0.0f32; 3];
                for (c, v) in v.iter_mut().enumerate() {
                    let at = |sx: usize, sy: usize| paper[(sy * sw + sx) * 3 + c];
                    let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
                    let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
                    let bg = (top + (bottom - top) * ty).max(1.0);
                    *v = (px[c] as f32 / bg).min(1.0).powf(gamma);
                }
                let l = 0.299 * v[0] + 0.587 * v[1] + 0.114 * v[2];
                for c in 0..3 {
                    px[c] = to_u8((l + (v[c] - l) * saturation) * 255.0);
                }
            }
        });
    out
}

/// Adaptive (Bradley) threshold: a pixel turns black when it is darker than
/// its neighbourhood mean by more than `sensitivity`. Robust to shadows.
/// `window` is the neighbourhood size as a fraction of the shorter side.
pub(crate) fn black_and_white(img: &RasterImage, window: f32, sensitivity: f32) -> RasterImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let l: Vec<f32> = img.pixels().par_iter().map(|&px| luma(px) as f32).collect();
    let radius = ((window * w.min(h) as f32) / 2.0).round().max(1.0) as usize;
    let mean = box_blur(&l, w, h, 1, radius, 1);
    let cut = 1.0 - sensitivity.clamp(0.0, 1.0);
    let mut out = img.clone();
    out.pixels_mut()
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, px)| {
            let v = if l[i] < mean[i] * cut { 0 } else { 255 };
            *px = [v, v, v, px[3]];
        });
    out
}

// ----- geometry --------------------------------------------------------------

/// Clockwise rotation by `quarter_turns` × 90°.
pub(crate) fn rotate(img: &RasterImage, quarter_turns: u8) -> RasterImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let turns = quarter_turns % 4;
    if turns == 0 {
        return img.clone();
    }
    let (nw, nh) = if turns == 2 { (w, h) } else { (h, w) };
    let src = img.pixels();
    let mut data = vec![[0u8; 4]; w * h];
    data.par_chunks_mut(nw.max(1))
        .enumerate()
        .for_each(|(ny, row)| {
            for (nx, px) in row.iter_mut().enumerate() {
                let (x, y) = match turns {
                    1 => (ny, h - 1 - nx),
                    2 => (w - 1 - nx, h - 1 - ny),
                    _ => (w - 1 - ny, nx),
                };
                *px = src[y * w + x];
            }
        });
    RasterImage::from_rgba8(nw as u32, nh as u32, data.into_flattened())
        .expect("same pixel count")
        .with_dpi(img.dpi())
}

/// Copies the pixel rectangle `[x0, x1) × [y0, y1)`.
pub(crate) fn sub_image(img: &RasterImage, x0: u32, y0: u32, x1: u32, y1: u32) -> RasterImage {
    let mut data = Vec::with_capacity((x1 - x0) as usize * (y1 - y0) as usize * 4);
    for y in y0..y1 {
        data.extend(img.row(y)[x0 as usize..x1 as usize].as_flattened());
    }
    RasterImage::from_rgba8(x1 - x0, y1 - y0, data)
        .expect("sized from the rectangle")
        .with_dpi(img.dpi())
}

/// Trims the given fractions of the width/height from each side.
pub(crate) fn crop_fractions(
    img: &RasterImage,
    [left, top, right, bottom]: [f32; 4],
) -> Result<RasterImage, NodeError> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let px = |f: f32, n: f32| (f.clamp(0.0, 1.0) * n).round() as u32;
    let (x0, y0) = (px(left, w), px(top, h));
    let (x1, y1) = (
        img.width().saturating_sub(px(right, w)),
        img.height().saturating_sub(px(bottom, h)),
    );
    if x1 <= x0 || y1 <= y0 {
        return Err(NodeError::InvalidParam {
            name: "crop",
            reason: "the margins leave nothing of the image".into(),
        });
    }
    Ok(sub_image(img, x0, y0, x1, y1))
}

/// Crops to the bounding box of everything that differs from the border
/// colour by more than `tolerance`, plus `padding` pixels. Ideal for flatbed
/// scans with a uniform lid colour.
pub(crate) fn auto_crop(img: &RasterImage, tolerance: u8, padding: u32) -> RasterImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w < 3 || h < 3 {
        return img.clone();
    }
    let bg = border_median(img);
    let tol = tolerance as i16;
    let differs = |px: &[u8; 4]| (0..3).any(|c| (px[c] as i16 - bg[c] as i16).abs() > tol);

    let (row_counts, col_counts) = img
        .pixels()
        .par_chunks(w)
        .map(|row| {
            let mut cols = vec![0u32; w];
            let mut n = 0;
            for (x, px) in row.iter().enumerate() {
                if differs(px) {
                    cols[x] += 1;
                    n += 1;
                }
            }
            (vec![n], cols)
        })
        .reduce(
            || (Vec::new(), vec![0; w]),
            |(mut rows_a, mut cols_a), (rows_b, cols_b)| {
                rows_a.extend(rows_b);
                cols_a.iter_mut().zip(cols_b).for_each(|(a, b)| *a += b);
                (rows_a, cols_a)
            },
        );
    // Ignore specks: a row/column counts only with enough differing pixels.
    let span = |counts: &[u32], min: u32| {
        let first = counts.iter().position(|&c| c >= min)?;
        let last = counts.iter().rposition(|&c| c >= min)?;
        Some((first as u32, last as u32 + 1))
    };
    let (Some((y0, y1)), Some((x0, x1))) = (
        span(&row_counts, (w as u32 / 500).max(2)),
        span(&col_counts, (h as u32 / 500).max(2)),
    ) else {
        return img.clone();
    };
    sub_image(
        img,
        x0.saturating_sub(padding),
        y0.saturating_sub(padding),
        (x1 + padding).min(w as u32),
        (y1 + padding).min(h as u32),
    )
}

fn border_median(img: &RasterImage) -> [u8; 3] {
    let (w, h) = (img.width(), img.height());
    let mut samples: Vec<[u8; 4]> = Vec::with_capacity(2 * (w + h) as usize);
    samples.extend(img.row(0));
    samples.extend(img.row(h - 1));
    for y in 1..h - 1 {
        samples.push(img.row(y)[0]);
        samples.push(img.row(y)[w as usize - 1]);
    }
    std::array::from_fn(|c| {
        let mut v: Vec<u8> = samples.iter().map(|px| px[c]).collect();
        let mid = v.len() / 2;
        *v.select_nth_unstable(mid).1
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img_from_fn(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> RasterImage {
        let f = &f;
        let data = (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| f(x, y)))
            .collect::<Vec<_>>();
        RasterImage::from_rgba8(w, h, data).unwrap()
    }

    #[test]
    fn box_blur_preserves_constants_and_mean() {
        let data = vec![7.0; 10 * 6];
        assert!(
            box_blur(&data, 10, 6, 1, 2, 3)
                .iter()
                .all(|v| (v - 7.0).abs() < 1e-4)
        );

        let mut spike = vec![0.0; 21];
        spike[10] = 21.0;
        let out = box_blur(&spike, 21, 1, 1, 2, 1);
        assert!((out.iter().sum::<f32>() - 21.0).abs() < 1e-3);
        assert!((out[10] - 21.0 / 5.0).abs() < 1e-4);
    }

    #[test]
    fn sharpen_with_factor_one_is_identity() {
        let img = img_from_fn(9, 7, |x, y| [(x * 20) as u8, (y * 30) as u8, 90, 255]);
        assert_eq!(sharpen(&img, 1.0, 1.5), img);
        // A higher factor increases local contrast at an edge.
        let edge = img_from_fn(10, 1, |x, _| if x < 5 { [50; 4] } else { [200; 4] });
        let sharp = sharpen(&edge, 2.0, 1.0);
        assert!(sharp.pixels()[4][0] < 50 && sharp.pixels()[5][0] > 200);
    }

    #[test]
    fn rotations_compose() {
        let img = img_from_fn(3, 2, |x, y| [x as u8, y as u8, 0, 255]);
        let cw = rotate(&img, 1);
        assert_eq!(cw.dimensions(), (2, 3));
        // Clockwise: the bottom-left pixel becomes the top-left one.
        assert_eq!(cw.pixels()[0], img.get_pixel(0, 1).unwrap());
        assert_eq!(rotate(&rotate(&img, 1), 3), img);
        assert_eq!(rotate(&rotate(&img, 2), 2), img);
    }

    #[test]
    fn crop_and_auto_crop() {
        let page = img_from_fn(100, 80, |x, y| {
            if (20..60).contains(&x) && (10..50).contains(&y) {
                [0, 0, 0, 255]
            } else {
                [240, 240, 240, 255]
            }
        });
        let auto = auto_crop(&page, 30, 0);
        assert_eq!(auto.dimensions(), (40, 40));
        assert_eq!(auto_crop(&page, 30, 5).dimensions(), (50, 50));

        let cropped = crop_fractions(&page, [0.1, 0.25, 0.1, 0.25]).unwrap();
        assert_eq!(cropped.dimensions(), (80, 40));
        assert!(crop_fractions(&page, [0.6, 0.0, 0.6, 0.0]).is_err());
    }

    #[test]
    fn magic_color_whitens_shaded_paper_but_keeps_ink() {
        // Paper with a left-to-right shadow and a dark ink stroke.
        let page = img_from_fn(400, 200, |x, y| {
            let paper = 120 + (x * 110 / 400) as u8;
            if (90..110).contains(&y) && (100..300).contains(&x) {
                [30, 30, 30, 255]
            } else {
                [paper, paper, paper - 10, 255]
            }
        });
        let out = magic_color(&page, 0.7);
        let (dark_paper, bright_paper, ink) = (
            out.pixels()[5][0],
            out.pixels()[395][0],
            out.get_pixel(200, 100).unwrap()[0],
        );
        assert!(
            dark_paper > 230 && bright_paper > 230,
            "{dark_paper} {bright_paper}"
        );
        assert!(ink < 80, "{ink}");
    }

    #[test]
    fn black_and_white_is_binary_and_shadow_tolerant() {
        let page = img_from_fn(200, 100, |x, y| {
            let paper = 90 + (x / 2) as u8; // strong gradient
            if (45..55).contains(&y) {
                [paper / 3, paper / 3, paper / 3, 255]
            } else {
                [paper; 4]
            }
        });
        let bw = black_and_white(&page, 0.3, 0.2);
        assert!(bw.pixels().iter().all(|p| p[0] == 0 || p[0] == 255));
        assert_eq!(bw.get_pixel(10, 50).unwrap()[0], 0);
        assert_eq!(bw.get_pixel(10, 10).unwrap()[0], 255);
        assert_eq!(bw.get_pixel(190, 10).unwrap()[0], 255);
    }

    #[test]
    fn brightness_contrast_neutral_is_identity() {
        let img = img_from_fn(16, 16, |x, y| [(x * 16) as u8, (y * 16) as u8, 128, 200]);
        assert_eq!(brightness_contrast(&img, 0.0, 0.0), img);
    }
}
