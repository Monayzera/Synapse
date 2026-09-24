export type RecordMode = "push_to_talk" | "toggle";

export type LlmBackend = "local" | "open_ai_compatible" | "anthropic" | "ollama" | "groq";

export type TranscriptionBackend = "local" | "groq";

export type StatusKind =
  | "idle"
  | "recording"
  | "processing"
  | "loading"
  | "error";

export interface Settings {
  hotkey_ptt: string;
  hotkey_toggle: string;
  record_mode: RecordMode;
  language: string;
  whisper_model: string;
  transcription_backend: TranscriptionBackend;
  groq_api_key: string;
  groq_model: string;
  groq_llm_model: string;
  groq_reuse_transcription_key: boolean;
  audio_device: string | null;
  vad_enabled: boolean;
  vad_threshold: number;
  min_silence_ms: number;
  speech_pad_ms: number;
  filler_removal: boolean;
  filler_words: string[];
  dictionary: Record<string, string>;
  vocabulary: string[];
  llm_enabled: boolean;
  llm_format_paragraphs: boolean;
  translation_enabled: boolean;
  translation_target: string;
  llm_backend: LlmBackend;
  llm_local_model: string;
  llm_endpoint: string;
  llm_api_key: string;
  llm_model_name: string;
  llm_timeout_ms: number;
  llm_temperature: number;
  llm_gpu_layers: number;
  autostart: boolean;
  restore_clipboard: boolean;
  paste_delay_ms: number;
  prefer_gpu: boolean;
}

export interface StatusPayload {
  status: StatusKind;
  recording: boolean;
  cpu_mode: boolean;
  engine_ready: boolean;
  model: string;
  audio_available: boolean;
  vad_active: boolean;
  error: string | null;
}

export type HwTier = "weak" | "modest" | "capable";

export interface HardwareInfo {
  total_ram_mb: number;
  logical_cores: number;
  build_gpu: boolean;
  os: string;
  tier: HwTier;
}

export interface HistoryEntry {
  id: number;
  created_at: number;
  duration_ms: number;
  word_count: number;
  raw_text: string;
  final_text: string;
  language: string;
  on_gpu: boolean;
  llm_used: boolean;
  cloud: boolean;
}

export interface Stats {
  total_entries: number;
  total_words: number;
  total_speaking_ms: number;
  avg_wpm: number;
}

export type ModelKind = "whisper" | "llm";

export interface ModelInfo {
  id: string;
  label: string;
  kind: ModelKind;
  filename: string;
  url: string;
  size_bytes: number;
}

export type HfParse =
  | { kind: "file"; repo: string | null; filename: string; url: string; guessed: ModelKind | null }
  | { kind: "repo"; repo: string }
  | { kind: "invalid"; reason: string };

export interface HfFile {
  filename: string;
  url: string;
  size_bytes: number;
  guessed: ModelKind | null;
}

export type LlamaStage =
  | "resolve_release"
  | "download_binary"
  | "unzip"
  | "download_model"
  | "configure_start";

export interface LlamaStatus {
  binary: boolean;
  model_present: boolean;
  ready: boolean;
}

export interface LlamaSetupProgress {
  stage: LlamaStage;
  pct: number;
  overall_pct: number;
  message: string;
  done: boolean;
  error: string | null;
}

export interface ModelStatus {
  info: ModelInfo;
  present: boolean;
  actual_bytes: number;
}

export interface DownloadProgress {
  id: string;
  downloaded: number;
  total: number;
  pct: number;
  done: boolean;
  error?: string | null;
}

export interface CompletePayload {
  raw_text: string;
  final_text: string;
  word_count: number;
  duration_ms: number;
  on_gpu: boolean;
  llm_used: boolean;
}

export interface PipelineErrorPayload {
  stage: string;
  message: string;
}
