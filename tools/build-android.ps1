# Builds the signed Android release APK (arm64-v8a), checks its 16 KB page alignment and
# signature, copies it to <repo>\releases\v2c_<versionName>_android.apk and records its SHA-256
# in releases\SHA256SUMS.txt (same "<hash>  <name>" format as tools\build-installer.ps1; the
# line for this file name is replaced, other lines are kept). Linux/macOS: tools/build-android.sh.
# See docs/android-install.md. Works on Windows PowerShell 5.1 and PowerShell 7.
#
#   powershell -ExecutionPolicy Bypass -File tools\build-android.ps1 -Check   # only check
#   powershell -ExecutionPolicy Bypass -File tools\build-android.ps1          # check, then build
#
# Never installs anything: when something is missing it prints the commands for you to run.
# The signing key is not created for you either; without apps\android\keystore.properties the
# script prints the keytool command and stops (no unsigned APK is produced).
#
# Environment: JAVA_HOME (JDK 17+), ANDROID_HOME or ANDROID_SDK_ROOT, ANDROID_NDK_HOME
# (default <sdk>\ndk\28.2.13676358), CARGO_TARGET_DIR (default <repo>\target).
param(
    [switch]$Check
)
$ErrorActionPreference = 'Stop'

$NdkVersion = '28.2.13676358'
$BuildToolsVersion = '36.0.0'

$repo = Split-Path -Parent $PSScriptRoot
$android = Join-Path $repo 'apps\android'
$gradleFile = Join-Path $android 'app\build.gradle.kts'
$keystoreProps = Join-Path $android 'keystore.properties'

# rustup installs to %USERPROFILE%\.cargo\bin, which the shell may lack.
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path $cargoBin) { $env:Path = "$cargoBin;$env:Path" }

# Runs a native command, returns @{ Code; Out }. Windows PowerShell 5.1 turns stderr output of
# native commands into errors under -ErrorAction Stop, so relax it for the call.
function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments)
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $out = & $Exe @Arguments 2>&1 | ForEach-Object { "$_" }
        return @{ Code = $LASTEXITCODE; Out = ($out -join "`n") }
    } catch {
        return @{ Code = 1; Out = "$_" }
    } finally {
        $ErrorActionPreference = $prev
    }
}

$missing = New-Object System.Collections.Generic.List[string]

