//! Parameter editors for the selected node.
//!
//! Every editor returns `true` when it changed a parameter, so the app knows
//! to re-evaluate.

use std::path::{Path, PathBuf};

use docforge_core::{Graph, NodeId, NodeRegistry, NodeType as _};
use docforge_nodes::{
    Effect, EnhanceImage, IfExists, InputImage, OutputImage, PngCompression, SaveFormat, Step,
    enhance::{
        AutoCrop, BlackAndWhite, BrightnessContrast, Crop, DetectDocument, Line, MagicColor,
        Rectangle, Rotate, Sharpen, Watermark,
    },
};
use eframe::egui::{self, Color32, DragValue, Grid, RichText, Slider, TextEdit, Ui};

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "tif", "tiff", "bmp", "gif"];

/// Edits the node's parameters. Returns whether anything changed.
pub fn node_params(ui: &mut Ui, graph: &mut Graph, id: NodeId, registry: &NodeRegistry) -> bool {
    let Some(kind) = graph.node(id).map(|n| n.kind()) else {
        return false;
    };
    let salt = id.0;
    match kind {
        k if k == InputImage::KIND => graph
            .edit_node::<InputImage, _>(id, |n| input_image(ui, n, salt))
            .unwrap_or(false),
        k if k == OutputImage::KIND => graph
            .edit_node::<OutputImage, _>(id, |n| output_image(ui, n, salt))
            .unwrap_or(false),
        k if k == EnhanceImage::KIND => graph
            .edit_node::<EnhanceImage, _>(id, |n| enhance_image(ui, n))
            .unwrap_or(false),
        _ => raw_json(ui, graph, id, registry),
    }
}

// ----- Input Image -----------------------------------------------------------

fn input_image(ui: &mut Ui, node: &mut InputImage, salt: u64) -> bool {
    let mut changed = false;
    ui.label("Image file");
    ui.horizontal(|ui| {
        changed |= path_field(ui, ("input_path", salt), &mut node.path, "Choose an image…");
        if ui.button("Browse…").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Images", IMAGE_EXTENSIONS)
                .add_filter("All files", &["*"])
                .set_directory(start_dir(&node.path))
                .pick_file()
        {
            node.path = path;
            changed = true;
        }
    });
    ui.add_space(4.0);
    changed |= ui
        .checkbox(
            &mut node.apply_orientation,
            "Rotate upright (EXIF orientation)",
        )
        .changed();
    changed |= optional_dpi(ui, &mut node.dpi, "Override DPI");
    changed
}

// ----- Output Image ----------------------------------------------------------

