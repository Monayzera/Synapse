$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$resources = Join-Path $root 'src-tauri\resources'
$binaries = Join-Path $resources 'binaries'
$cuda = Join-Path $resources 'cuda'

New-Item -ItemType Directory -Force -Path $resources, $binaries, $cuda | Out-Null

$strict = [bool]$env:CI
$llamaTag = if ($env:LLAMA_TAG) { $env:LLAMA_TAG } else { 'b11158' }
$llamaAsset = "llama-$llamaTag-bin-win-cuda-12.4-x64.zip"

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

Step "Resolving llama.cpp $llamaTag Windows CUDA build"
try {
    $headers = @{ 'User-Agent' = 'synapse' }
    if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/ggml-org/llama.cpp/releases/tags/$llamaTag" -Headers $headers
    $asset = $release.assets | Where-Object { $_.name -eq $llamaAsset } | Select-Object -First 1

    if ($null -eq $asset) {
        Fail "asset $llamaAsset not found in llama.cpp release $llamaTag"
    } else {
        Step "Downloading $($asset.name)"
        $zip = Join-Path $env:TEMP $asset.name
        Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -UseBasicParsing -Headers $headers
        $extract = Join-Path $env:TEMP 'llama-extract'
        if (Test-Path $extract) { Remove-Item $extract -Recurse -Force }
        Expand-Archive -Path $zip -DestinationPath $extract -Force

        $server = Get-ChildItem $extract -Recurse -Filter 'llama-server.exe' | Select-Object -First 1
        if ($null -ne $server) {
            Copy-Item $server.FullName (Join-Path $binaries 'llama-server.exe') -Force
            Get-ChildItem $server.Directory -Filter '*.dll' | ForEach-Object {
                Copy-Item $_.FullName (Join-Path $binaries $_.Name) -Force
            }
            Write-Host "    installed llama-server.exe + dlls into $binaries"
        } else {
            Fail 'llama-server.exe not found inside the archive'
        }
    }
} catch {
    Fail "llama.cpp fetch failed: $_"
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
