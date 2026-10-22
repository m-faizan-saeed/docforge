//! Packs printed pieces onto as few pages as possible.
//!
//! Uses the MaxRects algorithm (Jukka Jylänki, "A Thousand Ways to Pack the
//! Bin"): every page keeps a list of maximal free rectangles; each piece goes
//! into the free rectangle that fits it best, optionally rotated by 90°, on
//! the earliest page with room. Two placement heuristics are tried —
//! top-left ("rows", which produces tidy grids of identical pieces, easy to
//! cut) and best-short-side-fit (densest) — on both page orientations, and
//! the layout with the fewest pages and the most compact use of them wins.
//!
//! All units are millimetres with the origin at the page's top-left corner.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() - EPS
            && o.x < self.right() - EPS
            && self.y < o.bottom() - EPS
            && o.y < self.bottom() - EPS
    }

    fn contains(&self, o: &Rect) -> bool {
        o.x >= self.x - EPS
            && o.y >= self.y - EPS
            && o.right() <= self.right() + EPS
            && o.bottom() <= self.bottom() + EPS
    }
}

const EPS: f32 = 1e-3;

/// Where one copy of one item ended up.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub item: usize,
    pub copy: u32,
    pub page: usize,
    /// The piece itself on the page (gap excluded).
    pub rect: Rect,
    /// Turned by 90° relative to the item's natural orientation.
    pub rotated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Packing {
    /// Page size actually used (orientation applied), `[width, height]`.
    pub page: [f32; 2],
    pub pages: usize,
    pub placements: Vec<Placement>,
    /// Per item: 1.0, or the factor it was shrunk by to fit on a page.
    pub scale: Vec<f32>,
}

impl Packing {
    /// Share of the paper covered by pieces, 0..1.
    pub fn utilisation(&self) -> f32 {
        let used: f32 = self.placements.iter().map(|p| p.rect.w * p.rect.h).sum();
        used / (self.pages.max(1) as f32 * self.page[0] * self.page[1])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Auto,
    Portrait,
    Landscape,
}

pub struct Request<'a> {
    /// Natural `[width, height]` of each item in mm.
    pub sizes: &'a [[f32; 2]],
    pub copies: &'a [u32],
    /// Paper size, `[width, height]`, in portrait.
    pub paper: [f32; 2],
    pub orientation: Orientation,
    pub margin: f32,
    /// Space left between pieces (for cutting).
    pub gap: f32,
    pub allow_rotation: bool,
}

#[derive(Clone, Copy)]
enum Heuristic {
    /// Lowest bottom edge, then leftmost: fills row by row.
    Rows,
    /// Smallest leftover on the shorter side: densest.
    BestShortSide,
}

/// Returns `None` when the margins leave no printable area.
pub fn pack(req: &Request<'_>) -> Option<Packing> {
    let portrait = [
        req.paper[0].min(req.paper[1]),
        req.paper[0].max(req.paper[1]),
    ];
    let landscape = [portrait[1], portrait[0]];
    let pages: &[[f32; 2]] = match req.orientation {
        Orientation::Portrait => &[portrait],
        Orientation::Landscape => &[landscape],
        Orientation::Auto => &[portrait, landscape],
    };
    let mut best: Option<(Packing, f32)> = None;
    for &page in pages {
        for heuristic in [Heuristic::Rows, Heuristic::BestShortSide] {
            let Some(packing) = pack_on(req, page, heuristic) else {
                continue;
            };
            let footprint = footprint(&packing);
            let better = best
                .as_ref()
                .is_none_or(|(b, b_foot)| (packing.pages, footprint) < (b.pages, *b_foot - EPS));
            if better {
                best = Some((packing, footprint));
            }
        }
    }
    best.map(|(p, _)| p)
}

/// Total area of each page's bounding box of pieces: smaller means the
/// leftover paper is one larger, reusable piece.
fn footprint(p: &Packing) -> f32 {
    (0..p.pages)
        .map(|page| {
            let (r, b) = p
                .placements
                .iter()
                .filter(|pl| pl.page == page)
                .fold((0.0f32, 0.0f32), |(r, b), pl| {
                    (r.max(pl.rect.right()), b.max(pl.rect.bottom()))
                });
            r * b
        })
        .sum()
}

