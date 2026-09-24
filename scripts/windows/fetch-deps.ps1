$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$resources = Join-Path $root 'src-tauri\resources'
$binaries = Join-Path $resources 'binaries'
$cuda = Join-Path $resources 'cuda'

New-Item -ItemType Directory -Force -Path $resources, $binaries, $cuda | Out-Null

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

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

Step 'Resolving latest llama.cpp Windows CUDA release'
try {
    $headers = @{ 'User-Agent' = 'synapse' }
    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/ggml-org/llama.cpp/releases/latest' -Headers $headers
    $asset = $release.assets |
        Where-Object { $_.name -match '(?i)win.*cuda.*x64\.zip$' -or $_.name -match '(?i)cuda.*win.*x64\.zip$' } |
        Select-Object -First 1

    if ($null -eq $asset) {
        Write-Warning "    no Windows CUDA asset found in release $($release.tag_name). LLM cleanup will fall back to raw text until a llama-server.exe is placed in src-tauri/resources/binaries."
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
            Write-Warning '    llama-server.exe not found inside the archive'
        }
    }
} catch {
    Write-Warning "    llama.cpp fetch failed: $_"
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
                Copy-Item $_.FullName (Join-Path $binaries $_.Name) -Force
            }
        }
    }
    Write-Host "    copied CUDA runtime DLLs"
} else {
    Write-Warning '    CUDA Toolkit not found; GPU DLLs will rely on the user driver/runtime'
}

Write-Host 'fetch-deps complete' -ForegroundColor Green