fn output_image(ui: &mut Ui, node: &mut OutputImage, salt: u64) -> bool {
    let mut changed = false;
    ui.label("Save to");
    ui.horizontal(|ui| {
        changed |= path_field(ui, ("output_path", salt), &mut node.path, "Choose a file…");
        if ui.button("Browse…").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("PNG", &["png"])
                .add_filter("JPEG", &["jpg", "jpeg"])
                .add_filter("WebP", &["webp"])
                .add_filter("TIFF", &["tif", "tiff"])
                .add_filter("BMP", &["bmp"])
                .set_directory(start_dir(&node.path))
                .set_file_name("output.png")
                .save_file()
        {
            node.path = path;
            changed = true;
        }
    });
    ui.add_space(4.0);

    let effective = node.format.resolve(&node.path).ok();
    Grid::new("output_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Format");
            egui::ComboBox::from_id_salt("format")
                .selected_text(format_name(node.format))
                .show_ui(ui, |ui| {
                    for f in [
                        SaveFormat::Auto,
                        SaveFormat::Png,
                        SaveFormat::Jpeg,
                        SaveFormat::Webp,
                        SaveFormat::Tiff,
                        SaveFormat::Bmp,
                    ] {
                        changed |= ui
                            .selectable_value(&mut node.format, f, format_name(f))
                            .changed();
                    }
                });
            ui.end_row();

            if effective == Some(SaveFormat::Jpeg) {
                ui.label("JPEG quality");
                changed |= ui
                    .add(Slider::new(&mut node.jpeg_quality, 1..=100))
                    .changed();
                ui.end_row();
                ui.label("Background").on_hover_text(
                    "Transparent areas are filled with this colour (JPEG has no alpha)",
                );
                changed |= ui.color_edit_button_srgb(&mut node.background).changed();
                ui.end_row();
            }
            if effective == Some(SaveFormat::Png) {
                ui.label("PNG compression");
                egui::ComboBox::from_id_salt("png_compression")
                    .selected_text(format!("{:?}", node.png_compression))
                    .show_ui(ui, |ui| {
                        for c in [
                            PngCompression::Fast,
                            PngCompression::Balanced,
                            PngCompression::High,
                        ] {
                            changed |= ui
                                .selectable_value(&mut node.png_compression, c, format!("{c:?}"))
                                .changed();
                        }
                    });
                ui.end_row();
            }

            ui.label("If file exists");
            egui::ComboBox::from_id_salt("if_exists")
                .selected_text(if_exists_name(node.if_exists))
                .show_ui(ui, |ui| {
                    for p in [IfExists::Overwrite, IfExists::Error, IfExists::Rename] {
                        changed |= ui
                            .selectable_value(&mut node.if_exists, p, if_exists_name(p))
                            .changed();
                    }
                });
            ui.end_row();
        });
    changed |= optional_dpi(ui, &mut node.dpi, "Override DPI written to file");
    changed |= ui
        .checkbox(&mut node.create_dirs, "Create missing folders")
        .changed();
    changed |= ui
        .checkbox(&mut node.auto_save, "Save automatically on every change")
        .on_hover_text("When off, the image only passes through; use “Save now”.")
        .changed();
    if let Some(format) = effective
        && !format.stores_dpi()
    {
        ui.label(RichText::new(format!("{} files do not record DPI.", format_name(format))).weak());
    }
    changed
}

fn format_name(f: SaveFormat) -> &'static str {
    match f {
        SaveFormat::Auto => "From extension",
        SaveFormat::Png => "PNG",
        SaveFormat::Jpeg => "JPEG",
        SaveFormat::Webp => "WebP (lossless)",
        SaveFormat::Tiff => "TIFF",
        SaveFormat::Bmp => "BMP",
    }
}

fn if_exists_name(p: IfExists) -> &'static str {
    match p {
        IfExists::Overwrite => "Overwrite",
        IfExists::Error => "Fail",
        IfExists::Rename => "Add a number",
    }
}

// ----- Enhance Image ---------------------------------------------------------

enum StepAction {
    Up(usize),
    Down(usize),
    Remove(usize),
    Duplicate(usize),
}

fn enhance_image(ui: &mut Ui, node: &mut EnhanceImage) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.menu_button("➕ Add step", |ui| {
            for effect in Effect::catalogue() {
                if ui.button(effect.name()).clicked() {
                    node.steps.push(Step::from(effect));
                    changed = true;
                    ui.close();
                }
            }
        });
        ui.menu_button("Presets", |ui| {
            let presets: [(&str, Vec<Effect>); 4] = [
                (
                    "Document scan (colour)",
                    EnhanceImage::default()
                        .steps
                        .into_iter()
                        .map(|s| s.effect)
                        .collect(),
                ),
                (
                    "Document scan (black & white)",
                    vec![
                        Effect::DetectDocument(DetectDocument::default()),
                        Effect::MagicColor(MagicColor::default()),
                        Effect::BlackAndWhite(BlackAndWhite::default()),
                    ],
                ),
                (
                    "Draft copy",
                    vec![
                        Effect::DetectDocument(DetectDocument::default()),
                        Effect::MagicColor(MagicColor::default()),
                        Effect::Sharpen(Sharpen::default()),
                        Effect::Watermark(Watermark::default()),
                    ],
                ),
                ("Empty", vec![]),
            ];
            for (name, effects) in presets {
                if ui
                    .button(name)
                    .on_hover_text("Replaces the current steps")
                    .clicked()
                {
                    node.steps = effects.into_iter().map(Step::from).collect();
                    changed = true;
                    ui.close();
                }
            }
        });
    });
    ui.add_space(4.0);
    if node.steps.is_empty() {
        ui.label(RichText::new("No steps: the image passes through unchanged.").weak());
    }

    let mut action = None;
    let count = node.steps.len();
    for (i, step) in node.steps.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    changed |= ui
                        .checkbox(&mut step.enabled, "")
                        .on_hover_text("Enable step")
                        .changed();
                    let title =
                        RichText::new(format!("{}. {}", i + 1, step.effect.name())).strong();
                    ui.label(if step.enabled { title } else { title.weak() });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("🗑").on_hover_text("Remove").clicked() {
                            action = Some(StepAction::Remove(i));
                        }
                        if ui.small_button("⧉").on_hover_text("Duplicate").clicked() {
                            action = Some(StepAction::Duplicate(i));
                        }
                        if ui
                            .add_enabled(i + 1 < count, egui::Button::new("⏷").small())
                            .clicked()
                        {
                            action = Some(StepAction::Down(i));
                        }
                        if ui
                            .add_enabled(i > 0, egui::Button::new("⏶").small())
                            .clicked()
                        {
                            action = Some(StepAction::Up(i));
                        }
                    });
                });
                if step.enabled {
                    changed |= effect_params(ui, &mut step.effect, i);
                }
            });
        });
    }
    if let Some(action) = action {
        match action {
            StepAction::Up(i) => node.steps.swap(i, i - 1),
            StepAction::Down(i) => node.steps.swap(i, i + 1),
            StepAction::Remove(i) => {
                node.steps.remove(i);
            }
            StepAction::Duplicate(i) => {
                let copy = node.steps[i].clone();
                node.steps.insert(i + 1, copy);
            }
        }
        changed = true;
    }
    changed
}

