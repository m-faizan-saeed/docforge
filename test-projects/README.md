# Test projects

Projects for trying every tool in the app by hand, plus the inputs they use.
Paths are relative to this folder, so they work from any checkout. Outputs
(saved images and PDFs, screenshots) go to `out/`, which git ignores.

Open one with File → Open, or:

```sh
cargo build --release
./target/release/docforge test-projects/pcb-overlay.json
```

| Project | Tools | What to try |
|---|---|---|
| `enhance-edit.json` | Input Image → Enhance Image → Output Image | Each step's **✥**: page corners (Crop to document), crop box, rectangle, watermark position, line ends. Ctrl+D / Ctrl+C / Ctrl+V on nodes |
| `pdf-edit-manual-crop.json` | PDF Input → PDF Edit | Manual crop: **✥ Adjust on preview**, drag the box, then leave adjust mode; Output/Input tabs |
| `pcb-guide-one-pdf.json` | PDF Input → PDF Edit → Print Layout | Recolour by palette (red → black, black → gray), crop to content, fill an A4 sheet |
| `remove-white-overlay.json` | 2 × PDF Input → 2 × PDF Edit → PDF Overlay | **Remove** on a colour rule: the top PDF's white background is no longer drawn, so the red stripes below show through. Untick the white rule (or use Presets → Remove white background) to compare |
| `print-order.json` | Input Image → 3 × Enhance Image → Print Layout | **Order: As listed** — pieces follow the list row by row; reorder with ⏶⏷ (settings move along); switch to *Fewest sheets* to compare |
| `pcb-overlay.json` | PDF Input → 2 × PDF Edit → PDF Overlay → Print Layout | Assembly guide: gray bottom layer, black top layer (Multiply, nudged 4 × 3 mm). Reorder with ⏶⏷, drag a layer with **✥**, hide a layer, change blend/opacity |

The PCB projects use `inputs/simple-smps.pdf` (the reference PCB layout, also a test fixture).

## Inputs

| File | What it is |
|---|---|
| `inputs/photo.jpg` | A synthetic phone photo of an invoice: skewed, shaded, on a desk. Made by `inputs/make_photo.py` (Python with Pillow and NumPy): `python3 inputs/make_photo.py inputs/photo.jpg` |
| `inputs/white-background.pdf`, `inputs/red-stripes.pdf` | Small vector PDFs: black art and text on a page-sized white box, and red stripes to lay under it. Made by `inputs/make_white_bg.py`: `python3 inputs/make_white_bg.py inputs` |

## Screenshots

`screenshots.sh` opens each project in the app with the test hooks
(`DOCFORGE_SELECT`, `DOCFORGE_EDIT_STEP`, `DOCFORGE_DUPLICATE`), saves a PNG of
the window to `out/` and exits:

```sh
test-projects/screenshots.sh
```

If the window does not redraw under Wayland while the screen is off, run it
with X11: `env -u WAYLAND_DISPLAY test-projects/screenshots.sh`.

## Web version

Build the web version with `scripts/build.sh web`, then:

```sh
python3 test-projects/serve.py        # http://localhost:8000
# open http://localhost:8000/dist/web/?open=../../test-projects/pcb-overlay.json
# (add &simd=0 for the build for older browsers)

test-projects/web-screenshot.sh test-projects/out/web-overlay.png test-projects/pcb-overlay.json "select=3"
```

`web-screenshot.sh <out.png> [project] [hooks] [wait-ms]` screenshots the page
in headless Firefox. Hooks are page parameters, such as `select=1&edit_step=0`
or `simd=0`. The page's console log is saved next to the PNG, including which
build started.
