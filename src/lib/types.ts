export type RecordMode = "push_to_talk" | "toggle";

export type LlmBackend = "local" | "open_ai_compatible" | "anthropic" | "ollama" | "groq";

export type TranscriptionBackend = "local" | "groq";

export type UiLanguage = "auto" | "pt" | "en";

export type SectionId = "general" | "voice" | "ai" | "dictionary" | "advanced" | "about";

export type LlmState = "off" | "starting" | "ready" | "failed";

export type StatusKind =
  | "idle"
  | "recording"
  | "processing"
  | "loading"
  | "error";

export interface Settings {
  hotkey_ptt: string;
  record_mode: RecordMode;
  language: string;
  whisper_model: string;
  transcription_backend: TranscriptionBackend;
  groq_api_key: string;
  groq_model: string;
  groq_llm_model: string;
  groq_llm_api_key: string;
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
  ui_language: UiLanguage;
  auto_update: boolean;
}

export type UpdatePhase =
  | "unsupported"
  | "idle"
  | "checking"
  | "up_to_date"
  | "available"
  | "downloading"
  | "ready"
  | "installing"
  | "error";

export interface UpdateStatus {
  phase: UpdatePhase;
  current_version: string;
  version: string | null;
  downloaded: number;
  total: number;
  error: string | null;
  detail: string | null;
  last_checked: number | null;
  blocked: boolean;
}

export type UpdateNoticeKind =
  | "installing"
  | "installed"
  | "up_to_date"
  | "downloading"
  | "available"
  | "failed"
  | "check_failed"
  | "postponed"
  | "busy"
  | "relocate";

export interface UpdateNotice {
  kind: UpdateNoticeKind;
  version: string | null;
}

export interface GroqModel {
  id: string;
  label: string;
  reasoning: boolean;
  context_window: number | null;
  max_completion_tokens: number | null;
  created: number;
}

export interface GroqLimit {
  model: string;
  kind: string;
  limit: number;
  at: number;
}

export interface GroqModels {
  models: GroqModel[];
  error: string | null;
  limits?: GroqLimit[];
}

export type ErrorParams = Record<string, string | number>;

export interface TestReport {
  ok: boolean;
  text: string;
  ms: number;
  model: string;
  code: string | null;
  params: ErrorParams;
  detail: string;
}

export type LocalAiDevice = "gpu" | "cpu";

export interface LocalAiStatus {
  state: LlmState;
  installed: boolean;
  model_present: boolean;
  variant: string | null;
  device: LocalAiDevice | null;
  device_name: string | null;
  repairable: boolean;
  setup_running: boolean;
  last_error: string | null;
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
  starting: boolean;
  hotkey_ready: boolean;
  llm: LlmState;
  error_code: string | null;
  autostart_launch: boolean;
  mic_denied: boolean;
  accessibility_needed: boolean;
}

export type PrivacyKind = "microphone" | "accessibility";

export interface AutostartStatus {
  enabled: boolean;
  disabled_by_windows: boolean;
  error: string | null;
}

export type HwTier = "weak" | "modest" | "capable";

export type GpuVendor = "nvidia" | "amd" | "intel" | "other";

export interface GpuInfo {
  vendor: GpuVendor;
  name: string;
  compute_cap: string | null;
  driver_version: string | null;
}

export type WhisperGpu = "allowed" | "not_compiled" | "no_nvidia" | "unsupported" | "blocked";

export interface HardwareInfo {
  total_ram_mb: number;
  logical_cores: number;
  build_gpu: boolean;
  os: string;
  tier: HwTier;
  gpu?: GpuInfo | null;
  whisper_gpu?: WhisperGpu;
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

export type PipelineErrorKind = "error" | "info";

export interface PipelineErrorPayload {
  stage: string;
  code: string;
  message: string;
  params?: ErrorParams;
  kind?: PipelineErrorKind;
}
