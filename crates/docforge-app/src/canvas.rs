//! The node-graph canvas.
//!
//! It only reads the graph and reports what the user did as
//! [`CanvasEvent`]s; the app applies them. That keeps all graph mutation
//! (and change tracking) in one place.

use std::collections::HashMap;

use docforge_core::{Edge, Graph, NodeId, NodeRegistry, PortRef, PortSpec};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    Vec2, epaint::CubicBezierShape, pos2, vec2,
};

const NODE_W: f32 = 180.0;
const TITLE_H: f32 = 26.0;
const ROW_H: f32 = 22.0;
const PORT_R: f32 = 6.0;
const PORT_HIT: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Ready,
    Failed,
    Blocked,
}

pub enum CanvasEvent {
    Select(Option<NodeId>),
    Move(NodeId, Vec2),
    Connect { from: PortRef, to: PortRef },
    Disconnect(PortRef),
    Delete(NodeId),
    Add { kind: &'static str, at: [f32; 2] },
}

pub struct CanvasState {
    pan: Vec2,
    zoom: f32,
    wire: Option<WireDrag>,
    /// World position where the context menu was opened.
    menu_at: [f32; 2],
    /// Last known canvas rect, so the app can place new nodes in view.
    view: Rect,
}

enum WireDrag {
    /// Dragging from an output, looking for an input.
    FromOutput(PortRef),
    /// Dragging from an input, looking for an output.
    FromInput(PortRef),
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            pan: vec2(40.0, 40.0),
            zoom: 1.0,
            wire: None,
            menu_at: [0.0, 0.0],
            view: Rect::NOTHING,
        }
    }
}

struct Layout {
    origin: Pos2,
    pan: Vec2,
    zoom: f32,
}

impl Layout {
    fn to_screen(&self, world: Pos2) -> Pos2 {
        self.origin + self.pan + world.to_vec2() * self.zoom
    }

    fn to_world(&self, screen: Pos2) -> Pos2 {
        ((screen - self.origin - self.pan) / self.zoom).to_pos2()
    }

    fn node_rect(&self, position: [f32; 2], inputs: usize, outputs: usize) -> Rect {
        let rows = inputs.max(outputs).max(1) as f32;
        let size = vec2(NODE_W, TITLE_H + rows * ROW_H + 6.0) * self.zoom;
        Rect::from_min_size(self.to_screen(position.into()), size)
    }

    fn port_pos(&self, rect: Rect, index: usize, output: bool) -> Pos2 {
        let y = rect.top() + (TITLE_H + (index as f32 + 0.5) * ROW_H) * self.zoom;
        pos2(if output { rect.right() } else { rect.left() }, y)
    }
}

struct NodeView {
    id: NodeId,
    rect: Rect,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
}

impl NodeView {
    fn port(&self, layout: &Layout, name: &str, output: bool) -> Option<Pos2> {
        let ports = if output { &self.outputs } else { &self.inputs };
        let index = ports.iter().position(|p| p.name == name)?;
        Some(layout.port_pos(self.rect, index, output))
    }
}

impl CanvasState {
    /// Centre of the visible canvas in world coordinates.
    pub fn view_center(&self) -> [f32; 2] {
        if !self.view.is_positive() {
            return [100.0, 100.0];
        }
        let layout = self.layout(self.view);
        let c = layout.to_world(self.view.center());
        [c.x - NODE_W / 2.0, c.y - 40.0]
    }

