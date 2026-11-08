<#
.SYNOPSIS
  Downloads Breeze-ASR-25 from Hugging Face, converts it to whisper.cpp GGML, and quantizes it
  to q5_0 and q8_0. Run it yourself after reading it (see README.md in this folder).

.DESCRIPTION
  Steps (each is skipped if its output already exists):
    1. Create a Python venv in tools\convert-breeze\.venv and install:
         torch (CPU build, from download.pytorch.org), transformers 4.x, safetensors,
         numpy, huggingface_hub
    2. Download MediaTek-Research/Breeze-ASR-25 (only the files the converter needs, ~3.1 GB;
       optimizer.bin and the .pt checkpoints in that repo are skipped)
    3. Clone whisper.cpp at the tag bundled by whisper-rs-sys 0.15 (v1.8.3) and download
       openai/whisper's mel_filters.npz (the converter needs it)
    4. Convert with whisper.cpp models\convert-h5-to-ggml.py  -> f16 GGML (~3.1 GB)
    5. Build whisper.cpp's whisper-quantize (CPU only, CMake + MSVC) and quantize to q5_0, q8_0
    6. Print size and SHA-256 of each output

  Outputs (git-ignored):  <repo>\models\ggml-breeze-asr-25-{f16,q5_0,q8_0}.bin
  Work files (git-ignored): <repo>\models\.work\   (delete afterwards with -Clean)

  Needs: Python 3.12 (py launcher or python on PATH), git, VS 2022 Build Tools (C++),
  ~12 GB free disk. Installs nothing system-wide; everything Python goes into the venv.

  Usage (from the repo root, normal PowerShell, no admin needed):
    powershell -ExecutionPolicy Bypass -File tools\convert-breeze\convert-breeze.ps1
  Options: -Yes (skip the confirmation), -KeepF16 (keep the f16 file), -Clean (remove work files
  at the end), -ModelsDir <path>, -WorkDir <path>.
#>
[CmdletBinding()]
param(
    [string]$ModelsDir,
    [string]$WorkDir,
    [switch]$Yes,
    [switch]$KeepF16,
    [switch]$Clean
)

$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $ModelsDir) { $ModelsDir = Join-Path $RepoRoot 'models' }
if (-not $WorkDir) { $WorkDir = Join-Path $ModelsDir '.work' }
$Venv = Join-Path $PSScriptRoot '.venv'
$VenvPy = Join-Path $Venv 'Scripts\python.exe'

$HfRepo = 'MediaTek-Research/Breeze-ASR-25'
$WhisperCppTag = 'v1.8.3'   # matches whisper-rs-sys 0.15 (whisper-rs 0.16)
$MelFiltersUrl = 'https://raw.githubusercontent.com/openai/whisper/main/whisper/assets/mel_filters.npz'
$VsCmakeBin = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin"

$HfDir = Join-Path $WorkDir 'Breeze-ASR-25'
$WhisperCppDir = Join-Path $WorkDir 'whisper.cpp'
$OpenAiWhisperDir = Join-Path $WorkDir 'openai-whisper'
$F16 = Join-Path $ModelsDir 'ggml-breeze-asr-25-f16.bin'
$Quants = @('q5_0', 'q8_0')

function Test-Cmd($name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }
function Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Invoke-Checked {
    param([string]$Exe, [string[]]$Arguments)
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Exe failed with exit code $LASTEXITCODE" }
}

# ---------- prerequisites ----------
$Python = $null
if (Test-Cmd py) {
    try {
        $null = & py -3.12 -c "import sys" 2>&1
        if ($LASTEXITCODE -eq 0) { $Python = @('py', '-3.12') }
    } catch { }
}
if (-not $Python -and (Test-Cmd python)) { $Python = @('python') }
if (-not $Python) { throw 'Python 3.12 not found (need the py launcher or python on PATH).' }
if (-not (Test-Cmd git)) { throw 'git not found on PATH.' }
if (-not (Test-Cmd cmake)) {
    if (Test-Path (Join-Path $VsCmakeBin 'cmake.exe')) {
        $env:Path = "$VsCmakeBin;$env:Path"   # this PowerShell session only
    } else {
        throw "cmake not found on PATH or in $VsCmakeBin (install VS 2022 Build Tools with C++)."
    }
}

New-Item -ItemType Directory -Force $ModelsDir | Out-Null
$drive = (Get-Item $ModelsDir).PSDrive
$freeGb = [math]::Round($drive.Free / 1GB, 1)

Write-Host "`nBreeze-ASR-25 conversion - planned actions:" -ForegroundColor Yellow
Write-Host "  Python venv : $Venv"
Write-Host "                pip install torch (CPU, ~200 MB), transformers>=4.48,<5, safetensors, numpy, huggingface_hub"
Write-Host "  Download    : $HfRepo (~3.1 GB) -> $HfDir"
Write-Host "  Clone       : whisper.cpp $WhisperCppTag -> $WhisperCppDir"
Write-Host "  Download    : mel_filters.npz (openai/whisper, ~4 KB)"
Write-Host "  Build       : whisper-quantize (CMake + MSVC, CPU only)"
Write-Host "  Outputs     : $ModelsDir\ggml-breeze-asr-25-{f16,q5_0,q8_0}.bin"
Write-Host "  Disk        : ~12 GB needed, $freeGb GB free on $($drive.Name):"
Write-Host "  License     : Breeze-ASR-25 is Apache-2.0 (see README.md)."
if ($freeGb -lt 12) { Write-Warning "Less than 12 GB free; the conversion may run out of space." }
if (-not $Yes) {
    $answer = Read-Host "`nProceed? [y/N]"
    # Positive match only: Read-Host returns $null when stdin is not a console, and
    # `$null -notmatch ...` is not $true, so a negative test would silently proceed.
    $confirmed = "$answer" -match '^\s*(y|yes)\s*$'
    if (-not $confirmed) { Write-Host 'Aborted.'; exit 1 }
}
New-Item -ItemType Directory -Force $WorkDir | Out-Null

