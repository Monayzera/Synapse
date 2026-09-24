$ErrorActionPreference = "Stop"

try {
    $root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
    Set-Location $root

    try {
        Get-Process -Name "synapse" -ErrorAction Stop | Stop-Process -Force -ErrorAction Stop
        Start-Sleep -Milliseconds 600
    } catch [Microsoft.PowerShell.Commands.ProcessCommandException] {
    }

    . (Join-Path $PSScriptRoot "build-env.ps1") | Out-Null

    $logDir = Join-Path $root "logs"
    if (-not (Test-Path $logDir)) {
        New-Item -ItemType Directory -Path $logDir | Out-Null
    }
    $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
    $out = Join-Path $logDir "app-$stamp.out.log"
    $err = Join-Path $logDir "app-$stamp.err.log"

    Start-Process -FilePath (Join-Path $root "src-tauri\target\debug\synapse.exe") `
        -WorkingDirectory $root `
        -RedirectStandardOutput $out `
        -RedirectStandardError $err

    Start-Sleep -Seconds 3
    $p = Get-Process -Name "synapse" -ErrorAction Stop
    Write-Host "RODANDO PID $($p.Id)"
    Write-Host "LOG_OUT $out"
    Write-Host "LOG_ERR $err"
} catch {
    Write-Host "ERRO: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
