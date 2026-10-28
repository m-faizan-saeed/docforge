# docforge

A node-based desktop tool, written in Rust, for cleaning up scanned documents
and photos and producing printable PDFs.

You build a pipeline by wiring small tools (nodes) together: load an image or
PDF, apply CamScanner-style effects, and pack the results onto paper at their
real size with cutting lines. The preview updates as you edit. Pipelines save
as JSON project files that can be re-run from the command line.

**Highlights**

- **Seven tools**:
  - Input Image and PDF Input load files.
  - Enhance Image applies document effects and overlays.
  - PDF Edit crops PDF pages and changes the colours of vector graphics.
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
  - crops pages to that box, or to a box you drag on the preview;
  - recolours vector graphics (e.g. PCB assembly guides: top layer black,
    bottom layer gray), still as vectors;
  - multiplies vector artwork on a sheet without rasterising it;
  - extracts embedded images for enhancement.
- **Pure Rust**: `image` for raster files, `lopdf` for writing PDFs, `hayro`
  for rendering them, `egui`/`eframe` for the GUI.

---

## Contents

1. [Requirements and building](#requirements-and-building) ·
   [In a web browser](#in-a-web-browser)
2. [Quick start](#quick-start)
3. [The desktop app](#the-desktop-app)
4. [Tool reference](#tool-reference)
   - [Input Image](#input-image) · [Output Image](#output-image) ·
     [Enhance Image](#enhance-image) · [Print Layout (PDF)](#print-layout-pdf) ·
     [PDF Input](#pdf-input) · [PDF Edit](#pdf-edit) ·
     [PDF Overlay](#pdf-overlay) · [Groups](#groups)
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

### In a web browser

The same app also runs in a web page (WebAssembly). Every tool works there,
and the desktop build is unaffected: the browser-specific parts are only
compiled for the `wasm32` target.

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked

cd crates/docforge-app
trunk serve --release          # build and serve on http://127.0.0.1:8080
trunk build --release          # or only build, into target/web/
```

`target/web/` (an `index.html`, a `.js` file and a ~16 MB `.wasm` file, about
5 MB gzip-compressed) can be put on any static web server. The web build uses
its own Cargo profile, `web-release` (link-time optimisation, stripped), so
desktop release builds are untouched. For a smaller `.wasm` file, install
binaryen (`wasm-opt`) and set `data-wasm-opt="3"` in
`crates/docforge-app/index.html`.

**Opening a project from the address.** `index.html?open=<url>` loads a
project. It also loads the files its nodes read (each `path`/`font`
setting), resolved relative to the project like on desktop. For example,
serve the repository with `python3 test-projects/serve.py` and open
<http://localhost:8000/target/web/index.html?open=../../test-projects/pcb-overlay.json>.

**What is different in the browser:**

| | Desktop | Browser |
|---|---|---|
| Files | Read and written on disk by path | Kept in memory for the tab. **Browse…**, **File → Open** and drag-and-drop copy a file in; nodes then find it by path. A project's relative paths also find a file opened from anywhere if the name matches one file |
| Saving | Files are written; auto-save on every change | **Save now** / **Save PDF now** download the file; no auto-save. **File → Download project** saves the project |
| Processing | On background threads; the UI never waits | On the page's only thread, between frames: the page pauses while a step runs (it shows "Working…" first). Dragging a slider updates the preview when you pause |
| Speed | Full speed, multi-core | Roughly 2× slower and single-core (e.g. the Enhance test project: 186 ms vs 392 ms) |
| Projects that read files | Load at once | Open their input files again in the tab (or use `?open=`) |
| A crashing node | Reported; the app keeps running | Stops the page (reload it) |

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
| Select several | **Ctrl/Shift-click** to add or remove nodes, **Shift-drag** on empty canvas for a selection box (add **Ctrl** to keep the current selection), or **Ctrl+A** |
| Move nodes | Drag a node; dragging a selected node moves the whole selection |
| Duplicate | **Ctrl+D**, or right-click a node → *Duplicate*. Copies keep the wires among them and the wires feeding them, so a duplicated chain receives the same input immediately |
| Copy / cut / paste | **Ctrl+C / Ctrl+X / Ctrl+V** (also in the **Edit** menu). Nodes travel through the system clipboard as JSON, so they paste into another project or another docforge window; pasted nodes are not reconnected to outside nodes |
| Clone the whole graph | **Ctrl+A**, then **Ctrl+D** (or copy into another project) |
| Connect | Drag from an output port (right side) to an input port (left side), or the other way round |
| Detach a wire | Drag a connected input port away; drop it on another port to reconnect it, or anywhere else to remove it |
| Disconnect | Right-click an input port |
| Delete nodes | Select them and press **Delete** or **Backspace**, or right-click → *Delete* |
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
| **Output / Input** | Before/after: show what the node produces or what it receives (images and PDFs) |
| **📌 Pin** | Keep previewing this node while you select and edit others |

The toolbar shows the pixel size, DPI and physical size in mm, plus the
coordinates and RGBA value of the pixel under the cursor.

- **PDF documents** get page navigation (◀ ▶) and a summary line, for example
  *"7 pieces on 1 A4 page (landscape) · 61% of the paper used"*. Warnings
  appear below it in orange.
- **Zoom is kept** when you switch nodes or Output/Input, for comparing
  details. Entering or leaving editing on the preview (✥) fits the view
  again. If a smaller picture would end up outside the view at the kept
  zoom, it is centred.
- **Sharpness when zoomed:** pages re-render at higher resolution in the
  background as you zoom in, so vector art stays sharp.

### Editing positions and bounds on the preview

Settings that are positions or areas can be edited by dragging handles on
the preview instead of typing numbers. In Enhance Image, click **✥** on a
step's card:

| Step | Handles | Shown on |
|---|---|---|
| Crop to document | The four page corners. While detection is automatic, the detected page is shown dashed; dragging a corner switches to manual corners | The image the step receives |
| Crop | A crop box with 8 resize grips; drag inside to move it. The area that will be cut away is dimmed | The image the step receives |
| Rectangle | Resize grips and drag-to-move | The step's result |
| Line | Both ends, plus a middle grip that moves the whole line | The step's result |
| Text watermark | A cross at the text's centre | The step's result |

- **Which image:** handles are always drawn on the exact image that step works
  on, so they line up even after earlier crops or rotations.
- **Live updates:** drag at any zoom; only that step and the steps after it
  are recomputed while you drag.
- **Leaving edit mode:** press **Done** or **Esc**, or select another node.

The PDF tools work the same way:

| Tool | Click | Handles |
|---|---|---|
| PDF Edit | **✥ Adjust on preview** | A crop box on the *incoming* page |
| PDF Overlay | **✥** on a layer's card | The layer's outline on the stacked page; drag it to nudge the layer (other layers are dashed) |

### Files, projects and shortcuts

| Shortcut | Action |
|---|---|
| **Ctrl+N** | New project (starter pipeline) |
| **Ctrl+O** | Open project |
| **Ctrl+S** | Save project |
| **Ctrl+Shift+S** | Save project as… |
| **Ctrl+D** | Duplicate the selected nodes |
| **Ctrl+C / Ctrl+X / Ctrl+V** | Copy / cut / paste nodes |
| **Ctrl+A** | Select all nodes |
| **Delete / Backspace** | Delete the selected nodes |
| **Esc** | Leave preview edit mode |

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

Loads a PDF and passes it on **unchanged**, with a description of every page
(size, rotation, embedded images) and any embedded images you tick. To crop
it or change its colours, connect a [PDF Edit](#pdf-edit) node.

| Setting | Default | Meaning |
|---|---|---|
| `path` | — | PDF file (reloads automatically when it changes on disk) |
| `selected_images` | `[]` | Keys of embedded images to output (see below) |

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

> **Older projects:** PDF Input used to crop pages itself. Projects saved
> back then are updated automatically when loaded: a PDF Edit node with the
> same crop settings is inserted after the PDF Input, so they produce the
> same result.

### PDF Edit

**Kind:** `pdf_edit` · **Input:** `pdf` · **Output:** `pdf`

Crops pages and changes the colours of vector graphics. The result is still a
vector PDF: cropping only changes the page boxes, and recolouring rewrites the
colour instructions without rasterising anything.

**Crop**

| Setting | Default | Meaning |
|---|---|---|
| `crop` | `"content"` | `content` (detected artwork), `manual` (`manual_box`), `none` (keep the page size) |
| `padding_mm` | `0` | `content`: extra space around the artwork (kept within the page) |
| `ignore_white_fills` | `true` | `content`: plain-white filled shapes (e.g. full-page backgrounds) don't count as content |
| `manual_box` | `[0.1, 0.1, 0.9, 0.9]` | `manual`: `[left, top, right, bottom]` as fractions of each page *as displayed*; applies to every page |

- **Adjusting visually:** click **✥ Adjust on preview** to see the *incoming*
  page with a crop box to drag. With *Detected content*, the box starts at
  the detected artwork; dragging it switches to a manual box.
- **How the content box is found.** This builds on
  `reference-code/pdf-art-grid.rs`: walk the content stream, track the
  transformation matrix, and count a path only when it is actually painted.
  On top of that it handles:
  - embedded forms (followed recursively) and images;
  - exact curve bounds;
  - stroke widths;
  - text, measured from the font's glyph widths;
  - clipping paths and the CropBox;
  - page rotation and page sizes inherited from parent pages.

  Against rendered output, vector and image edges agree to within about
  0.3 pt (0.1 mm). Edges formed by text can be a few points roomier, because
  the box allows for the font's full height; it never cuts into the art.
- **Measured on the original colours,** so recolouring cannot change what
  counts as content.
- **Blank pages** (in `content` mode) are left as they are and skipped by
  Print Layout.
- **Encrypted PDFs** that open without a password are written out as a plain,
  readable copy.

**Colours** (`recolor`)

| Setting | Default | Meaning |
|---|---|---|
| `mode` | `"off"` | `off`, `single` (everything → `color`), `replace` (by `rules`), `grayscale` |
| `color` | `[0,0,0]` | `single`: the new colour |
| `keep_white` | `true` | `single`: near-white stays white (pads, holes, knock-outs) |
| `rules` | `[]` | `replace`: `{"from": [r,g,b], "to": [r,g,b]}` pairs, tried in order |
| `tolerance` | `24` | `replace`: how far (per channel, 0–255) a colour may be from `from` and still match |
| `fills` | `true` | Change fill colours (this includes most text) |
| `strokes` | `true` | Change line colours |

- **Picking colours:** in *Replace chosen colours* mode, the inspector lists
  every colour used in the incoming PDF, with a swatch and how often it is
  used. Tick a colour and pick what it becomes.
- **Presets:** *PCB top layer → black* and *PCB bottom layer → gray* (both
  keep white), *Grayscale*, *Original colours*.
- **Where colours are changed:** the colour instructions of every page,
  embedded form and coloured pattern. Gray, RGB, CMYK, ICC, Lab, palette and
  spot colours are understood.
- **Untouched colours stay exactly as they were,** including spot colours.
- **Not recoloured:** images and gradient fills (shadings); the summary says
  how many were left.

### PDF Overlay

**Kind:** `pdf_overlay` · **Input:** `pdfs` (any number) · **Output:** `pdf`

Stacks several PDFs onto one page, e.g. a gray PCB bottom layer under a
black top layer. Connect PDFs (from PDF Input or PDF Edit) to `pdfs`. The
first one connected is the bottom layer. Page *i* of the result stacks page
*i* of every layer, and a layer with fewer pages simply ends. Every layer
stays vector: it is embedded as one reusable object (Form XObject).

| Setting | Default | Meaning |
|---|---|---|
| `align` | `"original"` | `original` (each layer stays where it is on its own page), `center` (centres on the bottom layer's), `top_left` (top-left corners on the bottom layer's) |
| `page_size` | `"union"` | `union` (big enough for every layer), `bottom` (the bottom layer's page; the rest is clipped) |
| `layers` | `[]` | Settings per connected PDF, in connection order; missing entries use the defaults below |

Per layer (`layers[i]`):

| Setting | Default | Meaning |
|---|---|---|
| `visible` | `true` | Hidden layers are left out (and don't size the page) |
| `opacity` | `1.0` | 0–1 |
| `blend` | `"normal"` | `normal` (covers what is below), `multiply` (darkens; white becomes see-through), `darken` (keeps the darker colour) |
| `offset_mm` | `[0, 0]` | Extra shift, right and down, in mm |

- **Keeping layers in register.** With *Keep page positions* (`original`),
  layers exported from the same design line up exactly, even when PDF Edit
  cropped each to a different content box. Cropping only changes the page
  boxes and keeps the drawing's coordinates.
- **White backgrounds:** a layer with a white background hides everything
  below it. Set the upper layers to **Multiply**; the inspector has a button
  that does this for all of them.
- **Inspector:** the layer cards are listed top of the stack first.
  - **⏶ / ⏷** reorder the layers. This swaps the connections, and each
    layer's settings move with its PDF.
  - The checkbox shows or hides a layer.
  - **✥** lets you drag the layer on the preview. Dragging changes
    `offset_mm`; *Reset* puts it back.

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

**PDF Input → PDF Edit → Print Layout (`pdfs` input)**

1. PDF Edit crops the page to the artwork. Check the size in the preview
   summary, e.g. *"cropped to content 99.6 × 54.8 mm"*.
2. In Print Layout tick **Fill sheet** under *PDF artwork*. The artwork stays
   vector and is printed 1:1.

`reference-code/Simple SMPS.pdf` gives 7 copies on landscape A4 in a 36 KB
file. Use *Cutting lines → None* if the art has its own outline.

### PCB assembly guide (top layer black, bottom layer gray)

- **Both layers in one PDF** (e.g. red silkscreen and black tracks):
  **PDF Input → PDF Edit**. Set *Colours → Replace chosen colours*, tick the
  silkscreen colour → black and the track colour → gray. With
  `reference-code/Simple SMPS.pdf` this gives black component outlines over
  gray tracks, cropped to the board, still vector.
- **One PDF per layer:** recolour each layer with its own PDF Edit node,
  using the *PCB top layer → black* and *PCB bottom layer → gray* presets.
  Then stack them with PDF Overlay:

  ```
  PDF Input (bottom.pdf) → PDF Edit (bottom → gray) ─┐
                                                      ├→ PDF Overlay → Print Layout
  PDF Input (top.pdf)    → PDF Edit (top → black)  ──┘
  ```

  Connect the gray layer first, so it is the bottom layer, and set the top
  layer's blend to **Multiply** so its white areas don't hide the gray. Keep
  *Align layers → Keep page positions* and the layers stay in register. If
  the exports are slightly misaligned, click **✥** on the top layer and drag
  it into place.

  To stack two colourings of one PDF, connect one PDF Input to both PDF Edit
  nodes. From the command line, use the
  [`pdf_overlay`](#pdf_overlay) example.

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
| `pdf_edit` | `pdf_edit <file.pdf> [out.pdf] [top\|bottom\|gray\|none]` | List the vector colours; optionally save a copy cropped to content and recoloured with a PCB preset |
| `pdf_overlay` | `pdf_overlay <bottom.pdf> <top.pdf> <out.pdf>` | Make an assembly guide: the bottom PDF gray, the top PDF black (multiplied), both cropped to content and stacked |
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
PDF 1 page · cropped to content 99.6 × 54.8 mm

page 1: crop box [147.22775, 658.5102, 429.62735, 813.9048] pt, rotation 0°
  content: [147.23, 658.51, 429.63, 813.90] pt = 99.62 × 54.82 mm at (51.94, 232.31) mm
```

### pdf_edit

```sh
cargo run --release -p docforge-nodes --example pdf_edit -- board.pdf                  # list colours
cargo run --release -p docforge-nodes --example pdf_edit -- board.pdf top.pdf top      # black
cargo run --release -p docforge-nodes --example pdf_edit -- board.pdf bottom.pdf bottom # gray
```

```
colours used (fills / strokes):
  #ff0000       8 / 1640
  #000000     227 / 471
  #ffffff     200 / 200
  #00ff00       0 / 4
PDF 1 page · cropped to content 99.6 × 54.8 mm · 2350 colours changed
```

### pdf_overlay

```sh
cargo run --release -p docforge-nodes --example pdf_overlay -- bottom.pdf top.pdf guide.pdf
```

```
2 layers on 1 page · 99.6 × 54.8 mm
saved guide.pdf
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
| `pdf_edit` | `pdf` | `pdf` |
| `pdf_overlay` | `pdfs` (multi) | `pdf` |
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
| `Graph::{copy_nodes, paste, duplicate}` | Copy part of a graph (as a `GraphDoc`), paste it under fresh ids, or duplicate nodes with their wires; pasting is all-or-nothing |
| `Graph::probe` / `Node::probe` | Ask a node for an intermediate value from its last run, e.g. Enhance Image's `step_input/N` and `step_output/N` |
| `InputImage::load`, `OutputImage::save`, `EnhanceImage::process`, `PrintLayout::build_with_pdfs`, `PdfInput::load`, `PdfEdit::apply`, `PdfOverlay::apply` | Use a tool directly, without a graph |
| `docforge_nodes::pdf_edit::palette` | The vector colours used in a PDF |
| `PdfOverlay::layout` | Where each layer lands on each output page, without building the PDF |
| `Graph::swap_inputs` | Reorder two connections on a multi-input port |
| `NodeRegistry::register_migration` | Update old saved graphs on load (e.g. when a tool's job moves to another node) |
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
generic JSON settings editor. It can also be duplicated and copied like any
other node. For a proper settings panel, add a case to
`inspector::node_params` in `crates/docforge-app/src/inspector.rs`.

To make positions or areas editable on the preview, use the handle widgets in
`crates/docforge-app/src/handles.rs`: `point`, `line`, `rect` and `quad`.
They work in normalised image coordinates. Hook them up in
`crates/docforge-app/src/bounds.rs`. If the handles must be drawn on an
intermediate image, expose it from the node through `Node::probe`.

---

## Architecture

```
crates/
├── docforge-core    Engine: Node trait, Graph (wiring, cached evaluation,
│                    cycle checks, persistence), RasterImage, PdfDocument,
│                    NodeRegistry, groups, files (disk, or memory on the web)
├── docforge-nodes   The tools: input_image, output_image, enhance (filters,
│                    document detection, overlays), print (packing, PDF
│                    writer, vector art), pdf (content analysis, shared
│                    PDF helpers), pdf_input (loading, image extraction),
│                    pdf_edit (crop, recolour), pdf_overlay (stacking)
└── docforge-app     egui/eframe GUI: canvas, inspector, image/PDF preview,
                     background evaluator, thumbnails; web entry point,
                     index.html and Trunk.toml for the browser build
test-projects/       Projects and inputs for trying the app; test scripts
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
  thumbnails are rendered on their own threads. In a web page, which has
  no threads, the same work runs between frames (`background.rs`).
- **Files:** nodes read and write through `docforge_core::files`: thin
  wrappers around `std::fs` on desktop, an in-memory store in a web page.
- **PDF output:** written with `lopdf`. Images are stored once per piece, and
  PDF artwork is embedded once as a reusable vector object (Form XObject).
- **PDF input:** a small content-stream interpreter measures what is painted
  (see [PDF Input](#pdf-input)). Embedded images are decoded by rendering each
  one 1:1 with `hayro`.

---

## Development and testing

```sh
cargo test --workspace                          # all tests (~150)
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

### Test projects for the app

`test-projects/` holds the projects used to try every tool in the GUI, with
their inputs. Paths in them are relative, so they work from any checkout.
See [`test-projects/README.md`](test-projects/README.md).

```sh
cargo build --release
./target/release/docforge test-projects/pcb-overlay.json   # open one
test-projects/screenshots.sh                               # screenshot all → test-projects/out/
```

### Testing the web version

```sh
cd crates/docforge-app && trunk build --release && cd ../..
test-projects/web-screenshot.sh test-projects/out/web.png test-projects/pcb-overlay.json "select=3"
```

`web-screenshot.sh` serves the repository (`test-projects/serve.py`), opens
the page in headless Firefox and saves a screenshot. The page's console goes
to `<out>.png.log`. In a web page the test hooks below are page parameters
(`?screenshot=1&select=3&edit_step=1&duplicate=1`) instead of environment
variables.

### Automated GUI screenshots

The app can capture itself for automated checks:

```sh
DOCFORGE_SCREENSHOT=shot.png DOCFORGE_SELECT=1 ./target/release/docforge project.json
```

- `DOCFORGE_SCREENSHOT`: once evaluation, PDF rendering and thumbnails have
  finished, save a PNG of the window and exit.
- `DOCFORGE_SELECT`: select the node with this id first, so its panel and
  preview are shown. A comma-separated list selects several nodes.
- `DOCFORGE_DUPLICATE=1`: duplicate the selection at start-up, as Ctrl+D does.
- `DOCFORGE_EDIT_STEP=N`: open preview editing for step *N* (0-based) of the
  selected Enhance Image node, or for layer *N* of a selected PDF Overlay
  (any value opens a PDF Edit crop).

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
| **PDF content box includes a background** | The background isn't plain white. In PDF Edit use *Crop to → Manual box* (drag it with ✥ Adjust on preview) |
| **Overlay: lower layers are hidden** | The upper layer has a white background. Set its blend to *Multiply* |
| **Overlay: layers don't line up** | Use *Keep page positions* for layers exported from one design. Otherwise drag the layer with ✥ or set its offset |
| **"image `p1 Im0` is not in this PDF"** | The PDF changed and that image is gone. Untick it in PDF Input |
| **Slow processing** | Use a release build (`--release`). Very large photos are slower; Enhance only re-runs the steps after the one you edit |

---

## Known limitations

- **Groups** can't yet be created or edited on the canvas. To reuse part of a
  pipeline, copy and paste or duplicate it instead.
- **Inline images** in PDFs (images written directly into the page content,
  typically tiny icons) are counted but not extracted.
- **Recolouring** leaves images and gradient fills (shadings) in their
  colours. Spot colours (Separation/DeviceN) are matched by an approximate
  gray from their tint.
- **Text content boxes** use the font's full height, so they can be a few
  points roomier than the visible glyphs.
- **Miter joins:** the content box widens strokes by half their width; very
  sharp miter joins on thick strokes can stick out slightly beyond it.
- **Exact size** stretches a piece whose proportions differ from the target
  size. Crop first to avoid distortion.
- **No undo/redo** in the editor yet.
- **TIFF output** doesn't record DPI (PNG and JPEG do).