# ---------- 1. venv ----------
Step 'Python venv'
if (-not (Test-Path $VenvPy)) {
    $pyExe = $Python[0]
    $pyArgs = @()
    if ($Python.Count -gt 1) { $pyArgs = $Python[1..($Python.Count - 1)] }
    Invoke-Checked $pyExe ($pyArgs + @('-m', 'venv', $Venv))
}
Invoke-Checked $VenvPy @('-m', 'pip', 'install', '--upgrade', 'pip')
Invoke-Checked $VenvPy @('-m', 'pip', 'install', 'torch', '--index-url', 'https://download.pytorch.org/whl/cpu')
# transformers 5 changed the default load dtype to "auto" (bf16 here), which the converter's
# tensor.numpy() cannot handle; 4.x loads float32 as the model config says.
Invoke-Checked $VenvPy @('-m', 'pip', 'install', 'transformers>=4.48,<5', 'safetensors', 'numpy', 'huggingface_hub')

# ---------- 2. download ----------
Step "Download $HfRepo"
$dl = @"
from huggingface_hub import snapshot_download
snapshot_download(
    repo_id='$HfRepo',
    local_dir=r'$HfDir',
    allow_patterns=['config.json', 'generation_config.json', 'model.safetensors',
                    'vocab.json', 'added_tokens.json', 'merges.txt', 'normalizer.json',
                    'tokenizer.json', 'tokenizer_config.json', 'special_tokens_map.json',
                    'preprocessor_config.json', 'LICENSE', 'README.md'],
)
"@
Invoke-Checked $VenvPy @('-c', $dl)

# ---------- 3. whisper.cpp + mel filters ----------
Step "whisper.cpp $WhisperCppTag"
if (-not (Test-Path (Join-Path $WhisperCppDir 'CMakeLists.txt'))) {
    Invoke-Checked git @('clone', '--depth', '1', '--branch', $WhisperCppTag, 'https://github.com/ggml-org/whisper.cpp', $WhisperCppDir)
}
$melDir = Join-Path $OpenAiWhisperDir 'whisper\assets'
$mel = Join-Path $melDir 'mel_filters.npz'
if (-not (Test-Path $mel)) {
    New-Item -ItemType Directory -Force $melDir | Out-Null
    Invoke-WebRequest -Uri $MelFiltersUrl -OutFile $mel -UseBasicParsing
}

# ---------- 4. convert ----------
Step 'Convert to GGML f16'
if (-not (Test-Path $F16)) {
    $convOut = Join-Path $WorkDir 'ggml-out'
    New-Item -ItemType Directory -Force $convOut | Out-Null
    $script = Join-Path $WhisperCppDir 'models\convert-h5-to-ggml.py'
    # -I: isolated mode, so nothing in the downloaded folders is imported by accident.
    Invoke-Checked $VenvPy @('-I', $script, $HfDir, $OpenAiWhisperDir, $convOut)
    Move-Item -Force (Join-Path $convOut 'ggml-model.bin') $F16
}

# ---------- 5. quantize ----------
Step 'Build whisper-quantize'
$build = Join-Path $WorkDir 'whisper.cpp-build'
$quantExe = Join-Path $build 'bin\Release\whisper-quantize.exe'
if (-not (Test-Path $quantExe)) {
    Invoke-Checked cmake @('-S', $WhisperCppDir, '-B', $build, '-DGGML_CUDA=OFF', '-DWHISPER_BUILD_TESTS=OFF', '-DWHISPER_BUILD_EXAMPLES=ON', '-DWHISPER_SDL2=OFF')
    Invoke-Checked cmake @('--build', $build, '--config', 'Release', '--target', 'whisper-quantize', '-j')
}
if (-not (Test-Path $quantExe)) {
    $found = Get-ChildItem $build -Recurse -Filter 'whisper-quantize.exe' | Select-Object -First 1
    if (-not $found) { throw "whisper-quantize.exe not found under $build" }
    $quantExe = $found.FullName
}

foreach ($q in $Quants) {
    $out = Join-Path $ModelsDir "ggml-breeze-asr-25-$q.bin"
    Step "Quantize $q"
    if (-not (Test-Path $out)) { Invoke-Checked $quantExe @($F16, $out, $q) }
}

# ---------- 6. report ----------
Step 'Results'
$files = @($Quants | ForEach-Object { Join-Path $ModelsDir "ggml-breeze-asr-25-$_.bin" })
if ($KeepF16) { $files = @($F16) + $files }
foreach ($f in $files) {
    $size = [math]::Round((Get-Item $f).Length / 1MB)
    $hash = (Get-FileHash -Algorithm SHA256 $f).Hash.ToLower()
    Write-Host ("  {0}  {1} MB  sha256 {2}" -f (Split-Path $f -Leaf), $size, $hash)
}
if (-not $KeepF16) { Remove-Item -Force $F16; Write-Host '  (removed the f16 intermediate; use -KeepF16 to keep it)' }
if ($Clean) { Remove-Item -Recurse -Force $WorkDir; Write-Host "  (removed $WorkDir)" }

Write-Host "`nNext: copy the lines above into tasks/spike-results.md, then run the bench (README.md)." -ForegroundColor Green
