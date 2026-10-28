#!/usr/bin/env bash
# Builds docforge for distribution into dist/.
#
#   scripts/build.sh [desktop|windows|macos|web|all]...   (default: all)
#
#   desktop  The desktop app for this computer       → dist/<os>-<arch>/
#   windows  The Windows app (x86_64), from any OS    → dist/windows-x86_64/
#   macos    The macOS app (Apple silicon and Intel); only on a Mac
#                                                    → dist/macos-<arch>/
#   web      The browser version: a SIMD build plus a fallback for older
#            browsers, chosen when the page loads   → dist/web/
#   all      desktop, windows and web (skipping what this computer can't build)
#
# Needs: Rust (rustup). Windows from Linux/macOS: either the MinGW-w64
# toolchain (x86_64-w64-mingw32-gcc) or `cross` with Docker/Podman running.
# Web: `rustup target add wasm32-unknown-unknown` and `cargo install trunk`.
# Set DIST to change the output folder.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIST=${DIST:-$ROOT/dist}
cd "$ROOT"

bold() { printf '\n\033[1m%s\033[0m\n' "$*"; }
fail() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; return 1; }
have() { command -v "$1" >/dev/null 2>&1; }

host_os() {
    case "$(uname -s)" in
        Linux) echo linux ;;
        Darwin) echo macos ;;
        MINGW* | MSYS* | CYGWIN*) echo windows ;;
        *) uname -s | tr '[:upper:]' '[:lower:]' ;;
    esac
}

host_arch() {
    case "$(uname -m)" in
        x86_64 | amd64) echo x86_64 ;;
        arm64 | aarch64) echo aarch64 ;;
        *) uname -m ;;
    esac
}

# Copies a built binary into dist/<name>/.
publish() { # <binary> <name>
    mkdir -p "$DIST/$2"
    cp "$1" "$DIST/$2/"
    echo "→ $DIST/$2/$(basename "$1")"
}

build_desktop() {
    local os arch exe=""
    os=$(host_os)
    arch=$(host_arch)
    [ "$os" = windows ] && exe=.exe
    bold "Desktop app for this computer ($os-$arch)"
    cargo build --release -p docforge-app
    publish "target/release/docforge$exe" "$os-$arch"
}

build_windows() {
    local target=x86_64-pc-windows-gnu
    bold "Windows app ($target)"
    if [ "$(host_os)" = windows ]; then
        build_desktop
        return
    fi
    rustup target add "$target" >/dev/null
    if have x86_64-w64-mingw32-gcc; then
        CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
            cargo build --release -p docforge-app --target "$target"
    elif have cross && { docker info >/dev/null 2>&1 || podman info >/dev/null 2>&1; }; then
        echo "(building in a container with cross)"
        cross build --release -p docforge-app --target "$target"
    else
        fail "building for Windows needs the MinGW-w64 toolchain (x86_64-w64-mingw32-gcc) or 'cross' with Docker/Podman running"
        return 1
    fi
    publish "target/$target/release/docforge.exe" windows-x86_64
}

build_macos() {
    bold "macOS app"
    if [ "$(host_os)" != macos ]; then
        fail "macOS apps can only be built on a Mac (Apple's SDK is required)"
        return 1
    fi
    local target
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
        rustup target add "$target" >/dev/null
        cargo build --release -p docforge-app --target "$target"
        publish "target/$target/release/docforge" "macos-${target%%-*}"
    done
}

# One browser build: dist/web/<name>/docforge.js + docforge_bg.wasm.
build_web_variant() { # <name> <extra rustflags>
    local name=$1 flags=$2
    echo "-- $name build ${flags:+($flags)}"
    # Separate target folders: the two builds use different compiler flags,
    # so sharing one would rebuild everything each time.
    (
        cd crates/docforge-app
        RUSTFLAGS="${RUSTFLAGS:-} $flags" CARGO_TARGET_DIR="$ROOT/target/wasm-$name" \
            trunk build --release --dist "$ROOT/target/web-$name"
    )
    mkdir -p "$DIST/web/$name"
    cp "target/web-$name/docforge.js" "target/web-$name/docforge_bg.wasm" "$DIST/web/$name/"
}

build_web() {
    bold "Web app (browser)"
    have trunk || { fail "needs trunk: cargo install trunk --locked"; return 1; }
    rustup target list --installed | grep -qx wasm32-unknown-unknown \
        || rustup target add wasm32-unknown-unknown
    rm -rf "$DIST/web"
    build_web_variant simd "-C target-feature=+simd128"
    build_web_variant base ""
    cp crates/docforge-app/web/index.html "$DIST/web/index.html"
    local f
    for f in "$DIST"/web/*/docforge_bg.wasm; do
        printf '   %-7s %5d KB (%d KB gzip)\n' "$(basename "$(dirname "$f")")" \
            $(($(wc -c <"$f") / 1024)) $(($(gzip -9 -c "$f" | wc -c) / 1024))
    done
    echo "→ $DIST/web/  (put the whole folder on any static web server)"
}

main() {
    local targets=("$@") failed=()
    [ ${#targets[@]} -eq 0 ] && targets=(all)
    local t
    for t in "${targets[@]}"; do
        case "$t" in
            desktop) build_desktop || failed+=(desktop) ;;
            windows) build_windows || failed+=(windows) ;;
            macos) build_macos || failed+=(macos) ;;
            web) build_web || failed+=(web) ;;
            all)
                build_desktop || failed+=(desktop)
                [ "$(host_os)" != windows ] && { build_windows || failed+=(windows); }
                build_web || failed+=(web)
                ;;
            -h | --help | help)
                # The comment block at the top of this file.
                awk 'NR > 1 && !/^#/ { exit } NR > 1 { sub(/^# ?/, ""); print }' "$0"
                return 0
                ;;
            *) fail "unknown target '$t' (try: desktop windows macos web all)" ;;
        esac
    done
    if [ ${#failed[@]} -gt 0 ]; then
        printf '\n\033[31mNot built:\033[0m %s\n' "${failed[*]}"
        return 1
    fi
    printf '\nDone. Everything is in %s\n' "$DIST"
}

main "$@"
