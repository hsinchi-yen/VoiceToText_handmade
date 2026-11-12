import java.io.File
import java.net.URI
import java.security.MessageDigest
import javax.inject.Inject

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "app.v2c.ime"
    compileSdk = 37

    defaultConfig {
        applicationId = "app.v2c.ime"
        minSdk = 28
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        debug {
            // x86_64 only for the emulator; it never ships in release.
            ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
        }
        release {
            isMinifyEnabled = false
            ndk { abiFilters += "arm64-v8a" }
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
        if (!tmp.renameTo(dest)) throw GradleException("Cannot move $tmp to $dest")
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
