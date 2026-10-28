//! Parameter editors for the selected node.
//!
//! Every editor returns `true` when it changed a parameter, so the app knows
//! to re-evaluate.

use std::path::{Path, PathBuf};

use std::sync::Arc;

use crate::{
    dialogs,
    thumbs::{Thumb, Thumbnails},
};
use docforge_core::{Graph, NodeId, NodeRegistry, NodeType as _, PdfDocument};
use docforge_nodes::{
    Effect, EnhanceImage, IfExists, InputImage, OutputImage, PdfEdit, PdfInput, PdfOverlay,
    PngCompression, PrintLayout, SaveFormat, Step,
    enhance::{
        AutoCrop, BlackAndWhite, BrightnessContrast, Crop, DetectDocument, Line, MagicColor,
        Rectangle, Rotate, Sharpen, Watermark,
    },
    pdf_edit::{ColorRule, CropMode, PaletteEntry, Recolor, RecolorMode},
    pdf_input::images,
    pdf_overlay::{self, Align, Blend, PageSize},
    print::{self, CutMarks, ItemSize, PageOrientation, Paper, PrintItem},
};
use eframe::egui::{self, Color32, DragValue, Grid, RichText, Slider, TextEdit, Ui};

/// Edits the node's parameters. Returns whether anything changed.
/// What some editors need beyond the node itself.
pub struct Extras<'a> {
    /// The node's last PDF output, if any (PDF Input shows its images).
    pub pdf: Option<Arc<PdfDocument>>,
    pub thumbs: &'a mut Thumbnails,
    /// The step of this node being edited with handles on the preview.
    pub edit_step: &'a mut Option<usize>,
    /// Colours of the PDF arriving at the node (PDF Edit lists them).
    pub palette: Option<Arc<Vec<PaletteEntry>>>,
}

pub fn node_params(
    ui: &mut Ui,
    graph: &mut Graph,
    id: NodeId,
    registry: &NodeRegistry,
    extras: Extras<'_>,
) -> bool {
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
            .edit_node::<EnhanceImage, _>(id, |n| enhance_image(ui, n, extras.edit_step))
            .unwrap_or(false),
        k if k == PdfEdit::KIND => graph
            .edit_node::<PdfEdit, _>(id, |n| pdf_edit(ui, n, extras))
            .unwrap_or(false),
        k if k == PdfInput::KIND => graph
            .edit_node::<PdfInput, _>(id, |n| pdf_input(ui, n, salt, extras))
            .unwrap_or(false),
        k if k == PrintLayout::KIND => {
            let images = input_names(graph, registry, id, print::INPUT);
            let pdfs = input_names(graph, registry, id, print::PDF_INPUT);
            graph
                .edit_node::<PrintLayout, _>(id, |n| print_layout(ui, n, salt, &images, &pdfs))
                .unwrap_or(false)
        }
        k if k == PdfOverlay::KIND => {
            let layers = input_names(graph, registry, id, pdf_overlay::INPUT);
            let mut swap = None;
            let mut changed = graph
                .edit_node::<PdfOverlay, _>(id, |n| {
                    pdf_overlay(ui, n, &layers, extras.edit_step, &mut swap)
                })
                .unwrap_or(false);
            if let Some((a, b)) = swap
                && graph.swap_inputs(id, pdf_overlay::INPUT, a, b)
            {
                // The settings travel with their PDF.
                let _ = graph.edit_node::<PdfOverlay, _>(id, |n| {
                    n.layer_mut(a.max(b));
                    n.layers.swap(a, b);
                });
                if let Some(step) = extras.edit_step.as_mut() {
                    *step = match *step {
                        s if s == a => b,
                        s if s == b => a,
                        s => s,
                    };
                }
                changed = true;
            }
            changed
        }
        _ => raw_json(ui, graph, id, registry),
    }
}

/// Names of the nodes feeding `port`, in connection order: their labels, or
/// kind and id.
fn input_names(graph: &Graph, registry: &NodeRegistry, id: NodeId, port: &str) -> Vec<String> {
    graph
        .edges()
        .iter()
        .filter(|e| e.to.node == id && e.to.port == port)
        .map(|e| {
            let label = graph.meta(e.from.node).and_then(|m| m.label.clone());
            let kind = graph
                .node(e.from.node)
                .map(|n| n.kind())
                .unwrap_or_default();
            label.unwrap_or_else(|| {
                format!(
                    "{} {}",
                    crate::canvas::display_name(registry, kind),
                    e.from.node
                )
            })
        })
        .collect()
}

