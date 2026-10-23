# docforge

A node-based desktop tool, written in Rust, for cleaning up scanned documents
and photos and producing printable PDFs.

You build a pipeline by wiring small tools (nodes) together: load an image or
PDF, apply CamScanner-style effects, and pack the results onto paper at their
real size with cutting lines. The preview updates as you edit. Pipelines save
as JSON project files that can be re-run from the command line.

**Highlights**

- **Six tools**:
  - Input Image and PDF Input load files.
  - Enhance Image applies document effects and overlays.
  - Output Image saves images.
  - Print Layout packs pieces onto paper and writes a PDF.
  - Groups nest whole graphs (engine only for now).
- **Live editing**: evaluation runs in the background, and only what changed
  is recomputed (per node, and per effect step).
- **Real-world sizes**: DPI is read from files, carried through every step and
  written back out. Print Layout places an 85.6 × 54 mm ID card at exactly
  85.6 × 54 mm.
- **PDF in, PDF out**:
  - detects the visible content box of PDF pages (vectors, text, images);
  - crops pages to that box;
  - multiplies vector artwork on a sheet without rasterising it;
  - extracts embedded images for enhancement.
- **Pure Rust**: `image` for raster files, `lopdf` for writing PDFs, `hayro`
  for rendering them, `egui`/`eframe` for the GUI.

---

## Contents

