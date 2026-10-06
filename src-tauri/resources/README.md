# Bundled runtime resources

This folder is packaged into the installer. Large binaries are fetched by
`scripts/windows/fetch-deps.ps1` (run before `npm run tauri build`) and are not committed
to source control.

Expected contents after `fetch-deps`:

- `silero_vad.onnx` — Silero VAD model used to trim silence.
- `cuda/*.dll` — CUDA runtime DLLs (`cudart`, `cublas`, `cublasLt`) for the
  Whisper GPU path on machines without the full CUDA Toolkit installed.

The local AI server (`llama-server`) is not bundled. The app downloads the build that
matches the graphics card into the per-user data folder on first use.

Speech and LLM model weights (`ggml-*.bin`, `*.gguf`) are downloaded on first run
into the per-user app data folder, not bundled here.