// ----- Input Image -----------------------------------------------------------

fn input_image(ui: &mut Ui, node: &mut InputImage, salt: u64) -> bool {
    let mut changed = false;
    ui.label("Image file");
    ui.horizontal(|ui| {
        changed |= path_field(ui, ("input_path", salt), &mut node.path, "Choose an image…");
        if let Some(path) = browse(
            ui,
            ("input_browse", salt),
            &[dialogs::IMAGES, ("All files", &["*"])],
            &node.path,
        ) {
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
        changed |= browse_save(
            ui,
            &mut node.path,
            &[
                ("PNG", &["png"]),
                ("JPEG", &["jpg", "jpeg"]),
                ("WebP", &["webp"]),
                ("TIFF", &["tif", "tiff"]),
                ("BMP", &["bmp"]),
            ],
            "output.png",
        );
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
    if cfg!(not(target_arch = "wasm32")) {
        changed |= ui
            .checkbox(&mut node.create_dirs, "Create missing folders")
            .changed();
    }
    changed |= auto_save(
        ui,
        &mut node.auto_save,
        "When off, the image only passes through; use “Save now”.",
    );
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

// ----- PDF Input ---------------------------------------------------------------

fn pdf_input(ui: &mut Ui, node: &mut PdfInput, salt: u64, extras: Extras<'_>) -> bool {
    let mut changed = false;
    ui.label("PDF file");
    ui.horizontal(|ui| {
        changed |= path_field(
            ui,
            ("pdf_input_path", salt),
            &mut node.path,
            "Choose a PDF…",
        );
        if let Some(path) = browse(ui, ("pdf_input_browse", salt), &[dialogs::PDF], &node.path) {
            node.path = path;
            changed = true;
        }
    });
    ui.label(
        RichText::new("The PDF passes on unchanged. To crop it or change its colours, connect a PDF Edit node.")
            .weak()
            .small(),
    );
    changed |= embedded_images(ui, node, extras);
    changed
}

/// Thumbnails of every embedded image with a checkbox each; ticked images
/// become output ports. Returns whether the selection changed.
fn embedded_images(ui: &mut Ui, node: &mut PdfInput, extras: Extras<'_>) -> bool {
    ui.add_space(6.0);
    ui.strong("Embedded images");
    let Some(doc) = extras.pdf else {
        ui.label(RichText::new("Load a PDF to see its images.").weak());
        return false;
    };
    let entries = images::list(&doc);
    let inline = images::inline_count(&doc);
    if entries.is_empty() {
        ui.label(RichText::new("This PDF has no embedded images.").weak());
    }
    if inline > 0 {
        ui.label(RichText::new(format!("{inline} inline image(s) cannot be extracted.")).weak());
    }
    if entries.is_empty() {
        return false;
    }

    let mut selected: Vec<String> = node.selected_images.clone();
    ui.horizontal(|ui| {
        ui.label(format!("{} of {} selected", selected.len(), entries.len()));
        if ui.small_button("All").clicked() {
            selected = entries.iter().map(|e| e.key.clone()).collect();
        }
        if ui.small_button("None").clicked() {
            selected.clear();
        }
    });
    ui.label(
        RichText::new("Each ticked image becomes an output of this node.")
            .weak()
            .small(),
    );

    let thumbs = extras.thumbs.get(ui.ctx(), &doc);
    let side = 96.0;
    let columns = ((ui.available_width() / (side + 24.0)).floor() as usize).max(1);
    Grid::new("embedded_images")
        .num_columns(columns)
        .spacing([8.0, 8.0])
        .show(ui, |ui| {
            for (i, entry) in entries.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.set_width(side + 16.0);
                    let thumb = thumbs.and_then(|t| t.iter().find(|t| t.key == entry.key));
                    match thumb {
                        Some(Thumb {
                            texture: Some(tex), ..
                        }) => {
                            ui.add(
                                egui::Image::from_texture(egui::load::SizedTexture::from_handle(
                                    tex,
                                ))
                                .max_size(egui::vec2(side, side)),
                            );
                        }
                        Some(Thumb {
                            error: Some(err), ..
                        }) => {
                            ui.colored_label(Color32::LIGHT_RED, "Cannot decode")
                                .on_hover_text(err);
                        }
                        _ => {
                            ui.add_sized([side, side], egui::Spinner::new());
                        }
                    }
                    let mut on = selected.contains(&entry.key);
                    if ui.checkbox(&mut on, entry.key.as_str()).changed() {
                        if on {
                            selected.push(entry.key.clone());
                        } else {
                            selected.retain(|k| k != &entry.key);
                        }
                    }
                    ui.label(
                        RichText::new(format!(
                            "{} × {} px · {}",
                            entry.info.width_px,
                            entry.info.height_px,
                            entry.info.filter.as_deref().unwrap_or("raw")
                        ))
                        .weak()
                        .small(),
                    );
                });
                if (i + 1) % columns == 0 {
                    ui.end_row();
                }
            }
        });

    if selected == node.selected_images {
        return false;
    }
    // Ports follow document order. Keys no longer in the PDF are dropped only
    // when the user edits; until then the node reports them as missing.
    node.selected_images = entries
        .iter()
        .map(|e| e.key.clone())
        .filter(|k| selected.contains(k))
        .collect();
    true
}

// ----- PDF Edit ----------------------------------------------------------------

fn pdf_edit(ui: &mut Ui, node: &mut PdfEdit, extras: Extras<'_>) -> bool {
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.strong("Crop");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let on = *extras.edit_step == Some(0);
            if ui
                .selectable_label(on, "✥ Adjust on preview")
                .on_hover_text("Drag a crop box on the page instead of typing numbers")
                .clicked()
            {
                *extras.edit_step = (!on).then_some(0);
            }
        });
    });
    Grid::new("pdf_edit_crop")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Crop to");
            egui::ComboBox::from_id_salt("crop_mode")
                .selected_text(crop_name(node.crop))
                .show_ui(ui, |ui| {
                    for c in [CropMode::Content, CropMode::Manual, CropMode::None] {
                        changed |= ui
                            .selectable_value(&mut node.crop, c, crop_name(c))
                            .changed();
                    }
                });
            ui.end_row();
            match node.crop {
                CropMode::Content => {
                    ui.label("Padding");
                    changed |= ui
                        .add(
                            DragValue::new(&mut node.padding_mm)
                                .range(0.0..=50.0)
                                .speed(0.1)
                                .suffix(" mm"),
                        )
                        .changed();
                    ui.end_row();
                }
                CropMode::Manual => {
                    let b = &mut node.manual_box;
                    for (label, value) in [("Left", 0), ("Top", 1), ("Right", 2), ("Bottom", 3)] {
                        ui.label(label);
                        changed |= ui.add(percent_slider(&mut b[value], 0.0..=1.0)).changed();
                        ui.end_row();
                    }
                    // Keep it a valid box while typing.
                    for (lo, hi) in [(0, 2), (1, 3)] {
                        if b[hi] < b[lo] + 0.005 {
                            b[hi] = (b[lo] + 0.005).min(1.0);
                            b[lo] = b[lo].min(b[hi] - 0.005);
                        }
                    }
                }
                CropMode::None => {}
            }
        });
    if node.crop == CropMode::Content {
        changed |= ui
            .checkbox(&mut node.ignore_white_fills, "Ignore white areas")
            .on_hover_text("White-filled shapes (e.g. page backgrounds) don't count as content")
            .changed();
    }
    if node.crop == CropMode::Manual {
        ui.label(
            RichText::new("Fractions of each page as shown; applies to every page.")
                .weak()
                .small(),
        );
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.strong("Colours");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_button("Presets", |ui| {
                if ui.button("PCB top layer → black").clicked() {
                    node.recolor = Recolor::pcb_top();
                    changed = true;
                    ui.close();
                }
                if ui.button("PCB bottom layer → gray").clicked() {
                    node.recolor = Recolor::pcb_bottom();
                    changed = true;
                    ui.close();
                }
                if ui.button("Grayscale").clicked() {
                    node.recolor = Recolor {
                        mode: RecolorMode::Grayscale,
                        ..Recolor::default()
                    };
                    changed = true;
                    ui.close();
                }
                if ui.button("Original colours").clicked() {
                    node.recolor = Recolor::default();
                    changed = true;
                    ui.close();
                }
            });
        });
    });
    let r = &mut node.recolor;
    Grid::new("pdf_edit_colour")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Change");
            egui::ComboBox::from_id_salt("recolor_mode")
                .selected_text(recolor_name(r.mode))
                .show_ui(ui, |ui| {
                    for m in [
                        RecolorMode::Off,
                        RecolorMode::Single,
                        RecolorMode::Replace,
                        RecolorMode::Grayscale,
                    ] {
                        changed |= ui
                            .selectable_value(&mut r.mode, m, recolor_name(m))
                            .changed();
                    }
                });
            ui.end_row();
            if r.mode != RecolorMode::Off {
                ui.label("Apply to");
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut r.fills, "Fills & text").changed();
                    changed |= ui.checkbox(&mut r.strokes, "Lines").changed();
                });
                ui.end_row();
            }
            if r.mode == RecolorMode::Single {
                ui.label("New colour");
                changed |= ui.color_edit_button_srgb(&mut r.color).changed();
                ui.end_row();
            }
        });
    if r.mode == RecolorMode::Single {
        changed |= ui
            .checkbox(&mut r.keep_white, "Keep white")
            .on_hover_text("White details (pads, holes, knock-outs) stay white")
            .changed();
    }
    if r.mode == RecolorMode::Replace {
        changed |= color_rules(ui, r, extras.palette.as_deref().map(Vec::as_slice));
    }
    if r.mode != RecolorMode::Off {
        ui.label(
            RichText::new("Images and gradients keep their colours.")
                .weak()
                .small(),
        );
    }
    changed
}