# --- JDK -----------------------------------------------------------------------------------
$javaExe = 'java'
if ($env:JAVA_HOME) {
    $candidate = Join-Path $env:JAVA_HOME 'bin\java.exe'
    if (Test-Path $candidate) { $javaExe = $candidate }
}
if (-not (Get-Command $javaExe -ErrorAction SilentlyContinue)) {
    $missing.Add("JDK 17 not found (set JAVA_HOME).`n    Install: winget install EclipseAdoptium.Temurin.17.JDK`n    then set JAVA_HOME to its folder, e.g. C:\Program Files\Eclipse Adoptium\jdk-17...")
} else {
    $r = Invoke-Native $javaExe @('-version')
    $m = [regex]::Match($r.Out, 'version "(\d+)')
    if (-not $m.Success -or [int]$m.Groups[1].Value -lt 17) {
        $missing.Add("JDK 17 or newer required, found: $($r.Out.Split("`n")[0]).`n    Install: winget install EclipseAdoptium.Temurin.17.JDK ; set JAVA_HOME")
    }
}

# --- Android SDK / build-tools / NDK ---------------------------------------------------------
$sdk = $env:ANDROID_HOME
if (-not $sdk) { $sdk = $env:ANDROID_SDK_ROOT }
$localProps = Join-Path $android 'local.properties'
if (-not $sdk -and (Test-Path $localProps)) {
    # sdk.dir=C\:\\Users\\me\\AppData\\Local\\Android\\Sdk (java properties escaping)
    $line = Get-Content $localProps | Where-Object { $_ -match '^sdk\.dir=' } | Select-Object -First 1
    if ($line) { $sdk = ($line -replace '^sdk\.dir=', '') -replace '\\:', ':' -replace '\\\\', '\' }
}
$sdkHint = "Install with Android Studio's SDK Manager or cmdline-tools:`n" +
    "    sdkmanager `"platforms;android-37`" `"platforms;android-36`" `"build-tools;$BuildToolsVersion`" `"platform-tools`" `"ndk;$NdkVersion`"`n" +
    "    `$env:ANDROID_HOME = `"`$env:LOCALAPPDATA\Android\Sdk`"   # set as a user environment variable"
$buildTools = ''
$ndk = ''
if (-not $sdk -or -not (Test-Path $sdk)) {
    $missing.Add("Android SDK not found (set ANDROID_HOME or ANDROID_SDK_ROOT).`n    $sdkHint")
} else {
    $env:ANDROID_HOME = $sdk
    $buildTools = Join-Path $sdk "build-tools\$BuildToolsVersion"
    if (-not ((Test-Path (Join-Path $buildTools 'zipalign.exe')) -and (Test-Path (Join-Path $buildTools 'apksigner.bat')))) {
        $missing.Add("Android build-tools $BuildToolsVersion missing in $sdk (needs zipalign, apksigner).`n    $sdkHint")
    }
    $ndk = $env:ANDROID_NDK_HOME
    if (-not $ndk) { $ndk = Join-Path $sdk "ndk\$NdkVersion" }
    if (-not (Test-Path $ndk)) {
        $missing.Add("Android NDK not found at $ndk (need $NdkVersion).`n    $sdkHint`n    `$env:ANDROID_NDK_HOME = `"`$env:LOCALAPPDATA\Android\Sdk\ndk\$NdkVersion`"")
    } else {
        $props = Join-Path $ndk 'source.properties'
        $rev = ''
        if (Test-Path $props) {
            $revLine = Get-Content $props | Where-Object { $_ -match '^Pkg\.Revision' } | Select-Object -First 1
            if ($revLine) { $rev = ($revLine -split '=', 2)[1].Trim() }
        }
        if ($rev -ne $NdkVersion) {
            Write-Warning "$ndk is not NDK $NdkVersion; 16 KB alignment is only guaranteed with it"
        }
    }
}

# --- Rust ----------------------------------------------------------------------------------
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $missing.Add('cargo not found. Install Rust: https://rustup.rs')
} else {
    if ((Invoke-Native 'cargo' @('ndk', '--version')).Code -ne 0) {
        $missing.Add('cargo-ndk not found. Run: cargo install cargo-ndk')
    }
    if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
        $missing.Add('rustup not found (needed for the Android targets). Install Rust: https://rustup.rs')
    } else {
        $targets = (Invoke-Native 'rustup' @('target', 'list', '--installed')).Out -split "`n" | ForEach-Object { $_.Trim() }
        if ($targets -notcontains 'aarch64-linux-android') {
            $missing.Add('Rust target missing. Run: rustup target add aarch64-linux-android x86_64-linux-android')
        }
    }
}

