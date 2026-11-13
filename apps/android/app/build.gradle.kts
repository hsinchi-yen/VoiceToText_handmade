import java.io.File
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.net.URI
import java.util.Properties
import java.security.MessageDigest
import javax.inject.Inject
import org.gradle.process.ExecOperations

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

// apps/android/keystore.properties (git-ignored): storeFile, storePassword, keyAlias, keyPassword.
// Use forward slashes in storeFile; a relative path is resolved against apps/android.
val keystoreProps: Properties? = rootProject.file("keystore.properties").takeIf { it.isFile }
    ?.let { f -> Properties().also { p -> f.inputStream().use { input -> p.load(input) } } }

android {
    namespace = "app.v2c.ime"
    compileSdk = 37
    // Needed to strip/align the cargo-ndk .so files; scripts check this exact NDK.
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "app.v2c.ime"
        minSdk = 28
        targetSdk = 36
        versionCode = 2
        versionName = "0.2.0"
    }

    signingConfigs {
        // Only configured when apps/android/keystore.properties exists (git-ignored). Without it
        // the release build still configures but produces an UNSIGNED apk; tools/build-android.*
        // stop with instructions before that happens.
        if (keystoreProps != null) {
            create("release") {
                fun req(key: String): String = keystoreProps.getProperty(key)?.trim()?.takeIf { it.isNotEmpty() }
                    ?: throw GradleException("keystore.properties: missing '$key' (needs storeFile, storePassword, keyAlias, keyPassword)")
                val file = File(req("storeFile"))
                storeFile = if (file.isAbsolute) file else File(rootProject.projectDir, file.path)
                storePassword = req("storePassword")
                keyAlias = req("keyAlias")
                keyPassword = req("keyPassword")
            }
        }
    }

    buildTypes {
        debug {
            // x86_64 only for the emulator; it never ships in release.
            ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
        }
        release {
            // R8 stays off: the JNA/uniffi/sherpa-onnx JNI paths can only fail at run time and
            // nobody has been able to test a minified build yet; the APK is dominated by the
            // native libraries anyway. The keep rules are ready in proguard-rules.pro: to try R8,
            // set isMinifyEnabled = true and run the full smoke test (docs/android-install.md).
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            ndk { abiFilters += "arm64-v8a" }
            if (keystoreProps != null) signingConfig = signingConfigs.getByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }

    lint {
        abortOnError = true
    }
}

dependencies {
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime)
    implementation(libs.androidx.lifecycle.viewmodel)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.savedstate)
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.foundation)
    implementation(libs.androidx.compose.material3)
    // uniffi-generated Kotlin needs JNA; the @aar form carries libjnidispatch.so for Android.
    implementation("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar")

    testImplementation(libs.junit)
}

// ---------------------------------------------------------------------------------------------
// sherpa-onnx (T2.6). Not on Maven: the official Android release ships prebuilt .so files plus
// Kotlin API source files. Nothing large is committed; this task downloads both from the
// pinned GitHub tag into build/sherpa-onnx (git-ignored), verifies SHA-256, and feeds them to
// the build as generated jniLibs / Kotlin sources. Needs network on the first build only.
// Bumping the version means updating every hash below (see docs/android-local-model.md).
// ---------------------------------------------------------------------------------------------
abstract class FetchSherpaOnnx @Inject constructor(
    private val fs: FileSystemOperations,
    private val archives: ArchiveOperations,
) : DefaultTask() {
    @get:Input abstract val tag: Property<String>
    @get:OutputDirectory abstract val kotlinDir: DirectoryProperty
    @get:OutputDirectory abstract val jniLibsDir: DirectoryProperty
    @get:Internal abstract val cacheDir: DirectoryProperty

    private fun sha256(f: File): String {
        val md = MessageDigest.getInstance("SHA-256")
        f.inputStream().use { input ->
            val buf = ByteArray(1 shl 16)
            while (true) {
                val n = input.read(buf)
                if (n < 0) break
                md.update(buf, 0, n)
            }
        }
        return md.digest().joinToString("") { "%02x".format(it) }
    }

    private fun fetch(url: String, expectedSha256: String, name: String): File {
        val dest = File(cacheDir.get().asFile, name)
        if (dest.exists() && sha256(dest) == expectedSha256) return dest
        dest.parentFile.mkdirs()
        val tmp = File(dest.parentFile, "$name.part")
        URI(url).toURL().openStream().use { input -> tmp.outputStream().use { input.copyTo(it) } }
        val actual = sha256(tmp)
        if (actual != expectedSha256) {
            tmp.delete()
            throw GradleException("SHA-256 mismatch for $url: expected $expectedSha256, got $actual")
        }
        try {
            // renameTo() does not overwrite on Windows; replace a stale or corrupt cached file.
            Files.move(tmp.toPath(), dest.toPath(), StandardCopyOption.REPLACE_EXISTING)
        } catch (e: java.io.IOException) {
            throw GradleException("Cannot move $tmp to $dest", e)
        }
        return dest
    }

    @TaskAction
    fun run() {
        val t = tag.get()
        val kotlinOut = kotlinDir.get().asFile.apply { deleteRecursively(); mkdirs() }
        val raw = "https://raw.githubusercontent.com/k2-fsa/sherpa-onnx/$t/sherpa-onnx/kotlin-api"
        for ((file, hash) in KOTLIN_FILES) {
            fetch("$raw/$file", hash, "$t-$file").copyTo(File(kotlinOut, file), overwrite = true)
        }
        val archive = fetch(
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/$t/sherpa-onnx-$t-android.tar.bz2",
            ANDROID_ARCHIVE_SHA256,
            "sherpa-onnx-$t-android.tar.bz2",
        )
        val jniOut = jniLibsDir.get().asFile.apply { deleteRecursively(); mkdirs() }
        fs.copy {
            from(archives.tarTree(archives.bzip2(archive)))
            include("jniLibs/*/libonnxruntime.so", "jniLibs/*/libsherpa-onnx-jni.so")
            exclude("jniLibs/armeabi-v7a/**", "jniLibs/x86/**")
            eachFile { path = path.removePrefix("jniLibs/") }
            includeEmptyDirs = false
            into(jniOut)
        }
    }

    companion object {
        const val ANDROID_ARCHIVE_SHA256 =
            "39003753ec293425ed56723ea9b972b98841a490af3311670c3060548d654330"
        val KOTLIN_FILES = mapOf(
            "OfflineRecognizer.kt" to "8103b67ec65910a71e6b8d4bca7934f77c1af47b68a5c18cdca0ce3f956247a7",
            "OfflineStream.kt" to "5c90b7c060cdd0a2222cc935b54f69465a4a7544de29240bafe4d3a8d86dbb6e",
            "FeatureConfig.kt" to "829c7adc4db1324f21b9a9cbf5bf10eacf759714fe50956f48317c1ffef044f6",
            "QnnConfig.kt" to "86c963f27677f690eb0d6639735052eba5e60d9f81ccd4c1df0aa831fd680d6f",
            "HomophoneReplacerConfig.kt" to "fe926b6413f4d87e67c41feb21b8363923a67cbd3a5eb687b76390a5f1f7db60",
        )
    }
}