/// One row per colour found in the input PDF: tick it to replace it, pick
/// what it becomes.
fn color_rules(ui: &mut Ui, r: &mut Recolor, palette: Option<&[PaletteEntry]>) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("Match within");
        changed |= ui
            .add(Slider::new(&mut r.tolerance, 0..=128))
            .on_hover_text("Per channel, 0–255")
            .changed();
    });
    let Some(palette) = palette else {
        ui.label(RichText::new("Connect a PDF to list its colours.").weak());
        return changed;
    };
    ui.label(RichText::new(format!("Colours in the input PDF ({}):", palette.len())).weak());
    let swatch = |ui: &mut Ui, rgb: [u8; 3]| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(28.0, 16.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 3.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
        ui.painter().rect_stroke(
            rect,
            3.0,
            egui::Stroke::new(1.0, Color32::GRAY),
            egui::StrokeKind::Inside,
        );
    };
    Grid::new("color_rules")
        .num_columns(4)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            for entry in palette.iter().take(32) {
                let pos = r.rules.iter().position(|rule| rule.from == entry.rgb);
                let mut on = pos.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    match pos {
                        Some(i) if !on => {
                            r.rules.remove(i);
                        }
                        None if on => r.rules.push(ColorRule {
                            from: entry.rgb,
                            to: [0, 0, 0],
                        }),
                        _ => {}
                    }
                    changed = true;
                }
                swatch(ui, entry.rgb);
                match r.rules.iter_mut().find(|rule| rule.from == entry.rgb) {
                    Some(rule) => {
                        ui.horizontal(|ui| {
                            ui.label("→");
                            changed |= ui.color_edit_button_srgb(&mut rule.to).changed();
                        });
                    }
                    None => {
                        ui.label(RichText::new("unchanged").weak());
                    }
                }
                let hex = format!(
                    "#{:02x}{:02x}{:02x}",
                    entry.rgb[0], entry.rgb[1], entry.rgb[2]
                );
                ui.label(
                    RichText::new(format!(
                        "{hex} · {} fill / {} line",
                        entry.fills, entry.strokes
                    ))
                    .weak()
                    .small(),
                );
                ui.end_row();
            }
        });
    // Rules for colours no longer in the input (e.g. after changing files).
    let orphans: Vec<usize> = (0..r.rules.len())
        .filter(|&i| !palette.iter().any(|e| e.rgb == r.rules[i].from))
        .collect();
    if !orphans.is_empty() {
        ui.label(RichText::new("Rules for colours not in this PDF:").weak());
        let mut remove = None;
        for i in orphans {
            ui.horizontal(|ui| {
                swatch(ui, r.rules[i].from);
                ui.label("→");
                changed |= ui.color_edit_button_srgb(&mut r.rules[i].to).changed();
                if ui.small_button("🗑").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            r.rules.remove(i);
            changed = true;
        }
    }
    changed
}

fn crop_name(c: CropMode) -> &'static str {
    match c {
        CropMode::Content => "Detected content",
        CropMode::Manual => "Manual box",
        CropMode::None => "Keep page size",
    }
}