fn pack_on(req: &Request<'_>, page: [f32; 2], heuristic: Heuristic) -> Option<Packing> {
    // Each piece reserves its size plus one gap; the area gains one gap so
    // the last piece in a row/column needs none.
    let area = [
        page[0] - 2.0 * req.margin + req.gap,
        page[1] - 2.0 * req.margin + req.gap,
    ];
    if area[0] <= req.gap + EPS || area[1] <= req.gap + EPS {
        return None;
    }
    let printable = [area[0] - req.gap, area[1] - req.gap];

    let scale: Vec<f32> = req
        .sizes
        .iter()
        .map(|&[w, h]| {
            let fit = |pw: f32, ph: f32| (printable[0] / pw).min(printable[1] / ph);
            let mut s = fit(w, h);
            if req.allow_rotation {
                s = s.max(fit(h, w));
            }
            s.min(1.0)
        })
        .collect();

    // Biggest first; copies of the same item stay together so they form grids.
    let mut queue: Vec<(usize, u32)> = (0..req.sizes.len())
        .flat_map(|i| (0..req.copies.get(i).copied().unwrap_or(1)).map(move |c| (i, c)))
        .collect();
    let key = |i: usize| {
        let [w, h] = req.sizes[i].map(|v| v * scale[i]);
        (w.max(h), w.min(h))
    };
    queue.sort_by(|&(a, ca), &(b, cb)| {
        key(b)
            .partial_cmp(&key(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
            .then(ca.cmp(&cb))
    });

    let mut bins: Vec<Bin> = Vec::new();
    let mut placements = Vec::with_capacity(queue.len());
    for (item, copy) in queue {
        let [w, h] = req.sizes[item].map(|v| v * scale[item] + req.gap);
        let found = bins
            .iter()
            .position(|bin| bin.find(w, h, req.allow_rotation, heuristic).is_some());
        let page_index = match found {
            Some(i) => i,
            None => {
                bins.push(Bin::new(area));
                bins.len() - 1
            }
        };
        let bin = &mut bins[page_index];
        // A fresh page always fits: items were scaled to the printable area.
        let (slot, rotated) = bin.find(w, h, req.allow_rotation, heuristic).unwrap_or((
            Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            },
            false,
        ));
        bin.place(slot);
        placements.push(Placement {
            item,
            copy,
            page: page_index,
            rect: Rect {
                x: slot.x + req.margin,
                y: slot.y + req.margin,
                w: slot.w - req.gap,
                h: slot.h - req.gap,
            },
            rotated,
        });
    }
    Some(Packing {
        page,
        pages: bins.len().max(1),
        placements,
        scale,
    })
}

struct Bin {
    free: Vec<Rect>,
}

impl Bin {
    fn new([w, h]: [f32; 2]) -> Self {
        Self {
            free: vec![Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            }],
        }
    }

    /// Best slot for a `w`×`h` piece, and whether it is rotated.
    fn find(
        &self,
        w: f32,
        h: f32,
        allow_rotation: bool,
        heuristic: Heuristic,
    ) -> Option<(Rect, bool)> {
        let orientations: &[(f32, f32, bool)] = if allow_rotation && (w - h).abs() > EPS {
            &[(w, h, false), (h, w, true)]
        } else {
            &[(w, h, false)]
        };
        let mut best: Option<((f32, f32), Rect, bool)> = None;
        for free in &self.free {
            for &(pw, ph, rotated) in orientations {
                if pw > free.w + EPS || ph > free.h + EPS {
                    continue;
                }
                let score = match heuristic {
                    Heuristic::Rows => (free.y + ph, free.x),
                    Heuristic::BestShortSide => {
                        let (dw, dh) = (free.w - pw, free.h - ph);
                        (dw.min(dh), dw.max(dh))
                    }
                };
                if best.as_ref().is_none_or(|(s, _, _)| score < *s) {
                    best = Some((
                        score,
                        Rect {
                            x: free.x,
                            y: free.y,
                            w: pw,
                            h: ph,
                        },
                        rotated,
                    ));
                }
            }
        }
        best.map(|(_, r, rot)| (r, rot))
    }

    fn place(&mut self, used: Rect) {
        let mut next = Vec::with_capacity(self.free.len() + 4);
        for f in self.free.drain(..) {
            if !f.intersects(&used) {
                next.push(f);
                continue;
            }
            // Split the free rectangle into the (overlapping) maximal parts
            // that remain around the used area.
            if used.x > f.x + EPS {
                next.push(Rect {
                    w: used.x - f.x,
                    ..f
                });
            }
            if used.right() < f.right() - EPS {
                next.push(Rect {
                    x: used.right(),
                    w: f.right() - used.right(),
                    ..f
                });
            }
            if used.y > f.y + EPS {
                next.push(Rect {
                    h: used.y - f.y,
                    ..f
                });
            }
            if used.bottom() < f.bottom() - EPS {
                next.push(Rect {
                    y: used.bottom(),
                    h: f.bottom() - used.bottom(),
                    ..f
                });
            }
        }
        // Drop free rectangles contained in another one.
        let mut kept: Vec<Rect> = Vec::with_capacity(next.len());
        for (i, r) in next.iter().enumerate() {
            let redundant = next
                .iter()
                .enumerate()
                .any(|(j, o)| i != j && o.contains(r) && (!r.contains(o) || j < i));
            if !redundant {
                kept.push(*r);
            }
        }
        self.free = kept;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A4: [f32; 2] = [210.0, 297.0];
    const ID_CARD: [f32; 2] = [85.6, 54.0];

    fn request<'a>(sizes: &'a [[f32; 2]], copies: &'a [u32]) -> Request<'a> {
        Request {
            sizes,
            copies,
            paper: A4,
            orientation: Orientation::Auto,
            margin: 10.0,
            gap: 4.0,
            allow_rotation: true,
        }
    }

    fn assert_valid(p: &Packing, req: &Request<'_>) {
        let total: u32 = req.copies.iter().sum();
        assert_eq!(p.placements.len(), total as usize);
        for (i, a) in p.placements.iter().enumerate() {
            // Inside the margins.
            assert!(
                a.rect.x >= req.margin - EPS && a.rect.y >= req.margin - EPS,
                "{a:?}"
            );
            assert!(a.rect.right() <= p.page[0] - req.margin + EPS, "{a:?}");
            assert!(a.rect.bottom() <= p.page[1] - req.margin + EPS, "{a:?}");
            // At least one gap away from every other piece on the page.
            for b in &p.placements[i + 1..] {
                if a.page != b.page {
                    continue;
                }
                let grown = Rect {
                    x: a.rect.x - req.gap + EPS,
                    y: a.rect.y - req.gap + EPS,
                    w: a.rect.w + 2.0 * req.gap - 2.0 * EPS,
                    h: a.rect.h + 2.0 * req.gap - 2.0 * EPS,
                };
                assert!(!grown.intersects(&b.rect), "{a:?} touches {b:?}");
            }
        }
    }

    #[test]
    fn id_card_copies_and_a_document_share_one_page() {
        // Two ID-card copies next to an A6 document fit on one A4 sheet.
        let sizes = [ID_CARD, [105.0, 148.0]];
        let req = request(&sizes, &[2, 1]);
        let p = pack(&req).unwrap();
        assert_valid(&p, &req);
        assert_eq!(p.pages, 1);
    }

    #[test]
    fn many_cards_form_a_dense_grid() {
        // 85.6 × 54 mm cards with 4 mm gaps on A4 (190 × 277 mm printable):
        // 2 × 4 = 8 upright, but 3 × 3 = 9 when turned — the best possible.
        let req = request(&[ID_CARD], &[9]);
        let p = pack(&req).unwrap();
        assert_valid(&p, &req);
        assert_eq!(p.pages, 1, "{p:#?}");
        let req = request(&[ID_CARD], &[10]);
        assert_eq!(pack(&req).unwrap().pages, 2);

        let req = request(&[ID_CARD], &[25]);
        let p = pack(&req).unwrap();
        assert_valid(&p, &req);
        assert!(p.pages <= 3);
    }

    #[test]
    fn overflow_starts_a_new_page_and_fills_earlier_ones_first() {
        let sizes = [[180.0, 200.0], [60.0, 40.0]];
        let req = request(&sizes, &[2, 1]);
        let p = pack(&req).unwrap();
        assert_valid(&p, &req);
        assert_eq!(p.pages, 2);
        // The small piece fits beside the first big one, not on a third page.
        let small = p.placements.iter().find(|pl| pl.item == 1).unwrap();
        assert!(small.page < 2);
    }

    #[test]
    fn oversized_items_are_scaled_to_fit() {
        let req = request(&[[400.0, 300.0]], &[1]);
        let p = pack(&req).unwrap();
        assert_valid(&p, &req);
        // Largest possible: 277 / 400 on the long side, 190 / 300 on the short.
        assert!(
            (p.scale[0] - (277.0f32 / 400.0).min(190.0 / 300.0)).abs() < 1e-4,
            "{}",
            p.scale[0]
        );
    }

    #[test]
    fn fixed_orientation_is_respected() {
        let mut req = request(&[[150.0, 60.0]], &[3]);
        req.orientation = Orientation::Portrait;
        assert_eq!(pack(&req).unwrap().page, A4);
        req.orientation = Orientation::Landscape;
        assert_eq!(pack(&req).unwrap().page, [297.0, 210.0]);
    }

    #[test]
    fn rotation_can_be_disabled() {
        let mut req = request(&[[60.0, 250.0]], &[6]);
        req.orientation = Orientation::Portrait;
        req.allow_rotation = false;
        let p = pack(&req).unwrap();
        assert_valid(&p, &req);
        assert!(p.placements.iter().all(|pl| !pl.rotated));
    }

    #[test]
    fn huge_margins_are_rejected() {
        let mut req = request(&[ID_CARD], &[1]);
        req.margin = 120.0;
        assert!(pack(&req).is_none());
    }

    #[test]
    fn zero_copies_place_nothing() {
        let req = request(&[ID_CARD, [50.0, 50.0]], &[0, 1]);
        let p = pack(&req).unwrap();
        assert_eq!(p.placements.len(), 1);
        assert_eq!(p.placements[0].item, 1);
    }
}
