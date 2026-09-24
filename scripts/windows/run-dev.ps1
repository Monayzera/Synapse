$ErrorActionPreference = "Stop"

try {
    $root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    Set-Location $root

    Write-Host "Synapse - modo desenvolvimento" -ForegroundColor Cyan

    try {
        Get-Process -Name "synapse" -ErrorAction Stop | ForEach-Object {
            Write-Host "Encerrando instancia em execucao (PID $($_.Id))..." -ForegroundColor Yellow
            Stop-Process -Id $_.Id -Force -ErrorAction Stop
        }
        Start-Sleep -Milliseconds 600
    } catch [Microsoft.PowerShell.Commands.ProcessCommandException] {
    }

    $buildEnv = Join-Path $PSScriptRoot "build-env.ps1"
    if (-not (Test-Path $buildEnv)) {
        throw "build-env.ps1 nao encontrado."
    }
    . $buildEnv

    if (-not (Get-Command npm -ErrorAction SilentlyContinue)) {
        throw "npm nao encontrado no PATH. Instale o Node.js."
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "cargo nao encontrado no PATH. Instale o Rust (rustup)."
    }

    if (-not (Test-Path (Join-Path $root "node_modules"))) {
        Write-Host "Instalando dependencias do frontend (npm install)..." -ForegroundColor Cyan
        npm install
        if ($LASTEXITCODE -ne 0) {
            throw "npm install falhou com codigo $LASTEXITCODE."
        }
    }

    if ($env:CUDA_PATH -and (Test-Path $env:CUDA_PATH)) {
        Write-Host "Iniciando Tauri em modo dev com CUDA (compila e abre o app)..." -ForegroundColor Cyan
        npx tauri dev --features cuda
    } else {
        Write-Host "CUDA Toolkit nao encontrado: iniciando modo dev sem CUDA (Whisper local na CPU, APIs normais)..." -ForegroundColor Yellow
        npx tauri dev
    }
    exit $LASTEXITCODE
} catch {
    Write-Host ""
    Write-Host "ERRO: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