fn recolor_name(m: RecolorMode) -> &'static str {
    match m {
        RecolorMode::Off => "Nothing",
        RecolorMode::Single => "Everything to one colour",
        RecolorMode::Replace => "Replace chosen colours",
        RecolorMode::Grayscale => "Grayscale",
    }
}

// ----- Print Layout ----------------------------------------------------------

// ----- PDF Overlay ---------------------------------------------------------------

/// Layer cards, top layer first as in drawing apps. `swap` receives a pair
/// of connection indices to exchange.
fn pdf_overlay(
    ui: &mut Ui,
    node: &mut PdfOverlay,
    layers: &[String],
    edit_step: &mut Option<usize>,
    swap: &mut Option<(usize, usize)>,
) -> bool {
    let mut changed = false;
    ui.strong("Layers");
    if layers.is_empty() {
        ui.label(
            RichText::new(
                "Connect two or more PDFs to “pdfs”. The first connected is the bottom layer.",
            )
            .weak(),
        );
    } else {
        ui.label(RichText::new("Top of the stack first.").weak().small());
    }
    let n = layers.len();
    for (i, name) in layers.iter().enumerate().rev() {
        let mut layer = node.layer(i);
        let editing = *edit_step == Some(i);
        let before = layer;
        ui.push_id(("layer", i), |ui| {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.checkbox(&mut layer.visible, "")
                        .on_hover_text("Show this layer");
                    ui.label(RichText::new(format!("{}. {name}", i + 1)).strong());
                    match i {
                        0 => ui.label(RichText::new("bottom").weak().small()),
                        _ if i + 1 == n => ui.label(RichText::new("top").weak().small()),
                        _ => ui.label(""),
                    };
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(i > 0, egui::Button::new("⏷").small())
                            .on_hover_text("Move down")
                            .clicked()
                        {
                            *swap = Some((i, i - 1));
                        }
                        if ui
                            .add_enabled(i + 1 < n, egui::Button::new("⏶").small())
                            .on_hover_text("Move up")
                            .clicked()
                        {
                            *swap = Some((i, i + 1));
                        }
                        if ui
                            .selectable_label(editing, "✥")
                            .on_hover_text("Drag this layer on the preview")
                            .clicked()
                        {
                            *edit_step = (!editing).then_some(i);
                        }
                    });
                });
                Grid::new("layer_grid")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Opacity");
                        ui.add(percent_slider(&mut layer.opacity, 0.0..=1.0));
                        ui.end_row();
                        ui.label("Blend");
                        egui::ComboBox::from_id_salt("blend")
                            .selected_text(blend_name(layer.blend))
                            .show_ui(ui, |ui| {
                                for b in [Blend::Normal, Blend::Multiply, Blend::Darken] {
                                    ui.selectable_value(&mut layer.blend, b, blend_name(b))
                                        .on_hover_text(blend_hint(b));
                                }
                            });
                        ui.end_row();
                        ui.label("Offset (right, down)");
                        ui.horizontal(|ui| {
                            for v in &mut layer.offset_mm {
                                ui.add(DragValue::new(v).speed(0.1).suffix(" mm"));
                            }
                            if layer.offset_mm != [0.0, 0.0] && ui.small_button("Reset").clicked() {
                                layer.offset_mm = [0.0, 0.0];
                            }
                        });
                        ui.end_row();
                    });
            });
        });
        if layer != before {
            *node.layer_mut(i) = layer;
            changed = true;
        }
    }
    if n > 0 && node.layers.len() > n {
        // Settings of disconnected layers are kept, so reconnecting restores them.
        ui.label(
            RichText::new(format!(
                "{} more layer setting(s) are kept for PDFs not connected now.",
                node.layers.len() - n
            ))
            .weak()
            .small(),
        );
    }
    if n > 1
        && (0..n).all(|i| node.layer(i).blend == Blend::Normal)
        && ui
            .small_button("Make white see-through (Multiply upper layers)")
            .on_hover_text(
                "Upper layers with a white background would otherwise hide what is below",
            )
            .clicked()
    {
        for i in 1..n {
            node.layer_mut(i).blend = Blend::Multiply;
        }
        changed = true;
    }

    ui.add_space(6.0);
    ui.strong("Page");
    Grid::new("overlay_page")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Align layers");
            egui::ComboBox::from_id_salt("align")
                .selected_text(align_name(node.align))
                .show_ui(ui, |ui| {
                    for a in [Align::Original, Align::Center, Align::TopLeft] {
                        changed |= ui
                            .selectable_value(&mut node.align, a, align_name(a))
                            .changed();
                    }
                });
            ui.end_row();
            ui.label("Page size");
            egui::ComboBox::from_id_salt("page_size")
                .selected_text(page_size_name(node.page_size))
                .show_ui(ui, |ui| {
                    for p in [PageSize::Union, PageSize::Bottom] {
                        changed |= ui
                            .selectable_value(&mut node.page_size, p, page_size_name(p))
                            .changed();
                    }
                });
            ui.end_row();
        });
    if node.align == Align::Original {
        ui.label(
            RichText::new(
                "Layers stay where they are on their own pages, so layers exported from one \
                 design line up even when PDF Edit cropped them differently.",
            )
            .weak()
            .small(),
        );
    }
    changed
}

