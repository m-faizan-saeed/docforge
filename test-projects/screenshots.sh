#!/usr/bin/env bash
# Opens every test project in the app and saves screenshots to out/.
# Usage: test-projects/screenshots.sh   (after `cargo build --release`)
set -euo pipefail
cd "$(dirname "$0")"
app=../target/release/docforge
mkdir -p out
shot() { # name project node [edit-step]
    local name=$1 project=$2 node=$3 step=${4:-}
    echo "out/$name.png"
    env DOCFORGE_SCREENSHOT="out/$name.png" DOCFORGE_SELECT="$node" \
        ${step:+DOCFORGE_EDIT_STEP=$step} timeout 60 "$app" "$project"
}
for step in 0 1 2 3 4; do
    shot "enhance-step-$step" enhance-edit.json 1 "$step"
done
echo out/enhance-duplicated.png # Ctrl+D on the Enhance node
DOCFORGE_DUPLICATE=1 DOCFORGE_SCREENSHOT=out/enhance-duplicated.png DOCFORGE_SELECT=1 \
    timeout 60 "$app" enhance-edit.json
shot pdf-edit-manual-output pdf-edit-manual-crop.json 1
shot pdf-edit-manual-adjust pdf-edit-manual-crop.json 1 0
shot pcb-guide-edit pcb-guide-one-pdf.json 1
shot pcb-guide-sheet pcb-guide-one-pdf.json 2
shot pcb-overlay-panel pcb-overlay.json 3
shot pcb-overlay-drag-top pcb-overlay.json 3 1
