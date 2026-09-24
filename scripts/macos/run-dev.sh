#!/usr/bin/env bash
set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$ROOT"

. "$SCRIPT_DIR/mac-setup.sh"

MODE="${1:-metal}"
FEATURES=""
if [ "$MODE" = "cpu" ]; then
  echo "Synapse - modo desenvolvimento (CPU)"
else
  echo "Synapse - modo desenvolvimento (Metal GPU)"
  FEATURES="--features metal"
fi

ensure_mac_deps

if pgrep -x "synapse" >/dev/null 2>&1; then
  echo "Encerrando instancia em execucao..."
  pkill -x "synapse" || true
  sleep 0.6
fi

if [ ! -d node_modules ]; then
  echo "Instalando dependencias do frontend (npm install)..."
  npm install
fi

echo "Iniciando Tauri em modo dev (compila e abre o app)..."
exec npx tauri dev $FEATURES
