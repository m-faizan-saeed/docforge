//! Draggable handles drawn over the zoomable preview, for editing points,
//! lines, rectangles and four-corner shapes visually.
//!
//! All shapes use *normalised image coordinates*: `[0, 0]` is the image's
//! top-left corner, `[1, 1]` its bottom-right. That is how the tools store
//! positions, so handles work at any zoom and resolution. Each function
//! returns `true` when the user changed the shape this frame.

use eframe::egui::{
    self, Color32, CursorIcon, Id, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2,
    vec2,
};

/// Where the image is on screen.
#[derive(Clone, Copy, Debug)]
pub struct ImageFrame {
    /// Screen rectangle covered by the whole image.
    pub rect: Rect,
    /// The visible viewport (handles are clipped to it).
    pub clip: Rect,
}

impl ImageFrame {
    pub fn screen(self, [x, y]: [f32; 2]) -> Pos2 {
        self.rect.min + vec2(x * self.rect.width(), y * self.rect.height())
    }

    /// Screen-space movement → normalised movement.
    pub fn delta(self, d: Vec2) -> [f32; 2] {
        [
            d.x / self.rect.width().max(1e-3),
            d.y / self.rect.height().max(1e-3),
        ]
    }
}

/// Smallest size a rectangle may be resized to, as a fraction of the image.
const MIN_SIZE: f32 = 0.005;
const GRIP: f32 = 9.0;

/// The part of a rectangle being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grip {
    Move,
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

impl Grip {
    const ALL_EDGES: [Grip; 8] = [
        Grip::NW,
        Grip::N,
        Grip::NE,
        Grip::E,
        Grip::SE,
        Grip::S,
        Grip::SW,
        Grip::W,
    ];

    /// Position on the rectangle `[x0, y0, x1, y1]`.
    fn anchor(self, [x0, y0, x1, y1]: [f32; 4]) -> [f32; 2] {
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        match self {
            Grip::Move => [cx, cy],
            Grip::N => [cx, y0],
            Grip::S => [cx, y1],
            Grip::E => [x1, cy],
            Grip::W => [x0, cy],
            Grip::NE => [x1, y0],
            Grip::NW => [x0, y0],
            Grip::SE => [x1, y1],
            Grip::SW => [x0, y1],
        }
    }

    fn cursor(self) -> CursorIcon {
        match self {
            Grip::Move => CursorIcon::Grab,
            Grip::N | Grip::S => CursorIcon::ResizeVertical,
            Grip::E | Grip::W => CursorIcon::ResizeHorizontal,
            Grip::NE | Grip::SW => CursorIcon::ResizeNeSw,
            Grip::NW | Grip::SE => CursorIcon::ResizeNwSe,
        }
    }
}

/// The rectangle after dragging `grip` by `d` (normalised), kept inside the
/// image and at least [`MIN_SIZE`] large. `r` must be ordered (x0 ≤ x1).
pub fn resize(r: [f32; 4], grip: Grip, [dx, dy]: [f32; 2]) -> [f32; 4] {
    let [mut x0, mut y0, mut x1, mut y1] = r;
    if grip == Grip::Move {
        // Shift as a whole, without leaving the image.
        let dx = dx.clamp(-x0, 1.0 - x1);
        let dy = dy.clamp(-y0, 1.0 - y1);
        return [x0 + dx, y0 + dy, x1 + dx, y1 + dy];
    }
    if matches!(grip, Grip::W | Grip::NW | Grip::SW) {
        x0 = (x0 + dx).clamp(0.0, x1 - MIN_SIZE);
    }
    if matches!(grip, Grip::E | Grip::NE | Grip::SE) {
        x1 = (x1 + dx).clamp(x0 + MIN_SIZE, 1.0);
    }
    if matches!(grip, Grip::N | Grip::NE | Grip::NW) {
        y0 = (y0 + dy).clamp(0.0, y1 - MIN_SIZE);
    }
    if matches!(grip, Grip::S | Grip::SE | Grip::SW) {
        y1 = (y1 + dy).clamp(y0 + MIN_SIZE, 1.0);
    }
    [x0, y0, x1, y1]
}

/// Orders a rectangle's corners so that x0 ≤ x1 and y0 ≤ y1.
pub fn ordered([a, b, c, d]: [f32; 4]) -> [f32; 4] {
    [a.min(c), b.min(d), a.max(c), b.max(d)]
}

fn clamp_point([x, y]: [f32; 2]) -> [f32; 2] {
    [x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)]
}