fn blend_name(b: Blend) -> &'static str {
    match b {
        Blend::Normal => "Normal",
        Blend::Multiply => "Multiply",
        Blend::Darken => "Darken",
    }
}

fn blend_hint(b: Blend) -> &'static str {
    match b {
        Blend::Normal => "Covers what is below",
        Blend::Multiply => "Darkens what is below; white becomes see-through",
        Blend::Darken => "Keeps the darker colour of the two",
    }
}

fn align_name(a: Align) -> &'static str {
    match a {
        Align::Original => "Keep page positions",
        Align::Center => "Centres",
        Align::TopLeft => "Top-left corners",
    }
}

fn page_size_name(p: PageSize) -> &'static str {
    match p {
        PageSize::Union => "Fit all layers",
        PageSize::Bottom => "Bottom layer's page",
    }
}

fn print_layout(
    ui: &mut Ui,
    node: &mut PrintLayout,
    salt: u64,
    images: &[String],
    pdfs: &[String],
) -> bool {
    let mut changed = false;

    ui.strong("Pieces");
    if images.is_empty() && pdfs.is_empty() {
        ui.label(
            RichText::new(
                "Connect images to “images” or PDFs to “pdfs”; each image or PDF page becomes a piece.",
            )
            .weak(),
        );
    }
    for (i, source) in images.iter().enumerate() {
        let mut item = node.item(i);
        if piece_card(
            ui,
            ("piece", i),
            &format!("{}. {source}", i + 1),
            &mut item,
            false,
        ) {
            *node.item_mut(i) = item;
            changed = true;
        }
    }
    if !pdfs.is_empty() {
        ui.label(RichText::new("PDF artwork (vector)").weak());
    }
    for (i, source) in pdfs.iter().enumerate() {
        let mut item = node.pdf_item(i);
        let label = format!("PDF {}. {source}", i + 1);
        if piece_card(ui, ("pdf_piece", i), &label, &mut item, true) {
            *node.pdf_item_mut(i) = item;
            changed = true;
        }
    }

    ui.add_space(6.0);
    ui.strong("Paper");
    Grid::new("paper_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Size");
            egui::ComboBox::from_id_salt("paper")
                .selected_text(node.paper.name())
                .show_ui(ui, |ui| {
                    for p in Paper::ALL {
                        changed |= ui.selectable_value(&mut node.paper, p, p.name()).changed();
                    }
                });
            ui.end_row();
            if node.paper == Paper::Custom {
                ui.label("Custom size");
                changed |= mm_pair(ui, &mut node.custom_paper_mm);
                ui.end_row();
            }
            ui.label("Orientation");
            egui::ComboBox::from_id_salt("orientation")
                .selected_text(format!("{:?}", node.orientation))
                .show_ui(ui, |ui| {
                    for o in [
                        PageOrientation::Auto,
                        PageOrientation::Portrait,
                        PageOrientation::Landscape,
                    ] {
                        let label = if o == PageOrientation::Auto {
                            "Auto (fewest pages)".into()
                        } else {
                            format!("{o:?}")
                        };
                        changed |= ui
                            .selectable_value(&mut node.orientation, o, label)
                            .changed();
                    }
                });
            ui.end_row();
            ui.label("Margin");
            changed |= ui
                .add(
                    DragValue::new(&mut node.margin_mm)
                        .range(0.0..=60.0)
                        .speed(0.5)
                        .suffix(" mm"),
                )
                .changed();
            ui.end_row();
            ui.label("Gap between pieces");
            changed |= ui
                .add(
                    DragValue::new(&mut node.gap_mm)
                        .range(0.0..=40.0)
                        .speed(0.5)
                        .suffix(" mm"),
                )
                .changed();
            ui.end_row();
            ui.label("Cutting lines");
            egui::ComboBox::from_id_salt("cut_marks")
                .selected_text(cut_marks_name(node.cut_marks))
                .show_ui(ui, |ui| {
                    for c in [CutMarks::Outline, CutMarks::CropMarks, CutMarks::None] {
                        changed |= ui
                            .selectable_value(&mut node.cut_marks, c, cut_marks_name(c))
                            .changed();
                    }
                });
            ui.end_row();
        });
    changed |= ui
        .checkbox(
            &mut node.allow_rotation,
            "Turn pieces 90° when it saves paper",
        )
        .changed();

    ui.add_space(6.0);
    ui.strong("PDF");
    Grid::new("pdf_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Image resolution");
            changed |= ui
                .add(
                    DragValue::new(&mut node.image_dpi)
                        .range(72.0..=1200.0)
                        .speed(5.0)
                        .suffix(" dpi"),
                )
                .on_hover_text("Images are downsampled to this; higher means larger files")
                .changed();
            ui.end_row();
            ui.label("Compression");
            ui.horizontal(|ui| {
                let mut lossless = node.jpeg_quality.is_none();
                if ui.checkbox(&mut lossless, "Lossless").changed() {
                    node.jpeg_quality = (!lossless).then_some(90);
                    changed = true;
                }
                if let Some(q) = &mut node.jpeg_quality {
                    changed |= ui.add(Slider::new(q, 1..=100).text("JPEG")).changed();
                }
            });
            ui.end_row();
        });
    ui.label("Save PDF to");
    ui.horizontal(|ui| {
        changed |= path_field(
            ui,
            ("pdf_path", salt),
            &mut node.path,
            "Choose a .pdf file…",
        );
        changed |= browse_save(ui, &mut node.path, &[dialogs::PDF], "print.pdf");
    });
    changed |= auto_save(
        ui,
        &mut node.auto_save,
        "When off, use “Save PDF now” above.",
    );
    changed
}

