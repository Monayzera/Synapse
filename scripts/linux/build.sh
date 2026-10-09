#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$ROOT"

if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  . "$HOME/.cargo/env"
fi

if [ ! -d node_modules ]; then
  echo "Instalando dependencias do frontend (npm ci)..."
  npm ci
fi

echo "Compilando release e empacotando AppImage e .deb (demora alguns minutos)..."
npm run tauri build -- --features vulkan --bundles appimage,deb

BUNDLE_DIR="src-tauri/target/release/bundle"
FOUND="$(find "$BUNDLE_DIR/appimage" "$BUNDLE_DIR/deb" -maxdepth 1 -type f \( -name '*.AppImage' -o -name '*.deb' \) 2>/dev/null || true)"
echo ""
echo "Pronto. Instaladores Linux:"
if [ -n "$FOUND" ]; then
  printf '%s\n' "$FOUND" | sed 's/^/  /'
else
  echo "  (nenhum AppImage ou .deb encontrado em $BUNDLE_DIR)"
fi
