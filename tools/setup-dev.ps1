<#
.SYNOPSIS
  Installs the v2c Windows development toolchain. Run it yourself after reading it.

.DESCRIPTION
  Installs (skipping anything already present):
    1. Visual Studio 2022 Build Tools  - C++ workload + Windows SDK (required by Rust MSVC, whisper.cpp, Tauri)
    2. Rustup + stable-x86_64-pc-windows-msvc toolchain, clippy, rustfmt
    3. LLVM / Clang                    - libclang for bindgen (whisper-rs); sets LIBCLANG_PATH
    4. NVIDIA CUDA Toolkit (12.x+)     - GPU build of whisper.cpp. CUDA 11.0 found on this PC is too old
                                         for current MSVC; a newer toolkit installs side by side.
    5. Node.js LTS                     - Tauri UI (skipped if node is already on PATH)

  Run from an elevated PowerShell (installers need admin):
    powershell -ExecutionPolicy Bypass -File tools\setup-dev.ps1
  Optional: -SkipCuda to build CPU-only for now.
#>
[CmdletBinding()]
param(
    [switch]$SkipCuda,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'

function Test-Cmd($name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }

function Install-Winget($id, $extraArgs = @()) {
    Write-Host "==> winget install $id" -ForegroundColor Cyan
    $wgArgs = @('install', '--id', $id, '--exact', '--accept-package-agreements', '--accept-source-agreements') + $extraArgs
    & winget @wgArgs
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) {  # -1978335189 = already installed
        throw "winget install $id failed with exit code $LASTEXITCODE"
    }
}

function Test-VcTools {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) { return $false }
    $path = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    return [bool]$path
}

function Test-CudaModern {
    if (-not (Test-Cmd nvcc)) { return $false }
    $v = (& nvcc --version) -join ' '
    if ($v -match 'release (\d+)\.(\d+)') { return [int]$Matches[1] -ge 12 }
    return $false
}

$plan = @(
    @{ Name = 'VS 2022 Build Tools (C++)'; Present = (Test-VcTools) },
    @{ Name = 'Rustup (stable MSVC)';      Present = (Test-Cmd rustup) },
    @{ Name = 'LLVM / Clang';              Present = (Test-Cmd clang) -or (Test-Path "$env:ProgramFiles\LLVM\bin\libclang.dll") },
    @{ Name = 'CUDA Toolkit 12+';          Present = $SkipCuda -or (Test-CudaModern) },
    @{ Name = 'Node.js LTS';               Present = (Test-Cmd node) }
)

Write-Host "`nv2c dev setup - planned actions:" -ForegroundColor Yellow
foreach ($p in $plan) {
    $state = if ($p.Present) { 'skip (present)' } else { 'INSTALL' }
    Write-Host ("  {0,-28} {1}" -f $p.Name, $state)
}
if (-not $Yes) {
    $answer = Read-Host "`nProceed? (y/N)"
    if ($answer -notmatch '^[yY]') { Write-Host 'Aborted.'; exit 1 }
}

# 1. VS Build Tools
if (-not $plan[0].Present) {
    Install-Winget 'Microsoft.VisualStudio.2022.BuildTools' @(
        '--override',
        '--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
    )
}

# 2. Rust
if (-not $plan[1].Present) {
    Install-Winget 'Rustlang.Rustup'
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}
& rustup default stable-x86_64-pc-windows-msvc
& rustup component add clippy rustfmt

# 3. LLVM (+ LIBCLANG_PATH for bindgen)
if (-not $plan[2].Present) { Install-Winget 'LLVM.LLVM' }
$llvmBin = "$env:ProgramFiles\LLVM\bin"
if (Test-Path "$llvmBin\libclang.dll") {
    [Environment]::SetEnvironmentVariable('LIBCLANG_PATH', $llvmBin, 'User')
    $env:LIBCLANG_PATH = $llvmBin
}

# 4. CUDA
if (-not $plan[3].Present) { Install-Winget 'Nvidia.CUDA' }

# 5. Node.js
if (-not $plan[4].Present) { Install-Winget 'OpenJS.NodeJS.LTS' }

Write-Host "`n==> Versions (open a NEW terminal if something shows as missing):" -ForegroundColor Green
foreach ($c in 'rustc', 'cargo', 'clang', 'nvcc', 'node') {
    if (Test-Cmd $c) { Write-Host ("  {0,-6} {1}" -f $c, ((& $c --version) | Select-Object -First 1)) }
    else { Write-Host ("  {0,-6} not on PATH yet" -f $c) }
}
Write-Host "  LIBCLANG_PATH = $env:LIBCLANG_PATH"
Write-Host "`nNext: open a new terminal in the repo and run:  cargo test --workspace"