/// Settings card for one image or PDF piece. Returns whether it changed.
fn piece_card(
    ui: &mut Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    item: &mut PrintItem,
    is_pdf: bool,
) -> bool {
    let before = item.clone();
    ui.push_id(salt, |ui| {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(DragValue::new(&mut item.copies).range(0..=500).suffix(" ×"));
                    ui.label(if item.fill { "At least" } else { "Copies" });
                });
            });
            ui.checkbox(&mut item.fill, "Fill sheet")
                .on_hover_text("Add as many copies as fit without needing another sheet");
            item_size(ui, &mut item.size, is_pdf);
        });
    });
    *item != before
}

fn item_size(ui: &mut Ui, size: &mut ItemSize, is_pdf: bool) {
    let real = if is_pdf {
        "Real size (as in the PDF)"
    } else {
        "Real size (from DPI)"
    };
    let mode = match size {
        ItemSize::FromDpi { .. } => 0,
        ItemSize::Width { .. } => 1,
        ItemSize::Exact { .. } => 2,
    };
    ui.horizontal(|ui| {
        ui.label("Size");
        egui::ComboBox::from_id_salt("size_mode")
            .selected_text([real, "Set width", "Exact size"][mode])
            .show_ui(ui, |ui| {
                if ui.selectable_label(mode == 0, real).clicked() && mode != 0 {
                    *size = ItemSize::default();
                }
                if ui.selectable_label(mode == 1, "Set width").clicked() && mode != 1 {
                    *size = ItemSize::Width { width_mm: 85.6 };
                }
                if ui.selectable_label(mode == 2, "Exact size").clicked() && mode != 2 {
                    *size = ItemSize::Exact {
                        width_mm: 85.6,
                        height_mm: 54.0,
                    };
                }
            });
        ui.menu_button("Presets", |ui| {
            for (name, w, h) in ItemSize::PRESETS {
                if ui.button(name).clicked() {
                    *size = ItemSize::Exact {
                        width_mm: w,
                        height_mm: h,
                    };
                    ui.close();
                }
            }
        });
    });
    match size {
        ItemSize::FromDpi { .. } if is_pdf => {
            ui.label(RichText::new("Printed 1:1, exactly as large as in the PDF.").weak());
        }
        ItemSize::FromDpi { fallback_dpi } => {
            ui.horizontal(|ui| {
                ui.label("If the image has no DPI, assume");
                ui.add(
                    DragValue::new(fallback_dpi)
                        .range(10.0..=2400.0)
                        .suffix(" dpi"),
                );
            });
        }
        ItemSize::Width { width_mm } => {
            ui.horizontal(|ui| {
                ui.label("Width");
                ui.add(
                    DragValue::new(width_mm)
                        .range(1.0..=2000.0)
                        .speed(0.5)
                        .suffix(" mm"),
                );
                ui.label(RichText::new("height follows the image").weak());
            });
        }
        ItemSize::Exact {
            width_mm,
            height_mm,
        } => {
            ui.horizontal(|ui| {
                let mut pair = [*width_mm, *height_mm];
                mm_pair(ui, &mut pair);
                [*width_mm, *height_mm] = pair;
                if ui
                    .small_button("⟲")
                    .on_hover_text("Swap width and height")
                    .clicked()
                {
                    std::mem::swap(width_mm, height_mm);
                }
            });
        }
    }
}

