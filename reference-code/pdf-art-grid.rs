use eframe::egui;
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq)]
enum PageSize {
    A3,
    A4,
    A5,
    Letter,
    Legal,
    Tabloid,
    Custom,
}

impl PageSize {
    fn dimensions(&self) -> (f64, f64) {
        match self {
            PageSize::A3 => (841.89, 1190.55),
            PageSize::A4 => (595.28, 841.89),
            PageSize::A5 => (420.94, 595.28),
            PageSize::Letter => (612.0, 792.0),
            PageSize::Legal => (612.0, 1008.0),
            PageSize::Tabloid => (792.0, 1224.0),
            PageSize::Custom => (0.0, 0.0), // Handled by DragValues
        }
    }

    fn name(&self) -> &'static str {
        match self {
            PageSize::A3 => "A3 (297 x 420 mm)",
            PageSize::A4 => "A4 (210 x 297 mm)",
            PageSize::A5 => "A5 (148 x 210 mm)",
            PageSize::Letter => "US Letter (8.5 x 11 in)",
            PageSize::Legal => "US Legal (8.5 x 14 in)",
            PageSize::Tabloid => "Tabloid (11 x 17 in)",
            PageSize::Custom => "Custom Dimensions...",
        }
    }
}

// --- PDF Processing Logic & Math ---

#[derive(Clone, Copy, Debug)]
struct Transform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Transform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }
}

impl Transform {
    fn multiply(&mut self, m: &Transform) {
        let a = m.a * self.a + m.b * self.c;
        let b = m.a * self.b + m.b * self.d;
        let c = m.c * self.a + m.d * self.c;
        let d = m.c * self.b + m.d * self.d;
        let e = m.e * self.a + m.f * self.c + self.e;
        let f = m.e * self.b + m.f * self.d + self.f;

        self.a = a;
        self.b = b;
        self.c = c;
        self.d = d;
        self.e = e;
        self.f = f;
    }

    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
}

#[derive(Debug, Default)]
struct BoundingBox {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    initialized: bool,
}

impl BoundingBox {
    fn update(&mut self, x: f64, y: f64) {
        // print!("\t X : {}, Y : {}", x, y);
        if !self.initialized {
            self.min_x = x;
            self.max_x = x;
            self.min_y = y;
            self.max_y = y;
            self.initialized = true;
        } else {
            if x < self.min_x {
                self.min_x = x;
            }
            if x > self.max_x {
                self.max_x = x;
            }
            if y < self.min_y {
                self.min_y = y;
            }
            if y > self.max_y {
                self.max_y = y;
            }
        }
    }
}

