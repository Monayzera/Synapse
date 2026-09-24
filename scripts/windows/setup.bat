@echo off
setlocal

echo ============================================
echo  Synapse - Windows Setup
echo ============================================
echo.

where winget >nul 2>&1
if errorlevel 1 (
    echo ERRO: winget nao encontrado. Instale o "App Installer" pela Microsoft Store e rode novamente.
    pause
    exit /b 1
)

net session >nul 2>&1
if errorlevel 1 (
    echo AVISO: terminal sem privilegios de Administrador. Instalacoes via winget podem pedir UAC ou falhar.
    echo Se algo falhar, rode este setup.bat como Administrador.
    echo.
)

set "CUDA_CHOICE="
echo Quer instalar o CUDA Toolkit? Necessario apenas para build com GPU NVIDIA, ~3GB.
echo   1 - yes
echo   2 - no
set /p CUDA_CHOICE="Escolha [1/2]: "
echo.

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VSPATH="
if exist "%VSWHERE%" (
    for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSPATH=%%i"
)

echo [1/6] Visual Studio 2022 Build Tools - MSVC C++ e Windows SDK
if defined VSPATH (
    echo     ja instalado: %VSPATH%
) else (
    echo     instalando... pode demorar varios minutos.
    winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-source-agreements --accept-package-agreements --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
    if errorlevel 1 echo     AVISO: winget falhou no Build Tools.
)
echo.

echo [2/6] Rust - rustup + toolchain stable-msvc
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
    echo     ja instalado: %USERPROFILE%\.cargo\bin\cargo.exe
) else (
    echo     baixando rustup-init.exe...
    curl -fsSL -o "%TEMP%\rustup-init.exe" https://win.rustup.rs/x86_64
    if errorlevel 1 (
        echo     ERRO: falha ao baixar rustup-init.exe. Verifique a internet.
    ) else (
        echo     instalando Rust...
        "%TEMP%\rustup-init.exe" -y --default-toolchain stable-msvc --profile minimal
        del /q "%TEMP%\rustup-init.exe" >nul 2>&1
    )
)
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
echo.

echo [3/6] LLVM - libclang
if exist "%ProgramFiles%\LLVM\bin\clang.exe" (
    echo     ja instalado: %ProgramFiles%\LLVM\bin
) else (
    winget install --id LLVM.LLVM -e --accept-source-agreements --accept-package-agreements
    if errorlevel 1 echo     AVISO: winget falhou no LLVM.
)
echo.

echo [4/6] CMake
set "CMAKE_EXE="
if exist "%ProgramFiles%\CMake\bin\cmake.exe" set "CMAKE_EXE=%ProgramFiles%\CMake\bin\cmake.exe"
if not defined CMAKE_EXE (
    where cmake >nul 2>&1
    if not errorlevel 1 set "CMAKE_EXE=cmake"
)
if defined CMAKE_EXE (
    echo     ja instalado.
) else (
    winget install --id Kitware.CMake -e --accept-source-agreements --accept-package-agreements
    if errorlevel 1 echo     AVISO: winget falhou no CMake.
)
echo.

echo [5/6] Node.js LTS
set "NODE_EXE="
if exist "%ProgramFiles%\nodejs\node.exe" set "NODE_EXE=%ProgramFiles%\nodejs\node.exe"
if not defined NODE_EXE (
    where node >nul 2>&1
    if not errorlevel 1 set "NODE_EXE=node"
)
if defined NODE_EXE (
    echo     ja instalado.
) else (
    winget install --id OpenJS.NodeJS.LTS -e --accept-source-agreements --accept-package-agreements
    if errorlevel 1 echo     AVISO: winget falhou no Node.js.
)
set "PATH=%ProgramFiles%\nodejs;%PATH%"
echo.

echo [6/6] CUDA Toolkit
if "%CUDA_CHOICE%"=="1" (
    if exist "%ProgramFiles%\NVIDIA GPU Computing Toolkit\CUDA\v*" (
        echo     ja instalado.
    ) else (
        winget install --id Nvidia.CUDA -e --accept-source-agreements --accept-package-agreements
        if errorlevel 1 echo     AVISO: winget falhou no CUDA.
    )
) else (
    echo     pulado por escolha do usuario.
)
echo.

echo Instalando dependencias do frontend - npm install
pushd "%~dp0..\.."
where npm >nul 2>&1
if errorlevel 1 (
    echo     AVISO: npm ainda nao esta no PATH desta janela. Abra um novo terminal e rode: npm install
) else (
    call npm install
    if errorlevel 1 echo     AVISO: npm install falhou. Rode manualmente depois.
)
popd
echo.

echo ============================================
echo  Verificacao final
echo ============================================
set "FALTA=0"

set "VSPATH="
if exist "%VSWHERE%" (
    for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSPATH=%%i"
)
if defined VSPATH (
    echo   [OK]    MSVC Build Tools
) else (
    echo   [FALTA] MSVC Build Tools - rode o setup de novo ou instale manualmente
    set "FALTA=1"
)

if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
    echo   [OK]    Rust / cargo
) else (
    echo   [FALTA] Rust / cargo - https://rustup.rs
    set "FALTA=1"
)

if exist "%ProgramFiles%\LLVM\bin\clang.exe" (
    echo   [OK]    LLVM / libclang
) else (
    echo   [FALTA] LLVM - winget install LLVM.LLVM
    set "FALTA=1"
)

set "CMAKE_OK=0"
if exist "%ProgramFiles%\CMake\bin\cmake.exe" set "CMAKE_OK=1"
where cmake >nul 2>&1
if not errorlevel 1 set "CMAKE_OK=1"
if "%CMAKE_OK%"=="1" (
    echo   [OK]    CMake
) else (
    echo   [FALTA] CMake - winget install Kitware.CMake
    set "FALTA=1"
)

set "NODE_OK=0"
if exist "%ProgramFiles%\nodejs\node.exe" set "NODE_OK=1"
where node >nul 2>&1
if not errorlevel 1 set "NODE_OK=1"
if "%NODE_OK%"=="1" (
    echo   [OK]    Node.js
) else (
    echo   [FALTA] Node.js - winget install OpenJS.NodeJS.LTS
    set "FALTA=1"
)

if "%CUDA_CHOICE%"=="1" (
    if exist "%ProgramFiles%\NVIDIA GPU Computing Toolkit\CUDA\v*" (
        echo   [OK]    CUDA Toolkit
    ) else (
        echo   [FALTA] CUDA Toolkit - winget install Nvidia.CUDA
        set "FALTA=1"
    )
)

echo.
if "%FALTA%"=="1" (
    echo Alguns itens faltaram. Se acabou de instalar, FECHE este terminal, abra um novo e rode setup.bat de novo para verificar.
) else (
    echo Tudo pronto. Feche e reabra o terminal para o PATH atualizar.
    echo Build CPU:  scripts\windows\build-cpu.bat
    echo Modo dev:   scripts\windows\run-dev.bat
)
echo.
pause
endlocal
