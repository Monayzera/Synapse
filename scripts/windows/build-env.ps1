$ErrorActionPreference = 'Continue'

function Import-VcVars {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path $vswhere)) {
        Write-Host "build-env: vswhere not found; MSVC environment not loaded"
        return $false
    }
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vs) {
        Write-Host "build-env: Visual Studio with C++ tools not found"
        return $false
    }
    $vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
    if (-not (Test-Path $vcvars)) {
        Write-Host "build-env: vcvars64.bat not found"
        return $false
    }
    $output = cmd /c "`"$vcvars`" >nul 2>&1 && set"
    foreach ($line in $output) {
        if ($line -match '^([^=]+)=(.*)$') {
            Set-Item -Path "Env:\$($matches[1])" -Value $matches[2]
        }
    }
    return $true
}

$msvc = Import-VcVars

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$cmakeBin = 'C:\Program Files\CMake\bin'
$llvmBin = 'C:\Program Files\LLVM\bin'
$cudaRoot = 'C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA'

$cudaVer = Get-ChildItem $cudaRoot -Directory -ErrorAction SilentlyContinue |
    Sort-Object Name -Descending | Select-Object -First 1

$paths = @($cargoBin, $cmakeBin, $llvmBin)

if ($null -ne $cudaVer) {
    $env:CUDA_PATH = $cudaVer.FullName
    $verVar = "CUDA_PATH_" + (($cudaVer.Name -replace '\.', '_').ToUpper())
    Set-Item -Path "Env:\$verVar" -Value $cudaVer.FullName
    $env:CudaToolkitDir = $cudaVer.FullName + '\'
    $paths += (Join-Path $cudaVer.FullName 'bin')
    $paths += (Join-Path $cudaVer.FullName 'bin\x64')
    $paths += (Join-Path $cudaVer.FullName 'libnvvp')
}

if (Test-Path $llvmBin) { $env:LIBCLANG_PATH = $llvmBin }

$existing = $env:PATH -split ';'
foreach ($p in $paths) {
    if ((Test-Path $p) -and ($existing -notcontains $p)) {
        $env:PATH = "$p;$env:PATH"
        $existing = $env:PATH -split ';'
    }
}

Write-Host "build-env ready: MSVC=$msvc CUDA_PATH=$env:CUDA_PATH LIBCLANG_PATH=$env:LIBCLANG_PATH"
