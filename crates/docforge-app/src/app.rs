use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use docforge_core::{
    EvalContext, Graph, NodeId, NodeMeta, NodeRegistry, NodeType, PdfDocument, PortRef,
    RasterImage, Value,
};
use docforge_nodes::{
    EnhanceImage, InputImage, OutputImage, PrintLayout, input_image, output_image,
};
use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText, ViewportCommand};

use crate::{
    canvas::{self, CanvasEvent, CanvasState, Status},
    inspector,
    pdf_view::PdfView,
    thumbs::Thumbnails,
    viewport::Viewport,
    worker::{EvalResult, Evaluator, Job, NodeResult},
};

/// How often to re-check input files for changes made outside the app.
const FILE_POLL: Duration = Duration::from_millis(1500);

const SHORTCUT_NEW: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
const SHORTCUT_OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SHORTCUT_SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const SHORTCUT_SAVE_AS: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::S);

pub struct DocforgeApp {
    registry: NodeRegistry,
    /// The document. Never evaluated here; the worker evaluates snapshots.
    graph: Graph,
    project: Option<PathBuf>,
    modified: bool,
    /// Bumped on every change that affects evaluation.
    rev: u64,
    submitted_rev: u64,
    last_submit: Instant,
    evaluator: Evaluator,
    results: Option<EvalResult>,

    selected: Option<NodeId>,
    /// Preview this node regardless of the selection.
    pinned: Option<NodeId>,
    show_input: bool,
    canvas: CanvasState,
    viewport: Viewport,
    pdf_view: PdfView,
    thumbs: Thumbnails,
    message: Option<(String, bool)>,
    confirm: Option<Pending>,
    title: String,
    smoke_test: Option<SmokeTest>,
}

/// `DOCFORGE_SCREENSHOT=out.png`: once evaluation settles, save a screenshot
/// of the window and exit. Used for automated UI checks.
struct SmokeTest {
    path: PathBuf,
    frames: u32,
    requested: bool,
    done: Arc<std::sync::atomic::AtomicBool>,
}

/// An action waiting for "discard unsaved changes?" confirmation.
enum Pending {
    New,
    Open(Option<PathBuf>),
    Quit,
}