fn mm_pair(ui: &mut Ui, pair: &mut [f32; 2]) -> bool {
    ui.horizontal(|ui| {
        let w = ui.add(
            DragValue::new(&mut pair[0])
                .range(1.0..=2000.0)
                .speed(0.5)
                .suffix(" mm"),
        );
        ui.label("×");
        let h = ui.add(
            DragValue::new(&mut pair[1])
                .range(1.0..=2000.0)
                .speed(0.5)
                .suffix(" mm"),
        );
        w.changed() || h.changed()
    })
    .inner
}

fn cut_marks_name(c: CutMarks) -> &'static str {
    match c {
        CutMarks::Outline => "Dashed outline",
        CutMarks::CropMarks => "Corner crop marks",
        CutMarks::None => "None",
    }
}

// ----- Enhance Image ---------------------------------------------------------

enum StepAction {
    Up(usize),
    Down(usize),
    Remove(usize),
    Duplicate(usize),
}

fn enhance_image(ui: &mut Ui, node: &mut EnhanceImage, edit_step: &mut Option<usize>) -> bool {
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
                        if crate::bounds::editable(&step.effect).is_some() {
                            let on = *edit_step == Some(i);
                            if ui
                                .selectable_label(on, "✥")
                                .on_hover_text(
                                    "Edit on the preview: drag handles instead of typing numbers",
                                )
                                .clicked()
                            {
                                *edit_step = (!on).then_some(i);
                            }
                        }
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
        // The step being edited on the preview follows its card.
        let follow = |e: usize, a: usize, b: usize| {
            if e == a {
                b
            } else if e == b {
                a
            } else {
                e
            }
        };
        match action {
            StepAction::Up(i) => {
                node.steps.swap(i, i - 1);
                *edit_step = edit_step.map(|e| follow(e, i, i - 1));
            }
            StepAction::Down(i) => {
                node.steps.swap(i, i + 1);
                *edit_step = edit_step.map(|e| follow(e, i, i + 1));
            }
            StepAction::Remove(i) => {
                node.steps.remove(i);
                *edit_step = edit_step.and_then(|e| match e.cmp(&i) {
                    std::cmp::Ordering::Less => Some(e),
                    std::cmp::Ordering::Equal => None,
                    std::cmp::Ordering::Greater => Some(e - 1),
                });
            }
            StepAction::Duplicate(i) => {
                let copy = node.steps[i].clone();
                node.steps.insert(i + 1, copy);
                *edit_step = edit_step.map(|e| if e > i { e + 1 } else { e });
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
        // Unique to this watermark step's card.
        let id = ui.make_persistent_id("font_picker");
        if ui.small_button("Choose…").clicked() {
            dialogs::pick(ui.ctx(), id, &[dialogs::FONTS], font.as_deref());
        }
        if let Some(path) = dialogs::picked(id) {
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

/// A "Browse…" button for a file to open. Returns the chosen path, which on
/// the web arrives a few frames after the click.
fn browse(
    ui: &mut Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    filters: &[dialogs::Filter],
    current: &Path,
) -> Option<PathBuf> {
    let id = egui::Id::unique(salt);
    if ui.button("Browse…").clicked() {
        dialogs::pick(ui.ctx(), id, filters, Some(current));
    }
    dialogs::picked(id)
}

/// A "Browse…" button choosing where to save. A web page has no folders to
/// choose from (files are downloaded), so there is no button there.
fn browse_save(ui: &mut Ui, path: &mut PathBuf, filters: &[dialogs::Filter], name: &str) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    if ui.button("Browse…").clicked()
        && let Some(chosen) = dialogs::save(filters, Some(path), name)
    {
        *path = chosen;
        return true;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (ui, path, filters, name);
    false
}

/// The "save automatically" checkbox. A web page can't write files in the
/// background, so there it explains how to download instead.
fn auto_save(ui: &mut Ui, on: &mut bool, hint: &str) -> bool {
    if cfg!(target_arch = "wasm32") {
        ui.label(
            RichText::new("In the browser, “Save now” downloads the file.")
                .weak()
                .small(),
        );
        return false;
    }
    ui.checkbox(on, "Save automatically on every change")
        .on_hover_text(hint)
        .changed()
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