fn effect_params(ui: &mut Ui, effect: &mut Effect, index: usize) -> bool {
    let mut changed = false;
    Grid::new(("effect", index)).num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
        let mut row = |ui: &mut Ui, label: &str, add: &mut dyn FnMut(&mut Ui) -> bool| {
            ui.label(label);
            changed |= add(ui);
            ui.end_row();
        };
        match effect {
            Effect::DetectDocument(DetectDocument { corners }) => {
                row(ui, "Corners", &mut |ui| {
                    let mut manual = corners.is_some();
                    let r = ui.checkbox(&mut manual, "Set manually").on_hover_text(
                        "Off: the page is detected automatically.\nOn: give the page corners as fractions of the image (0–1).",
                    );
                    if r.changed() {
                        *corners = manual.then_some([[0.05, 0.05], [0.95, 0.05], [0.95, 0.95], [0.05, 0.95]]);
                    }
                    r.changed()
                });
                if let Some(quad) = corners {
                    for (name, corner) in ["Top left", "Top right", "Bottom right", "Bottom left"].iter().zip(quad.iter_mut()) {
                        row(ui, name, &mut |ui| point_edit(ui, corner));
                    }
                }
            }
            Effect::AutoCrop(AutoCrop { tolerance, padding }) => {
                row(ui, "Tolerance", &mut |ui| ui.add(Slider::new(tolerance, 1..=200)).changed());
                row(ui, "Padding", &mut |ui| ui.add(DragValue::new(padding).range(0..=1000).suffix(" px")).changed());
            }
            Effect::Crop(Crop { left, top, right, bottom }) => {
                for (name, v) in [("Left", left), ("Top", top), ("Right", right), ("Bottom", bottom)] {
                    row(ui, name, &mut |ui| ui.add(percent_slider(v, 0.0..=0.49)).changed());
                }
            }
            Effect::Rotate(Rotate { quarter_turns }) => {
                row(ui, "Clockwise", &mut |ui| {
                    let mut c = false;
                    ui.horizontal(|ui| {
                        for (turns, label) in [(1, "90°"), (2, "180°"), (3, "270°")] {
                            c |= ui.selectable_value(quarter_turns, turns, label).changed();
                        }
                    });
                    c
                });
            }
            Effect::MagicColor(MagicColor { strength }) => {
                row(ui, "Strength", &mut |ui| ui.add(Slider::new(strength, 0.0..=1.0)).changed());
            }
            Effect::Sharpen(Sharpen { factor, radius }) => {
                row(ui, "Factor", &mut |ui| {
                    ui.add(Slider::new(factor, 0.0..=4.0)).on_hover_text("1 = unchanged, 1.5 = moderate").changed()
                });
                row(ui, "Radius", &mut |ui| ui.add(Slider::new(radius, 0.5..=10.0).suffix(" px")).changed());
            }
            Effect::BrightnessContrast(BrightnessContrast { brightness, contrast }) => {
                row(ui, "Brightness", &mut |ui| ui.add(Slider::new(brightness, -1.0..=1.0)).changed());
                row(ui, "Contrast", &mut |ui| ui.add(Slider::new(contrast, -1.0..=1.0)).changed());
            }
            Effect::Grayscale => {
                ui.label(RichText::new("No settings").weak());
                ui.end_row();
            }
            Effect::BlackAndWhite(BlackAndWhite { window, sensitivity }) => {
                row(ui, "Window", &mut |ui| {
                    ui.add(percent_slider(window, 0.005..=0.25))
                        .on_hover_text("Neighbourhood size relative to the shorter side")
                        .changed()
                });
                row(ui, "Sensitivity", &mut |ui| ui.add(Slider::new(sensitivity, 0.0..=0.6)).changed());
            }
            Effect::Watermark(w) => {
                row(ui, "Text", &mut |ui| ui.add(TextEdit::multiline(&mut w.text).desired_rows(1)).changed());
                row(ui, "Size", &mut |ui| ui.add(percent_slider(&mut w.size, 0.01..=0.6)).changed());
                row(ui, "Colour", &mut |ui| {
                    ui.color_edit_button_srgba_unmultiplied(&mut w.color)
                        .on_hover_text("Alpha sets the opacity")
                        .changed()
                });
                row(ui, "Angle", &mut |ui| ui.add(Slider::new(&mut w.angle, -180.0..=180.0).suffix("°")).changed());
                row(ui, "Centre", &mut |ui| point_edit(ui, &mut w.position));
                row(ui, "Repeat", &mut |ui| ui.checkbox(&mut w.tiled, "Tile across page").changed());
                row(ui, "Font", &mut |ui| font_picker(ui, &mut w.font));
            }
            Effect::Line(Line { from, to, width, color }) => {
                row(ui, "From", &mut |ui| point_edit(ui, from));
                row(ui, "To", &mut |ui| point_edit(ui, to));
                row(ui, "Width", &mut |ui| ui.add(percent_slider(width, 0.0005..=0.05)).changed());
                row(ui, "Colour", &mut |ui| ui.color_edit_button_srgba_unmultiplied(color).changed());
            }
            Effect::Rectangle(Rectangle { min, max, stroke_width, stroke, fill }) => {
                row(ui, "Top left", &mut |ui| point_edit(ui, min));
                row(ui, "Bottom right", &mut |ui| point_edit(ui, max));
                row(ui, "Outline", &mut |ui| ui.add(percent_slider(stroke_width, 0.0..=0.05)).changed());
                row(ui, "Outline colour", &mut |ui| ui.color_edit_button_srgba_unmultiplied(stroke).changed());
                row(ui, "Fill", &mut |ui| {
                    let mut c = false;
                    ui.horizontal(|ui| {
                        let mut on = fill.is_some();
                        if ui.checkbox(&mut on, "").changed() {
                            *fill = on.then_some([255, 235, 59, 110]);
                            c = true;
                        }
                        if let Some(color) = fill {
                            c |= ui.color_edit_button_srgba_unmultiplied(color).changed();
                        }
                    });
                    c
                });
            }
        }
    });
    if matches!(
        effect,
        Effect::Line(_) | Effect::Rectangle(_) | Effect::Watermark(_)
    ) {
        ui.label(
            RichText::new(
                "Positions are fractions of the image (0–1); sizes are % of its shorter side.",
            )
            .weak()
            .small(),
        );
    }
    changed
}

