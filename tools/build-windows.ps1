<#
.SYNOPSIS
  One-step build of the v2c Windows installer on any PC: detects the toolchain, picks GPU (CUDA)
  or CPU-only, then runs tools\build-installer.ps1. The installer lands in <repo>\releases\.

.DESCRIPTION
  Steps:
    1. Checks the tools: Visual Studio 2022 (any edition) with the C++ workload and its bundled
       CMake, Rust (MSVC), LLVM (libclang), Node.js, and optionally CUDA Toolkit 12+ with an NVIDIA GPU.
    2. Anything missing -> prints what to install (tools\setup-dev.ps1 installs all of it) and stops.
       This script never installs software itself.
    3. Picks GPU or CPU-only, the GPU architecture (from nvidia-smi), and the job count (from RAM).
    4. Runs tools\build-installer.ps1 with those values.

  Run in a normal (non-admin) PowerShell, in the foreground, from anywhere:
    powershell -ExecutionPolicy Bypass -File tools\build-windows.ps1
    powershell -ExecutionPolicy Bypass -File tools\build-windows.ps1 -CheckOnly   # check only
    powershell -ExecutionPolicy Bypass -File tools\build-windows.ps1 -CpuOnly     # force CPU-only

  See docs\build-guide.md.
#>
[CmdletBinding()]
param(
    [switch]$CpuOnly,     # build without CUDA even when a GPU and CUDA Toolkit are present
    [switch]$CheckOnly,   # only report what was found; do not build
    [int]$Jobs = 0,       # 0 = choose from installed RAM
    [string]$CudaArch = '', # '' = read from nvidia-smi (e.g. 86 for RTX 30)
    [switch]$Yes          # do not ask before starting the build
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$problems = New-Object System.Collections.Generic.List[string]
$notes = New-Object System.Collections.Generic.List[string]

function Find-First($paths) {
    foreach ($p in $paths) { if ($p -and (Test-Path $p)) { return $p } }
    return $null
}

# --- Visual Studio (C++ workload) and its bundled CMake ---------------------------------------
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vsPath = $null
if (Test-Path $vswhere) {
    $vsPath = & $vswhere -latest -products * -version '[17.0,18.0)' `
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath |
        Select-Object -First 1
}
$cmakeBin = $null
if ($vsPath) {
    $cmakeBin = Find-First @("$vsPath\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin")
    if (-not $cmakeBin) {
        $problems.Add("Visual Studio at '$vsPath' has no bundled CMake. In Visual Studio Installer, modify it and add 'C++ CMake tools for Windows'.")
    }
} else {
    $problems.Add('Visual Studio 2022 with the "Desktop development with C++" workload was not found (Build Tools, Community or Professional all work).')
}

# --- Rust (MSVC toolchain) ---------------------------------------------------------------------
$cargo = Find-First @("$env:USERPROFILE\.cargo\bin\cargo.exe", (Get-Command cargo -ErrorAction SilentlyContinue).Source)
if (-not $cargo) { $problems.Add('Rust was not found (rustup with the stable-x86_64-pc-windows-msvc toolchain).') }

# --- LLVM (libclang for bindgen) ---------------------------------------------------------------
$llvmBin = $null
foreach ($d in @($env:LIBCLANG_PATH, "$env:ProgramFiles\LLVM\bin")) {
    if ($d -and (Test-Path (Join-Path $d 'libclang.dll'))) { $llvmBin = $d; break }
}
if (-not $llvmBin) { $problems.Add('LLVM (libclang.dll) was not found. Install LLVM to C:\Program Files\LLVM or set LIBCLANG_PATH.') }

# --- Node.js -----------------------------------------------------------------------------------
$node = Find-First @("$env:ProgramFiles\nodejs\node.exe", (Get-Command node -ErrorAction SilentlyContinue).Source)
if (-not $node) { $problems.Add('Node.js (LTS) was not found.') }

# --- NVIDIA GPU and CUDA Toolkit 12+ (optional) -------------------------------------------------
# Every GPU's compute capability (e.g. "8.6" -> 86). Only well-formed rows count: with a driver
# but no usable device, or a driver too old for `compute_cap`, nvidia-smi prints a message instead.
$gpuNames = @()
$gpuArchs = @()
$smi = Find-First @("$env:SystemRoot\System32\nvidia-smi.exe", (Get-Command nvidia-smi -ErrorAction SilentlyContinue).Source)
if ($smi) {
    try {
        $rows = @(& $smi --query-gpu=name,compute_cap --format=csv,noheader 2>$null)
        if ($LASTEXITCODE -eq 0) {
            foreach ($row in $rows) {
                if ("$row" -match '^\s*(.+?),\s*(\d+)\.(\d+)\s*$') {
                    $gpuNames += $Matches[1]
                    $gpuArchs += [int]("$($Matches[2])$($Matches[3])")
                }
            }
        }
    } catch { }
}
$gpuArchs = @($gpuArchs | Sort-Object -Unique)
$gpuName = if ($gpuNames.Count -gt 0) { ($gpuNames | Select-Object -Unique) -join ' + ' } else { $null }
$archOverride = [bool]$CudaArch
if ($archOverride) {
    $parts = @($CudaArch -split ';' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    if (@($parts | Where-Object { $_ -notmatch '^\d+$' }).Count -gt 0 -or $parts.Count -eq 0) {
        Write-Host "-CudaArch must be numbers separated by ';' (e.g. 86 or 75;86), got '$CudaArch'" -ForegroundColor Red
        exit 1
    }
    $gpuArchs = @($parts | ForEach-Object { [int]$_ })
}

# CUDA Toolkits 12+ (newest first): the standard folder, plus CUDA_PATH for custom locations.
$cudaDirs = @()
$cudaRoot = "$env:ProgramFiles\NVIDIA GPU Computing Toolkit\CUDA"
if (Test-Path $cudaRoot) { $cudaDirs += @(Get-ChildItem $cudaRoot -Directory | ForEach-Object FullName) }
if ($env:CUDA_PATH) { $cudaDirs += $env:CUDA_PATH }
$cudas = @()
foreach ($d in ($cudaDirs | Select-Object -Unique)) {
    $nvcc = Join-Path $d 'bin\nvcc.exe'
    if (-not (Test-Path $nvcc)) { continue }
    $out = (& $nvcc --version 2>$null) -join ' '
    if ($out -match 'release (\d+)\.(\d+)' -and [int]$Matches[1] -ge 12) {
        $cudas += [pscustomobject]@{ Path = $d; Version = [version]"$($Matches[1]).$($Matches[2])" }
    }
}
$cudas = @($cudas | Sort-Object Version -Descending)

# CUDA 13 dropped GPUs older than Turing (arch < 75: GTX 10xx and older); RTX 50 (arch >= 100)
# needs CUDA 12.8+. Pick the newest installed toolkit that supports every GPU found.
function Test-CudaFits($version, $archs) {
    foreach ($a in $archs) {
        if ($version.Major -ge 13 -and $a -lt 75) { return $false }
        if ($a -ge 100 -and $version -lt [version]'12.8') { return $false }
    }
    return $true
}
$cuda = $cudas | Where-Object { Test-CudaFits $_.Version $gpuArchs } | Select-Object -First 1
$cudaPath = if ($cuda) { $cuda.Path } else { $null }

$gpuBuild = -not $CpuOnly
if ($gpuBuild -and -not $gpuName -and -not $archOverride) {
    $gpuBuild = $false
    $notes.Add('No usable NVIDIA GPU found (nvidia-smi): building the CPU-only version (SenseVoice).')
}
if ($gpuBuild -and $cudas.Count -eq 0) {
    $gpuBuild = $false
    $notes.Add('NVIDIA GPU found but no CUDA Toolkit 12+: building the CPU-only version. Install the CUDA Toolkit for Breeze / TEA-ASR on the GPU.')
}
if ($gpuBuild -and -not $cuda) {
    $gpuBuild = $false
    $have = ($cudas | ForEach-Object { "$($_.Version)" }) -join ', '
    $old = @($gpuArchs | Where-Object { $_ -lt 75 }).Count -gt 0
    $new = @($gpuArchs | Where-Object { $_ -ge 100 }).Count -gt 0
    $need = if ($old -and $new) { 'CUDA 12.8 or a newer 12.x (not 13: it no longer supports GPUs older than RTX 20)' }
        elseif ($old) { 'a CUDA 12.x Toolkit (CUDA 13 no longer supports GPUs older than RTX 20)' }
        else { 'CUDA 12.8 or newer (RTX 50 series)' }
    $notes.Add("The installed CUDA ($have) does not support this GPU; building the CPU-only version. For the GPU build install $need.")
}
if ($gpuBuild -and $cuda.Version -lt [version]'12.4') {
    $notes.Add("CUDA $($cuda.Version) may reject newer Visual Studio 2022 compilers; if the build fails with 'unsupported Microsoft Visual Studio version', install CUDA 12.4 or newer.")
}
if ($gpuBuild -and $gpuArchs.Count -eq 0) {
    $gpuArchs = @(75)
    $notes.Add('Could not read the GPU architecture; using 75 (RTX 20). Pass -CudaArch 86 (RTX 30) / 89 (RTX 40) if needed.')
}
# CMAKE_CUDA_ARCHITECTURES accepts a list: build for every GPU in the PC.
$CudaArch = ($gpuArchs | Sort-Object -Unique) -join ';'

# --- Job count from RAM (the link step needs several GB per job) -----------------------------
$ramGb = [math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB)
if ($Jobs -le 0) {
    if ($ramGb -ge 48) { $Jobs = 4 } elseif ($ramGb -ge 24) { $Jobs = 2 } else { $Jobs = 1 }
}

# --- Report -----------------------------------------------------------------------------------
$version = [regex]::Match((Get-Content "$repo\apps\desktop\src-tauri\tauri.conf.json" -Raw), '"version"\s*:\s*"([^"]+)"').Groups[1].Value
Write-Host "`nv2c $version - Windows build check" -ForegroundColor Yellow
'  {0,-14} {1}' -f 'Visual Studio', $(if ($vsPath) { $vsPath } else { 'MISSING' })
'  {0,-14} {1}' -f 'CMake', $(if ($cmakeBin) { $cmakeBin } else { 'MISSING' })
'  {0,-14} {1}' -f 'Rust (cargo)', $(if ($cargo) { $cargo } else { 'MISSING' })
'  {0,-14} {1}' -f 'LLVM', $(if ($llvmBin) { $llvmBin } else { 'MISSING' })
'  {0,-14} {1}' -f 'Node.js', $(if ($node) { $node } else { 'MISSING' })
'  {0,-14} {1}' -f 'NVIDIA GPU', $(if ($gpuName) { "$gpuName (arch $CudaArch)" } else { 'none' })
'  {0,-14} {1}' -f 'CUDA Toolkit', $(if ($cudaPath) { "$cudaPath ($($cuda.Version))" } elseif ($cudas.Count -gt 0) { 'found ' + (($cudas | ForEach-Object { "$($_.Version)" }) -join ', ') + ', none supports this GPU' } else { 'none (12+ needed for the GPU build)' })
'  {0,-14} {1} GB -> {2} job(s)' -f 'RAM', $ramGb, $Jobs
'  {0,-14} {1}' -f 'Build', $(if ($gpuBuild) { 'GPU (CUDA): Breeze, TEA-ASR, SenseVoice' } else { 'CPU-only: SenseVoice' })
foreach ($n in $notes) { Write-Host "  note: $n" -ForegroundColor Cyan }

if ($problems.Count -gt 0) {
    Write-Host "`nMissing tools:" -ForegroundColor Red
    foreach ($p in $problems) { Write-Host "  - $p" -ForegroundColor Red }
    Write-Host "`nInstall them with tools\setup-dev.ps1 (read it first, run it in an ADMIN PowerShell):"
    Write-Host '  powershell -ExecutionPolicy Bypass -File tools\setup-dev.ps1            # add -SkipCuda for CPU-only'
    Write-Host 'then open a NEW PowerShell window and run this script again. Details: docs\build-guide.md'
    exit 1
}
if ($CheckOnly) { Write-Host "`nAll required tools found." -ForegroundColor Green; exit 0 }

Write-Host "`nThe first build takes about 30-60 minutes; keep this window in the foreground."
if (-not $Yes) {
    $answer = Read-Host 'Start the build now? (y/N)'
    if ($answer -notmatch '^[yY]') { Write-Host 'Cancelled.'; exit 1 }
}

$env:LIBCLANG_PATH = $llvmBin
$buildArgs = @{ Jobs = $Jobs; CMakeBin = $cmakeBin }
if ($gpuBuild) { $buildArgs.CudaPath = $cudaPath; $buildArgs.CudaArch = $CudaArch } else { $buildArgs.CpuOnly = $true }
& (Join-Path $PSScriptRoot 'build-installer.ps1') @buildArgs

$installer = Join-Path $repo "releases\v2c_${version}_x64-setup.exe"
if (Test-Path $installer) {
    Write-Host "`nDone. Installer:" -ForegroundColor Green
    Write-Host "  $installer"
    Write-Host "  SHA-256 list: $(Join-Path $repo 'releases\SHA256SUMS.txt')"
} else {
    Write-Host "`nBuild finished but the installer was not found at $installer" -ForegroundColor Red
    exit 1
}
