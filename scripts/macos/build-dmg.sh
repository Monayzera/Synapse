#!/usr/bin/env bash
set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$ROOT"

. "$SCRIPT_DIR/mac-setup.sh"

echo "Building Synapse .dmg (arm64 Apple Silicon, Metal)..."

ensure_mac_deps

if [ ! -d node_modules ]; then
  echo "Instalando dependencias do frontend (npm install)..."
  npm install
fi

SILERO="src-tauri/resources/silero_vad.onnx"
if [ ! -f "$SILERO" ]; then
  echo "Baixando modelo Silero VAD..."
  mkdir -p "$(dirname "$SILERO")"
  curl -fL "https://raw.githubusercontent.com/snakers4/silero-vad/master/src/silero_vad/data/silero_vad.onnx" -o "$SILERO"
fi

echo "Compilando release e empacotando .dmg (demora alguns minutos)..."
npx tauri build --features metal --bundles dmg

DMG_DIR="src-tauri/target/release/bundle/dmg"
echo ""
echo "Pronto. Instalador DMG:"
if [ -d "$DMG_DIR" ]; then
  find "$DMG_DIR" -name "*.dmg" -print | while IFS= read -r f; do
    echo "  $f"
  done
else
  echo "  (nenhum .dmg encontrado em $DMG_DIR)"
fi