    fn layout(&self, rect: Rect) -> Layout {
        Layout {
            origin: rect.min,
            pan: self.pan,
            zoom: self.zoom,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        graph: &Graph,
        registry: &NodeRegistry,
        statuses: &HashMap<NodeId, (Status, Option<String>)>,
        selected: Option<NodeId>,
    ) -> Vec<CanvasEvent> {
        let mut events = Vec::new();
        let (rect, bg) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.view = rect;
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals().clone();

        // --- background: pan, zoom, deselect, context menu ---
        if bg.dragged() {
            self.pan += bg.drag_delta();
        }
        if bg.clicked() {
            events.push(CanvasEvent::Select(None));
        }
        if let Some(pointer) = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| rect.contains(*p))
        {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y + (i.zoom_delta() - 1.0) * 200.0);
            if scroll != 0.0 {
                let new_zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.3, 2.5);
                let world = self.layout(rect).to_world(pointer);
                self.pan = pointer - rect.min - world.to_vec2() * new_zoom;
                self.zoom = new_zoom;
            }
        }
        if bg.secondary_clicked()
            && let Some(p) = bg.interact_pointer_pos()
        {
            let w = self.layout(rect).to_world(p);
            self.menu_at = [w.x, w.y];
        }
        bg.context_menu(|ui| {
            ui.label("Add node");
            ui.separator();
            if let Some(kind) = add_node_menu(ui, registry) {
                events.push(CanvasEvent::Add {
                    kind,
                    at: self.menu_at,
                });
            }
        });

        let layout = self.layout(rect);
        draw_grid(&painter, rect, &layout, &visuals);

        let views: Vec<NodeView> = graph
            .nodes()
            .map(|(id, node)| {
                let inputs = node.inputs().into_owned();
                let outputs = node.outputs().into_owned();
                let pos = graph.meta(id).map_or([0.0, 0.0], |m| m.position);
                NodeView {
                    id,
                    rect: layout.node_rect(pos, inputs.len(), outputs.len()),
                    inputs,
                    outputs,
                }
            })
            .collect();
        let view_of = |id: NodeId| views.iter().find(|v| v.id == id);

        // --- wires ---
        let wire_color = Color32::from_rgb(110, 170, 230);
        for Edge { from, to } in graph.edges() {
            let (Some(a), Some(b)) = (
                view_of(from.node).and_then(|v| v.port(&layout, &from.port, true)),
                view_of(to.node).and_then(|v| v.port(&layout, &to.port, false)),
            ) else {
                continue;
            };
            painter.add(wire(a, b, layout.zoom, wire_color));
        }

        // --- nodes ---
        let font = FontId::proportional(13.0 * layout.zoom);
        let small = FontId::proportional(11.5 * layout.zoom);
        for view in &views {
            let id = view.id;
            let node = graph.node(id).expect("listed above");
            let (status, error) = statuses
                .get(&id)
                .cloned()
                .unwrap_or((Status::Pending, None));
            let response = ui
                .interact(
                    view.rect,
                    ui.scope_id().with(("node", id.0)),
                    Sense::click_and_drag(),
                )
                .on_hover_ui(|ui| {
                    ui.label(format!("{} {}", display_name(registry, node.kind()), id));
                    if let Some(err) = &error {
                        ui.colored_label(Color32::LIGHT_RED, err);
                    }
                });
            if response.drag_started() || response.clicked() {
                events.push(CanvasEvent::Select(Some(id)));
            }
            if response.dragged() {
                events.push(CanvasEvent::Move(id, response.drag_delta() / layout.zoom));
            }
            response.context_menu(|ui| {
                if ui.button("Delete node").clicked() {
                    events.push(CanvasEvent::Delete(id));
                    ui.close();
                }
            });

            let radius = CornerRadius::same((6.0 * layout.zoom) as u8);
            painter.rect_filled(view.rect, radius, visuals.widgets.noninteractive.bg_fill);
            let title_rect = Rect::from_min_size(
                view.rect.min,
                vec2(view.rect.width(), TITLE_H * layout.zoom),
            );
            painter.rect_filled(
                title_rect,
                CornerRadius {
                    sw: 0,
                    se: 0,
                    ..radius
                },
                category_color(registry, node.kind()),
            );
            let title = graph
                .meta(id)
                .and_then(|m| m.label.clone())
                .unwrap_or_else(|| display_name(registry, node.kind()).to_owned());
            painter.text(
                title_rect.left_center() + vec2(10.0 * layout.zoom, 0.0),
                Align2::LEFT_CENTER,
                title,
                font.clone(),
                Color32::WHITE,
            );
            painter.circle_filled(
                title_rect.right_center() - vec2(12.0 * layout.zoom, 0.0),
                4.5 * layout.zoom,
                status_color(status),
            );
            let outline = if selected == Some(id) {
                Stroke::new(2.0, visuals.selection.stroke.color)
            } else {
                Stroke::new(1.0, status_color(status).gamma_multiply(0.6))
            };
            painter.rect_stroke(view.rect, radius, outline, StrokeKind::Outside);

            // Ports.
            for (output, ports) in [(false, &view.inputs), (true, &view.outputs)] {
                for (i, spec) in ports.iter().enumerate() {
                    let pos = layout.port_pos(view.rect, i, output);
                    let port = PortRef::new(id, spec.name.as_ref());
                    let connected = graph
                        .edges()
                        .iter()
                        .any(|e| if output { e.from == port } else { e.to == port });
                    painter.circle(
                        pos,
                        PORT_R * layout.zoom,
                        if connected {
                            wire_color
                        } else {
                            visuals.extreme_bg_color
                        },
                        Stroke::new(1.5, wire_color),
                    );
                    let (anchor, offset) = if output {
                        (Align2::RIGHT_CENTER, -12.0)
                    } else {
                        (Align2::LEFT_CENTER, 12.0)
                    };
                    let mut label = spec.name.to_string();
                    if !output && !spec.required {
                        label += " (optional)";
                    }
                    painter.text(
                        pos + vec2(offset * layout.zoom, 0.0),
                        anchor,
                        label,
                        small.clone(),
                        visuals.text_color(),
                    );

                    let hit = Rect::from_center_size(
                        pos,
                        Vec2::splat(PORT_HIT * 2.0 * layout.zoom.max(0.6)),
                    );
                    let port_id = ui.scope_id().with(("port", id.0, output, i));
                    let r = ui.interact(hit, port_id, Sense::click_and_drag());
                    if r.drag_started() {
                        self.wire = Some(if output {
                            WireDrag::FromOutput(port.clone())
                        } else if let Some(edge) = graph.edges().iter().find(|e| e.to == port) {
                            // Pick up the existing wire by its loose end.
                            events.push(CanvasEvent::Disconnect(port.clone()));
                            WireDrag::FromOutput(edge.from.clone())
                        } else {
                            WireDrag::FromInput(port.clone())
                        });
                    }
                    if !output && r.secondary_clicked() {
                        events.push(CanvasEvent::Disconnect(port));
                    }
                }
            }
        }

        // --- wire being dragged ---
        if let Some(drag) = &self.wire {
            let pointer = ui.input(|i| i.pointer.interact_pos());
            let anchor = match drag {
                WireDrag::FromOutput(p) => {
                    view_of(p.node).and_then(|v| v.port(&layout, &p.port, true))
                }
                WireDrag::FromInput(p) => {
                    view_of(p.node).and_then(|v| v.port(&layout, &p.port, false))
                }
            };
            if let (Some(anchor), Some(pointer)) = (anchor, pointer) {
                let (a, b) = match drag {
                    WireDrag::FromOutput(_) => (anchor, pointer),
                    WireDrag::FromInput(_) => (pointer, anchor),
                };
                painter.add(wire(a, b, layout.zoom, wire_color.gamma_multiply(0.7)));
            }
            if ui.input(|i| i.pointer.any_released()) {
                let target = pointer.and_then(|p| {
                    let want_output = matches!(drag, WireDrag::FromInput(_));
                    views.iter().find_map(|v| {
                        let ports = if want_output { &v.outputs } else { &v.inputs };
                        ports.iter().enumerate().find_map(|(i, spec)| {
                            let pos = layout.port_pos(v.rect, i, want_output);
                            (pos.distance(p) <= PORT_HIT * layout.zoom.max(0.6))
                                .then(|| PortRef::new(v.id, spec.name.as_ref()))
                        })
                    })
                });
                if let Some(target) = target {
                    events.push(match drag {
                        WireDrag::FromOutput(from) => CanvasEvent::Connect {
                            from: from.clone(),
                            to: target,
                        },
                        WireDrag::FromInput(to) => CanvasEvent::Connect {
                            from: target,
                            to: to.clone(),
                        },
                    });
                }
                self.wire = None;
            }
        }

        if graph.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Right-click to add a node",
                FontId::proportional(15.0),
                visuals.weak_text_color(),
            );
        }
        events
    }
}

