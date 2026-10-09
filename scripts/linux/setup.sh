#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$ROOT"

APT_PACKAGES=(
  libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev
  libayatana-appindicator3-dev librsvg2-dev libgtk-3-dev patchelf xdg-utils
  libfuse2t64 pkg-config libasound2-dev cmake clang libclang-dev libvulkan-dev
  glslc libxkbcommon-dev libdbus-1-dev
)

if ! command -v apt-get >/dev/null 2>&1; then
  echo "ERRO: este script precisa de apt (Ubuntu 24.04 ou superior)." >&2
  exit 1
fi

OS_ID="$( (. /etc/os-release && printf '%s' "${ID:-}") 2>/dev/null || true)"
OS_VERSION="$( (. /etc/os-release && printf '%s' "${VERSION_ID:-}") 2>/dev/null || true)"
if [ "$OS_ID" != "ubuntu" ] || ! dpkg --compare-versions "$OS_VERSION" ge 24.04; then
  echo "ERRO: este script suporta apenas Ubuntu 24.04 ou superior (detectado: ${OS_ID:-desconhecido} ${OS_VERSION:-desconhecida})." >&2
  exit 1
fi

if ! command -v node >/dev/null 2>&1 || [ "$(node -p 'process.versions.node.split(".")[0]')" -lt 22 ]; then
  echo "ERRO: Node.js 22 ou superior e necessario. Instale em https://nodejs.org e rode o script novamente." >&2
  exit 1
fi

echo "Instalando dependencias do sistema (o sudo vai pedir sua senha)..."
sudo apt-get update
sudo apt-get install -y "${APT_PACKAGES[@]}"

if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "Instalando Rust (rustup)..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile default
  . "$HOME/.cargo/env"
fi

echo "Instalando dependencias do frontend (npm ci)..."
npm ci

echo ""
echo "Dependencias prontas:"
printf "  Rust   "; cargo --version
printf "  Node   "; node -v
printf "  npm    "; npm -v
echo ""
echo "Agora rode:"
echo "  ./scripts/linux/run-dev.sh   (modo dev, Vulkan)"
echo "  ./scripts/linux/build.sh     (gera AppImage e .deb)"