fn process_pdf(
    input_path: &Path,
    output_path: &Path,
    margin: f64,
    spacing: f64,
    portrait_w: f64,
    portrait_h: f64,
    override_w: Option<f64>,
    override_h: Option<f64>,
    draw_debug_box: bool,
) -> Result<String, String> {
    let mut doc = Document::load(input_path).map_err(|e| format!("Failed to load PDF: {}", e))?;

    let pages = doc.get_pages();
    let page_id = pages.values().next().ok_or("No pages found in PDF")?;

    let content_data = doc.get_page_content(*page_id);
    let original_content = Content::decode(&content_data).map_err(|e| e.to_string())?;

    let mut bbox = BoundingBox::default();
    let mut pending_bbox = BoundingBox::default(); // Temporary tracker for paths being drawn
    let mut ctm_stack: Vec<Transform> = Vec::new();
    let mut current_ctm = Transform::default();

    let get_f64 = |obj: Option<&Object>| -> Option<f64> {
        match obj {
            Some(Object::Real(r)) => Some(*r as f64),
            Some(Object::Integer(i)) => Some(*i as f64),
            _ => None,
        }
    };

    // 1. Calculate Original Art Bounding Box (With matrix transformations)
    for operation in &original_content.operations {
        let operator = operation.operator.as_str();
        let operands = &operation.operands;
        // print!("\nOperator : {}", operator);
        match operator {
            "q" => {
                ctm_stack.push(current_ctm);
            }
            "Q" => {
                if let Some(saved) = ctm_stack.pop() {
                    current_ctm = saved;
                }
            }
            "cm" => {
                if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) = (
                    get_f64(operands.get(0)),
                    get_f64(operands.get(1)),
                    get_f64(operands.get(2)),
                    get_f64(operands.get(3)),
                    get_f64(operands.get(4)),
                    get_f64(operands.get(5)),
                ) {
                    let matrix = Transform { a, b, c, d, e, f };
                    current_ctm.multiply(&matrix);
                }
            }

            // -- PATH CONSTRUCTION (Drafting) --
            "m" | "l" => {
                if let (Some(x), Some(y)) = (get_f64(operands.get(0)), get_f64(operands.get(1))) {
                    let (tx, ty) = current_ctm.apply(x, y);
                    pending_bbox.update(tx, ty); // Update the TEMPORARY box
                }
            }
            "re" => {
                if let (Some(x), Some(y), Some(w), Some(h)) = (
                    get_f64(operands.get(0)),
                    get_f64(operands.get(1)),
                    get_f64(operands.get(2)),
                    get_f64(operands.get(3)),
                ) {
                    let p1 = current_ctm.apply(x, y);
                    let p2 = current_ctm.apply(x + w, y);
                    let p3 = current_ctm.apply(x, y + h);
                    let p4 = current_ctm.apply(x + w, y + h);
                    pending_bbox.update(p1.0, p1.1);
                    pending_bbox.update(p2.0, p2.1);
                    pending_bbox.update(p3.0, p3.1);
                    pending_bbox.update(p4.0, p4.1);
                }
            }
            "c" => {
                if let (Some(x3), Some(y3)) = (get_f64(operands.get(4)), get_f64(operands.get(5))) {
                    let (tx, ty) = current_ctm.apply(x3, y3);
                    pending_bbox.update(tx, ty);
                }
            }

            // -- PATH PAINTING (Visibility Triggers) --
            // S/s = Stroke, f/F/f* = Fill, B/B*/b/b* = Fill & Stroke
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                if pending_bbox.initialized {
                    // The path is actually painted! Commit it to the main bounding box.
                    bbox.update(pending_bbox.min_x, pending_bbox.min_y);
                    bbox.update(pending_bbox.max_x, pending_bbox.max_y);

                    // Reset the temporary box for the next drawing path
                    pending_bbox = BoundingBox::default();
                }
            }

            // -- PATH DISCARD (Invisible Geometry) --
            // n = End Path without filling or stroking (used for clipping masks & CAD boundaries)
            "n" => {
                // Wipe the coordinates from memory without committing them
                pending_bbox = BoundingBox::default();
            }
            _ => {}
        }
    }

    println!("{:?}", bbox);

    if !bbox.initialized {
        return Err("Could not find any vector art bounding box in the document.".to_string());
    }

    let mut art_width = bbox.max_x - bbox.min_x;
    let mut art_height = bbox.max_y - bbox.min_y;

    // Apply manual overrides
    if let Some(w) = override_w {
        art_width = w;
    }
    if let Some(h) = override_h {
        art_height = h;
    }

    // 2. Dynamic Orientation Logic (Tightly Packed)
    let calculate_fit = |page_w: f64, page_h: f64| -> (usize, usize, usize, f64, f64) {
        let printable_w = page_w - (margin * 2.0);
        let printable_h = page_h - (margin * 2.0);

        if printable_w < art_width || printable_h < art_height {
            return (0, 0, 0, 0.0, 0.0);
        }

        let cols = ((printable_w + spacing) / (art_width + spacing)).floor() as usize;
        let rows = ((printable_h + spacing) / (art_height + spacing)).floor() as usize;
        let total = cols * rows;

        (total, cols, rows, spacing, spacing)
    };

    let landscape_w = portrait_h;
    let landscape_h = portrait_w;

    let (p_total, p_cols, p_rows, p_gap_x, p_gap_y) = calculate_fit(portrait_w, portrait_h);
    let (l_total, l_cols, l_rows, l_gap_x, l_gap_y) = calculate_fit(landscape_w, landscape_h);

    let (final_w, final_h, total_fits, cols, rows, gap_x, gap_y, orientation_str) =
        if p_total >= l_total && p_total > 0 {
            (
                portrait_w, portrait_h, p_total, p_cols, p_rows, p_gap_x, p_gap_y, "Portrait",
            )
        } else if l_total > 0 {
            (
                landscape_w,
                landscape_h,
                l_total,
                l_cols,
                l_rows,
                l_gap_x,
                l_gap_y,
                "Landscape",
            )
        } else {
            return Err(
                "The vector art is too large to fit on the sheet with the current margins!"
                    .to_string(),
            );
        };

    let mut grid_operations = Vec::new();

    // 3. Grid Transformation Loop (Top-to-Bottom)
    for r in 0..rows {
        for c in 0..cols {
            let target_x = margin + (c as f64 * (art_width + gap_x));
            let target_y = final_h - margin - art_height - (r as f64 * (art_height + gap_y));

            let tx = target_x - bbox.min_x;
            let ty = target_y - bbox.min_y;

            // Translate and draw the original art
            grid_operations.push(Operation::new("q", vec![]));
            grid_operations.push(Operation::new(
                "cm",
                vec![
                    Object::Real(1.0),
                    Object::Real(0.0),
                    Object::Real(0.0),
                    Object::Real(1.0),
                    Object::Real(tx as f32),
                    Object::Real(ty as f32),
                ],
            ));
            grid_operations.extend(original_content.operations.clone());
            grid_operations.push(Operation::new("Q", vec![]));

            // Optional: Draw the debugging bounding box in RED
            if draw_debug_box {
                grid_operations.push(Operation::new("q", vec![]));
                grid_operations.push(Operation::new(
                    "RG",
                    vec![Object::Real(1.0), Object::Real(0.0), Object::Real(0.0)],
                )); // Red
                grid_operations.push(Operation::new("w", vec![Object::Real(1.0)])); // 1pt thickness
                grid_operations.push(Operation::new(
                    "re",
                    vec![
                        Object::Real(target_x as f32),
                        Object::Real(target_y as f32),
                        Object::Real(art_width as f32),
                        Object::Real(art_height as f32),
                    ],
                ));
                grid_operations.push(Operation::new("S", vec![])); // Stroke rectangle
                grid_operations.push(Operation::new("Q", vec![]));
            }
        }
    }

    // 4. Tooling Paths Injection
    grid_operations.push(Operation::new("q", vec![]));
    grid_operations.push(Operation::new(
        "RG",
        vec![Object::Real(0.5), Object::Real(0.5), Object::Real(0.5)],
    ));
    grid_operations.push(Operation::new("w", vec![Object::Real(0.5)]));
    grid_operations.push(Operation::new(
        "d",
        vec![
            Object::Array(vec![Object::Real(3.0), Object::Real(3.0)]),
            Object::Real(0.0),
        ],
    ));

    for r in 1..rows {
        let y = final_h - margin - (r as f64 * art_height) - (r as f64 * gap_y) + (gap_y / 2.0);
        grid_operations.push(Operation::new(
            "m",
            vec![Object::Real(10.0), Object::Real(y as f32)],
        ));
        grid_operations.push(Operation::new(
            "l",
            vec![
                Object::Real((final_w - 10.0) as f32),
                Object::Real(y as f32),
            ],
        ));
    }

    for c in 1..cols {
        let x = margin + (c as f64 * art_width) + (c as f64 * gap_x) - (gap_x / 2.0);
        grid_operations.push(Operation::new(
            "m",
            vec![Object::Real(x as f32), Object::Real(10.0)],
        ));
        grid_operations.push(Operation::new(
            "l",
            vec![
                Object::Real(x as f32),
                Object::Real((final_h - 10.0) as f32),
            ],
        ));
    }

    grid_operations.push(Operation::new(
        "d",
        vec![Object::Array(vec![]), Object::Real(0.0)],
    ));

    let mut draw_crosshair = |ops: &mut Vec<Operation>, cx: f32, cy: f32| {
        let size = 5.0;
        ops.push(Operation::new(
            "m",
            vec![Object::Real(cx), Object::Real(cy - size)],
        ));
        ops.push(Operation::new(
            "l",
            vec![Object::Real(cx), Object::Real(cy + size)],
        ));
        ops.push(Operation::new(
            "m",
            vec![Object::Real(cx - size), Object::Real(cy)],
        ));
        ops.push(Operation::new(
            "l",
            vec![Object::Real(cx + size), Object::Real(cy)],
        ));
    };

    let print_max_x = final_w - margin;
    let print_max_y = final_h - margin;

    draw_crosshair(&mut grid_operations, margin as f32, margin as f32);
    draw_crosshair(&mut grid_operations, print_max_x as f32, margin as f32);
    draw_crosshair(&mut grid_operations, margin as f32, print_max_y as f32);
    draw_crosshair(&mut grid_operations, print_max_x as f32, print_max_y as f32);

    grid_operations.push(Operation::new("S", vec![]));
    grid_operations.push(Operation::new("Q", vec![]));

    // 5. Build and Save PDF
    let final_content = Content {
        operations: grid_operations,
    };
    let final_bytes = final_content.encode().map_err(|e| e.to_string())?;

    if let Ok(page_dict) = doc.get_object_mut(*page_id) {
        if let Object::Dictionary(dict) = page_dict {
            dict.set(
                "MediaBox",
                vec![
                    Object::Real(0.0),
                    Object::Real(0.0),
                    Object::Real(final_w as f32),
                    Object::Real(final_h as f32),
                ],
            );
            dict.remove(b"CropBox");
            dict.remove(b"BleedBox");
            dict.remove(b"TrimBox");
            dict.remove(b"ArtBox");
            dict.remove(b"Rotate");
        }
    }

    doc.change_page_content(*page_id, final_bytes)
        .map_err(|e| e.to_string())?;
    doc.save(output_path)
        .map_err(|e| format!("Failed to save output: {}", e))?;

    Ok(format!(
        "Success! Generated {} copies\nOptimized for {}\nSaved to: {}",
        total_fits,
        orientation_str,
        output_path.display()
    ))
}