/// One square grip; returns its response.
fn grip(ui: &mut Ui, frame: &ImageFrame, id: Id, at: Pos2, cursor: CursorIcon) -> egui::Response {
    let response = ui
        .interact(
            Rect::from_center_size(at, Vec2::splat(GRIP * 1.8)),
            id,
            Sense::drag(),
        )
        .on_hover_cursor(cursor);
    let active = response.hovered() || response.dragged();
    let size = if active { GRIP + 3.0 } else { GRIP };
    let painter = ui.painter_at(frame.clip);
    let r = Rect::from_center_size(at, Vec2::splat(size));
    painter.rect_filled(
        r,
        2.0,
        if active {
            Color32::from_rgb(255, 210, 60)
        } else {
            Color32::WHITE
        },
    );
    painter.rect_stroke(r, 2.0, Stroke::new(1.5, Color32::BLACK), StrokeKind::Middle);
    response
}

/// Outline with a dark halo so it shows on any background.
fn outline(painter: &egui::Painter, points: Vec<Pos2>, closed: bool, color: Color32, dashed: bool) {
    let halo = Stroke::new(3.5, Color32::from_black_alpha(140));
    let line = Stroke::new(1.5, color);
    let mut pts = points;
    if closed && let Some(&first) = pts.first() {
        pts.push(first);
    }
    painter.add(Shape::line(pts.clone(), halo));
    if dashed {
        painter.extend(Shape::dashed_line(&pts, line, 6.0, 4.0));
    } else {
        painter.add(Shape::line(pts, line));
    }
}

/// A single draggable point (e.g. a watermark's centre).
pub fn point(ui: &mut Ui, frame: &ImageFrame, id: Id, p: &mut [f32; 2], color: Color32) -> bool {
    let at = frame.screen(*p);
    let painter = ui.painter_at(frame.clip);
    for (a, b) in [
        (vec2(-14.0, 0.0), vec2(14.0, 0.0)),
        (vec2(0.0, -14.0), vec2(0.0, 14.0)),
    ] {
        painter.line_segment(
            [at + a, at + b],
            Stroke::new(3.5, Color32::from_black_alpha(140)),
        );
        painter.line_segment([at + a, at + b], Stroke::new(1.5, color));
    }
    let r = grip(ui, frame, id, at, CursorIcon::Grab);
    if r.dragged() && r.drag_delta() != Vec2::ZERO {
        let [dx, dy] = frame.delta(r.drag_delta());
        *p = clamp_point([p[0] + dx, p[1] + dy]);
        return true;
    }
    false
}

/// A line with draggable ends and a grip in the middle that moves it whole.
pub fn line(
    ui: &mut Ui,
    frame: &ImageFrame,
    id: Id,
    a: &mut [f32; 2],
    b: &mut [f32; 2],
    color: Color32,
) -> bool {
    let painter = ui.painter_at(frame.clip);
    outline(
        &painter,
        vec![frame.screen(*a), frame.screen(*b)],
        false,
        color,
        false,
    );
    let mut changed = false;
    for (i, end) in [&mut *a, &mut *b].into_iter().enumerate() {
        let r = grip(
            ui,
            frame,
            id.with(i),
            frame.screen(*end),
            CursorIcon::Crosshair,
        );
        if r.dragged() && r.drag_delta() != Vec2::ZERO {
            let [dx, dy] = frame.delta(r.drag_delta());
            *end = clamp_point([end[0] + dx, end[1] + dy]);
            changed = true;
        }
    }
    let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let r = grip(
        ui,
        frame,
        id.with("move"),
        frame.screen(mid),
        CursorIcon::Grab,
    );
    if r.dragged() && r.drag_delta() != Vec2::ZERO {
        let [dx, dy] = frame.delta(r.drag_delta());
        // Keep both ends inside the image.
        let bounds = ordered([a[0], a[1], b[0], b[1]]);
        let moved = resize(bounds, Grip::Move, [dx, dy]);
        let (dx, dy) = (moved[0] - bounds[0], moved[1] - bounds[1]);
        *a = [a[0] + dx, a[1] + dy];
        *b = [b[0] + dx, b[1] + dy];
        changed = true;
    }
    changed
}

