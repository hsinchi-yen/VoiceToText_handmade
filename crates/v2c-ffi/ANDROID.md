# Wiring v2c-ffi into apps/android (T2.3)

> **Status (2026-11-12, branch `t2.5-android-release`): wired in `apps/android/app/build.gradle.kts`,
> not yet build-verified** (no JDK/SDK/NDK/cargo-ndk where it was written). Sections 2-4 below are
> implemented as `cargoNdk`, `generateUniffiKotlin` (a typed task registered through
> `addGeneratedSourceDirectory`) and `copyPresets`, all before `preBuild`; `CARGO_TARGET_DIR`
> (relative = against the repo root) and `~/.cargo/bin` (also `cargo.exe`) are honoured; release
> builds only arm64-v8a, debug adds x86_64; `-Pv2c.skipRust=true` skips the two cargo tasks. JNA is
> `net.java.dev.jna:jna:5.19.1@aar` (version in `libs.versions.toml`; 5.19.1 is the latest on Maven
> Central, checked 2026-11-12; the 5.17.0 below was from memory). `app/proguard-rules.pro` holds the
> R8 keep rules (JNA, `uniffi.v2c_ffi`, sherpa-onnx); R8 itself stays off until a minified build is
> smoke-tested. `src/main/jniLibs` and `src/main/assets/presets` are git-ignored. Build with
> `tools/build-android.sh` / `.ps1` (docs/android-install.md).

`apps/android` is created by T2.1 on another branch. These steps are applied when both
branches are merged. Everything marked "blocked" needs tools the human still has to install
(`cargo-ndk`, the Android rustup targets, the Android SDK/NDK).

## Versions (verified 2026-11-12)

- uniffi 0.32.2, proc-macro mode (`uniffi::setup_scaffolding!()` + `#[uniffi::export]`, no UDL),
  Kotlin bindings generated in **library mode** from the built `.so`/`.dll`.
- Generated package: `uniffi.v2c_ffi`, file `v2c_ffi.kt` (needs JNA at runtime, see below).
- Verified on the host: bindings generate from `v2c_ffi.dll` and contain `AsrPlan`, `AsrHint`,
  `AsrPick`, `AsrSource`, `Lan`, `LanService(s)` and `FfiException`.

## 1. Rust toolchain (human, once)

```sh
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
```

NDK 28.2 aligns `.so` files to 16 KB pages by default. Verify (below); if a build uses an older
NDK, add `RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384"`.

## 2. Build the .so with cargo-ndk

```sh
cargo ndk -t arm64-v8a -t x86_64 -o apps/android/app/src/main/jniLibs build -p v2c-ffi --release
```

Gradle task for `apps/android/app/build.gradle.kts` (runs before `preBuild`):

```kotlin
val cargoNdk by tasks.registering(Exec::class) {
    val repoRoot = rootProject.projectDir.parentFile.parentFile // apps/android -> repo root
    workingDir = repoRoot
    commandLine(
        "cargo", "ndk", "-t", "arm64-v8a", "-t", "x86_64",
        "-o", "${projectDir}/src/main/jniLibs",
        "build", "-p", "v2c-ffi", "--release",
    )
    inputs.dir(repoRoot.resolve("crates/v2c-ffi/src"))
    inputs.dir(repoRoot.resolve("crates/v2c-core/src"))
    outputs.dir("${projectDir}/src/main/jniLibs")
}
```

`CARGO_TARGET_DIR` must stay the shared `C:\Users\lance.tn\.v2ct` (build cache only); the
`.so` ends up in `jniLibs`, which should be git-ignored.

## 3. Generate the Kotlin bindings

Generated sources are never committed: they go to `app/build/generated/uniffi` and are
registered as a source dir. Hand-written Kotlin wrappers live in
`app/src/main/java/app/v2c/ime/ffi/` (the Android package T2.1 uses is `app.v2c.ime`); the
generated code declares `package uniffi.v2c_ffi`.

The library-mode generator reads a built library; use the cargo-ndk output so the bindings match
exactly what ships. The `bindgen` feature keeps the generator out of the Android build.