// --- GUI State & Logic ---

struct PdfGridApp {
    input_path: Option<PathBuf>,
    output_path: PathBuf,
    margin: f64,
    spacing: f64,
    page_size: PageSize,
    page_width: f64,
    page_height: f64,
    use_custom_size: bool,
    custom_w: f64,
    custom_h: f64,
    draw_debug_box: bool,
    status_message: String,
    is_error: bool,
}

impl Default for PdfGridApp {
    fn default() -> Self {
        Self {
            input_path: None,
            output_path: PathBuf::from("grid_output.pdf"),
            margin: 20.0,
            spacing: 10.0,
            page_size: PageSize::A4,
            page_width: 595.28,
            page_height: 841.89,
            use_custom_size: false,
            custom_w: 250.0,
            custom_h: 150.0,
            draw_debug_box: false,
            status_message: "Ready. Please select an input PDF.".to_string(),
            is_error: false,
        }
    }
}

impl eframe::App for PdfGridApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.heading("PDF Vector Grid Generator");
        ui.add_space(10.0);

        ui.group(|ui| {
            ui.label(egui::RichText::new("File Settings").strong());
            ui.add_space(5.0);

            ui.horizontal(|ui| {
                if ui.button("Select Input PDF").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("PDF Documents", &["pdf"])
                        .pick_file()
                    {
                        self.input_path = Some(path);
                        self.status_message = "Input file loaded.".to_string();
                        self.is_error = false;
                    }
                }
                if let Some(path) = &self.input_path {
                    ui.label(path.display().to_string());
                } else {
                    ui.label("No file selected.");
                }
            });

            ui.add_space(5.0);

            ui.horizontal(|ui| {
                if ui.button("Select Output File").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("PDF Documents", &["pdf"])
                        .set_file_name("grid_output.pdf")
                        .save_file()
                    {
                        self.output_path = path;
                    }
                }
                ui.label(self.output_path.display().to_string());
            });
        });

        ui.add_space(10.0);

        ui.group(|ui| {
            ui.label(egui::RichText::new("Grid Parameters").strong());
            ui.add_space(5.0);

            ui.horizontal(|ui| {
                ui.label("Margin (pt):");
                ui.add(
                    egui::DragValue::new(&mut self.margin)
                        .speed(0.5)
                        .range(0.0..=100.0),
                );

                ui.add_space(15.0);
                ui.label("Spacing (pt):");
                ui.add(
                    egui::DragValue::new(&mut self.spacing)
                        .speed(0.5)
                        .range(0.0..=100.0),
                );
            });

            ui.add_space(5.0);
            ui.label("Page Dimensions (Portrait pt):");

            // 1. The Dropdown
            ui.horizontal(|ui| {
                ui.label("Format:");
                egui::ComboBox::from_id_salt("page_size_combo")
                    .selected_text(self.page_size.name())
                    .show_ui(ui, |ui| {
                        let sizes = [
                            PageSize::A3,
                            PageSize::A4,
                            PageSize::A5,
                            PageSize::Letter,
                            PageSize::Legal,
                            PageSize::Tabloid,
                            PageSize::Custom,
                        ];
                        for size in sizes {
                            if ui
                                .selectable_value(&mut self.page_size, size, size.name())
                                .changed()
                            {
                                // If the user clicked a standard size, update the numerical widths
                                if size != PageSize::Custom {
                                    let (w, h) = size.dimensions();
                                    self.page_width = w;
                                    self.page_height = h;
                                }
                            }
                        }
                    });
            });

            // 2. The Width/Height Drag Values
            ui.horizontal(|ui| {
                ui.label("Width:");
                // If the user manually drags these numbers, switch the dropdown to "Custom"
                if ui
                    .add(
                        egui::DragValue::new(&mut self.page_width)
                            .speed(1.0)
                            .range(100.0..=5000.0),
                    )
                    .changed()
                {
                    self.page_size = PageSize::Custom;
                }

                ui.label("Height:");
                if ui
                    .add(
                        egui::DragValue::new(&mut self.page_height)
                            .speed(1.0)
                            .range(100.0..=5000.0),
                    )
                    .changed()
                {
                    self.page_size = PageSize::Custom;
                }
            });

            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.use_custom_size, "Override Detected Art Size");
                if self.use_custom_size {
                    ui.label("Width:");
                    ui.add(
                        egui::DragValue::new(&mut self.custom_w)
                            .speed(1.0)
                            .range(10.0..=2000.0),
                    );
                    ui.label("Height:");
                    ui.add(
                        egui::DragValue::new(&mut self.custom_h)
                            .speed(1.0)
                            .range(10.0..=2000.0),
                    );
                }
            });

            ui.add_space(5.0);
            ui.checkbox(
                &mut self.draw_debug_box,
                "Draw Red Bounding Box (Debug Mode)",
            );
        });

        ui.add_space(20.0);

        let generate_btn = ui.add_sized(
            [ui.available_width(), 40.0],
            egui::Button::new("Generate Grid PDF"),
        );

        if generate_btn.clicked() {
            if let Some(input) = &self.input_path {
                let override_w = if self.use_custom_size {
                    Some(self.custom_w)
                } else {
                    None
                };
                let override_h = if self.use_custom_size {
                    Some(self.custom_h)
                } else {
                    None
                };

                match process_pdf(
                    input,
                    &self.output_path,
                    self.margin,
                    self.spacing,
                    self.page_width,
                    self.page_height,
                    override_w,
                    override_h,
                    self.draw_debug_box,
                ) {
                    Ok(msg) => {
                        self.status_message = msg;
                        self.is_error = false;
                    }
                    Err(e) => {
                        self.status_message = e;
                        self.is_error = true;
                    }
                }
            } else {
                self.status_message = "Error: Please select an input PDF first.".to_string();
                self.is_error = true;
            }
        }

        ui.add_space(10.0);

        let color = if self.is_error {
            egui::Color32::RED
        } else {
            egui::Color32::DARK_GREEN
        };
        ui.label(egui::RichText::new(&self.status_message).color(color));
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([450.0, 520.0])
            .with_min_inner_size([400.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "PDF Grid Generator",
        options,
        Box::new(|_cc| Ok(Box::new(PdfGridApp::default()))),
    )
}
