<p align="center">
  <img src="https://readme-typing-svg.demolab.com?font=Fira+Code&weight=600&size=22&pause=1000&color=B3C499&center=true&vCenter=true&width=620&height=58&lines=Hold+a+hotkey.+Talk.+Release.;Local+speech-to-text%2C+fully+offline.;Optional+AI+cleanup+%26+translation." alt="Hold a hotkey. Talk. Release.">
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Windows-NVIDIA%20%7C%20CPU-4f5d3b?style=flat-square&logo=windows&logoColor=white" alt="Windows">
  <img src="https://img.shields.io/badge/macOS-Apple%20Silicon-4f5d3b?style=flat-square&logo=apple&logoColor=white" alt="macOS">
  <img src="https://img.shields.io/badge/Tauri%202-Svelte%205%20%2B%20Rust-b3c499?style=flat-square&logo=tauri&logoColor=white" alt="Tauri 2">
  <img src="https://img.shields.io/badge/offline-first-e8a384?style=flat-square" alt="Offline first">
  <img src="https://img.shields.io/badge/license-MIT-cbc9c0?style=flat-square" alt="MIT">
</p>

Press a hotkey, talk, release. Your speech is transcribed **locally** and pasted into
whatever window has focus. Fully offline. The network is only touched to download
models, or if you point the AI at a remote endpoint.

## Quick start

1. Grab the latest build from **Releases** — Windows (NVIDIA or CPU) or macOS `.dmg`
   (Apple Silicon).
2. Open it, go to **Settings → Models**, download a Whisper model.
3. Hold `Ctrl+Shift+Space`, talk, release. Text lands at your cursor.

That's it. Everything else below is optional.

## Features

| | |
|---|---|
| 🎙️ **Offline STT** | whisper.cpp `large-v3-turbo` by default. GPU first, automatic CPU fallback. |
| ⚡ **Instant capture** | Mic stays pre-warmed, so recording starts the moment you press. |
| ⌨️ **Global hotkey** | Fires even when unfocused. Hold-to-talk or toggle. Bind mouse buttons too. |
| 🔊 **Confirmation beeps** | Soft tones on record start and stop, so you never miss the window. |
| ✨ **AI cleanup** | Optional local LLM rewrites the raw transcript: grammar, fillers, casing. Off by default. |
| 🌐 **AI translation** | Speak one language, paste another. Pick input and output languages. |
| 🧩 **Any model** | Paste a Hugging Face link to add Whisper or LLM models. One-click automatic llama-server setup. Delete with one click. |
| 📖 **Vocabulary** | Bias recognition toward your jargon, names and brands. Plus dictionary replacements and filler stripping. |
| 🗂️ **History** | Every transcript saved to a local SQLite database with full-text search and stats. |
| 🔌 **Bring your own backend** | Local llama-server, OpenAI-compatible, Anthropic, or Ollama. |

## How it works

```
mic -> cpal capture -> Silero VAD (trim silence) -> whisper.cpp (GPU or CPU)
    -> filler/dictionary cleanup -> optional LLM rewrite/translate -> clipboard + paste
```

- **Windows:** global hotkey via low-level `WH_KEYBOARD_LL` + `WH_MOUSE_LL` hooks.
  Bind mouse buttons (middle, back, forward) on top of Ctrl/Shift/Alt/Win.
- **macOS:** global hotkey via `CGEventTap` on a dedicated run-loop thread.
- Text is delivered by writing the clipboard and sending paste (`Ctrl+V` / `Cmd+V`),
  then restoring your previous clipboard. Elevated Windows apps block simulated input;
  there the text is left on the clipboard to paste by hand.
- The AI pass treats the transcript as untrusted data: it is fenced and the prompt is
  hardened, so spoken text cannot hijack the model into answering instead of correcting.

## Install

| Platform | Build |
|---|---|
| **Windows + NVIDIA** | GPU build. ~4 GB VRAM for turbo, +3 GB if you enable LLM cleanup. RTX 50-series needs a CUDA 12.8+ driver. |
| **Windows, no GPU** | CPU build, runs anywhere. `medium` is the practical model; turbo works but is heavy. |
| **macOS** | Apple Silicon (M1+), macOS 13+. Metal GPU by default. |

All builds need 8 GB RAM and ~5 GB free disk (turbo 1.6 GB, optional LLM 2.4 GB) plus a microphone.

## Build from source

<details>
<summary><b>Windows</b></summary>

Install the toolchain once (also needs Visual Studio 2022 with the Desktop C++ workload):

```powershell
winget install Rustlang.Rustup Kitware.CMake LLVM.LLVM
winget install Nvidia.CUDA   # GPU build only
```

Build from the project root:

```powershell
scripts\windows\build.bat        # GPU — NSIS .exe + .msi
scripts\windows\build-cpu.bat    # CPU — runs on any Windows machine
```

Both wrap the same steps:

```powershell
. .\scripts\windows\build-env.ps1   # sets CUDA_PATH, LIBCLANG_PATH, PATH
npm install
npm run fetch-deps                  # GPU: llama-server + CUDA DLLs + VAD model
npx tauri build --features cuda     # GPU
```

Installers land in `src-tauri/target/release/bundle/`.

</details>

<details>
<summary><b>macOS</b></summary>

The setup script installs every missing dependency (Homebrew, Rust, Node, CMake). Run it once.

```bash
./scripts/macos/mac-setup.sh     # check + install deps
./scripts/macos/build-dmg.sh     # build the .dmg (Apple Silicon, Metal)
```

The `.dmg` lands in `src-tauri/target/release/bundle/dmg/`.

</details>

<details>
<summary><b>Run in dev mode</b></summary>

```powershell
scripts\windows\run-dev.bat      # Windows
```

```bash
./scripts/macos/run-dev.sh       # macOS (Metal)
./scripts/macos/run-dev.sh cpu   # macOS (CPU)
```

Each kills any running instance and launches `tauri dev` with hot reload. The first
run compiles the Rust side and takes a few minutes.

</details>

## Where your data lives

Everything is kept in one folder under your Documents:

- **Windows:** `Documents\Synapse\`
- **macOS:** `~/Documents/Synapse/`

```
Synapse/
  config/    settings.json, custom_models.json
  data/      history.db, models/, bin/ (llama-server)
```

All settings are editable from the app. Everything stays on the machine.

## Source layout

```
src-tauri/src/
  audio.rs          pre-warmed cpal capture, downmix + resample to 16 kHz
  vad.rs            Silero v5 VAD via onnxruntime, energy-gated fallback
  transcribe.rs     whisper-rs, GPU then CPU, vocabulary biasing
  inputhook*.rs     global hotkey (Windows hooks / macOS CGEventTap)
  dictionary.rs     filler stripping + exact replacements
  cleanup.rs        LLM correct/translate client, prompt-injection hardened
  sound.rs          confirmation beeps via cpal output
  hf.rs             Hugging Face link parsing + file listing
  custom_models.rs  user model registry
  llama_setup.rs    one-click llama-server download + configure
  sidecar.rs        llama-server lifecycle (Job Object, killed on exit)
  models.rs         model registry + downloader with progress
  pipeline.rs       capture -> transcribe -> clean -> inject -> history
  history.rs        SQLite + FTS5, stats
  commands.rs       Tauri command surface
src/                Svelte 5 frontend: widget, settings, history windows
scripts/windows/    build-env, fetch-deps, build / build-cpu / run-dev
scripts/macos/      mac-setup, build-dmg, run-dev
```

## License

MIT. See [LICENSE](LICENSE).