1. [Requirements and building](#requirements-and-building)
2. [Quick start](#quick-start)
3. [The desktop app](#the-desktop-app)
4. [Tool reference](#tool-reference)
   - [Input Image](#input-image) · [Output Image](#output-image) ·
     [Enhance Image](#enhance-image) · [Print Layout (PDF)](#print-layout-pdf) ·
     [PDF Input](#pdf-input) · [Groups](#groups)
5. [Recipes](#recipes)
6. [Command-line tools](#command-line-tools)
7. [Project files](#project-files)
8. [Using docforge as a library](#using-docforge-as-a-library)
9. [Architecture](#architecture)
10. [Development and testing](#development-and-testing)
11. [Troubleshooting](#troubleshooting)
12. [Known limitations](#known-limitations)

---

## Requirements and building

| Requirement | Notes |
|---|---|
| **Rust 1.95 or newer** | Required by `egui`/`eframe` 0.37. Install via [rustup](https://rustup.rs). |
| **A GPU driver with Vulkan or OpenGL** | The GUI renders through `wgpu`. |
| **Linux: an XDG desktop portal** | Needed for the Open/Save file dialogs (`rfd`). Standard on KDE, GNOME and most desktops. |

There are no C libraries to install; every dependency is pure Rust.

```sh
# Build everything (release mode is strongly recommended for image work)
cargo build --release --workspace

# Start the app
cargo run --release -p docforge-app

# …or run the built binary directly
./target/release/docforge
```

The app accepts an optional file to open:

```sh
./target/release/docforge project.json   # open a saved project
./target/release/docforge scan.jpg       # start with this image loaded
```

> Dependencies are compiled with optimisations even in debug builds
> (`[profile.dev.package."*"] opt-level = 3`), so `cargo run` without
> `--release` is usable. Release builds are still several times faster.

---

## Quick start

1. Run `cargo run --release -p docforge-app`. A starter pipeline is already
   set up: **Input Image → Enhance Image → Output Image** and
   **→ Print Layout (PDF)**.
2. Click the **Input Image** node, then **Browse…** in the panel on the right.
   You can also drag an image file onto the window.
3. Click **Enhance Image** to see the cleaned-up document. It uses the default
   steps: *Crop to document → Magic colour → Sharpen ×1.5*. Toggle, reorder or
   add steps and the preview updates as you edit.
4. Click **Output Image**, choose a file with **Browse…**, and it saves
   automatically whenever the result changes.
5. Click **Print Layout (PDF)** to see the printable sheet. Set the copies and
   size of each piece, choose a PDF file under *Save PDF to*, and print it.
6. Save the whole pipeline with **Ctrl+S**.

---

## The desktop app

```
┌───────────────────────────────────────────────┬──────────────────┐
│ Preview (zoomable image or PDF)               │ Inspector        │
│                                               │ settings of the  │
│                                               │ selected node    │
├───────────────────────────────────────────────┤                  │
│ Node canvas (pipeline)                        │                  │
├───────────────────────────────────────────────┴──────────────────┤
│ Status bar: evaluation state, timings, messages                  │
└──────────────────────────────────────────────────────────────────┘
```

All panels can be resized by dragging their borders.

### Node canvas

| Action | How |
|---|---|
| Add a node | Right-click on empty canvas, or use the **Add** menu |
| Select a node | Click it |
| Move a node | Drag it |
| Connect | Drag from an output port (right side) to an input port (left side), or the other way round |
| Detach a wire | Drag a connected input port away; drop it on another port to reconnect it, or anywhere else to remove it |
| Disconnect | Right-click an input port |
| Delete a node | Select it and press **Delete** or **Backspace**, or right-click it → *Delete node* |
| Pan | Drag empty space |
| Zoom | Scroll wheel or pinch |

- **Port colours** show the data type: **blue = image**, **orange = PDF document**.
  Only ports of the same type connect.
- **The dot in each node's title** shows its state: green = done, red = failed
  (hover for the error), orange = waiting for an upstream node, grey = not run
  yet.

### Preview

The preview shows the output of the selected node, or of the pinned node.

| Control | Effect |
|---|---|
| Scroll / pinch | Zoom around the cursor |
| Drag | Pan |
| Double-click, **Fit** | Fit to the view |
| **100%** | One image pixel per screen point; for PDFs, roughly life-size |
| **Output / Input** | Before/after: show what the node produces or what it receives |
| **📌 Pin** | Keep previewing this node while you select and edit others |

The toolbar shows the pixel size, DPI and physical size in mm, plus the
coordinates and RGBA value of the pixel under the cursor.

- **PDF documents** get page navigation (◀ ▶) and a summary line, for example
  *"7 pieces on 1 A4 page (landscape) · 61% of the paper used"*. Warnings
  appear below it in orange.
- **Sharpness when zoomed:** pages re-render at higher resolution in the
  background as you zoom in, so vector art stays sharp.

### Files, projects and shortcuts

| Shortcut | Action |
|---|---|
| **Ctrl+N** | New project (starter pipeline) |
| **Ctrl+O** | Open project |
| **Ctrl+S** | Save project |
| **Ctrl+Shift+S** | Save project as… |
| **Delete / Backspace** | Delete the selected node |

- **File → Open image…** or **dropping an image** on the window puts it into
  the selected Input Image node, else the first empty one, else a new node.
- **Dropping a `.json` file** opens it as a project.
- **Relative paths** inside a project resolve against the project file's
  folder, so a project folder can be moved or shared as a whole.
- **Input files are watched.** If an image or PDF changes on disk (for example
  after editing it in another program), the pipeline re-runs automatically
  within about 1.5 seconds.
- **Unsaved changes** are marked with ● in the window title. You're asked
  before they are discarded (New, Open, Quit).

---

## Tool reference

Every tool is a node with typed ports and a set of settings. The setting names
below are the keys used in [project files](#project-files); the GUI shows the
same settings in the inspector.

### Input Image

**Kind:** `input_image` · **Output:** `image`

Loads an image file (PNG, JPEG, WebP, TIFF, BMP, GIF).

- The format is detected from the file's content, not its extension.
- Phone photos are turned upright using their EXIF orientation.
- The print resolution is read from the file (PNG `pHYs`, JPEG JFIF density)
  unless overridden.
- The file reloads automatically when it changes on disk.

| Setting | Default | Meaning |
|---|---|---|
| `path` | — | Image file (relative paths resolve against the project folder) |
| `apply_orientation` | `true` | Rotate/flip according to EXIF |
| `dpi` | `null` | Override the print resolution |

### Output Image

**Kind:** `output_image` · **Input:** `image` · **Output:** `image` (pass-through)

Saves its input to a file and passes the image on, so it can also sit in the
middle of a pipeline as a checkpoint.

- **Safe writes:** files are written atomically (temporary file, then rename),
  so a crash never leaves a broken file.
- **DPI:** written into PNG and JPEG files.
- **Smaller files:** fully opaque images are stored without an alpha channel.
- **JPEG and transparency:** JPEG has no transparency, so transparent areas
  are filled with `background`.

| Setting | Default | Meaning |
|---|---|---|
| `path` | — | Target file |
| `format` | `"auto"` | `auto` (from the extension), `png`, `jpeg`, `webp` (lossless), `tiff`, `bmp` |
| `jpeg_quality` | `92` | 1–100 |
| `png_compression` | `"balanced"` | `fast`, `balanced`, `high` |
| `background` | `[255,255,255]` | Fill colour for transparency when saving JPEG |
| `dpi` | `null` | Override the DPI written to the file |
| `if_exists` | `"overwrite"` | `overwrite`, `error` (refuse), `rename` (`name-1.png`, `name-2.png`, …) |
| `create_dirs` | `true` | Create missing folders |
| `auto_save` | `true` | Save on every change. When off, use **Save now** in the inspector |

The node's own previous output never counts as a conflict for `error` or
`rename`, so live re-saving keeps updating the same file.

### Enhance Image

**Kind:** `enhance_image` · **Input:** `image` · **Output:** `image`

Applies a list of **steps** in order. Each step can be switched on/off,
reordered, duplicated or removed. Every step's result is cached, so editing
step 4 only re-runs steps 4 onwards. **Presets** offer ready-made recipes:
colour scan, black & white scan, draft copy and empty.

| Effect (`effect`) | Settings (default) | What it does |
|---|---|---|
| **Crop to document** `detect_document` | `corners` (`null` = automatic) | Finds the page in a photo and straightens its perspective. Manual corners are fractions of the image (0–1): top-left, top-right, bottom-right, bottom-left |
| **Auto-crop border** `auto_crop` | `tolerance` (40), `padding` px (10) | Trims a uniform border (flatbed scans) |
| **Crop** `crop` | `left`, `top`, `right`, `bottom` (0) | Trims fractions of the width/height from each side |
| **Rotate** `rotate` | `quarter_turns` | Clockwise 90° steps |
| **Magic colour** `magic_color` | `strength` 0–1 (0.6) | Evens out lighting and shadows, whitens the paper, deepens ink, boosts colour |
| **Sharpen** `sharpen` | `factor` (1.5), `radius` px (1.5) | Unsharp mask on luminance; 1 = unchanged |
| **Brightness / contrast** `brightness_contrast` | `brightness`, `contrast` −1…1 (0) | Tone adjustment |
| **Grayscale** `grayscale` | — | Removes colour |
| **Black & white** `black_and_white` | `window` (0.04), `sensitivity` (0.15) | Adaptive threshold that copes with shadows |
| **Text watermark** `watermark` | `text` ("DRAFT"), `size` (0.15), `color` RGBA ([220,30,30,90]), `angle` ° (30), `position` ([0.5,0.5]), `tiled` (false), `font` (built-in) | Draws text, optionally repeated across the page. Alpha sets the opacity. `font` accepts any TTF/OTF file |
| **Line** `line` | `from`, `to`, `width` (0.004), `color` | Straight line |
| **Rectangle** `rectangle` | `min`, `max`, `stroke_width` (0.004), `stroke`, `fill` (`null`) | Outline and/or fill |

Positions are fractions of the image (0–1), and sizes and line widths are
fractions of its shorter side. Overlays therefore land in the same place
whatever the resolution, even after earlier crops.

**Typical times** on a 12-megapixel photo (8 cores, release build):
- crop to document: about 70 ms;
- magic colour: about 150 ms;
- sharpen: about 160 ms;
- every other effect: under 80 ms.

Run the [`bench_effects`](#bench_effects) example to time them on your machine.

### Print Layout (PDF)

**Kind:** `print_layout` · **Inputs:** `images` (any number), `pdfs` (any
number) · **Output:** `pdf`

Places every connected image, and every non-blank page of every connected
PDF, onto as few sheets as possible at real size, with cutting lines.

- **Pieces:**
  - each connected image is one piece (`items[i]` configures the i-th image,
    in connection order);
  - each non-blank page of each connected PDF is one piece (`pdf_items[i]`
    configures all pages of the i-th PDF).
- **Packing:**
  - uses the MaxRects algorithm;
  - tries both page orientations and two placement strategies (tidy rows vs.
    densest);
  - turns pieces 90° where that saves paper;
  - keeps the result with the fewest sheets and the largest single leftover
    area.
- **PDF pieces stay vector art:** each page is embedded once as a reusable
  object and drawn once per copy, so 7 copies of a 2,300-path PCB layout take
  about as much space as one.
- **Images are stored once** however many copies are printed, and are reduced
  to `image_dpi`.

**Per piece** (`items[]` / `pdf_items[]`):

| Setting | Default | Meaning |
|---|---|---|
| `copies` | `1` | Number of copies (0 = skip) |
| `fill` | `false` | **Fill sheet**: add copies until another sheet would be needed (at least `copies`) |
| `size` | `{"mode":"from_dpi","fallback_dpi":300}` | **Real size**: from the image's DPI (or `fallback_dpi` if it has none); for PDF art, its size in the PDF |
| | `{"mode":"width","width_mm":…}` | Given width; height follows the proportions |
| | `{"mode":"exact","width_mm":…,"height_mm":…}` | Exactly this size (stretches if proportions differ) |

Size presets in the GUI: ID card / credit card 85.6 × 54, passport photo
35 × 45, business card 85 × 55, photo 10 × 15 cm, A6, A5, A4.

**Sheet and output:**

| Setting | Default | Meaning |
|---|---|---|
| `paper` | `"a4"` | `a4`, `a3`, `a5`, `letter`, `legal`, `custom` |
| `custom_paper_mm` | `[210, 297]` | Paper size for `custom` (width, height) |
| `orientation` | `"auto"` | `auto` (fewest sheets), `portrait`, `landscape` |
| `margin_mm` | `10` | Unprinted border |
| `gap_mm` | `4` | Space between pieces, for cutting |
| `allow_rotation` | `true` | Allow turning pieces by 90° |
| `cut_marks` | `"outline"` | `outline` (dashed line around each piece), `crop_marks` (corner marks), `none` |
| `image_dpi` | `300` | Maximum resolution of embedded images |
| `jpeg_quality` | `90` | JPEG quality of embedded images; `null` = lossless |
| `path` | `""` | Save the PDF here; nothing is written while empty |
| `auto_save` | `true` | Save on every change. When off, use **Save PDF now** |

Pieces too big for the paper are shrunk to fit, and the summary says so.

### PDF Input

**Kind:** `pdf_input` · **Outputs:** `pdf`, plus one image port per selected
embedded image

Loads a PDF, finds the **bounding box of the actual content** on every page,
and by default crops the pages to it. The output can go straight into Print
Layout's `pdfs` input to multiply the artwork.

| Setting | Default | Meaning |
|---|---|---|
| `path` | — | PDF file |
| `crop_to_content` | `true` | Set each page's MediaBox/CropBox to its content box. Drawing commands are untouched, so the art stays vector. Off = the file passes through unchanged |
| `bounds` | `"content"` | `content` (detected), `crop_box` (visible page), `media_box` (full page) |
| `padding_mm` | `0` | Extra space around the detected content (kept within the page) |
| `ignore_white_fills` | `true` | Plain-white filled shapes (e.g. full-page backgrounds) don't count as content |
| `selected_images` | `[]` | Keys of embedded images to output (see below) |

**How the content box is found.** This builds on the approach in
`reference-code/pdf-art-grid.rs`: walk the content stream, track the
transformation matrix, and count a path only when it is actually painted. On
top of that it handles:
- embedded forms (followed recursively) and images;
- exact curve bounds;
- stroke widths;
- text, measured from the font's glyph widths;
- clipping paths and the CropBox;
- page rotation and page sizes inherited from parent pages.

Against rendered output, vector and image edges agree to within about 0.3 pt
(0.1 mm). Edges formed by text can be a few points roomier, because the box
allows for the font's full height; it never cuts into the art.

**Encrypted PDFs** that open without a password are cropped into a plain,
readable copy.

**Embedded images.** The inspector shows a thumbnail of every image in the PDF
with a checkbox.
- **Outputs:** each ticked image becomes its own output port (named like
  `p1 Im0`: page and image name). Connect it to Enhance Image, Output Image or
  Print Layout.
- **Formats:** images are decoded with `hayro`: JPEG, Flate/LZW,
  CCITT/JBIG2 (scanner PDFs), JPEG 2000, ICC/Indexed/CMYK/spot colours,
  transparency (soft masks).
- **Orientation and size:** images come out as they appear on the page
  (placement turn or mirror and page `/Rotate` applied). Their DPI comes from
  the size they're printed at, so they keep their real size when laid out
  again.

### Groups

**Kinds:** `group`, `group_input`, `group_output`

The engine supports nesting a whole graph inside one node. The group's ports
are the `group_input`/`group_output` nodes inside it. Groups save, load and
evaluate (with their own cache), but they can't yet be created or edited on
the canvas. They're hidden from the Add menu and edited as JSON in the
inspector.

---

## Recipes

### Clean up a phone photo of a document

**Input Image → Enhance Image → Output Image**

Default Enhance steps: Crop to document → Magic colour → Sharpen. For text
documents, use the *Document scan (black & white)* preset. Save as PNG for
crisp text, or JPEG for smaller files.

### Print ID card copies on A4

**Input Image → Enhance Image → Print Layout**

In Print Layout set the piece size to *Presets → ID card (85.6 × 54)*, set
the copies, or tick **Fill sheet** (9 cards fit on A4 with the default 10 mm
margins and 4 mm gaps). Use *Cutting lines → Corner crop marks* for a
guillotine.

### Front and back of a card, plus a document, on one sheet

Connect two Enhance chains (front and back of a card) plus a full-page
document into the same Print Layout. It works out whether everything fits on
one sheet and packs it together.

### Multiply vector artwork (PCB layouts, labels, stickers)

**PDF Input → Print Layout (`pdfs` input)**

1. PDF Input crops the page to the artwork. Check the size in the preview
   summary, e.g. *"cropped to content 99.6 × 54.8 mm"*.
2. In Print Layout tick **Fill sheet** under *PDF artwork*. The artwork stays
   vector and is printed 1:1.

`reference-code/Simple SMPS.pdf` gives 7 copies on landscape A4 in a 36 KB
file. Use *Cutting lines → None* if the art has its own outline.

### Enhance images embedded in a PDF

**PDF Input (tick images) → Enhance Image → Print Layout / Output Image**

For example, a scanned PDF from a phone app: tick the page images, connect
each `p1 …` port to an Enhance chain, and lay the results out again at their
original printed size.

### Re-run a pipeline without the GUI

Save the project, then:

```sh
cargo run --release -p docforge-nodes --example run_graph -- path/to/project.json
```

---

## Command-line tools

The `docforge-nodes` crate ships with small command-line programs (Cargo
examples). Run them with:

```sh
cargo run --release -p docforge-nodes --example <name> -- <arguments>
```

Built binaries end up in `target/release/examples/<name>`.

| Example | Usage | Purpose |
|---|---|---|
| `run_graph` | `run_graph <project.json>` | Run a saved project headlessly |
| `convert` | `convert <input> <output> [dpi] [project.json]` | Convert an image, optionally set its DPI, optionally save the pipeline |
| `inspect_image` | `inspect_image <image> [dpi]` | Show size, DPI and physical size, and demonstrate caching |
| `detect_page` | `detect_page <photo>` | Print the page corners *Crop to document* finds |
| `bench_effects` | `bench_effects [image]` | Time every Enhance effect (12 MP synthetic page by default) |
| `pdf_bounds` | `pdf_bounds <file.pdf> [cropped.pdf]` | Show each page's content box and images; optionally save the cropped PDF |
| `pdf_images` | `pdf_images <file.pdf> [out-dir]` | List embedded images; optionally extract all of them as PNG |

### run_graph

```sh
cargo run --release -p docforge-nodes --example run_graph -- project.json
```

Loads the project and evaluates every node; Output Image and Print Layout nodes
write their files. Relative paths resolve against the project's folder. It
prints each node's status and exits with code **1** if any node failed:

```
evaluated 5 nodes in 488.4ms
  #0 input_image    ok
  #1 enhance_image  ok
  #2 output_image   ok
  #3 pdf_input      ok
  #4 print_layout   ok
```

### convert

```sh
cargo run --release -p docforge-nodes --example convert -- photo.jpg out/photo.png 300 project.json
```

Converts between formats (chosen from the output extension), sets the DPI if
given, and saves the two-node pipeline as `project.json` for later use with
`run_graph`.

### inspect_image

```sh
cargo run --release -p docforge-nodes --example inspect_image -- scan.jpg
```

Prints pixel size, DPI and physical size in mm, then shows that re-evaluating
an unchanged graph is served from cache. It writes a small JSON file to your
temp folder.

### detect_page

```sh
cargo run --release -p docforge-nodes --example detect_page -- photo.jpg
```

Prints the four page corners (in pixels) that *Crop to document* would use.
If detection fails, enter the corners manually in the effect.

### bench_effects

```sh
cargo run --release -p docforge-nodes --example bench_effects            # synthetic 12 MP page
cargo run --release -p docforge-nodes --example bench_effects -- my.jpg  # your image
```

### pdf_bounds

```sh
cargo run --release -p docforge-nodes --example pdf_bounds -- art.pdf
cargo run --release -p docforge-nodes --example pdf_bounds -- art.pdf art-cropped.pdf
```

Example output:

```
PDF 1 page · cropped to content 99.6 × 54.8 mm (was at 51.9, 232.3 mm on the page) · 2314 paths

page 1: crop box [147.22775, 658.5102, 429.62735, 813.9048] pt, rotation 0°
  content: [147.23, 658.51, 429.63, 813.90] pt = 99.62 × 54.82 mm at (51.94, 232.31) mm
```

### pdf_images

```sh
cargo run --release -p docforge-nodes --example pdf_images -- scan.pdf             # list
cargo run --release -p docforge-nodes --example pdf_images -- scan.pdf extracted/  # extract all
```

```
1 image(s), 0 inline (not extractable)
  p1 image     page 1   2400 × 3000 px  DCTDecode
  saved extracted/p1_image.png (2400 × 3000 px, 300 dpi) in 203ms
```

---

## Project files

A project is a versioned JSON document listing nodes and the connections
between them.

```json
{
  "version": 1,
  "nodes": [
    { "id": 0, "kind": "input_image",
      "params": { "path": "scan.jpg" },
      "meta": { "position": [0, 40] } },
    { "id": 1, "kind": "enhance_image",
      "params": { "steps": [
        { "effect": "detect_document" },
        { "effect": "magic_color", "strength": 0.7 },
        { "effect": "sharpen", "factor": 1.5 },
        { "effect": "watermark", "text": "COPY", "tiled": true, "size": 0.06 },
        { "effect": "black_and_white", "enabled": false }
      ] },
      "meta": { "position": [240, 40], "label": "Clean-up" } },
    { "id": 2, "kind": "print_layout",
      "params": {
        "items": [ { "size": { "mode": "exact", "width_mm": 85.6, "height_mm": 54.0 }, "copies": 2 } ],
        "cut_marks": "crop_marks",
        "path": "out/cards.pdf" },
      "meta": { "position": [480, 40] } }
  ],
  "edges": [
    { "from": { "node": 0, "port": "image" }, "to": { "node": 1, "port": "image" } },
    { "from": { "node": 1, "port": "image" }, "to": { "node": 2, "port": "images" } }
  ]
}
```

- **Defaults:** any setting may be left out and takes its default. Omitting
  `params` entirely gives a node with all defaults.
- **Validation on load:** every connection is checked (port names, port types,
  no cycles), so a hand-edited file can't create an invalid pipeline.
- **`meta`:** holds the editor position and an optional label. It never
  affects results.
- **Ports per node:**

| Kind | Inputs | Outputs |
|---|---|---|
| `input_image` | — | `image` |
| `output_image` | `image` | `image` |
| `enhance_image` | `image` | `image` |
| `print_layout` | `images` (multi), `pdfs` (multi) | `pdf` |
| `pdf_input` | — | `pdf`, + one per `selected_images` key |
| `group_input` | — | `value` |
| `group_output` | `value` | `value` |

---

## Using docforge as a library

The engine (`docforge-core`) and the tools (`docforge-nodes`) can be used
without the GUI:

```rust
use docforge_core::{EvalContext, Graph};
use docforge_nodes::{
    EnhanceImage, InputImage, OutputImage, PdfInput, PrintLayout,
    enhance::{Effect, Watermark},
    print::{ItemSize, PrintItem},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut graph = Graph::new();

    // Load a phone photo of a document.
    let scan = graph.add(InputImage::new("scan.jpg"));

    // Crop to document → magic colour → sharpen (the default), plus a watermark.
    let mut enhance = EnhanceImage::default();
    enhance.steps.push(
        Effect::Watermark(Watermark { text: "COPY".into(), ..Watermark::default() }).into(),
    );
    let enhance = graph.add(enhance);

    // Save the cleaned-up scan.
    let png = graph.add(OutputImage::new("out/scan.png"));

    // Vector artwork from a PDF, cropped to its content.
    let art = graph.add(PdfInput::new("label.pdf"));

    // Two ID-card-sized copies of the scan + as many copies of the art as fit.
    let mut layout = PrintLayout::default();
    layout.items = vec![PrintItem {
        size: ItemSize::Exact { width_mm: 85.6, height_mm: 54.0 },
        copies: 2,
        ..PrintItem::default()
    }];
    layout.pdf_items = vec![PrintItem { fill: true, ..PrintItem::default() }];
    layout.path = "out/print.pdf".into();
    let print = graph.add(layout);

    graph.connect(scan, "image", enhance, "image")?;
    graph.connect(enhance, "image", png, "image")?;
    graph.connect(enhance, "image", print, "images")?;
    graph.connect(art, "pdf", print, "pdfs")?;

    let report = graph.evaluate(&EvalContext::new().with_base_dir("."));
    for (id, err) in &report.failed {
        eprintln!("node {id} failed: {err}");
    }
    graph.save("project.json")?; // re-run later with `run_graph`
    Ok(())
}
```

Useful entry points:

| API | Purpose |
|---|---|
| `Graph::{add, connect, disconnect, edit_node, remove_node}` | Build and change a pipeline |
| `Graph::evaluate` / `evaluate_for(&[ids])` | Run everything / only what certain nodes need (cached) |
| `Graph::{output, outputs, status, failures}` | Read results and errors |
| `Graph::{save, load, to_json, from_json}` | Projects (loading needs `docforge_nodes::registry()`) |
| `Graph::sync_from_doc` | Update a graph from a snapshot, keeping the caches of unchanged nodes |
| `InputImage::load`, `OutputImage::save`, `EnhanceImage::process`, `PrintLayout::build_with_pdfs`, `PdfInput::load` | Use a tool directly, without a graph |
| `docforge_nodes::pdf_input::images::{list, extract}` | List and decode images embedded in a PDF |

### Adding a new tool

1. In `docforge-nodes`, define a struct with `#[derive(Serialize, Deserialize, Default)]`
   holding its settings.
2. Implement `Node`: `kind`, `inputs`, `outputs` and `evaluate`, and
   optionally `needs_refresh` (react to outside changes) and `inherit_state`
   (keep internal caches across edits).
3. Implement `NodeType`: `KIND`, `DISPLAY_NAME` and `CATEGORY`.
4. Register it in `register_all` (`crates/docforge-nodes/src/lib.rs`).

It then appears in the app's Add menu, saves and loads in projects, and gets a
generic JSON settings editor. For a proper settings panel, add a case to
`inspector::node_params` in `crates/docforge-app/src/inspector.rs`.

---

## Architecture

```
crates/
├── docforge-core    Engine: Node trait, Graph (wiring, cached evaluation,
│                    cycle checks, persistence), RasterImage, PdfDocument,
│                    NodeRegistry, groups
├── docforge-nodes   The tools: input_image, output_image, enhance (filters,
│                    document detection, overlays), print (packing, PDF
│                    writer, vector art), pdf_input (content analysis,
│                    image extraction)
└── docforge-app     egui/eframe GUI: canvas, inspector, image/PDF preview,
                     background evaluator, thumbnails
reference-code/      Original grid tool and sample PDFs
```

- **Data model:** values flow between nodes as `Arc<RasterImage>` (RGBA8 plus
  DPI) or `Arc<PdfDocument>` (PDF bytes plus page analysis). Fan-out never
  copies pixels.
- **Caching:** each node remembers the versions of the inputs it was computed
  from. Editing a setting recomputes that node and everything downstream, and
  nothing else. Enhance Image also caches every step.
- **Error isolation:** a failing or panicking node is reported, and the nodes
  after it wait; the app keeps running.
- **Responsive GUI:** the UI owns the project and sends snapshots to a
  background evaluator, which syncs its own graph (keeping caches) and posts
  results back. A newer edit cancels work in progress. PDF pages and
  thumbnails are rendered on their own threads.
- **PDF output:** written with `lopdf`. Images are stored once per piece, and
  PDF artwork is embedded once as a reusable vector object (Form XObject).
- **PDF input:** a small content-stream interpreter measures what is painted
  (see [PDF Input](#pdf-input)). Embedded images are decoded by rendering each
  one 1:1 with `hayro`.

---

## Development and testing

```sh
cargo test --workspace                          # all tests (~120)
cargo test -p docforge-nodes pdf_input          # one area
cargo clippy --workspace --all-targets          # lints (kept warning-free)
cargo fmt --all                                 # formatting
```

**Test fixtures** live in `crates/docforge-nodes/tests/fixtures/`:

| File | Made with | Exercises |
|---|---|---|
| `badge.pdf` (+ `badge.svg`) | Inkscape | Transformed group, thick strokes, curves, embedded-font text, clip path, white background |
| `cards.pdf` | Cairo | Rotated and scaled embedded photo, dashed circle, text, blank page |
| `simple-smps.pdf` | Foxit PDF printer | Real PCB layout (2,300+ paths), the reference grid tool's input |

The PDF tests render pages with `hayro` and compare against the measured
content boxes, so the geometry is checked against a real renderer rather than
only against hand-calculated numbers.

### Automated GUI screenshots

The app can capture itself for automated checks:

```sh
DOCFORGE_SCREENSHOT=shot.png DOCFORGE_SELECT=1 ./target/release/docforge project.json
```

- `DOCFORGE_SCREENSHOT`: once evaluation, PDF rendering and thumbnails have
  finished, save a PNG of the window and exit.
- `DOCFORGE_SELECT`: select the node with this id first, so its panel and
  preview are shown.

---

## Troubleshooting

| Problem | Solution |
|---|---|
| **Open/Save dialogs don't appear (Linux)** | Install and run an XDG desktop portal (`xdg-desktop-portal` plus the `-kde` or `-gnome` backend). You can still type paths into the path fields |
| **Window doesn't redraw (Wayland)** | Wayland pauses drawing for windows that aren't visible, e.g. when the display is asleep. For headless or automated runs, force X11: `env -u WAYLAND_DISPLAY ./target/release/docforge` |
| **A node is red** | Hover it on the canvas, or select it: the error is shown in the inspector and the preview |
| **"required input `images` or `pdfs` is not connected"** | Print Layout needs at least one image or PDF connected |
| **Image prints at the wrong size** | It probably has no DPI. Set `dpi` in Input Image, use *Set width*/*Exact size* in Print Layout, or check the orange note in the PDF preview ("assumed 300 dpi") |
| **Crop to document picks the wrong area** | Low contrast between page and background (e.g. white paper on a white table). Tick *Set manually* and enter the corners, or use Auto-crop border/Crop |
| **PDF content box includes a background** | The background isn't plain white. Use `padding_mm`/`bounds`, or switch `crop_to_content` off |
| **"image `p1 Im0` is not in this PDF"** | The PDF changed and that image is gone. Untick it in PDF Input |
| **Slow processing** | Use a release build (`--release`). Very large photos are slower; Enhance only re-runs the steps after the one you edit |

---

## Known limitations

- **Groups** can't yet be created or edited on the canvas.
- **Inline images** in PDFs (images written directly into the page content,
  typically tiny icons) are counted but not extracted.
- **Text content boxes** use the font's full height, so they can be a few
  points roomier than the visible glyphs.
- **Miter joins:** the content box widens strokes by half their width; very
  sharp miter joins on thick strokes can stick out slightly beyond it.
- **Exact size** stretches a piece whose proportions differ from the target
  size. Crop first to avoid distortion.
- **No undo/redo** in the editor yet.
- **TIFF output** doesn't record DPI (PNG and JPEG do).
