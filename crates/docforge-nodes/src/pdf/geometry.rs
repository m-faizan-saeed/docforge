//! Affine matrices and boxes in PDF coordinates.

/// PDF transformation matrix `[a b c d e f]`, row-vector convention:
/// `(x, y) → (a·x + c·y + e, b·x + d·y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Matrix {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn new([a, b, c, d, e, f]: [f64; 6]) -> Self {
        Self { a, b, c, d, e, f }
    }

    pub fn translate(x: f64, y: f64) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::IDENTITY
        }
    }

    /// `self × other`: apply `self` first, then `other`. This is how `cm`
    /// combines with the current matrix: `CTM' = M × CTM`.
    pub fn then(&self, o: &Matrix) -> Matrix {
        Matrix {
            a: self.a * o.a + self.b * o.c,
            b: self.a * o.b + self.b * o.d,
            c: self.c * o.a + self.d * o.c,
            d: self.c * o.b + self.d * o.d,
            e: self.e * o.a + self.f * o.c + o.e,
            f: self.e * o.b + self.f * o.d + o.f,
        }
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Largest factor by which this matrix stretches a length; used to
    /// turn a line width into page units conservatively.
    pub fn max_scale(&self) -> f64 {
        self.a.hypot(self.b).max(self.c.hypot(self.d))
    }
}

/// Axis-aligned box `[x0, y0] – [x1, y1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bbox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Bbox {
    pub fn point(x: f64, y: f64) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: x,
            y1: y,
        }
    }

    /// From a PDF rectangle array in any corner order.
    pub fn from_rect([a, b, c, d]: [f64; 4]) -> Self {
        Self {
            x0: a.min(c),
            y0: b.min(d),
            x1: a.max(c),
            y1: b.max(d),
        }
    }

    /// Bounds of the rectangle `r` after transforming it by `m`.
    pub fn transformed(r: &Bbox, m: &Matrix) -> Self {
        let mut out = Self::point_t(m, r.x0, r.y0);
        for (x, y) in [(r.x1, r.y0), (r.x0, r.y1), (r.x1, r.y1)] {
            out.add(m.apply(x, y));
        }
        out
    }

    fn point_t(m: &Matrix, x: f64, y: f64) -> Self {
        let (x, y) = m.apply(x, y);
        Self::point(x, y)
    }

    pub fn add(&mut self, (x, y): (f64, f64)) {
        self.x0 = self.x0.min(x);
        self.y0 = self.y0.min(y);
        self.x1 = self.x1.max(x);
        self.y1 = self.y1.max(y);
    }

    pub fn union(&self, o: &Bbox) -> Bbox {
        Bbox {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    /// Overlap, or `None` if the boxes do not overlap.
    pub fn intersect(&self, o: &Bbox) -> Option<Bbox> {
        let r = Bbox {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        };
        (r.x0 <= r.x1 && r.y0 <= r.y1).then_some(r)
    }

    pub fn expand(&self, by: f64) -> Bbox {
        Bbox {
            x0: self.x0 - by,
            y0: self.y0 - by,
            x1: self.x1 + by,
            y1: self.y1 + by,
        }
    }

    pub fn to_array(self) -> [f32; 4] {
        [
            self.x0 as f32,
            self.y0 as f32,
            self.x1 as f32,
            self.y1 as f32,
        ]
    }
}

/// Exact bounds of a cubic Bézier: the end points plus wherever the curve
/// turns around (roots of the derivative). The reference approach used only
/// the end point, which cuts off curves that bulge outward.
pub fn cubic_bounds(p: [(f64, f64); 4]) -> Bbox {
    let mut b = Bbox::point(p[0].0, p[0].1);
    b.add(p[3]);
    let coord = |i: usize, t: f64| {
        let get = |k: usize| if i == 0 { p[k].0 } else { p[k].1 };
        let mt = 1.0 - t;
        mt * mt * mt * get(0)
            + 3.0 * mt * mt * t * get(1)
            + 3.0 * mt * t * t * get(2)
            + t * t * t * get(3)
    };
    for axis in 0..2 {
        let get = |k: usize| if axis == 0 { p[k].0 } else { p[k].1 };
        let (p0, p1, p2, p3) = (get(0), get(1), get(2), get(3));
        // B'(t)/3 = a·t² + b·t + c
        let a = -p0 + 3.0 * p1 - 3.0 * p2 + p3;
        let bq = 2.0 * (p0 - 2.0 * p1 + p2);
        let c = p1 - p0;
        let mut roots = [None, None];
        if a.abs() < 1e-12 {
            if bq.abs() > 1e-12 {
                roots[0] = Some(-c / bq);
            }
        } else {
            let disc = bq * bq - 4.0 * a * c;
            if disc >= 0.0 {
                let s = disc.sqrt();
                roots = [Some((-bq + s) / (2.0 * a)), Some((-bq - s) / (2.0 * a))];
            }
        }
        for t in roots.into_iter().flatten().filter(|t| *t > 0.0 && *t < 1.0) {
            b.add((coord(0, t), coord(1, t)));
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_composition_matches_pdf_cm() {
        // Translate then scale: `cm` with T after S means CTM = T × S.
        let scale = Matrix::new([2.0, 0.0, 0.0, 3.0, 0.0, 0.0]);
        let ctm = Matrix::translate(10.0, 20.0).then(&scale);
        assert_eq!(ctm.apply(1.0, 1.0), (22.0, 63.0));
    }

    #[test]
    fn cubic_bounds_include_bulges() {
        // Arch from (0,0) to (100,0) with control points at height 100:
        // the true top is 75, which an end-point-only box would miss.
        let b = cubic_bounds([(0.0, 0.0), (0.0, 100.0), (100.0, 100.0), (100.0, 0.0)]);
        assert!((b.y1 - 75.0).abs() < 1e-9, "{b:?}");
        assert_eq!((b.x0, b.x1, b.y0), (0.0, 100.0, 0.0));
    }
}
