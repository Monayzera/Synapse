$ErrorActionPreference = "Stop"

try {
    $root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    Set-Location $root

    Write-Host "Building Synapse ..." -ForegroundColor Cyan

    . (Join-Path $PSScriptRoot "build-env.ps1")

    if (-not $env:LIBCLANG_PATH) {
        throw "LIBCLANG_PATH not set. Install LLVM (e.g. winget install LLVM.LLVM)."
    }
    if (-not (Get-Command npm -ErrorAction SilentlyContinue)) {
        throw "npm not found. Install Node.js 18+."
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "cargo not found. Install Rust (https://rustup.rs)."
    }
    if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
        throw "cmake not found. Install CMake (winget install Kitware.CMake) for whisper.cpp."
    }
    if (-not $env:CUDA_PATH) {
        throw "CUDA Toolkit not found. Install CUDA 12.8+ to build the Windows installer."
    }

    if (-not (Test-Path (Join-Path $root "node_modules"))) {
        Write-Host "Installing frontend dependencies..." -ForegroundColor Cyan
        npm install
        if ($LASTEXITCODE -ne 0) { throw "npm install failed ($LASTEXITCODE)." }
    }

    $silero = Join-Path $root "src-tauri\resources\silero_vad.onnx"
    if (-not (Test-Path $silero)) {
        Write-Host "Downloading Silero VAD model..." -ForegroundColor Cyan
        New-Item -ItemType Directory -Force -Path (Split-Path $silero) | Out-Null
        $url = "https://raw.githubusercontent.com/snakers4/silero-vad/master/src/silero_vad/data/silero_vad.onnx"
        Invoke-WebRequest -Uri $url -OutFile $silero -UseBasicParsing
    }

    Write-Host "Fetching native deps (CUDA runtime DLLs)..." -ForegroundColor Cyan
    powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "fetch-deps.ps1")
    Write-Host "Compiling release (this takes a while)..." -ForegroundColor Cyan
    npx tauri build --features cuda
    if ($LASTEXITCODE -ne 0) { throw "tauri build failed ($LASTEXITCODE)." }

    $bundle = Join-Path $root "src-tauri\target\release\bundle"
    Write-Host ""
    Write-Host "Done. Installers:" -ForegroundColor Green
    Get-ChildItem $bundle -Recurse -Include *.exe, *.msi -ErrorAction SilentlyContinue |
        ForEach-Object { Write-Host "  $($_.FullName)" }
    Write-Host "Standalone exe:" -ForegroundColor Green
    Write-Host "  $(Join-Path $root 'src-tauri\target\release\synapse.exe')"
} catch {
    Write-Host ""
    Write-Host "BUILD ERROR: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