fn percent_slider(value: &mut f32, range: std::ops::RangeInclusive<f32>) -> Slider<'_> {
    Slider::new(value, range)
        .custom_formatter(|v, _| format!("{:.1}%", v * 100.0))
        .custom_parser(|s| {
            s.trim()
                .trim_end_matches('%')
                .parse::<f64>()
                .ok()
                .map(|v| v / 100.0)
        })
}

fn point_edit(ui: &mut Ui, p: &mut [f32; 2]) -> bool {
    ui.horizontal(|ui| {
        let x = ui.add(
            DragValue::new(&mut p[0])
                .range(0.0..=1.0)
                .speed(0.002)
                .fixed_decimals(3)
                .prefix("x "),
        );
        let y = ui.add(
            DragValue::new(&mut p[1])
                .range(0.0..=1.0)
                .speed(0.002)
                .fixed_decimals(3)
                .prefix("y "),
        );
        x.changed() || y.changed()
    })
    .inner
}

fn font_picker(ui: &mut Ui, font: &mut Option<PathBuf>) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let name = font
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("Built-in".to_owned(), |n| n.to_string_lossy().into_owned());
        ui.label(name);
        if ui.small_button("Choose…").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Fonts", &["ttf", "otf"])
                .pick_file()
        {
            *font = Some(path);
            changed = true;
        }
        if font.is_some() && ui.small_button("Reset").clicked() {
            *font = None;
            changed = true;
        }
    });
    changed
}

