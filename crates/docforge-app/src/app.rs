use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use docforge_core::{
    EvalContext, Graph, NodeId, NodeMeta, NodeRegistry, NodeType, PdfDocument, PortRef,
    RasterImage, Value,
};
use docforge_nodes::{
    EnhanceImage, InputImage, OutputImage, PdfEdit, PdfOverlay, PrintLayout,
    enhance::{step_input_key, step_output_key},
    input_image, output_image,
    pdf::{self as pdfkit, PageBox},
    pdf_edit::{CropMode, PaletteEntry},
    pdf_overlay,
};
use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText, ViewportCommand};

use crate::{
    bounds,
    canvas::{self, CanvasEvent, CanvasState, Status},
    handles, inspector,
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
const SHORTCUT_DUPLICATE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::D);
const SHORTCUT_SELECT_ALL: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::A);

/// Marks clipboard text produced by docforge (see [`DocforgeApp::copy_selection`]).
const CLIPBOARD_TAG: &str = "docforge-nodes";
/// Where duplicates appear relative to their originals, in canvas units.
const DUPLICATE_OFFSET: [f32; 2] = [40.0, 40.0];

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

    /// The primary selection: shown in the inspector and the preview.
    selected: Option<NodeId>,
    /// Every selected node (includes `selected`).
    selection: BTreeSet<NodeId>,
    /// An Enhance step whose geometry is being edited on the preview.
    edit_step: Option<(NodeId, usize)>,
    /// Page detected in the image shown for "Crop to document" editing.
    detected: Option<DetectedPage>,
    /// Probe requests sent with the last job.
    submitted_probes: Vec<(NodeId, String)>,
    /// Colours of the last PDF shown in a PDF Edit inspector.
    palette: Option<PaletteCache>,
    /// Preview this node regardless of the selection.
    pinned: Option<NodeId>,
    show_input: bool,
    /// The preview's edit mode last frame (node and step), to refit the
    /// view when entering or leaving it.
    preview_edit: Option<(NodeId, usize)>,
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

/// An image and the page found in it (normalised corners), if any.
type DetectedPage = (Arc<RasterImage>, Option<[[f32; 2]; 4]>);

