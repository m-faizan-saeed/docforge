#!/usr/bin/env bash
# Screenshots the web build in headless Firefox.
#
#   test-projects/web-screenshot.sh <out.png> [project.json] [hooks] [wait-ms]
#
# `project.json` is relative to the repository; `hooks` are extra page
# parameters, e.g. "select=3&edit_step=1" (like DOCFORGE_SELECT etc. on
# desktop). Build first: `trunk build --release` in crates/docforge-app.
set -euo pipefail
cd "$(dirname "$0")/.."
out=$(realpath -m "$1"); project=${2:-}; hooks=${3:-}; wait=${4:-30000}
port=8765
python3 test-projects/serve.py $port & server=$!
profile=$(mktemp -d)
trap 'kill $server; rm -rf "$profile" target/web/screenshot.html' EXIT
sleep 1
# A copy of the page that holds its load event back while the app runs; the
# app shows its own screenshot over the page (`screenshot` hook), which is
# what Firefox captures (it does not capture WebGL canvases).
sed "s|</body>|<img src=\"/__delay?ms=$wait\" style=\"display:none\"></body>|" \
    target/web/index.html > target/web/screenshot.html
url="http://127.0.0.1:$port/target/web/screenshot.html?screenshot=1"
[ -n "$project" ] && url="$url&open=../../$project"
[ -n "$hooks" ] && url="$url&$hooks"
# Software WebGL for the headless browser; the page's console → <out>.log.
cat > "$profile/user.js" <<'PREFS'
user_pref("webgl.force-enabled", true);
user_pref("devtools.console.stdout.content", true);
PREFS
firefox --headless --no-remote --profile "$profile" --window-size=1440,900 \
    --screenshot "$out" "$url" >"$out.log" 2>&1 || true
echo "$out"