// ----- shared widgets --------------------------------------------------------

/// Text field for a path that commits on Enter or when focus leaves, so a
/// half-typed path is not loaded on every keystroke.
fn path_field(
    ui: &mut Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    path: &mut PathBuf,
    hint: &str,
) -> bool {
    let id = ui.make_persistent_id(salt);
    let mut text = ui
        .data(|d| d.get_temp::<String>(id))
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let response = ui.add(
        TextEdit::singleline(&mut text)
            .id(id)
            .hint_text(hint)
            .desired_width(ui.available_width() - 80.0),
    );
    if response.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, text));
        false
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
        let commit = response.lost_focus() && Path::new(text.trim()) != path.as_path();
        if commit {
            *path = PathBuf::from(text.trim());
        }
        commit
    }
}

fn optional_dpi(ui: &mut Ui, dpi: &mut Option<f32>, label: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let mut on = dpi.is_some();
        if ui.checkbox(&mut on, label).changed() {
            *dpi = on.then_some(300.0);
            changed = true;
        }
        if let Some(v) = dpi {
            changed |= ui
                .add(
                    DragValue::new(v)
                        .range(1.0..=4800.0)
                        .speed(1.0)
                        .suffix(" dpi"),
                )
                .changed();
        }
    });
    changed
}

fn start_dir(current: &Path) -> PathBuf {
    current
        .parent()
        .filter(|p| p.is_dir())
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

/// Fallback editor for node kinds without a dedicated one: their saved
/// parameters as JSON.
fn raw_json(ui: &mut Ui, graph: &mut Graph, id: NodeId, registry: &NodeRegistry) -> bool {
    let Some(node) = graph.node(id) else {
        return false;
    };
    let kind = node.kind();
    let buffer_id = ui.make_persistent_id(("raw_json", id.0));
    let mut text = ui
        .data(|d| d.get_temp::<String>(buffer_id))
        .unwrap_or_else(|| {
            node.save_params()
                .and_then(|v| serde_json::to_string_pretty(&v))
                .unwrap_or_default()
        });
    ui.label(
        RichText::new("No dedicated editor for this node; edit its parameters as JSON.").weak(),
    );
    ui.add(
        TextEdit::multiline(&mut text)
            .code_editor()
            .desired_rows(12)
            .desired_width(f32::INFINITY),
    );
    ui.data_mut(|d| d.insert_temp(buffer_id, text.clone()));

    let mut changed = false;
    ui.horizontal(|ui| {
        if ui.button("Apply").clicked() {
            let result = serde_json::from_str(&text)
                .map_err(|e| e.to_string())
                .and_then(|params| {
                    registry
                        .deserialize(kind, params)
                        .map_err(|e| e.to_string())
                })
                .and_then(|node| graph.replace_node(id, node).map_err(|e| e.to_string()));
            match result {
                Ok(_) => {
                    ui.data_mut(|d| d.remove::<String>(buffer_id));
                    changed = true;
                }
                Err(err) => {
                    ui.data_mut(|d| d.insert_temp(buffer_id.with("err"), err));
                }
            }
        }
        if ui.button("Revert").clicked() {
            ui.data_mut(|d| d.remove::<String>(buffer_id));
        }
    });
    if let Some(err) = ui.data(|d| d.get_temp::<String>(buffer_id.with("err"))) {
        ui.colored_label(Color32::LIGHT_RED, err);
    }
    changed
}