impl DocforgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        let registry = docforge_nodes::registry();
        let ctx = cc.egui_ctx.clone();
        let evaluator = Evaluator::spawn(registry.clone(), move || ctx.request_repaint());
        let mut app = Self {
            registry,
            graph: Graph::new(),
            project: None,
            modified: false,
            rev: 1,
            submitted_rev: 0,
            last_submit: Instant::now(),
            evaluator,
            results: None,
            selected: None,
            pinned: None,
            show_input: false,
            canvas: CanvasState::default(),
            viewport: Viewport::default(),
            pdf_view: PdfView::new(cc.egui_ctx.clone()),
            thumbs: Thumbnails::new(cc.egui_ctx.clone()),
            message: None,
            confirm: None,
            title: String::new(),
            smoke_test: std::env::var_os("DOCFORGE_SCREENSHOT").map(|path| SmokeTest {
                path: path.into(),
                frames: 0,
                requested: false,
                done: Arc::default(),
            }),
        };
        app.new_document();
        match open {
            Some(path) if is_project(&path) => app.open_project(&path),
            Some(path) => app.open_image(path),
            None => {}
        }
        if let Some(id) = std::env::var("DOCFORGE_SELECT")
            .ok()
            .and_then(|v| v.parse().ok())
        {
            app.selected = Some(NodeId(id));
        }
        app
    }

    fn run_smoke_test(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        let busy = self.evaluator.is_busy()
            || self.rev != self.submitted_rev
            || self.pdf_view.is_rendering()
            || self.thumbs.is_busy();
        let Some(test) = &mut self.smoke_test else {
            return;
        };
        test.frames += 1;
        if test.done.load(Ordering::SeqCst) {
            self.modified = false;
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        let settled = test.frames > 30 && !busy && self.results.is_some();
        if !test.requested && (settled || test.frames > 1200) {
            test.requested = true;
            let (path, done, repaint) = (test.path.clone(), Arc::clone(&test.done), ctx.clone());
            ctx.request_screenshot(move |image| {
                let rgba = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                let result =
                    RasterImage::from_rgba8(image.size[0] as u32, image.size[1] as u32, rgba)
                        .and_then(|img| OutputImage::new(&path).save(&img, &EvalContext::new()));
                if let Err(err) = result {
                    eprintln!("screenshot failed: {err}");
                }
                done.store(true, Ordering::SeqCst);
                repaint.request_repaint();
            });
        }
        ctx.request_repaint();
    }

    // ----- document lifecycle ------------------------------------------------

    /// A ready-made scan pipeline, so there is something to try immediately.
    fn new_document(&mut self) {
        let mut g = Graph::new();
        let at = |x, y| NodeMeta {
            label: None,
            position: [x, y],
        };
        let input = g.add_node_with_meta(Box::new(InputImage::default()), at(0.0, 40.0));
        let enhance = g.add_node_with_meta(Box::new(EnhanceImage::default()), at(240.0, 40.0));
        let output = g.add_node_with_meta(Box::new(OutputImage::default()), at(480.0, 40.0));
        g.connect(
            input,
            input_image::OUTPUT,
            enhance,
            docforge_nodes::enhance::INPUT,
        )
        .expect("template wiring is valid");
        g.connect(
            enhance,
            docforge_nodes::enhance::OUTPUT,
            output,
            output_image::INPUT,
        )
        .expect("template wiring is valid");
        let print = g.add_node_with_meta(Box::new(PrintLayout::default()), at(480.0, 150.0));
        g.connect(
            enhance,
            docforge_nodes::enhance::OUTPUT,
            print,
            docforge_nodes::print::INPUT,
        )
        .expect("template wiring is valid");
        self.replace_document(g, None);
        self.selected = Some(input);
        self.message = Some((
            "Choose an image in the Input Image node (or drop one on the window) to start.".into(),
            false,
        ));
    }

    fn replace_document(&mut self, graph: Graph, project: Option<PathBuf>) {
        self.graph = graph;
        self.project = project;
        self.modified = false;
        self.selected = None;
        self.pinned = None;
        self.results = None;
        self.viewport.request_fit();
        self.touch();
        self.modified = false;
    }

    fn open_project(&mut self, path: &Path) {
        match Graph::load(path, &self.registry) {
            Ok(graph) => {
                self.replace_document(graph, Some(path.to_owned()));
                self.selected = self.graph.nodes().map(|(id, _)| id).last();
                self.info(format!("Opened {}", path.display()));
            }
            Err(err) => self.error(format!("Could not open {}: {err}", path.display())),
        }
    }

    fn save_project(&mut self, save_as: bool) {
        let path = match (&self.project, save_as) {
            (Some(path), false) => path.clone(),
            _ => {
                let Some(path) = rfd::FileDialog::new()
                    .add_filter("docforge project", &["json"])
                    .set_file_name("project.json")
                    .save_file()
                else {
                    return;
                };
                path
            }
        };
        match self.graph.save(&path) {
            Ok(()) => {
                self.info(format!("Saved {}", path.display()));
                self.project = Some(path);
                self.modified = false;
                self.touch(); // relative paths now resolve against the new folder
                self.modified = false;
            }
            Err(err) => self.error(format!("Could not save: {err}")),
        }
    }

    /// Runs `action` now, or after confirmation if there are unsaved changes.
    fn guarded(&mut self, action: Pending) {
        if self.modified {
            self.confirm = Some(action);
        } else {
            self.run_pending(action);
        }
    }

    fn run_pending(&mut self, action: Pending) {
        match action {
            Pending::New => self.new_document(),
            Pending::Open(Some(path)) => self.open_project(&path),
            Pending::Open(None) => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("docforge project", &["json"])
                    .pick_file()
                {
                    self.open_project(&path);
                }
            }
            Pending::Quit => {
                self.modified = false;
                self.confirm = None;
            }
        }
    }

    /// Puts a dropped/opened image into the graph: into the selected Input
    /// Image node, else the first empty one, else a new node.
    fn open_image(&mut self, path: PathBuf) {
        let is_input = |g: &Graph, id: NodeId| g.node_as::<InputImage>(id).is_some();
        let target = self
            .selected
            .filter(|&id| is_input(&self.graph, id))
            .or_else(|| {
                self.graph.nodes().find_map(|(id, n)| {
                    n.downcast_ref::<InputImage>()
                        .filter(|i| i.path.as_os_str().is_empty())
                        .map(|_| id)
                })
            });
        let id = match target {
            Some(id) => {
                let _ = self
                    .graph
                    .edit_node::<InputImage, _>(id, |n| n.path = path.clone());
                id
            }
            None => self.graph.add_node_with_meta(
                Box::new(InputImage::new(&path)),
                NodeMeta {
                    label: None,
                    position: self.canvas.view_center(),
                },
            ),
        };
        self.selected = Some(id);
        self.pinned = None;
        self.viewport.request_fit();
        self.touch();
        self.info(format!("Loaded {}", path.display()));
    }

    // ----- change tracking and evaluation ------------------------------------

    fn touch(&mut self) {
        self.rev += 1;
        self.modified = true;
    }

    fn base_dir(&self) -> Option<PathBuf> {
        self.project
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
    }

    fn submit_if_needed(&mut self) {
        let stale = self.rev != self.submitted_rev;
        let poll = self.last_submit.elapsed() >= FILE_POLL && !self.evaluator.is_busy();
        if !stale && !poll {
            return;
        }
        match self.graph.to_doc() {
            Ok(doc) => {
                self.evaluator.submit(Job {
                    rev: self.rev,
                    doc,
                    base_dir: self.base_dir(),
                });
                self.submitted_rev = self.rev;
                self.last_submit = Instant::now();
            }
            Err(err) => {
                self.submitted_rev = self.rev;
                self.error(format!("Cannot evaluate: {err}"));
            }
        }
    }

    fn statuses(&self) -> HashMap<NodeId, (Status, Option<String>)> {
        let Some(results) = &self.results else {
            return HashMap::new();
        };
        results
            .nodes
            .iter()
            .map(|(&id, r)| {
                let s = match r {
                    NodeResult::Ready(_) => (Status::Ready, None),
                    NodeResult::Failed(e) => (Status::Failed, Some(e.to_string())),
                    NodeResult::Blocked => {
                        (Status::Blocked, Some("waiting for an upstream node".into()))
                    }
                    NodeResult::Pending => (Status::Pending, None),
                };
                (id, s)
            })
            .collect()
    }

    fn node_image(&self, id: NodeId) -> Option<&Arc<RasterImage>> {
        match self.results.as_ref()?.nodes.get(&id)? {
            NodeResult::Ready(values) => values.iter().find_map(Value::as_image),
            _ => None,
        }
    }

    fn node_document(&self, id: NodeId) -> Option<&Arc<PdfDocument>> {
        match self.results.as_ref()?.nodes.get(&id)? {
            NodeResult::Ready(values) => values.iter().find_map(Value::as_document),
            _ => None,
        }
    }

    /// The image arriving at the node's first connected input.
    fn input_image_of(&self, id: NodeId) -> Option<&Arc<RasterImage>> {
        let edge = self.graph.edges().iter().find(|e| e.to.node == id)?;
        // The value on the exact port the wire leaves from: a node can have
        // several image outputs (e.g. PDF Input's extracted images).
        let index = self
            .graph
            .node(edge.from.node)?
            .outputs()
            .iter()
            .position(|p| p.name == edge.from.port)?;
        match self.results.as_ref()?.nodes.get(&edge.from.node)? {
            NodeResult::Ready(values) => values.get(index)?.as_image(),
            _ => None,
        }
    }

    fn info(&mut self, text: String) {
        self.message = Some((text, false));
    }

    fn error(&mut self, text: String) {
        self.message = Some((text, true));
    }

    // ----- input handling ----------------------------------------------------

    fn handle_global_input(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for path in dropped {
            if is_project(&path) {
                self.guarded(Pending::Open(Some(path)));
            } else {
                self.open_image(path);
            }
        }

        if ctx.input_mut(|i| i.consume_shortcut(&SHORTCUT_SAVE_AS)) {
            self.save_project(true);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&SHORTCUT_SAVE)) {
            self.save_project(false);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&SHORTCUT_OPEN)) {
            self.guarded(Pending::Open(None));
        }
        if ctx.input_mut(|i| i.consume_shortcut(&SHORTCUT_NEW)) {
            self.guarded(Pending::New);
        }
        if !ctx.egui_wants_keyboard_input()
            && let Some(id) = self.selected
            && ctx.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace))
        {
            self.delete_node(id);
        }

        if ctx.input(|i| i.viewport().close_requested()) && self.modified {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.confirm = Some(Pending::Quit);
        }
    }

    fn delete_node(&mut self, id: NodeId) {
        if self.graph.remove_node(id).is_some() {
            if self.selected == Some(id) {
                self.selected = None;
            }
            if self.pinned == Some(id) {
                self.pinned = None;
            }
            self.touch();
        }
    }

    fn apply_canvas_events(&mut self, events: Vec<CanvasEvent>) {
        for event in events {
            match event {
                CanvasEvent::Select(id) => self.selected = id,
                CanvasEvent::Move(id, delta) => {
                    if let Some(meta) = self.graph.meta_mut(id) {
                        meta.position[0] += delta.x;
                        meta.position[1] += delta.y;
                        self.modified = true;
                    }
                }
                CanvasEvent::Connect { from, to } => {
                    match self.graph.connect(from.node, &from.port, to.node, &to.port) {
                        Ok(()) => self.touch(),
                        Err(err) => self.error(format!("Cannot connect: {err}")),
                    }
                }
                CanvasEvent::Disconnect(PortRef { node, port }) => {
                    if self.graph.disconnect(node, &port) > 0 {
                        self.touch();
                    }
                }
                CanvasEvent::Delete(id) => self.delete_node(id),
                CanvasEvent::Add { kind, at } => self.add_node(kind, Some(at)),
            }
        }
    }

    fn add_node(&mut self, kind: &str, at: Option<[f32; 2]>) {
        match self.registry.create(kind) {
            Ok(node) => {
                let position = at.unwrap_or_else(|| self.canvas.view_center());
                let id = self.graph.add_node_with_meta(
                    node,
                    NodeMeta {
                        label: None,
                        position,
                    },
                );
                self.selected = Some(id);
                self.touch();
            }
            Err(err) => self.error(err.to_string()),
        }
    }

    // ----- panels ------------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("New").shortcut_text(ui.ctx().format_shortcut(&SHORTCUT_NEW))).clicked() {
                    self.guarded(Pending::New);
                }
                if ui.add(egui::Button::new("Open project…").shortcut_text(ui.ctx().format_shortcut(&SHORTCUT_OPEN))).clicked() {
                    self.guarded(Pending::Open(None));
                }
                if ui.button("Open image…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg", "webp", "tif", "tiff", "bmp", "gif"])
                        .pick_file()
                {
                    self.open_image(path);
                }
                ui.separator();
                if ui.add(egui::Button::new("Save project").shortcut_text(ui.ctx().format_shortcut(&SHORTCUT_SAVE))).clicked() {
                    self.save_project(false);
                }
                if ui.add(egui::Button::new("Save project as…").shortcut_text(ui.ctx().format_shortcut(&SHORTCUT_SAVE_AS))).clicked() {
                    self.save_project(true);
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
            });
            ui.menu_button("Add", |ui| {
                if let Some(kind) = canvas::add_node_menu(ui, &self.registry) {
                    self.add_node(kind, None);
                }
            });
            ui.menu_button("Help", |ui| {
                ui.label(RichText::new("Canvas").strong());
                ui.label("Drag empty space to pan, scroll to zoom.");
                ui.label("Drag from a port to another to connect.");
                ui.label("Drag a connected input away to detach it;\nright-click an input to disconnect.");
                ui.label("Right-click empty space to add nodes;\nDelete removes the selected node.");
                ui.separator();
                ui.label(RichText::new("Preview").strong());
                ui.label("Scroll to zoom, drag to pan, double-click to fit.");
                ui.separator();
                ui.label("Drop image files or .json projects onto the window.");
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.evaluator.is_busy() {
                ui.spinner();
                ui.label("Evaluating…");
            } else if let Some(r) = &self.results {
                if let Some(err) = &r.sync_error {
                    ui.colored_label(Color32::LIGHT_RED, err);
                } else {
                    let failed = r
                        .nodes
                        .values()
                        .filter(|n| matches!(n, NodeResult::Failed(_)))
                        .count();
                    let text = format!(
                        "{} · {} recomputed in {:.0?}",
                        if failed == 0 {
                            "Up to date".to_owned()
                        } else {
                            format!("{failed} node(s) failed")
                        },
                        r.recomputed,
                        r.elapsed
                    );
                    if failed == 0 {
                        ui.label(text);
                    } else {
                        ui.colored_label(Color32::from_rgb(235, 120, 80), text);
                    }
                    if r.rev != self.rev {
                        ui.label(RichText::new("(before your last edit)").weak());
                    }
                }
            }
            if let Some((text, is_error)) = &self.message {
                ui.separator();
                let color = if *is_error {
                    Color32::LIGHT_RED
                } else {
                    ui.visuals().text_color()
                };
                ui.colored_label(color, text);
            }
        });
    }

    fn inspector_panel(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected.filter(|&id| self.graph.contains(id)) else {
            ui.add_space(8.0);
            ui.label(RichText::new("Select a node to edit its settings.").weak());
            return;
        };
        let kind = self.graph.node(id).map(|n| n.kind()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.heading(canvas::display_name(&self.registry, kind));
            ui.label(RichText::new(id.to_string()).weak());
        });
        ui.horizontal(|ui| {
            ui.label("Label");
            let mut label = self
                .graph
                .meta(id)
                .and_then(|m| m.label.clone())
                .unwrap_or_default();
            if ui.text_edit_singleline(&mut label).changed()
                && let Some(meta) = self.graph.meta_mut(id)
            {
                meta.label = (!label.trim().is_empty()).then_some(label);
                self.modified = true;
            }
        });

        // Result of the last evaluation.
        match self.results.as_ref().and_then(|r| r.nodes.get(&id)) {
            Some(NodeResult::Failed(err)) => {
                egui::Frame::group(ui.style())
                    .fill(Color32::from_rgb(70, 30, 30))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.colored_label(Color32::from_rgb(255, 170, 170), err.to_string());
                    });
            }
            Some(NodeResult::Blocked) => {
                ui.colored_label(
                    Color32::from_rgb(230, 160, 60),
                    "Waiting for an upstream node to succeed.",
                );
            }
            _ => {
                if let Some(img) = self.node_image(id) {
                    let mut facts = format!("Output: {} × {} px", img.width(), img.height());
                    if let Some(dpi) = img.dpi() {
                        facts += &format!(", {dpi:.0} dpi");
                    }
                    ui.label(RichText::new(facts).weak());
                }
            }
        }
        if let Some(path) = self.results.as_ref().and_then(|r| r.written.get(&id)) {
            ui.label(RichText::new(format!("Last saved: {}", path.display())).weak());
        }
        if kind == OutputImage::KIND {
            self.save_now_button(ui, id);
        }
        if kind == PrintLayout::KIND {
            self.save_pdf_now_button(ui, id);
        }
        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let extras = inspector::Extras {
                    pdf: self.node_document(id).cloned(),
                    thumbs: &mut self.thumbs,
                };
                if inspector::node_params(ui, &mut self.graph, id, &self.registry, extras) {
                    self.touch();
                }
            });
    }

    fn save_now_button(&mut self, ui: &mut egui::Ui, id: NodeId) {
        let image = self
            .node_image(id)
            .or_else(|| self.input_image_of(id))
            .cloned();
        let clicked = ui
            .add_enabled(image.is_some(), egui::Button::new("💾 Save now"))
            .on_disabled_hover_text("Nothing to save yet")
            .clicked();
        if let (true, Some(image)) = (clicked, image) {
            let node = self.graph.node_as::<OutputImage>(id).expect("checked kind");
            let mut ctx = EvalContext::new();
            if let Some(dir) = self.base_dir() {
                ctx = ctx.with_base_dir(dir);
            }
            match node.save(&image, &ctx) {
                Ok(path) => self.info(format!("Saved {}", path.display())),
                Err(err) => self.error(format!("Save failed: {err}")),
            }
        }
    }

    fn save_pdf_now_button(&mut self, ui: &mut egui::Ui, id: NodeId) {
        let doc = self.node_document(id).cloned();
        let clicked = ui
            .add_enabled(doc.is_some(), egui::Button::new("💾 Save PDF now"))
            .on_disabled_hover_text("No PDF yet")
            .clicked();
        if let (true, Some(doc)) = (clicked, doc) {
            let node = self.graph.node_as::<PrintLayout>(id).expect("checked kind");
            let mut ctx = EvalContext::new();
            if let Some(dir) = self.base_dir() {
                ctx = ctx.with_base_dir(dir);
            }
            match node.save(&doc, &ctx) {
                Ok(path) => self.info(format!("Saved {}", path.display())),
                Err(err) => self.error(format!("Save failed: {err}")),
            }
        }
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        let target = self
            .pinned
            .or(self.selected)
            .filter(|&id| self.graph.contains(id));
        ui.horizontal(|ui| {
            let name = target.map_or("Preview".to_owned(), |id| {
                let kind = self.graph.node(id).map(|n| n.kind()).unwrap_or_default();
                let label = self.graph.meta(id).and_then(|m| m.label.clone());
                format!(
                    "Preview: {}",
                    label.unwrap_or_else(|| canvas::display_name(&self.registry, kind).to_owned())
                )
            });
            ui.strong(name);
            ui.separator();
            ui.selectable_value(&mut self.show_input, false, "Output");
            ui.selectable_value(&mut self.show_input, true, "Input")
                .on_hover_text("Show what arrives at this node, for before/after comparison");
            ui.separator();
            let mut pinned = self.pinned.is_some();
            if ui
                .toggle_value(&mut pinned, "📌 Pin")
                .on_hover_text("Keep previewing this node while selecting others")
                .changed()
            {
                self.pinned = if pinned { target } else { None };
            }
        });
        ui.separator();

        if !self.show_input
            && let Some(doc) = target.and_then(|id| self.node_document(id)).cloned()
        {
            self.pdf_view.show(ui, &doc);
            return;
        }
        let image = target.and_then(|id| {
            if self.show_input {
                self.input_image_of(id)
            } else {
                self.node_image(id)
            }
        });
        let placeholder = match (
            target,
            self.results
                .as_ref()
                .and_then(|r| target.and_then(|t| r.nodes.get(&t))),
        ) {
            (None, _) => "Select a node to preview its output".to_owned(),
            (Some(_), Some(NodeResult::Failed(err))) => format!("This node failed:\n{err}"),
            (Some(_), Some(NodeResult::Blocked)) => "Waiting for an upstream node".to_owned(),
            (Some(_), _) if self.evaluator.is_busy() => "Working…".to_owned(),
            (Some(_), _) => "No image here yet".to_owned(),
        };
        self.viewport
            .show(ui, image.cloned().as_ref(), &placeholder);
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.confirm else { return };
        let what = match pending {
            Pending::New => "start a new project",
            Pending::Open(_) => "open another project",
            Pending::Quit => "quit",
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::unique("confirm_discard")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading("Unsaved changes");
            ui.label(format!("Discard your changes and {what}?"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Discard").clicked() {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(false);
                }
            });
        });
        match choice {
            Some(true) => {
                let action = self.confirm.take().expect("checked above");
                let quit = matches!(action, Pending::Quit);
                self.modified = false;
                self.run_pending(action);
                if quit {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let name = self
            .project
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("Untitled".to_owned(), |n| n.to_string_lossy().into_owned());
        let title = format!("{}{name} — docforge", if self.modified { "● " } else { "" });
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for DocforgeApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.smoke_test.is_some() {
            // The smoke test must also run when the window is not on screen.
            ctx.send_viewport_cmd(ViewportCommand::RequestPaintWhileHidden);
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(result) = self.evaluator.poll() {
            self.results = Some(result);
        }
        self.handle_global_input(&ctx);

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(360.0)
            .min_size(260.0)
            .show(ui, |ui| self.inspector_panel(ui));
        egui::Panel::bottom("canvas")
            .resizable(true)
            .default_size(300.0)
            .min_size(140.0)
            .show(ui, |ui| {
                let statuses = self.statuses();
                let events =
                    self.canvas
                        .show(ui, &self.graph, &self.registry, &statuses, self.selected);
                self.apply_canvas_events(events);
            });
        egui::CentralPanel::default().show(ui, |ui| self.preview_panel(ui));

        self.confirm_dialog(&ctx);
        self.submit_if_needed();
        self.update_title(&ctx);
        self.run_smoke_test(&ctx);
        // Keep polling input files for outside changes.
        ctx.request_repaint_after(FILE_POLL);
    }
}

fn is_project(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
}