/// A document and the vector colours found in it.
type PaletteCache = (Arc<PdfDocument>, Arc<Vec<PaletteEntry>>);

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
            selection: BTreeSet::new(),
            edit_step: None,
            detected: None,
            submitted_probes: Vec::new(),
            palette: None,
            pinned: None,
            show_input: false,
            preview_edit: None,
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
        // Test hooks (see the README's "Automated GUI screenshots").
        if let Ok(list) = std::env::var("DOCFORGE_SELECT") {
            let ids: Vec<NodeId> = list
                .split(',')
                .filter_map(|v| v.trim().parse().ok())
                .map(NodeId)
                .collect();
            app.select(ids.first().copied());
            app.selection.extend(ids);
        }
        if std::env::var_os("DOCFORGE_DUPLICATE").is_some() {
            app.duplicate_selection();
        }
        if let (Some(id), Some(step)) = (
            app.selected,
            std::env::var("DOCFORGE_EDIT_STEP")
                .ok()
                .and_then(|v| v.parse().ok()),
        ) {
            app.edit_step = Some((id, step));
        }
        app
    }

    /// Selects only `id` (or nothing).
    fn select(&mut self, id: Option<NodeId>) {
        self.selected = id;
        self.selection = id.into_iter().collect();
        if self.edit_step.is_some_and(|(node, _)| Some(node) != id) {
            self.edit_step = None;
        }
    }

    /// Selected nodes that still exist, in id order.
    fn selection_ids(&self) -> Vec<NodeId> {
        self.selection
            .iter()
            .copied()
            .filter(|id| self.graph.contains(*id))
            .collect()
    }

    fn run_smoke_test(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        let busy = self.evaluator.is_busy()
            || self.rev != self.submitted_rev
            || self.pdf_view.is_rendering()
            || self.thumbs.is_busy()
            || self.edit_image_pending();
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
        self.select(Some(input));
        self.message = Some((
            "Choose an image in the Input Image node (or drop one on the window) to start.".into(),
            false,
        ));
    }

    fn replace_document(&mut self, graph: Graph, project: Option<PathBuf>) {
        self.graph = graph;
        self.project = project;
        self.modified = false;
        self.select(None);
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
                let last = self.graph.nodes().map(|(id, _)| id).last();
                self.select(last);
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
        self.select(Some(id));
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

    /// Intermediate images the preview needs (while editing a step's geometry).
    fn wanted_probes(&self) -> Vec<(NodeId, String)> {
        match self.edit_step {
            Some((node, step)) if self.graph.node_as::<EnhanceImage>(node).is_some() => {
                vec![(node, step_input_key(step)), (node, step_output_key(step))]
            }
            _ => Vec::new(),
        }
    }

    fn submit_if_needed(&mut self) {
        let probes = self.wanted_probes();
        let stale = self.rev != self.submitted_rev || probes != self.submitted_probes;
        let poll = self.last_submit.elapsed() >= FILE_POLL && !self.evaluator.is_busy();
        if !stale && !poll {
            return;
        }
        self.submitted_probes = probes.clone();
        match self.graph.to_doc() {
            Ok(doc) => {
                self.evaluator.submit(Job {
                    rev: self.rev,
                    doc,
                    base_dir: self.base_dir(),
                    probes,
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

    /// The document arriving at the node's first connected input.
    fn input_document_of(&self, id: NodeId) -> Option<&Arc<PdfDocument>> {
        let edge = self.graph.edges().iter().find(|e| e.to.node == id)?;
        let index = self
            .graph
            .node(edge.from.node)?
            .outputs()
            .iter()
            .position(|p| p.name == edge.from.port)?;
        match self.results.as_ref()?.nodes.get(&edge.from.node)? {
            NodeResult::Ready(values) => values.get(index)?.as_document(),
            _ => None,
        }
    }

    /// The documents arriving at `port` of the node, in connection order;
    /// `None` for inputs that are not ready.
    fn input_documents_of(&self, id: NodeId, port: &str) -> Vec<Option<Arc<PdfDocument>>> {
        self.graph
            .edges()
            .iter()
            .filter(|e| e.to.node == id && e.to.port == port)
            .map(|edge| {
                let index = self
                    .graph
                    .node(edge.from.node)?
                    .outputs()
                    .iter()
                    .position(|p| p.name == edge.from.port)?;
                match self.results.as_ref()?.nodes.get(&edge.from.node)? {
                    NodeResult::Ready(values) => values.get(index)?.as_document().cloned(),
                    _ => None,
                }
            })
            .collect()
    }

    /// The vector colours of `doc`, computed once per document.
    fn palette_of(&mut self, doc: &Arc<PdfDocument>) -> Arc<Vec<PaletteEntry>> {
        if let Some((seen, colours)) = &self.palette
            && Arc::ptr_eq(seen, doc)
        {
            return Arc::clone(colours);
        }
        let colours = Arc::new(docforge_nodes::pdf_edit::palette(&doc.bytes));
        self.palette = Some((Arc::clone(doc), Arc::clone(&colours)));
        colours
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
        if !ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
                self.delete_selection();
            }
            if ctx.input_mut(|i| i.consume_shortcut(&SHORTCUT_DUPLICATE)) {
                self.duplicate_selection();
            }
            if ctx.input_mut(|i| i.consume_shortcut(&SHORTCUT_SELECT_ALL)) {
                self.selection = self.graph.nodes().map(|(id, _)| id).collect();
                if self.selected.is_none() {
                    self.selected = self.selection.first().copied();
                }
            }
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.edit_step = None;
            }
            // Ctrl+C / Ctrl+X / Ctrl+V arrive as clipboard events.
            for event in ctx.input(|i| i.events.clone()) {
                match event {
                    egui::Event::Copy => self.copy_selection(ctx),
                    egui::Event::Cut => {
                        self.copy_selection(ctx);
                        self.delete_selection();
                    }
                    egui::Event::Paste(text) => self.paste(&text),
                    _ => {}
                }
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) && self.modified {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.confirm = Some(Pending::Quit);
        }
    }

    fn delete_node(&mut self, id: NodeId) {
        if self.graph.remove_node(id).is_some() {
            self.selection.remove(&id);
            if self.selected == Some(id) {
                self.selected = self.selection.first().copied();
            }
            if self.pinned == Some(id) {
                self.pinned = None;
            }
            if self.edit_step.is_some_and(|(node, _)| node == id) {
                self.edit_step = None;
            }
            self.touch();
        }
    }

    fn delete_selection(&mut self) {
        for id in self.selection_ids() {
            self.delete_node(id);
        }
    }

    /// Duplicates the selection (with the wires among the selected nodes and
    /// the wires feeding them) and selects the copies.
    fn duplicate_selection(&mut self) {
        let ids = self.selection_ids();
        if ids.is_empty() {
            return;
        }
        match self
            .graph
            .duplicate(&ids, &self.registry, DUPLICATE_OFFSET, true)
        {
            Ok(copies) => {
                let primary = self
                    .selected
                    .and_then(|p| ids.iter().position(|&id| id == p))
                    .and_then(|i| copies.get(i).copied());
                self.select(primary.or(copies.first().copied()));
                self.selection = copies.iter().copied().collect();
                self.info(format!("Duplicated {} node(s).", copies.len()));
                self.touch();
            }
            Err(err) => self.error(format!("Cannot duplicate: {err}")),
        }
    }

    /// Puts the selection on the system clipboard as JSON, so it can be
    /// pasted into this or another project (or another docforge window).
    fn copy_selection(&mut self, ctx: &egui::Context) {
        let ids = self.selection_ids();
        if ids.is_empty() {
            return;
        }
        let text = self.graph.copy_nodes(&ids).and_then(|doc| {
            Ok(serde_json::to_string_pretty(&serde_json::json!({
                "format": CLIPBOARD_TAG,
                "graph": doc,
            }))?)
        });
        match text {
            Ok(text) => {
                ctx.copy_text(text);
                self.info(format!("Copied {} node(s).", ids.len()));
            }
            Err(err) => self.error(format!("Cannot copy: {err}")),
        }
    }

    /// Pastes nodes copied by docforge; other clipboard text is ignored.
    fn paste(&mut self, text: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        if value.get("format").and_then(|f| f.as_str()) != Some(CLIPBOARD_TAG) {
            return;
        }
        let doc: docforge_core::GraphDoc = match serde_json::from_value(value["graph"].clone()) {
            Ok(doc) => doc,
            Err(err) => return self.error(format!("Cannot paste: {err}")),
        };
        // Place the pasted group's top-left corner at the centre of the view.
        let min = doc.nodes.iter().fold([f32::MAX, f32::MAX], |m, n| {
            [m[0].min(n.meta.position[0]), m[1].min(n.meta.position[1])]
        });
        let target = self.canvas.view_center();
        let offset = if doc.nodes.is_empty() {
            [0.0, 0.0]
        } else {
            [target[0] - min[0], target[1] - min[1]]
        };
        match self.graph.paste(&doc, &self.registry, offset) {
            Ok(map) => {
                let new: Vec<NodeId> = map.iter().map(|(_, n)| *n).collect();
                self.select(new.first().copied());
                self.selection = new.iter().copied().collect();
                self.info(format!("Pasted {} node(s).", new.len()));
                self.touch();
            }
            Err(err) => self.error(format!("Cannot paste: {err}")),
        }
    }

    fn apply_canvas_events(&mut self, ctx: &egui::Context, events: Vec<CanvasEvent>) {
        for event in events {
            match event {
                CanvasEvent::Select(id) => self.select(id),
                CanvasEvent::Toggle(id) => {
                    if !self.selection.remove(&id) {
                        self.selection.insert(id);
                        self.selected = Some(id);
                    } else if self.selected == Some(id) {
                        self.selected = self.selection.first().copied();
                    }
                }
                CanvasEvent::SelectMany { ids, add } => {
                    if !add {
                        self.selection.clear();
                    }
                    self.selection.extend(ids.iter().copied());
                    self.selected = ids.first().copied().or(self.selected.filter(|_| add));
                }
                CanvasEvent::Move(id, delta) => {
                    // Dragging a selected node moves the whole selection.
                    let movers = if self.selection.contains(&id) {
                        self.selection_ids()
                    } else {
                        vec![id]
                    };
                    for node in movers {
                        if let Some(meta) = self.graph.meta_mut(node) {
                            meta.position[0] += delta.x;
                            meta.position[1] += delta.y;
                            self.modified = true;
                        }
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
                CanvasEvent::Delete(id) => {
                    if self.selection.contains(&id) {
                        self.delete_selection();
                    } else {
                        self.delete_node(id);
                    }
                }
                CanvasEvent::Duplicate(id) => {
                    if !self.selection.contains(&id) {
                        self.select(Some(id));
                    }
                    self.duplicate_selection();
                }
                CanvasEvent::Copy(id) => {
                    if !self.selection.contains(&id) {
                        self.select(Some(id));
                    }
                    self.copy_selection(ctx);
                }
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
                self.select(Some(id));
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
            ui.menu_button("Edit", |ui| {
                let some = !self.selection.is_empty();
                let fmt = |s: &KeyboardShortcut| ui.ctx().format_shortcut(s);
                let (dup, all) = (fmt(&SHORTCUT_DUPLICATE), fmt(&SHORTCUT_SELECT_ALL));
                if ui.add_enabled(some, egui::Button::new("Duplicate").shortcut_text(dup)).clicked() {
                    self.duplicate_selection();
                    ui.close();
                }
                if ui.add_enabled(some, egui::Button::new("Copy").shortcut_text("Ctrl+C")).clicked() {
                    self.copy_selection(&ui.ctx().clone());
                    ui.close();
                }
                if ui.add_enabled(some, egui::Button::new("Cut").shortcut_text("Ctrl+X")).clicked() {
                    self.copy_selection(&ui.ctx().clone());
                    self.delete_selection();
                    ui.close();
                }
                if ui.add(egui::Button::new("Paste").shortcut_text("Ctrl+V")).clicked() {
                    // Delivered as a paste event on the next frame.
                    ui.ctx().send_viewport_cmd(ViewportCommand::RequestPaste);
                    ui.close();
                }
                ui.separator();
                if ui.add(egui::Button::new("Select all").shortcut_text(all)).clicked() {
                    self.selection = self.graph.nodes().map(|(id, _)| id).collect();
                    ui.close();
                }
                if ui.add_enabled(some, egui::Button::new("Delete").shortcut_text("Del")).clicked() {
                    self.delete_selection();
                    ui.close();
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
                ui.label("Right-click empty space to add nodes;\nDelete removes the selected nodes.");
                ui.label("Ctrl/Shift-click or Shift-drag to select several;\nCtrl+D duplicates, Ctrl+C/V copy and paste.");
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
        let count = self.selection_ids().len();
        if count > 1 {
            ui.heading(format!("{count} nodes selected"));
            ui.label(
                RichText::new("Drag any of them to move all; edit one by clicking it alone.")
                    .weak(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("⧉ Duplicate").on_hover_text("Ctrl+D").clicked() {
                    self.duplicate_selection();
                }
                if ui.button("Copy").on_hover_text("Ctrl+C").clicked() {
                    self.copy_selection(&ui.ctx().clone());
                }
                if ui.button("🗑 Delete").on_hover_text("Delete").clicked() {
                    self.delete_selection();
                }
            });
            return;
        }
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
                let mut edit_step = self
                    .edit_step
                    .filter(|(node, _)| *node == id)
                    .map(|(_, s)| s);
                let palette = if kind == PdfEdit::KIND {
                    self.input_document_of(id)
                        .cloned()
                        .map(|doc| self.palette_of(&doc))
                } else {
                    None
                };
                let extras = inspector::Extras {
                    pdf: self.node_document(id).cloned(),
                    thumbs: &mut self.thumbs,
                    edit_step: &mut edit_step,
                    palette,
                };
                if inspector::node_params(ui, &mut self.graph, id, &self.registry, extras) {
                    self.touch();
                }
                self.edit_step = edit_step.map(|s| (id, s));
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

        // Editing shows a different picture (e.g. the whole incoming page
        // instead of the cropped result): start each from a fitted view.
        let edit = self.edit_step.filter(|(node, _)| Some(*node) == target);
        if edit != self.preview_edit {
            self.preview_edit = edit;
            self.pdf_view.request_fit();
            self.viewport.request_fit();
        }
        if let Some((node, step, frame)) = self.edit_target(target) {
            self.edit_preview(ui, node, step, frame);
            return;
        }
        if let Some(node) = self.pdf_crop_target(target) {
            self.pdf_crop_preview(ui, node);
            return;
        }
        if let Some((node, layer)) = self.overlay_target(target) {
            self.overlay_preview(ui, node, layer);
            return;
        }
        let doc = target.and_then(|id| {
            if self.show_input {
                self.input_document_of(id)
            } else {
                self.node_document(id)
            }
        });
        if let Some(doc) = doc.cloned() {
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

    // ----- visual geometry editing ------------------------------------------

    /// The step being edited with handles, if it belongs to the previewed
    /// node and still has editable geometry.
    fn edit_target(&self, target: Option<NodeId>) -> Option<(NodeId, usize, bounds::Frame)> {
        let (node, step) = self.edit_step?;
        if Some(node) != target {
            return None;
        }
        let effect = &self
            .graph
            .node_as::<EnhanceImage>(node)?
            .steps
            .get(step)?
            .effect;
        Some((node, step, bounds::editable(effect)?))
    }

    /// A PDF Edit node whose crop is being adjusted on the preview.
    fn pdf_crop_target(&self, target: Option<NodeId>) -> Option<NodeId> {
        let (node, _) = self.edit_step?;
        (Some(node) == target && self.graph.node_as::<PdfEdit>(node).is_some()).then_some(node)
    }

    /// The preview while adjusting a PDF Edit crop: the *incoming* page with
    /// a crop box. Dragging it switches the node to a manual box.
    fn pdf_crop_preview(&mut self, ui: &mut egui::Ui, node: NodeId) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("✥ Adjusting the crop").strong());
            ui.label(
                RichText::new("Drag the edges or corners; drag inside to move. The box applies to every page.")
                    .weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Done").on_hover_text("Esc").clicked() {
                    self.edit_step = None;
                }
            });
        });
        let Some(input) = self.input_document_of(node).cloned() else {
            ui.label(RichText::new("Connect a PDF to this node first.").weak());
            return;
        };
        let settings = self
            .graph
            .node_as::<PdfEdit>(node)
            .expect("checked")
            .clone();
        let page = self.pdf_view.page();
        let page_box = pdfkit::page_boxes(&input)
            .get(page)
            .copied()
            .unwrap_or(PageBox {
                crop: [0.0, 0.0, 1.0, 1.0],
                rotation: 0,
            });
        // The box as currently set, in fractions of the displayed page.
        let mut rect = match settings.crop {
            CropMode::Manual => settings.manual_box,
            CropMode::Content => self
                .node_document(node)
                .and_then(|out| out.page_info.get(page)?.content_box)
                .map_or([0.0, 0.0, 1.0, 1.0], |b| {
                    pdfkit::page_to_displayed(b, page_box)
                }),
            CropMode::None => [0.0, 0.0, 1.0, 1.0],
        };
        let graph = &mut self.graph;
        let mut changed = false;
        self.pdf_view
            .show_with_overlay(ui, &input, &mut |ui, frame| {
                let id = ui.make_persistent_id(("pdf_crop", node.0));
                let accent = Color32::from_rgb(80, 200, 255);
                if handles::rect(ui, frame, id, &mut rect, accent, true) {
                    let _ = graph.edit_node::<PdfEdit, _>(node, |n| {
                        n.crop = CropMode::Manual;
                        n.manual_box = rect.map(|v| v.clamp(0.0, 1.0));
                    });
                    changed = true;
                }
            });
        if changed {
            self.touch();
        }
    }

    /// A PDF Overlay layer being dragged on the preview.
    fn overlay_target(&self, target: Option<NodeId>) -> Option<(NodeId, usize)> {
        let (node, layer) = self.edit_step?;
        (Some(node) == target && self.graph.node_as::<PdfOverlay>(node).is_some())
            .then_some((node, layer))
    }

    /// The preview while placing an overlay layer: the stacked page with
    /// every layer outlined; dragging the chosen one changes its offset.
    fn overlay_preview(&mut self, ui: &mut egui::Ui, node: NodeId, layer: usize) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("✥ Placing layer {}", layer + 1)).strong());
            ui.label(RichText::new("Drag the highlighted layer; other layers are dashed.").weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Done").on_hover_text("Esc").clicked() {
                    self.edit_step = None;
                }
            });
        });
        let inputs = self.input_documents_of(node, pdf_overlay::INPUT);
        if inputs.iter().any(Option::is_none) {
            ui.label(RichText::new("Waiting for the layers' PDFs…").weak());
            return;
        }
        let Some(output) = self.node_document(node).cloned() else {
            ui.label(
                RichText::new("The overlay has no result yet (see the node's status).").weak(),
            );
            return;
        };
        let settings = self
            .graph
            .node_as::<PdfOverlay>(node)
            .expect("checked")
            .clone();
        let boxes: Vec<Vec<PageBox>> = inputs
            .iter()
            .flatten()
            .map(|d| pdfkit::page_boxes(d))
            .collect();
        // Output pages without any layer are not written; skip them likewise.
        let layout: Vec<_> = settings
            .layout(&boxes)
            .into_iter()
            .filter(|p| !p.layers.is_empty())
            .collect();
        let Some(page) = layout.get(self.pdf_view.page()).cloned() else {
            self.pdf_view.show(ui, &output);
            return;
        };
        let [w, h] = page.size.map(|v| v.max(1e-3));
        // Placement rects are in points from the bottom-left; handles use
        // fractions of the page from the top-left.
        let norm = |[x0, y0, x1, y1]: [f32; 4]| [x0 / w, 1.0 - y1 / h, x1 / w, 1.0 - y0 / h];
        let graph = &mut self.graph;
        let mut changed = false;
        self.pdf_view
            .show_with_overlay(ui, &output, &mut |ui, frame| {
                let mut active = None;
                for p in &page.layers {
                    if p.layer == layer {
                        active = Some(p);
                    } else {
                        handles::ghost(ui, frame, norm(p.rect), Color32::from_gray(200));
                    }
                }
                let Some(p) = active else { return };
                let id = ui.make_persistent_id(("overlay_layer", node.0, layer));
                let accent = Color32::from_rgb(80, 200, 255);
                if let Some([dx, dy]) = handles::movable(ui, frame, id, norm(p.rect), accent) {
                    let _ = graph.edit_node::<PdfOverlay, _>(node, |n| {
                        let offset = &mut n.layer_mut(layer).offset_mm;
                        offset[0] += dx * w / pdfkit::PT_PER_MM;
                        offset[1] += dy * h / pdfkit::PT_PER_MM;
                    });
                    changed = true;
                }
            });
        if !page.layers.iter().any(|p| p.layer == layer) {
            ui.label(RichText::new("This layer is hidden or has no page here.").weak());
        }
        if changed {
            self.touch();
        }
    }

    fn probe_image(&self, node: NodeId, key: String) -> Option<Arc<RasterImage>> {
        self.results
            .as_ref()?
            .probes
            .get(&(node, key))?
            .as_image()
            .cloned()
    }

    /// Whether the image needed for the current edit is still on its way.
    fn edit_image_pending(&self) -> bool {
        self.edit_step.is_some()
            && self
                .wanted_probes()
                .iter()
                .any(|(node, key)| self.probe_image(*node, key.clone()).is_none())
    }

    /// The page "Crop to document" detects in `image`, normalised; cached.
    fn detected_page(&mut self, image: &Arc<RasterImage>) -> Option<[[f32; 2]; 4]> {
        if let Some((seen, quad)) = &self.detected
            && Arc::ptr_eq(seen, image)
        {
            return *quad;
        }
        let (w, h) = (image.width() as f32, image.height() as f32);
        let quad =
            docforge_nodes::enhance::detect_page(image).map(|q| q.map(|[x, y]| [x / w, y / h]));
        self.detected = Some((Arc::clone(image), quad));
        quad
    }

    /// The preview while editing step `step` of an Enhance node: the image
    /// that step works on, with handles for its geometry.
    fn edit_preview(&mut self, ui: &mut egui::Ui, node: NodeId, step: usize, frame: bounds::Frame) {
        let effect = self
            .graph
            .node_as::<EnhanceImage>(node)
            .expect("checked")
            .steps[step]
            .effect
            .clone();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("✥ Editing step {}: {}", step + 1, effect.name())).strong(),
            );
            ui.label(RichText::new(bounds::hint(&effect)).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Done").on_hover_text("Esc").clicked() {
                    self.edit_step = None;
                }
            });
        });
        if self.edit_step.is_none() {
            return;
        }
        let key = match frame {
            bounds::Frame::StepInput => step_input_key(step),
            bounds::Frame::StepOutput => step_output_key(step),
        };
        let image = self.probe_image(node, key);
        let detected = match (&effect, &image) {
            (docforge_nodes::Effect::DetectDocument(_), Some(img)) => self.detected_page(img),
            _ => None,
        };
        let graph = &mut self.graph;
        let mut changed = false;
        self.viewport.show_with_overlay(
            ui,
            image.as_ref(),
            "Waiting for this step's image…",
            &mut |ui, frame| {
                let id = ui.make_persistent_id(("edit", node.0, step));
                changed |= graph
                    .edit_node::<EnhanceImage, _>(node, |n| {
                        bounds::edit_effect(ui, frame, id, &mut n.steps[step].effect, detected)
                    })
                    .unwrap_or(false);
            },
        );
        if changed {
            self.touch();
        }
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
                let events = self.canvas.show(
                    ui,
                    &self.graph,
                    &self.registry,
                    &statuses,
                    &self.selection,
                    self.selected,
                );
                self.apply_canvas_events(&ctx, events);
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