val fetchSherpaOnnx = tasks.register<FetchSherpaOnnx>("fetchSherpaOnnx") {
    group = "v2c"
    description = "Downloads sherpa-onnx Android libs and Kotlin API (SHA-256 pinned)"
    tag.set("v2.0.1")
    kotlinDir.set(layout.buildDirectory.dir("sherpa-onnx/kotlin"))
    jniLibsDir.set(layout.buildDirectory.dir("sherpa-onnx/jniLibs"))
    // Outside build/ so `clean` does not force a re-download (apps/android/.gradle is git-ignored).
    cacheDir.set(rootProject.layout.projectDirectory.dir(".gradle/sherpa-onnx-cache"))
}

androidComponents {
    onVariants { variant ->
        variant.sources.kotlin?.addGeneratedSourceDirectory(fetchSherpaOnnx, FetchSherpaOnnx::kotlinDir)
        variant.sources.jniLibs?.addGeneratedSourceDirectory(fetchSherpaOnnx, FetchSherpaOnnx::jniLibsDir)
    }
}

// ---------------------------------------------------------------------------------------------
// v2c-ffi wiring (T2.3, crates/v2c-ffi/ANDROID.md): cargo-ndk builds libv2c_ffi.so into
// build/v2c-ffi/jniLibs, uniffi generates the Kotlin bindings into build/generated/uniffi, and
// the TN/RD presets are copied byte for byte into build/v2c-ffi/assets/presets. Each task is
// typed with declared outputs and registered as a generated source directory per variant (same
// pattern as fetchSherpaOnnx), so Gradle wires the task dependencies itself.
// Needs rustup targets + cargo-ndk + NDK (tools/build-android.* --check lists what is missing).
// `-Pv2c.skipRust=true` leaves the generated jniLibs and bindings empty (lint-style runs only: the FFI
// Kotlin does not compile without the generated bindings).
// ---------------------------------------------------------------------------------------------
val repoRoot: File = rootProject.projectDir.parentFile.parentFile // apps/android -> repo root
val skipRust: Boolean = providers.gradleProperty("v2c.skipRust").orNull == "true"

// Same directory cargo / cargo-ndk use: CARGO_TARGET_DIR (relative = against the repo root).
val cargoTargetDir: File = System.getenv("CARGO_TARGET_DIR")?.takeIf { it.isNotBlank() }
    ?.let { dir -> File(dir).let { f -> if (f.isAbsolute) f else File(repoRoot, dir) } }
    ?: File(repoRoot, "target")

// The Gradle daemon often lacks ~/.cargo/bin on PATH; look there first (cargo.exe on Windows).
val cargoExe: String = run {
    val cargoHome = System.getenv("CARGO_HOME")?.takeIf { it.isNotBlank() }
        ?: File(System.getProperty("user.home"), ".cargo").path
    listOf("cargo.exe", "cargo").map { File(cargoHome, "bin/$it") }.firstOrNull { it.isFile }?.path ?: "cargo"
}