/// Node kinds grouped by category. Group kinds are hidden: groups cannot be
/// edited in this version of the canvas.
pub fn add_node_menu(ui: &mut egui::Ui, registry: &NodeRegistry) -> Option<&'static str> {
    let categories: std::collections::BTreeSet<&str> = registry
        .iter()
        .map(|d| d.category)
        .filter(|c| *c != "Group")
        .collect();
    let mut chosen = None;
    for category in categories {
        ui.label(egui::RichText::new(category).weak().small());
        for d in registry.iter().filter(|d| d.category == category) {
            if ui.button(d.display_name).clicked() {
                chosen = Some(d.kind);
                ui.close();
            }
        }
    }
    chosen
}

pub fn display_name<'a>(registry: &'a NodeRegistry, kind: &'a str) -> &'a str {
    registry.get(kind).map_or(kind, |d| d.display_name)
}

fn category_color(registry: &NodeRegistry, kind: &str) -> Color32 {
    match registry.get(kind).map(|d| d.category) {
        Some("Input / Output") => Color32::from_rgb(52, 110, 84),
        Some("Processing") => Color32::from_rgb(70, 82, 150),
        Some("Group") => Color32::from_rgb(120, 90, 50),
        _ => Color32::from_rgb(90, 90, 90),
    }
}

fn status_color(status: Status) -> Color32 {
    match status {
        Status::Ready => Color32::from_rgb(90, 200, 120),
        Status::Failed => Color32::from_rgb(235, 80, 80),
        Status::Blocked => Color32::from_rgb(230, 160, 60),
        Status::Pending => Color32::GRAY,
    }
}

fn wire(a: Pos2, b: Pos2, zoom: f32, color: Color32) -> Shape {
    let bend = ((b.x - a.x).abs() * 0.5).max(40.0 * zoom);
    CubicBezierShape::from_points_stroke(
        [a, a + vec2(bend, 0.0), b - vec2(bend, 0.0), b],
        false,
        Color32::TRANSPARENT,
        Stroke::new(2.5 * zoom.max(0.5), color),
    )
    .into()
}

fn draw_grid(painter: &egui::Painter, rect: Rect, layout: &Layout, visuals: &egui::Visuals) {
    painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
    let step = 32.0 * layout.zoom;
    if step < 8.0 {
        return;
    }
    let color = visuals.weak_text_color().gamma_multiply(0.15);
    let offset = layout.pan;
    let mut x = rect.left() + offset.x.rem_euclid(step);
    while x < rect.right() {
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, color),
        );
        x += step;
    }
    let mut y = rect.top() + offset.y.rem_euclid(step);
    while y < rect.bottom() {
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, color),
        );
        y += step;
    }
}
