$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$resources = Join-Path $root 'src-tauri\resources'
$cuda = Join-Path $resources 'cuda'

New-Item -ItemType Directory -Force -Path $resources, $cuda | Out-Null

$strict = [bool]$env:CI

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Fail($msg) { if ($strict) { throw $msg } else { Write-Warning "    $msg" } }

Step 'Downloading Silero VAD model'
$sileroOut = Join-Path $resources 'silero_vad.onnx'
if (-not (Test-Path $sileroOut)) {
    $sileroUrl = 'https://raw.githubusercontent.com/snakers4/silero-vad/master/src/silero_vad/data/silero_vad.onnx'
    try {
        Invoke-WebRequest -Uri $sileroUrl -OutFile $sileroOut -UseBasicParsing
        Write-Host "    saved $sileroOut"
    } catch {
        Write-Warning "    could not download Silero VAD: $_"
    }
} else {
    Write-Host '    already present'
}

$binaries = Join-Path $resources 'binaries'
if (Test-Path $binaries) {
    Step 'Removing the old bundled llama-server'
    try {
        Remove-Item $binaries -Recurse -Force
        Write-Host "    removed $binaries"
    } catch {
        Fail "could not remove ${binaries}: $_"
    }
}

Step 'Copying CUDA runtime DLLs'
$cudaRoot = 'C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA'
$cudaVer = Get-ChildItem $cudaRoot -Directory -ErrorAction SilentlyContinue |
    Sort-Object Name -Descending | Select-Object -First 1
if ($null -ne $cudaVer) {
    $cudaDirs = @((Join-Path $cudaVer.FullName 'bin\x64'), (Join-Path $cudaVer.FullName 'bin'))
    foreach ($cudaBin in $cudaDirs) {
        if (-not (Test-Path $cudaBin)) { continue }
        foreach ($pattern in @('cudart64_*.dll', 'cublas64_*.dll', 'cublasLt64_*.dll')) {
            Get-ChildItem $cudaBin -Filter $pattern -ErrorAction SilentlyContinue | ForEach-Object {
                Copy-Item $_.FullName (Join-Path $cuda $_.Name) -Force
            }
        }
    }
    foreach ($pattern in @('cudart64_*.dll', 'cublas64_*.dll', 'cublasLt64_*.dll')) {
        if (-not (Get-ChildItem $cuda -Filter $pattern -ErrorAction SilentlyContinue)) {
            Fail "CUDA runtime DLL matching $pattern not found in $cudaVer"
        }
    }
    Write-Host "    copied CUDA runtime DLLs into $cuda"
} else {
    Fail 'CUDA Toolkit not found; GPU DLLs will rely on the user driver/runtime'
}

Write-Host 'fetch-deps complete' -ForegroundColor Green
