//! "Crop to subject": find the sheet of paper in a photo and flatten it.

use std::collections::VecDeque;

use docforge_core::RasterImage;
use rayon::prelude::*;

use super::filters::{box_blur, downscale};

/// Corner order used everywhere: top-left, top-right, bottom-right, bottom-left.
pub type Quad = [[f32; 2]; 4];

/// Finds the page outline, in pixel coordinates.
///
/// Works on a ~512 px copy: blur, Otsu threshold (paper is normally the
/// brightest large region), keep the largest bright connected region, then
/// take its extreme points along the diagonals as corners. Returns `None`
/// when no plausible page is found (too small, or the whole frame).
pub fn detect_page(img: &RasterImage) -> Option<Quad> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w < 16 || h < 16 {
        return None;
    }
    let factor = (w.max(h) / 512).max(1);
    let (sw, sh, small) = downscale(img, factor, true);
    let small = box_blur(&small, sw, sh, 1, 2, 2);

    let threshold = otsu(&small);
    let mask: Vec<bool> = small.iter().map(|&v| v > threshold).collect();
    let region = largest_region(&mask, sw, sh)?;

    let coverage = region.len() as f32 / (sw * sh) as f32;
    if !(0.05..=0.97).contains(&coverage) {
        return None;
    }

    // Extremes of x+y and x−y give the four corners of a roughly upright quad.
    let pick = |key: fn(f32, f32) -> f32, max: bool| {
        let best = region
            .iter()
            .map(|&(x, y)| (x as f32, y as f32))
            .max_by(|a, b| {
                let (ka, kb) = (key(a.0, a.1), key(b.0, b.1));
                if max {
                    ka.total_cmp(&kb)
                } else {
                    kb.total_cmp(&ka)
                }
            })?;
        let scale = |v: f32| (v + 0.5) * factor as f32;
        Some([scale(best.0).min(w as f32), scale(best.1).min(h as f32)])
    };
    let quad = [
        pick(|x, y| x + y, false)?,
        pick(|x, y| x - y, true)?,
        pick(|x, y| x + y, true)?,
        pick(|x, y| x - y, false)?,
    ];
    (quad_area(&quad) > 0.05 * (w * h) as f32).then_some(quad)
}

fn otsu(values: &[f32]) -> f32 {
    let mut hist = [0u32; 256];
    for &v in values {
        hist[v.clamp(0.0, 255.0) as usize] += 1;
    }
    let total = values.len() as f64;
    let sum_all: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &n)| i as f64 * n as f64)
        .sum();
    let (mut w0, mut sum0, mut best, mut best_t) = (0.0, 0.0, -1.0, 127);
    for (t, &n) in hist.iter().enumerate() {
        w0 += n as f64;
        sum0 += t as f64 * n as f64;
        let w1 = total - w0;
        if w0 == 0.0 || w1 == 0.0 {
            continue;
        }
        let (m0, m1) = (sum0 / w0, (sum_all - sum0) / w1);
        let between = w0 * w1 * (m0 - m1).powi(2);
        if between > best {
            best = between;
            best_t = t;
        }
    }
    best_t as f32
}

fn largest_region(mask: &[bool], w: usize, h: usize) -> Option<Vec<(usize, usize)>> {
    let mut seen = vec![false; mask.len()];
    let mut best: Vec<(usize, usize)> = Vec::new();
    let mut queue = VecDeque::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        let mut region = Vec::new();
        seen[start] = true;
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            let (x, y) = (i % w, i / w);
            region.push((x, y));
            let neighbours = [
                (x > 0).then(|| i - 1),
                (x + 1 < w).then(|| i + 1),
                (y > 0).then(|| i - w),
                (y + 1 < h).then(|| i + w),
            ];
            for j in neighbours.into_iter().flatten() {
                if mask[j] && !seen[j] {
                    seen[j] = true;
                    queue.push_back(j);
                }
            }
        }
        if region.len() > best.len() {
            best = region;
        }
    }
    (!best.is_empty()).then_some(best)
}

