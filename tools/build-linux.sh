#!/usr/bin/env bash
# Builds the Linux desktop bundles (.deb and AppImage), copies them to <repo>/releases/, and
# records their SHA-256 in releases/SHA256SUMS.txt (same "<hash>  <name>" format as
# tools/build-installer.ps1). See tasks/plan.md section 9 and docs/linux-setup.md.
#
#   bash tools/build-linux.sh --check   # list missing build dependencies, exit 1 if any
#   bash tools/build-linux.sh           # check, then build (CPU build: no CUDA on Linux)
#
# Never installs anything: when a dependency is missing it prints the apt command for you to
# run and exits. Run it in the foreground; the link step needs several GB of RAM.
#
# Environment: CARGO_TARGET_DIR (default <repo>/target), CARGO_BUILD_JOBS (default 1).
set -euo pipefail

usage() {
    sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
}

mode=build
case "${1:-}" in
    "") ;;
    --check) mode=check ;;
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
    # tauri.conf.json targets NSIS (Windows); override the bundle list on the command line.
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