```kotlin
val generateUniffiKotlin by tasks.registering(Exec::class) {
    dependsOn(cargoNdk)
    val repoRoot = rootProject.projectDir.parentFile.parentFile
    workingDir = repoRoot
    val out = layout.buildDirectory.dir("generated/uniffi")
    // Same directory cargo/cargo-ndk use (the shared build cache, not repo/target).
    val targetDir = System.getenv("CARGO_TARGET_DIR") ?: "target"
    commandLine(
        "cargo", "run", "-p", "v2c-ffi", "--features", "bindgen", "--bin", "uniffi-bindgen", "--",
        "generate",
        "--library", "$targetDir/aarch64-linux-android/release/libv2c_ffi.so",
        "--language", "kotlin", "--out-dir", out.get().asFile.path,
    )
    outputs.dir(out)
}
android.sourceSets["main"].java.srcDir(layout.buildDirectory.dir("generated/uniffi"))
tasks.named("preBuild") { dependsOn(cargoNdk, generateUniffiKotlin) }
```

Host-only check (no NDK needed), as used to verify this crate:

```sh
cargo build -p v2c-ffi
cargo run -p v2c-ffi --features bindgen --bin uniffi-bindgen -- generate \
  --library "$CARGO_TARGET_DIR/debug/v2c_ffi.dll" --language kotlin --out-dir tests/tmp/kt
```

(On Linux/macOS use `libv2c_ffi.so` / `.dylib`.)

Runtime dependency of the generated code: `implementation("net.java.dev.jna:jna:5.17.0@aar")`.
The JNA version is from memory: check the current release when wiring. The `@aar` form is the
Android one.

### R8 / ProGuard (release builds)

Add to `app/proguard-rules.pro`; without them R8 strips JNA/uniffi classes and the app crashes
at the first FFI call:

```
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keep class uniffi.v2c_ffi.** { *; }
-dontwarn java.awt.**
```

## 4. Preset assets

Copy the two preset files byte for byte; do not edit them:

```kotlin
val copyPresets by tasks.registering(Copy::class) {
    from(rootProject.projectDir.parentFile.parentFile.resolve("apps/desktop/src-tauri/presets")) {
        include("TN_CUSTOM_LLM_ADDR.toml", "RD_CUSTOM_LLM_ADDR.toml")
    }
    into("${projectDir}/src/main/assets/presets")
}
tasks.named("preBuild") { dependsOn(copyPresets) }
```

Kotlin side:

```kotlin
val tn = assets.open("presets/TN_CUSTOM_LLM_ADDR.toml").bufferedReader().readText()
val rd = assets.open("presets/RD_CUSTOM_LLM_ADDR.toml").bufferedReader().readText()
val services = withContext(Dispatchers.IO) { loadLanServices(tn, rd) } // once, at keyboard start
```

## 5. Kotlin usage (all calls are synchronous: use `Dispatchers.IO`)

| Kotlin | Notes |
|--------|-------|
| `toZhTw(text)` | every recognition result goes through it before `commitText` |
| `loadLanServices(tn, rd)` | must succeed before `transcribePcm` / `translate`; throws `FfiException.Config` on a bad file |
| `chooseAsr(lan, systemAvailable, localModelReady, pick)` | pure; `AsrPlan.hint` explains an unusable source |
| `lanServiceId(lan, source)` | `Lan.Tn` + `AsrSource.LanTea` -> `"tn-tea-asr"` |
| `transcribePcm(pcmLe: ByteArray, rate: UInt, serviceId)` | 16 kHz mono PCM, 16-bit little-endian bytes (an odd length throws `InvalidAudio`); returns the raw transcript |
| `translate(text, serviceId)` | Qwen service id, e.g. `"tn-qwen"` / `"rd-qwen"`; blank text throws `EmptyText` without a request. Timeout is a fixed 8000 ms, deliberately longer than the desktop `llm_cloud_ms` default of 3000 ms (27B model, no settings page) |

Errors arrive as `FfiException` subclasses (codes only, no text).

## 6. Verification checklist

Done on the host: `cargo test -p v2c-ffi`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all --check`, Kotlin binding generation from the host library.

Blocked: tools not installed (human installs, then run):

- `cargo ndk -t arm64-v8a build -p v2c-ffi --release` (blocked: cargo-ndk / Android targets not installed)
- `cd apps/android && ./gradlew assembleDebug` (blocked: needs the T2.1 branch, SDK, NDK)
- `zipalign -c -P 16 -v 4 <apk>` and a check that the APK contains `lib/arm64-v8a/libv2c_ffi.so`
  (blocked: Android SDK build-tools not installed)
- Kotlin calls `toZhTw` on a device/emulator (blocked: depends on the APK)
