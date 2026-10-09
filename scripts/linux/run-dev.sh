#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$ROOT"

if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi

echo "Synapse - modo desenvolvimento (Vulkan)"

if pgrep -x "synapse" >/dev/null 2>&1; then
  echo "Encerrando instancia em execucao..."
  pkill -x "synapse" || true
  sleep 0.6
fi

if [ ! -d node_modules ]; then
  echo "Instalando dependencias do frontend (npm ci)..."
  npm ci
fi

echo "Iniciando Tauri em modo dev (compila e abre o app)..."
exec npm run tauri dev -- --features vulkan