// Release ships arm64 only; debug builds also get x86_64 for the emulator.
val releaseOnly: Boolean = gradle.startParameter.taskNames.let { names ->
    names.isNotEmpty() && names.all { it.contains("Release", ignoreCase = true) }
}
val ndkAbis: List<String> = if (releaseOnly) listOf("arm64-v8a") else listOf("arm64-v8a", "x86_64")

abstract class CargoNdk @Inject constructor(
    private val execOps: ExecOperations,
) : DefaultTask() {
    @get:Input abstract val cargo: Property<String>
    @get:Input abstract val abis: ListProperty<String>
    @get:Input abstract val skip: Property<Boolean>
    // From the Android plugin (honours ndkVersion, also in Android Studio builds).
    @get:Input @get:Optional abstract val ndkDir: Property<String>
    @get:Internal abstract val repoDir: DirectoryProperty
    @get:OutputDirectory abstract val outDir: DirectoryProperty

    @TaskAction
    fun run() {
        val out = outDir.get().asFile.apply { deleteRecursively(); mkdirs() }
        if (skip.get()) return
        execOps.exec {
            workingDir = repoDir.get().asFile
            ndkDir.orNull?.let { environment("ANDROID_NDK_HOME", it) }
            commandLine(
                buildList {
                    addAll(listOf(cargo.get(), "ndk"))
                    abis.get().forEach { addAll(listOf("-t", it)) }
                    addAll(listOf("-o", out.path, "build", "-p", "v2c-ffi", "--release"))
                },
            )
        }
    }
}

abstract class GenerateUniffiKotlin @Inject constructor(
    private val execOps: ExecOperations,
) : DefaultTask() {
    @get:Input abstract val cargo: Property<String>
    @get:Internal abstract val repoDir: DirectoryProperty
    // Absent when -Pv2c.skipRust=true.
    @get:InputFile @get:Optional abstract val library: RegularFileProperty
    @get:OutputDirectory abstract val outDir: DirectoryProperty

    @TaskAction
    fun run() {
        val out = outDir.get().asFile.apply { deleteRecursively(); mkdirs() }
        val lib = library.orNull?.asFile ?: return
        execOps.exec {
            workingDir = repoDir.get().asFile
            commandLine(
                cargo.get(), "run", "-p", "v2c-ffi", "--features", "bindgen", "--bin", "uniffi-bindgen", "--",
                "generate", "--library", lib.path, "--language", "kotlin", "--out-dir", out.path,
            )
        }
    }
}

abstract class CopyPresets @Inject constructor(
    private val fs: FileSystemOperations,
) : DefaultTask() {
    @get:InputDirectory @get:PathSensitive(PathSensitivity.RELATIVE) abstract val presetsDir: DirectoryProperty
    @get:OutputDirectory abstract val assetsDir: DirectoryProperty

    @TaskAction
    fun run() {
        val out = assetsDir.get().asFile.apply { deleteRecursively(); mkdirs() }
        fs.copy {
            from(presetsDir) { include("TN_CUSTOM_LLM_ADDR.toml", "RD_CUSTOM_LLM_ADDR.toml") }
            into(File(out, "presets"))
        }
    }
}

val cargoNdk = tasks.register<CargoNdk>("cargoNdk") {
    group = "v2c"
    description = "Builds libv2c_ffi.so with cargo-ndk into build/v2c-ffi/jniLibs"
    cargo.set(cargoExe)
    abis.set(ndkAbis)
    skip.set(skipRust)
    if (!skipRust) ndkDir.set(androidComponents.sdkComponents.ndkDirectory.map { it.asFile.path })
    repoDir.set(repoRoot)
    outDir.set(layout.buildDirectory.dir("v2c-ffi/jniLibs"))
    // cargo does its own incremental builds; always run it.
    outputs.upToDateWhen { false }
}

val generateUniffiKotlin = tasks.register<GenerateUniffiKotlin>("generateUniffiKotlin") {
    group = "v2c"
    description = "Generates the uniffi Kotlin bindings (package uniffi.v2c_ffi) from libv2c_ffi.so"
    dependsOn(cargoNdk)
    cargo.set(cargoExe)
    repoDir.set(repoRoot)
    if (!skipRust) library.set(File(cargoTargetDir, "aarch64-linux-android/release/libv2c_ffi.so"))
    outDir.set(layout.buildDirectory.dir("generated/uniffi"))
}

val copyPresets = tasks.register<CopyPresets>("copyPresets") {
    group = "v2c"
    description = "Copies the TN/RD preset TOML files into generated assets/presets (byte for byte)"
    presetsDir.set(File(repoRoot, "apps/desktop/src-tauri/presets"))
    assetsDir.set(layout.buildDirectory.dir("v2c-ffi/assets"))
}

androidComponents {
    onVariants { variant ->
        variant.sources.kotlin?.addGeneratedSourceDirectory(generateUniffiKotlin, GenerateUniffiKotlin::outDir)
        variant.sources.jniLibs?.addGeneratedSourceDirectory(cargoNdk, CargoNdk::outDir)
        variant.sources.assets?.addGeneratedSourceDirectory(copyPresets, CopyPresets::assetsDir)
    }
}