/// A rectangle `[x0, y0, x1, y1]` with eight resize grips; dragging inside
/// moves it. With `shade_outside`, everything outside is dimmed (crop view).
pub fn rect(
    ui: &mut Ui,
    frame: &ImageFrame,
    id: Id,
    r: &mut [f32; 4],
    color: Color32,
    shade_outside: bool,
) -> bool {
    let current = ordered(*r);
    let screen = Rect::from_two_pos(
        frame.screen([current[0], current[1]]),
        frame.screen([current[2], current[3]]),
    );
    let painter = ui.painter_at(frame.clip);
    if shade_outside {
        let shade = Color32::from_black_alpha(150);
        let img = frame.rect;
        for band in [
            Rect::from_min_max(img.min, pos2(img.max.x, screen.min.y)),
            Rect::from_min_max(pos2(img.min.x, screen.max.y), img.max),
            Rect::from_min_max(
                pos2(img.min.x, screen.min.y),
                pos2(screen.min.x, screen.max.y),
            ),
            Rect::from_min_max(
                pos2(screen.max.x, screen.min.y),
                pos2(img.max.x, screen.max.y),
            ),
        ] {
            painter.rect_filled(band, 0.0, shade);
        }
    }
    outline(
        &painter,
        vec![
            screen.left_top(),
            screen.right_top(),
            screen.right_bottom(),
            screen.left_bottom(),
        ],
        true,
        color,
        false,
    );

    let mut next = current;
    // The body first, so the grips (registered later) win where they overlap.
    let body = ui
        .interact(screen.shrink(GRIP), id.with("body"), Sense::drag())
        .on_hover_cursor(Grip::Move.cursor());
    if body.dragged() && body.drag_delta() != Vec2::ZERO {
        next = resize(next, Grip::Move, frame.delta(body.drag_delta()));
    }
    for g in Grip::ALL_EDGES {
        let resp = grip(
            ui,
            frame,
            id.with(g as u8),
            frame.screen(g.anchor(current)),
            g.cursor(),
        );
        if resp.dragged() && resp.drag_delta() != Vec2::ZERO {
            next = resize(next, g, frame.delta(resp.drag_delta()));
        }
    }
    if next != current {
        *r = next;
        return true;
    }
    false
}

