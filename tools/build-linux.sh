#!/usr/bin/env bash
# Builds the Linux desktop bundles (.deb and AppImage), copies them to <repo>/releases/, and
# records their SHA-256 in releases/SHA256SUMS.txt (same "<hash>  <name>" format as
# tools/build-installer.ps1). See tasks/plan.md section 9 and docs/linux-setup.md.
#
#   bash tools/build-linux.sh --check   # list missing build dependencies, exit 1 if any
#   bash tools/build-linux.sh           # check, then build (CPU build: no CUDA on Linux)
#   bash tools/build-linux.sh --stage-libs   # (beforeBundleCommand) copy the sherpa-onnx .so
#
# Never installs anything: when a dependency is missing it prints the apt command for you to
# run and exits. Run it in the foreground; the link step needs several GB of RAM.
#
# Environment: CARGO_TARGET_DIR (default <repo>/target), CARGO_BUILD_JOBS (default 1).
set -euo pipefail

usage() {
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
}

# SenseVoice (sherpa-onnx, shared build) libraries the v2c binary links (readelf NEEDED).
# sherpa-onnx-sys copies them to target/<profile>/; the bundles install them to /usr/lib/v2c/
# (the binary's rpath is $ORIGIN:$ORIGIN/../lib/v2c, see apps/desktop/src-tauri/build.rs).
sherpa_libs=(libsherpa-onnx-c-api.so libonnxruntime.so)

mode=build
case "${1:-}" in
    "") ;;
    --check) mode=check ;;
    --stage-libs) mode=stage ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        echo "unknown argument: $1" >&2
        usage >&2
        exit 2
        ;;
esac

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# rustup installs to ~/.cargo/bin, which non-login shells often lack.
if [[ -d "$HOME/.cargo/bin" ]]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

# Run by `tauri build` (build.beforeBundleCommand in apps/desktop/src-tauri/tauri.linux.conf.json)
# after the binary is compiled and before the .deb/AppImage are made: copies the sherpa-onnx
# libraries to the git-ignored stage folder that bundle.linux.{deb,appimage}.files read.
# (They cannot be bundle resources: tauri-build checks resources while compiling, before
# sherpa-onnx-sys has produced them.)
stage_libs() {
    local target profile src stage lib
    target="${CARGO_TARGET_DIR:-}"
    if [[ -z "$target" ]]; then
        target="$(cargo metadata --format-version 1 --no-deps --manifest-path "$repo/Cargo.toml" |
            sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
    fi
    profile=release
    [[ "${TAURI_ENV_DEBUG:-}" == true ]] && profile=debug
    src="$target/$profile"
    if [[ -n "${TAURI_ENV_TARGET_TRIPLE:-}" && -d "$target/$TAURI_ENV_TARGET_TRIPLE/$profile" ]]; then
        src="$target/$TAURI_ENV_TARGET_TRIPLE/$profile"
    fi
    stage="$repo/apps/desktop/src-tauri/runtime-dlls"
    mkdir -p "$stage"
    for lib in "${sherpa_libs[@]}"; do
        if [[ ! -f "$src/$lib" ]]; then
            echo "stage-libs: $src/$lib not found (did the release build run?)" >&2
            return 1
        fi
        cp -f "$src/$lib" "$stage/$lib"
        echo "stage-libs: $lib -> $stage/"
    done
}

if [[ "$mode" == stage ]]; then
    stage_libs
    exit 0
fi

# pkg-config module -> Debian/Ubuntu package (Tauri v2 Linux prerequisites + cpal's ALSA).
pkg_modules=(
    "webkit2gtk-4.1:libwebkit2gtk-4.1-dev"
    "javascriptcoregtk-4.1:libwebkit2gtk-4.1-dev"
    "libsoup-3.0:libsoup-3.0-dev"
    "gtk+-3.0:libgtk-3-dev"
    "ayatana-appindicator3-0.1:libayatana-appindicator3-dev"
    "librsvg-2.0:librsvg2-dev"
    "openssl:libssl-dev"
    "alsa:libasound2-dev"
)
# command -> package. cmake builds whisper.cpp; clang/libclang are needed by bindgen.
commands=(
    "cmake:cmake"
    "clang:clang"
    "cc:build-essential"
    "file:file"
    "cargo:"
    "npm:"
    "node:"
)

missing_pkgs=()
missing_other=()

add_pkg() {
    local p
    for p in "${missing_pkgs[@]+"${missing_pkgs[@]}"}"; do
        [[ "$p" == "$1" ]] && return 0
    done
    missing_pkgs+=("$1")
}

