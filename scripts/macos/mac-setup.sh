#!/usr/bin/env bash

ensure_mac_deps() {
  if ! xcode-select -p >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
    echo "Instalando Xcode Command Line Tools (uma janela do sistema vai abrir)..."
    xcode-select --install >/dev/null 2>&1 || true
    echo "ERRO: conclua a instalacao das Command Line Tools e rode o script novamente." >&2
    exit 1
  fi

  if ! command -v brew >/dev/null 2>&1; then
    if [ -x /opt/homebrew/bin/brew ]; then
      eval "$(/opt/homebrew/bin/brew shellenv)"
    elif [ -x /usr/local/bin/brew ]; then
      eval "$(/usr/local/bin/brew shellenv)"
    else
      echo "Instalando Homebrew..."
      NONINTERACTIVE=1 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
      if [ -x /opt/homebrew/bin/brew ]; then
        eval "$(/opt/homebrew/bin/brew shellenv)"
      elif [ -x /usr/local/bin/brew ]; then
        eval "$(/usr/local/bin/brew shellenv)"
      fi
    fi
  fi

  if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
  fi
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Instalando Rust (rustup)..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile default
    . "$HOME/.cargo/env"
  fi

  if ! command -v node >/dev/null 2>&1; then
    echo "Instalando Node.js..."
    brew install node
  fi

  if ! command -v cmake >/dev/null 2>&1; then
    echo "Instalando CMake..."
    brew install cmake
  fi
}

if [ "${BASH_SOURCE[0]:-$0}" = "$0" ]; then
  echo "Verificando e instalando dependencias do Synapse (macOS)..."
  ensure_mac_deps
  echo ""
  echo "Dependencias prontas:"
  printf "  Rust   "; command -v cargo >/dev/null 2>&1 && cargo --version || echo "AUSENTE"
  printf "  Node   "; command -v node >/dev/null 2>&1 && node -v || echo "AUSENTE"
  printf "  cmake  "; command -v cmake >/dev/null 2>&1 && cmake --version | head -1 || echo "AUSENTE"
  printf "  clang  "; command -v clang >/dev/null 2>&1 && echo "ok" || echo "AUSENTE"
  echo ""
  echo "Nada para baixar quando ja esta tudo instalado. Agora rode:"
  echo "  ./scripts/macos/run-dev.sh        (modo dev, Metal)"
  echo "  ./scripts/macos/build-dmg.sh      (gera o .dmg)"
fi