/// Four corners (top-left, top-right, bottom-right, bottom-left), each
/// draggable. `dashed` marks a suggestion that is not yet in use.
pub fn quad(
    ui: &mut Ui,
    frame: &ImageFrame,
    id: Id,
    q: &mut [[f32; 2]; 4],
    color: Color32,
    dashed: bool,
) -> bool {
    let painter = ui.painter_at(frame.clip);
    outline(
        &painter,
        q.iter().map(|p| frame.screen(*p)).collect(),
        true,
        color,
        dashed,
    );
    let mut changed = false;
    for (i, corner) in q.iter_mut().enumerate() {
        let r = grip(
            ui,
            frame,
            id.with(i),
            frame.screen(*corner),
            CursorIcon::Crosshair,
        );
        if r.dragged() && r.drag_delta() != Vec2::ZERO {
            let [dx, dy] = frame.delta(r.drag_delta());
            *corner = clamp_point([corner[0] + dx, corner[1] + dy]);
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: [f32; 4] = [0.2, 0.3, 0.6, 0.7];

    #[test]
    fn corner_and_edge_grips_move_only_their_sides() {
        assert_close(resize(R, Grip::E, [0.1, 0.5]), [0.2, 0.3, 0.7, 0.7]);
        assert_close(resize(R, Grip::N, [0.5, -0.1]), [0.2, 0.2, 0.6, 0.7]);
        assert_close(resize(R, Grip::SW, [-0.1, 0.1]), [0.1, 0.3, 0.6, 0.8]);
    }

    #[track_caller]
    fn assert_close(a: [f32; 4], b: [f32; 4]) {
        assert!(
            a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5),
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn rectangles_stay_inside_the_image_and_never_invert() {
        // Dragged far past the image edge.
        assert_close(resize(R, Grip::NW, [-5.0, -5.0]), [0.0, 0.0, 0.6, 0.7]);
        // Dragged past the opposite side: stops at the minimum size.
        let r = resize(R, Grip::W, [1.0, 0.0]);
        assert!((r[0] - (0.6 - MIN_SIZE)).abs() < 1e-6 && r[0] < r[2]);
    }

    #[test]
    fn moving_keeps_the_size_and_stops_at_the_border() {
        let r = resize(R, Grip::Move, [0.9, -0.9]);
        assert!((r[2] - 1.0).abs() < 1e-6 && r[1].abs() < 1e-6);
        assert!(((r[2] - r[0]) - 0.4).abs() < 1e-6 && ((r[3] - r[1]) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn frame_maps_normalised_to_screen() {
        let f = ImageFrame {
            rect: Rect::from_min_size(pos2(100.0, 50.0), vec2(400.0, 200.0)),
            clip: Rect::EVERYTHING,
        };
        assert_eq!(f.screen([0.5, 0.25]), pos2(300.0, 100.0));
        assert_eq!(f.delta(vec2(40.0, -20.0)), [0.1, -0.1]);
        assert_eq!(ordered([0.8, 0.9, 0.1, 0.2]), [0.1, 0.2, 0.8, 0.9]);
    }
}

/// Drives the real handle widgets with simulated mouse input in a headless
/// egui context — no window or GPU needed.
#[cfg(test)]
mod interaction_tests {
    use eframe::egui::{Context, Event, Modifiers, PointerButton, RawInput};

    use super::*;

    /// The image occupies (100, 100)–(500, 500) on screen: 1 normalised
    /// unit = 400 px.
    const FRAME: ImageFrame = ImageFrame {
        rect: Rect {
            min: pos2(100.0, 100.0),
            max: pos2(500.0, 500.0),
        },
        clip: Rect {
            min: pos2(0.0, 0.0),
            max: pos2(800.0, 800.0),
        },
    };

    /// Presses at `from`, drags to `to` in a few steps, releases; `draw`
    /// runs every frame like the app's overlay does.
    fn drag(from: Pos2, to: Pos2, mut draw: impl FnMut(&mut Ui)) {
        let ctx = Context::default();
        let button = |pressed| Event::PointerButton {
            pos: if pressed { from } else { to },
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut frames = vec![vec![Event::PointerMoved(from)], vec![button(true)]];
        for i in 1..=4 {
            frames.push(vec![Event::PointerMoved(
                from + (to - from) * (i as f32 / 4.0),
            )]);
        }
        frames.push(vec![button(false)]);
        frames.push(vec![]);
        for events in frames {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 800.0))),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| draw(ui));
            // No renderer here: discard the font atlas uploads.
            output.textures_delta.clear();
        }
    }

    #[track_caller]
    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "{a} != {b}");
    }

    #[test]
    fn dragging_an_edge_grip_resizes_one_side() {
        let mut r = [0.25, 0.25, 0.75, 0.75];
        // East grip at x = 100 + 0.75·400 = 400, y = 300; drag 40 px right.
        drag(pos2(400.0, 300.0), pos2(440.0, 300.0), |ui| {
            rect(ui, &FRAME, Id::unique("r"), &mut r, Color32::WHITE, true);
        });
        close(r[2], 0.85);
        assert_eq!((r[0], r[1], r[3]), (0.25, 0.25, 0.75));
    }

    #[test]
    fn dragging_inside_a_rectangle_moves_it() {
        let mut r = [0.25, 0.25, 0.75, 0.75];
        drag(pos2(260.0, 280.0), pos2(220.0, 240.0), |ui| {
            rect(ui, &FRAME, Id::unique("r"), &mut r, Color32::WHITE, false);
        });
        close(r[0], 0.15);
        close(r[1], 0.15);
        close(r[2] - r[0], 0.5);
    }

    #[test]
    fn quad_corners_points_and_line_ends_follow_the_pointer() {
        let mut q = [[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.1, 0.9]];
        // Bottom-right corner at (460, 460) → (480, 420).
        drag(pos2(460.0, 460.0), pos2(480.0, 420.0), |ui| {
            quad(ui, &FRAME, Id::unique("q"), &mut q, Color32::WHITE, true);
        });
        close(q[2][0], 0.95);
        close(q[2][1], 0.8);
        assert_eq!(q[0], [0.1, 0.1], "other corners unchanged");

        let mut p = [0.5, 0.5];
        drag(pos2(300.0, 300.0), pos2(900.0, 300.0), |ui| {
            point(ui, &FRAME, Id::unique("p"), &mut p, Color32::WHITE);
        });
        assert_eq!(p, [1.0, 0.5], "clamped to the image edge");

        let (mut a, mut b) = ([0.0, 0.5], [1.0, 0.5]);
        // The middle grip moves the whole line down by 0.1.
        drag(pos2(300.0, 300.0), pos2(300.0, 340.0), |ui| {
            line(ui, &FRAME, Id::unique("l"), &mut a, &mut b, Color32::WHITE);
        });
        close(a[1], 0.6);
        close(b[1], 0.6);
    }
}