# --- Signing key ---------------------------------------------------------------------------
function Get-KeystoreProp([string]$Key) {
    $line = Get-Content $keystoreProps | Where-Object { $_ -match ('^' + [regex]::Escape($Key) + '\s*=') } | Select-Object -First 1
    if ($line) { return ($line -replace ('^' + [regex]::Escape($Key) + '\s*=\s*'), '').Trim() }
    return ''
}
if (-not (Test-Path $keystoreProps)) {
    $jks = Join-Path $android 'v2c-release.jks'
    $missing.Add(@"
apps\android\keystore.properties not found.
    No signing key. Create one ONCE and keep it safe (losing it means users cannot update; never commit it):
      keytool -genkeypair -v -keystore "$jks" -alias v2c -keyalg RSA -keysize 4096 -validity 36500
    Then create $keystoreProps (git-ignored) with:
      storeFile=v2c-release.jks
      storePassword=<the keystore password you typed>
      keyAlias=v2c
      keyPassword=<the key password you typed>
    (storeFile is relative to apps\android; use forward slashes in absolute paths, e.g. C:/keys/v2c.jks.)
"@)
} else {
    foreach ($key in @('storeFile', 'storePassword', 'keyAlias', 'keyPassword')) {
        if (-not (Get-KeystoreProp $key)) {
            $missing.Add("keystore.properties: missing key '$key' (needs storeFile, storePassword, keyAlias, keyPassword).")
        }
    }
    $storeFile = Get-KeystoreProp 'storeFile'
    if ($storeFile) {
        if (-not [System.IO.Path]::IsPathRooted($storeFile)) { $storeFile = Join-Path $android $storeFile }
        if (-not (Test-Path $storeFile)) { $missing.Add("keystore file not found: $storeFile") }
    }
}

# --- Report --------------------------------------------------------------------------------
if ($missing.Count -gt 0) {
    [Console]::Error.WriteLine('Missing prerequisites (this script installs nothing):')
    foreach ($m in $missing) { [Console]::Error.WriteLine("- $m") }
    exit 1
}
Write-Host 'All prerequisites found.'
if ($Check) { exit 0 }

# --- Build ---------------------------------------------------------------------------------
$env:ANDROID_NDK_HOME = $ndk
$gradleText = Get-Content $gradleFile -Raw
$version = [regex]::Match($gradleText, '(?m)^\s*versionName\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $version) { throw "cannot read versionName from $gradleFile" }
$name = "v2c_${version}_android.apk"
$apk = Join-Path $android 'app\build\outputs\apk\release\app-release.apk'

Write-Host "Building $name (first build downloads sherpa-onnx and Gradle; needs network)..."
Push-Location $android
try {
    & (Join-Path $android 'gradlew.bat') assembleRelease
    if ($LASTEXITCODE -ne 0) { throw "gradlew assembleRelease failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}
if (-not (Test-Path $apk)) { throw "expected $apk was not produced (unsigned build?)" }

# --- Verify --------------------------------------------------------------------------------
Write-Host 'Checking 16 KB alignment...'
$align = Invoke-Native (Join-Path $buildTools 'zipalign.exe') @('-c', '-P', '16', '-v', '4', $apk)
if ($align.Code -ne 0) {
    ($align.Out -split "`n" | Select-Object -Last 20) | ForEach-Object { [Console]::Error.WriteLine($_) }
    throw 'zipalign: APK is NOT 16 KB aligned'
}
$align.Out -split "`n" | Where-Object { $_ -match '\.so|Verification' } | ForEach-Object { Write-Host $_ }

Write-Host 'Checking signature...'
$sign = Invoke-Native (Join-Path $buildTools 'apksigner.bat') @('verify', '--verbose', '--print-certs', $apk)
if ($sign.Code -ne 0) {
    [Console]::Error.WriteLine($sign.Out)
    throw 'apksigner: signature verification failed'
}
$sign.Out -split "`n" | Where-Object { $_ -match 'Verifies|v\d scheme|SHA-256 digest' } | ForEach-Object { Write-Host $_ }

Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead($apk)
try {
    $entries = @($zip.Entries | ForEach-Object { $_.FullName })
} finally {
    $zip.Dispose()
}
foreach ($lib in @('libv2c_ffi.so', 'libjnidispatch.so', 'libsherpa-onnx-jni.so', 'libonnxruntime.so')) {
    if ($entries -notcontains "lib/arm64-v8a/$lib") { throw "APK is missing lib/arm64-v8a/$lib" }
}

# --- Publish to releases\ --------------------------------------------------------------------
$releases = Join-Path $repo 'releases'
New-Item -ItemType Directory -Force $releases | Out-Null
$dest = Join-Path $releases $name
Copy-Item -Force $apk $dest
$hash = (Get-FileHash $dest -Algorithm SHA256).Hash.ToLower()
$sums = Join-Path $releases 'SHA256SUMS.txt'
$lines = @()
if (Test-Path $sums) {
    # Keep every line except the one for this file name.
    $lines = @(Get-Content $sums | Where-Object { $_ -notmatch ([regex]::Escape("  $name") + '$') })
}
($lines + "$hash  $name") | Set-Content -Encoding ascii $sums

Write-Host "Done: $dest"
Write-Host "SHA-256: $hash"