fn quad_area(q: &Quad) -> f32 {
    (0..4)
        .map(|i| {
            let (a, b) = (q[i], q[(i + 1) % 4]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f32>()
        .abs()
        / 2.0
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Maps the quad onto an upright rectangle (perspective correction). The
/// output size follows the longer opposite edges, so no detail is lost.
pub fn warp(img: &RasterImage, quad: &Quad) -> Option<RasterImage> {
    let [tl, tr, br, bl] = *quad;
    let out_w = dist(tl, tr).max(dist(bl, br)).round() as u32;
    let out_h = dist(tl, bl).max(dist(tr, br)).round() as u32;
    if out_w < 2 || out_h < 2 {
        return None;
    }
    let (wf, hf) = (out_w as f32, out_h as f32);
    let h = homography(&[[0.0, 0.0], [wf, 0.0], [wf, hf], [0.0, hf]], quad)?;

    let mut data = vec![[0u8; 4]; out_w as usize * out_h as usize];
    data.par_chunks_mut(out_w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, px) in row.iter_mut().enumerate() {
                let (u, v) = (x as f64 + 0.5, y as f64 + 0.5);
                let d = h[6] * u + h[7] * v + 1.0;
                let sx = (h[0] * u + h[1] * v + h[2]) / d;
                let sy = (h[3] * u + h[4] * v + h[5]) / d;
                *px = sample_bilinear(img, sx as f32 - 0.5, sy as f32 - 0.5);
            }
        });
    Some(
        RasterImage::from_rgba8(out_w, out_h, data.into_flattened())
            .expect("sized from output dimensions")
            .with_dpi(img.dpi()),
    )
}

/// Bilinear sample at continuous pixel coordinates (pixel centres on
/// integers), clamped to the edges.
pub(crate) fn sample_bilinear(img: &RasterImage, x: f32, y: f32) -> [u8; 4] {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let x = x.clamp(0.0, w - 1.0);
    let y = y.clamp(0.0, h - 1.0);
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (x - x0, y - y0);
    let (x0, y0) = (x0 as u32, y0 as u32);
    let (x1, y1) = ((x0 + 1).min(w as u32 - 1), (y0 + 1).min(h as u32 - 1));
    let p = |x, y| img.get_pixel(x, y).expect("clamped");
    let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    std::array::from_fn(|i| {
        let top = a[i] as f32 + (b[i] as f32 - a[i] as f32) * tx;
        let bottom = c[i] as f32 + (d[i] as f32 - c[i] as f32) * tx;
        (top + (bottom - top) * ty).round() as u8
    })
}

/// Solves for the projective transform mapping each `from[i]` onto `to[i]`.
/// Returns `[h11, h12, h13, h21, h22, h23, h31, h32]` with `h33 = 1`.
fn homography(from: &Quad, to: &Quad) -> Option<[f64; 8]> {
    let mut m = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let (u, v) = (from[i][0] as f64, from[i][1] as f64);
        let (x, y) = (to[i][0] as f64, to[i][1] as f64);
        m[2 * i] = [u, v, 1.0, 0.0, 0.0, 0.0, -u * x, -v * x, x];
        m[2 * i + 1] = [0.0, 0.0, 0.0, u, v, 1.0, -u * y, -v * y, y];
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..8 {
        let pivot = (col..8).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        for row in 0..8 {
            if row != col {
                let pivot_row = m[col];
                let f = m[row][col] / pivot_row[col];
                for (v, p) in m[row].iter_mut().zip(pivot_row).skip(col) {
                    *v -= f * p;
                }
            }
        }
    }
    Some(std::array::from_fn(|i| m[i][8] / m[i][i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white page with dark text-like bars, drawn as a skewed quad on a
    /// dark desk.
    fn photo(quad: &Quad, w: u32, h: u32) -> RasterImage {
        let inside = |x: f32, y: f32| {
            (0..4).all(|i| {
                let (a, b) = (quad[i], quad[(i + 1) % 4]);
                (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]) >= 0.0
            })
        };
        let data = (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    if inside(fx, fy) {
                        [235, 232, 225, 255]
                    } else {
                        [60, 50, 45, 255]
                    }
                })
            })
            .collect();
        RasterImage::from_rgba8(w, h, data).unwrap()
    }

    #[test]
    fn detects_a_skewed_page() {
        let quad = [[120.0, 80.0], [690.0, 110.0], [650.0, 560.0], [90.0, 520.0]];
        let found = detect_page(&photo(&quad, 800, 600)).expect("page found");
        for (a, b) in found.iter().zip(&quad) {
            assert!(dist(*a, *b) < 6.0, "{found:?} vs {quad:?}");
        }
    }

    #[test]
    fn rejects_frames_without_a_page() {
        let blank = RasterImage::new(300, 200, [200, 200, 200, 255]);
        assert_eq!(detect_page(&blank), None);
    }

    #[test]
    fn warp_flattens_the_quad() {
        let quad = [[120.0, 80.0], [690.0, 110.0], [650.0, 560.0], [90.0, 520.0]];
        let flat = warp(&photo(&quad, 800, 600), &quad).unwrap();
        // Every border pixel of the result lies on the page, not the desk.
        let (w, h) = flat.dimensions();
        for (x, y) in [
            (2, 2),
            (w - 3, 2),
            (w - 3, h - 3),
            (2, h - 3),
            (w / 2, h / 2),
        ] {
            assert!(flat.get_pixel(x, y).unwrap()[0] > 200, "({x},{y})");
        }
    }

    #[test]
    fn homography_maps_corners() {
        let from = [[0.0, 0.0], [10.0, 0.0], [10.0, 5.0], [0.0, 5.0]];
        let to = [[3.0, 1.0], [20.0, 2.0], [18.0, 14.0], [1.0, 12.0]];
        let h = homography(&from, &to).unwrap();
        for (f, t) in from.iter().zip(&to) {
            let (u, v) = (f[0] as f64, f[1] as f64);
            let d = h[6] * u + h[7] * v + 1.0;
            assert!(((h[0] * u + h[1] * v + h[2]) / d - t[0] as f64).abs() < 1e-6);
            assert!(((h[3] * u + h[4] * v + h[5]) / d - t[1] as f64).abs() < 1e-6);
        }
    }
}
