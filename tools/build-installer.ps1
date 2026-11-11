# Builds the Windows NSIS installer (CUDA build by default), moves it to <repo>\releases\,
# records its SHA-256 in releases\SHA256SUMS.txt, and runs a Windows Defender scan on the
# installer and v2c.exe. See docs/build-guide.md.
#
#   powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1
#   powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1 -CpuOnly -Jobs 2
#
# Run it in the foreground: the build needs several GB of RAM at the link step.
param(
    [switch]$CpuOnly,
    [int]$Jobs = 1,
    [string]$CudaPath = 'C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5',
    [string]$CudaArch = '75',
    [string]$TargetDir = (Join-Path $env:USERPROFILE '.v2ct')
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

$vsCmake = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin'
$front = @($vsCmake, (Join-Path $env:USERPROFILE '.cargo\bin'), 'C:\Program Files\nodejs')
if (-not $CpuOnly) {
    # Older CUDA versions on PATH make CMake pick the wrong nvcc and headers.
    $front = @("$CudaPath\bin\x64", "$CudaPath\bin") + $front
}
$rest = ($env:Path -split ';') | Where-Object {
    $_ -and ($CpuOnly -or $_ -notmatch 'CUDA\\v\d' -or $_.StartsWith($CudaPath, 'OrdinalIgnoreCase'))
}
$env:Path = ($front + $rest) -join ';'
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
$env:CARGO_TARGET_DIR = $TargetDir
$env:CARGO_BUILD_JOBS = "$Jobs"
$env:CMAKE_BUILD_PARALLEL_LEVEL = "$Jobs"
# MSVC reads source files in the system code page (950 on zh-TW Windows) unless told otherwise;
# transcribe.cpp has UTF-8 characters in comments that then swallow the next line (C2065...).
$env:CFLAGS_x86_64_pc_windows_msvc = '/utf-8'
$env:CXXFLAGS_x86_64_pc_windows_msvc = '/utf-8'
if (-not $CpuOnly) {
    $env:CUDA_PATH = $CudaPath
    $env:CUDAToolkit_ROOT = $CudaPath
    $env:CMAKE_CUDA_ARCHITECTURES = $CudaArch
}

Push-Location (Join-Path $repo 'apps\desktop')
try {
    if (-not (Test-Path node_modules)) { npm ci; if ($LASTEXITCODE) { throw 'npm ci failed' } }
    if ($CpuOnly) { npm run tauri build } else { npm run tauri:build:cuda }
    if ($LASTEXITCODE) { throw "build failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}

# The CUDA build also ships TEA-ASR (transcribe.cpp as transcribe.dll + its own ggml*.dll);
# build.rs stages them in runtime-dlls\, which the installer bundles. v2c.exe imports
# transcribe.dll, so a CUDA installer without it would not even start.
if (-not $CpuOnly) {
    $stage = Join-Path $repo 'apps\desktop\src-tauri\runtime-dlls'
    foreach ($pattern in @('transcribe.dll', 'ggml.dll', 'ggml-base.dll', 'ggml-cpu.dll', 'ggml-cuda.dll')) {
        if (-not (Get-ChildItem -Path $stage -Filter $pattern -ErrorAction SilentlyContinue)) {
            throw "TEA-ASR runtime DLL missing from ${stage}: $pattern"
        }
    }
}

# Regex instead of ConvertFrom-Json: Windows PowerShell 5 rejects keys that differ only in case.
$conf = Get-Content (Join-Path $repo 'apps\desktop\src-tauri\tauri.conf.json') -Raw
$version = [regex]::Match($conf, '"version"\s*:\s*"([^"]+)"').Groups[1].Value
$name = "v2c_${version}_x64-setup.exe"
$built = Join-Path $TargetDir "release\bundle\nsis\$name"
$exe = Join-Path $TargetDir 'release\v2c.exe'

# Deliverables go to <repo>\releases (git-ignored); the target dir keeps only build caches.
$releases = Join-Path $repo 'releases'
New-Item -ItemType Directory -Force $releases | Out-Null
$installer = Join-Path $releases $name
Move-Item -Force $built $installer
$sums = Join-Path $releases 'SHA256SUMS.txt'
$hash = (Get-FileHash $installer -Algorithm SHA256).Hash.ToLower()
$lines = @()
if (Test-Path $sums) { $lines = @(Get-Content $sums | Where-Object { $_ -notmatch [regex]::Escape("  $name") + '$' }) }
($lines + "$hash  $name") | Set-Content -Encoding ascii $sums

$defender = Join-Path $env:ProgramFiles 'Windows Defender\MpCmdRun.exe'
foreach ($f in @($installer, $exe)) {
    $i = Get-Item $f
    '{0}  {1:N1} MiB  {2}' -f $i.FullName, ($i.Length / 1MB), $i.LastWriteTime
    'sha256 ' + (Get-FileHash $f -Algorithm SHA256).Hash
    if (Test-Path $defender) { & $defender -Scan -ScanType 3 -File $f -DisableRemediation | Select-Object -Last 1 }
}
