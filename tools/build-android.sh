#!/usr/bin/env bash
# Builds the signed Android release APK (arm64-v8a), checks its 16 KB page alignment and
# signature, copies it to <repo>/releases/v2c_<versionName>_android.apk and records its SHA-256
# in releases/SHA256SUMS.txt (same "<hash>  <name>" format as tools/build-installer.ps1; the
# line for this file name is replaced, other lines are kept). Windows: tools/build-android.ps1.
# See docs/android-install.md.
#
#   bash tools/build-android.sh --check   # only check prerequisites, exit 1 if any is missing
#   bash tools/build-android.sh           # check, then build
#
# Never installs anything: when something is missing it prints the commands for you to run.
# The signing key is not created for you either; without apps/android/keystore.properties the
# script prints the keytool command and stops (no unsigned APK is produced).
#
# Environment: JAVA_HOME (JDK 17+), ANDROID_HOME or ANDROID_SDK_ROOT, ANDROID_NDK_HOME
# (default <sdk>/ndk/28.2.13676358), CARGO_TARGET_DIR (default <repo>/target).
set -euo pipefail

NDK_VERSION="28.2.13676358"
BUILD_TOOLS_VERSION="36.0.0"

usage() {
    sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
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
android="$repo/apps/android"
gradle_file="$android/app/build.gradle.kts"
keystore_props="$android/keystore.properties"

# rustup installs to ~/.cargo/bin, which non-login shells often lack.
if [[ -d "$HOME/.cargo/bin" ]]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

missing=()
add_missing() {
    missing+=("$1")
}

# --- JDK -----------------------------------------------------------------------------------
java_bin=java
if [[ -n "${JAVA_HOME:-}" && -x "$JAVA_HOME/bin/java" ]]; then
    java_bin="$JAVA_HOME/bin/java"
fi
if ! command -v "$java_bin" >/dev/null 2>&1; then
    add_missing "JDK 17 not found (set JAVA_HOME).
    Ubuntu: sudo apt install openjdk-17-jdk
    then:   export JAVA_HOME=/usr/lib/jvm/java-17-openjdk-amd64"
else
    java_major="$("$java_bin" -version 2>&1 | grep -m1 'version "' | sed 's/.*version "\([0-9]*\).*/\1/' || true)"
    if [[ -z "$java_major" || "$java_major" -lt 17 ]]; then
        add_missing "JDK 17 or newer required, found: ${java_major:-unknown}.
    Ubuntu: sudo apt install openjdk-17-jdk ; export JAVA_HOME=/usr/lib/jvm/java-17-openjdk-amd64"
    fi
fi

# --- Android SDK / build-tools / NDK ---------------------------------------------------------
sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [[ -z "$sdk" && -f "$android/local.properties" ]]; then
    sdk="$(sed -n 's/^sdk\.dir=//p' "$android/local.properties" | head -n 1)"
fi
sdkmanager_hint="Install with cmdline-tools (https://developer.android.com/studio#command-tools):
    sdkmanager \"platforms;android-37\" \"platforms;android-36\" \"build-tools;$BUILD_TOOLS_VERSION\" \"platform-tools\" \"ndk;$NDK_VERSION\"
    export ANDROID_HOME=\$HOME/Android/Sdk"
build_tools=""
ndk=""
if [[ -z "$sdk" || ! -d "$sdk" ]]; then
    add_missing "Android SDK not found (set ANDROID_HOME or ANDROID_SDK_ROOT).
    $sdkmanager_hint"
else
    export ANDROID_HOME="$sdk"
    build_tools="$sdk/build-tools/$BUILD_TOOLS_VERSION"
    if [[ ! -x "$build_tools/zipalign" || ! -x "$build_tools/apksigner" ]]; then
        add_missing "Android build-tools $BUILD_TOOLS_VERSION missing in $sdk (needs zipalign, apksigner).
    $sdkmanager_hint"
    fi
    ndk="${ANDROID_NDK_HOME:-$sdk/ndk/$NDK_VERSION}"
    if [[ ! -d "$ndk" ]]; then
        add_missing "Android NDK not found at $ndk (need $NDK_VERSION).
    $sdkmanager_hint
    export ANDROID_NDK_HOME=\$ANDROID_HOME/ndk/$NDK_VERSION"
    elif [[ "$(sed -n 's/^Pkg\.Revision *= *//p' "$ndk/source.properties" 2>/dev/null | head -n 1)" != "$NDK_VERSION" ]]; then
        echo "warning: $ndk is not NDK $NDK_VERSION; 16 KB alignment is only guaranteed with it" >&2
    fi
fi

# --- Rust ----------------------------------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
    add_missing "cargo not found. Install Rust: https://rustup.rs"
else
    if ! cargo ndk --version >/dev/null 2>&1; then
        add_missing "cargo-ndk not found. Run: cargo install cargo-ndk"
    fi
    if ! command -v rustup >/dev/null 2>&1; then
        add_missing "rustup not found (needed for the Android targets). Install Rust: https://rustup.rs"
    elif ! rustup target list --installed 2>/dev/null | grep -qx 'aarch64-linux-android'; then
        add_missing "Rust target missing. Run: rustup target add aarch64-linux-android x86_64-linux-android"
    fi
fi

# --- Signing key ---------------------------------------------------------------------------
keytool_hint() {
    cat <<EOF
    No signing key. Create one ONCE and keep it safe (losing it means users cannot update; never commit it):
      keytool -genkeypair -v -keystore "$android/v2c-release.jks" -alias v2c \\
        -keyalg RSA -keysize 4096 -validity 36500
    Then create $keystore_props (git-ignored) with:
      storeFile=v2c-release.jks
      storePassword=<the keystore password you typed>
      keyAlias=v2c
      keyPassword=<the key password you typed>
    (storeFile is relative to apps/android; use forward slashes in absolute paths.)
EOF
}
prop() {
    sed -n "s/^$1[[:space:]]*=[[:space:]]*//p" "$keystore_props" | head -n 1 | tr -d '\r'
}
if [[ ! -f "$keystore_props" ]]; then
    add_missing "apps/android/keystore.properties not found.
$(keytool_hint)"
else
    for key in storeFile storePassword keyAlias keyPassword; do
        if [[ -z "$(prop "$key")" ]]; then
            add_missing "keystore.properties: missing key '$key' (needs storeFile, storePassword, keyAlias, keyPassword)."
        fi
    done
    store_file="$(prop storeFile)"
    if [[ -n "$store_file" ]]; then
        [[ "$store_file" = /* ]] || store_file="$android/$store_file"
        [[ -f "$store_file" ]] || add_missing "keystore file not found: $store_file"
    fi
fi

# --- Report --------------------------------------------------------------------------------
if ((${#missing[@]} > 0)); then
    echo "Missing prerequisites (this script installs nothing):" >&2
    for m in "${missing[@]}"; do
        echo "- $m" >&2
    done
    exit 1
fi
echo "All prerequisites found."
if [[ "$mode" == check ]]; then
    exit 0
fi

# --- Build ---------------------------------------------------------------------------------
export ANDROID_NDK_HOME="$ndk"
version="$(sed -n 's/^[[:space:]]*versionName[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$gradle_file" | head -n 1)"
if [[ -z "$version" ]]; then
    echo "cannot read versionName from $gradle_file" >&2
    exit 1
fi
name="v2c_${version}_android.apk"
apk="$android/app/build/outputs/apk/release/app-release.apk"

echo "Building $name (first build downloads sherpa-onnx and Gradle; needs network)..."
(cd "$android" && sh ./gradlew assembleRelease)
[[ -f "$apk" ]] || {
    echo "expected $apk was not produced (unsigned build?)" >&2
    exit 1
}

# --- Verify --------------------------------------------------------------------------------
echo "Checking 16 KB alignment..."
if ! align_out="$("$build_tools/zipalign" -c -P 16 -v 4 "$apk" 2>&1)"; then
    echo "$align_out" | tail -n 20 >&2
    echo "zipalign: APK is NOT 16 KB aligned" >&2
    exit 1
fi
echo "$align_out" | grep -E '\.so|Verification' || true

echo "Checking signature..."
"$build_tools/apksigner" verify --verbose --print-certs "$apk" | grep -E 'Verifies|v[0-9] scheme|SHA-256 digest' || true
"$build_tools/apksigner" verify "$apk" >/dev/null

if command -v unzip >/dev/null 2>&1; then
    contents="$(unzip -l "$apk")"
    for lib in libv2c_ffi.so libjnidispatch.so libsherpa-onnx-jni.so libonnxruntime.so; do
        if ! grep -q "lib/arm64-v8a/$lib" <<<"$contents"; then
            echo "APK is missing lib/arm64-v8a/$lib" >&2
            exit 1
        fi
    done
else
    echo "warning: unzip not found; skipped the native library content check" >&2
fi

# --- Publish to releases/ --------------------------------------------------------------------
releases="$repo/releases"
mkdir -p "$releases"
cp -f "$apk" "$releases/$name"
if command -v sha256sum >/dev/null 2>&1; then
    hash="$(sha256sum "$releases/$name" | cut -d ' ' -f 1)"
else
    hash="$(shasum -a 256 "$releases/$name" | cut -d ' ' -f 1)"
fi
sums="$releases/SHA256SUMS.txt"
tmp="$(mktemp "$releases/.SHA256SUMS.XXXXXX")"
if [[ -f "$sums" ]]; then
    # Keep every line except the one for this file name.
    awk -v n="$name" '{ sub(/\r$/, "") } $2 != n' "$sums" >"$tmp"
fi
printf '%s  %s\n' "$hash" "$name" >>"$tmp"
chmod 644 "$tmp"
mv -f "$tmp" "$sums"

echo "Done: $releases/$name"
echo "SHA-256: $hash"