check_deps() {
    if ! command -v pkg-config >/dev/null 2>&1; then
        echo "missing: pkg-config"
        add_pkg pkg-config
    fi
    local entry mod pkg cmd
    for entry in "${pkg_modules[@]}"; do
        mod="${entry%%:*}"
        pkg="${entry#*:}"
        if command -v pkg-config >/dev/null 2>&1 && pkg-config --exists "$mod"; then
            echo "ok:      $mod ($(pkg-config --modversion "$mod"))"
        else
            echo "missing: $mod (apt package $pkg)"
            add_pkg "$pkg"
        fi
    done
    for entry in "${commands[@]}"; do
        cmd="${entry%%:*}"
        pkg="${entry#*:}"
        if command -v "$cmd" >/dev/null 2>&1; then
            echo "ok:      $cmd"
        elif [[ -n "$pkg" ]]; then
            echo "missing: $cmd (apt package $pkg)"
            add_pkg "$pkg"
        else
            echo "missing: $cmd (install it yourself: https://rustup.rs for cargo, Node.js for npm/node)"
            missing_other+=("$cmd")
        fi
    done
    # bindgen loads libclang.so at build time; a clang binary alone is not enough.
    if [[ -n "${LIBCLANG_PATH:-}" ]] && compgen -G "$LIBCLANG_PATH/libclang*.so*" >/dev/null; then
        echo "ok:      libclang ($LIBCLANG_PATH)"
    elif compgen -G "/usr/lib/llvm-*/lib/libclang*.so*" >/dev/null ||
        compgen -G "/usr/lib/x86_64-linux-gnu/libclang*.so*" >/dev/null; then
        echo "ok:      libclang"
    else
        echo "missing: libclang (apt package libclang-dev)"
        add_pkg libclang-dev
    fi

    if ((${#missing_pkgs[@]} == 0 && ${#missing_other[@]} == 0)); then
        echo "all build dependencies found"
        return 0
    fi
    echo
    if ((${#missing_pkgs[@]} > 0)); then
        echo "Install the missing packages yourself (this script never runs sudo or apt):"
        echo "  sudo apt update && sudo apt install -y ${missing_pkgs[*]}"
    fi
    if ((${#missing_other[@]} > 0)); then
        echo "Also install: ${missing_other[*]}"
    fi
    return 1
}

if ! check_deps; then
    exit 1
fi
if [[ "$mode" == check ]]; then
    exit 0
fi

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo/target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export CMAKE_BUILD_PARALLEL_LEVEL="$CARGO_BUILD_JOBS"

(
    cd "$repo/apps/desktop"
    if [[ ! -d node_modules ]]; then
        npm ci
    fi
    # tauri.linux.conf.json (merged over tauri.conf.json on Linux) selects deb + AppImage, the
    # IBus component and the sherpa-onnx libraries; --bundles only makes the list explicit.
    npm run tauri build -- --bundles deb,appimage
)

version="$(sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$repo/apps/desktop/src-tauri/tauri.conf.json" | head -n 1)"
bundle="$CARGO_TARGET_DIR/release/bundle"
shopt -s nullglob
built=("$bundle"/deb/v2c_"${version}"_*.deb "$bundle"/appimage/v2c_"${version}"_*.AppImage)
shopt -u nullglob
if ((${#built[@]} == 0)); then
    echo "no bundles for version $version found under $bundle" >&2
    exit 1
fi

# Deliverables go to <repo>/releases (git-ignored); the target dir keeps only build caches.
releases="$repo/releases"
sums="$releases/SHA256SUMS.txt"
mkdir -p "$releases"
touch "$sums"
for f in "${built[@]}"; do
    name="$(basename "$f")"
    cp -f "$f" "$releases/$name"
    hash="$(sha256sum "$releases/$name" | cut -d' ' -f1)"
    # Replace an older line for the same file name, keep everything else.
    tmp="$(mktemp "$releases/.SHA256SUMS.XXXXXX")"
    trap 'rm -f "$tmp"' EXIT
    # (A trailing CR from a file last written on Windows is ignored when matching.)
    awk -v n="  $name" '{ l = $0; sub(/\r$/, "", l) } substr(l, length(l) - length(n) + 1) != n' \
        "$sums" >"$tmp"
    printf '%s  %s\n' "$hash" "$name" >>"$tmp"
    mv -f "$tmp" "$sums"
    printf '%s  %s MiB\nsha256 %s\n' "$releases/$name" \
        "$(awk -v b="$(stat -c %s "$releases/$name")" 'BEGIN { printf "%.1f", b / 1048576 }')" "$hash"
done
