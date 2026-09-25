import { api, on, type UnlistenFn } from "../lib/ipc";
import type {
  Settings,
  ModelStatus,
  DownloadProgress,
  HardwareInfo,
  StatusPayload,
  LlamaStatus,
  LlamaSetupProgress,
  AutostartStatus,
  SectionId,
  UpdateStatus,
  GroqModels,
} from "../lib/types";
import { tOr, locale } from "../lib/i18n.svelte";

export const SECTIONS: SectionId[] = ["general", "voice", "ai", "dictionary", "advanced", "about"];

export function isSection(value: unknown): value is SectionId {
  return typeof value === "string" && (SECTIONS as string[]).includes(value);
}

export type Patch = Partial<Settings>;

type KeysOf<T> = { [K in keyof Settings]: Settings[K] extends T ? K : never }[keyof Settings];
export type NumberKey = KeysOf<number>;
export type TextKey = KeysOf<string>;
export type ListKey = KeysOf<string[]>;
export type BoolKey = KeysOf<boolean>;

const DEFAULTS: Settings = {
  hotkey_ptt: "Ctrl+Shift+Space",
  record_mode: "push_to_talk",
  language: "auto",
  whisper_model: "large-v3-turbo",
  transcription_backend: "local",
  groq_api_key: "",
  groq_model: "whisper-large-v3-turbo",
  groq_llm_model: "qwen/qwen3.8-27b",
  groq_llm_api_key: "",
  audio_device: null,
  vad_enabled: true,
  vad_threshold: 0.5,
  min_silence_ms: 200,
  speech_pad_ms: 120,
  filler_removal: true,
  filler_words: [],
  dictionary: {},
  vocabulary: [],
  llm_enabled: false,
  llm_format_paragraphs: false,
  translation_enabled: false,
  translation_target: "English",
  llm_backend: "local",
  llm_local_model: "google_gemma-3-4b-it-Q4_K_M.gguf",
  llm_endpoint: "http://127.0.0.1:8123/v1",
  llm_api_key: "",
  llm_model_name: "local",
  llm_timeout_ms: 2000,
  llm_temperature: 0.1,
  llm_gpu_layers: 99,
  autostart: false,
  restore_clipboard: true,
  paste_delay_ms: 120,
  prefer_gpu: true,
  ui_language: "auto",
  auto_update: true,
};

type SaveState = "idle" | "saved" | "error";

export const app = $state({
  settings: null as Settings | null,
  loadSlow: false,
  section: "general" as SectionId,
  devices: [] as string[],
  models: [] as ModelStatus[],
  downloads: {} as Record<string, DownloadProgress>,
  hw: null as HardwareInfo | null,
  status: null as StatusPayload | null,
  llama: null as LlamaStatus | null,
  llamaProgress: null as LlamaSetupProgress | null,
  llamaRunning: false,
  autostart: null as AutostartStatus | null,
  update: null as UpdateStatus | null,
  groq: null as GroqModels | null,
  save: "idle" as SaveState,
  savePulse: 0,
  saveError: "",
});

export function describeError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  try {
    return JSON.stringify(e) ?? String(e);
  } catch {
    return String(e);
  }
}

export function errorText(e: unknown, fallback: string): string {
  const detail = describeError(e).trim();
  if (/^[a-z_]+$/.test(detail)) return tOr("err." + detail, fallback);
  return fallback;
}

export function fmtBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "";
  const fmt = (v: number, digits: number) =>
    v.toLocaleString(locale(), { minimumFractionDigits: digits, maximumFractionDigits: digits });
  const gb = n / 1_073_741_824;
  if (gb >= 1) return fmt(gb, 1) + " GB";
  return fmt(n / 1_048_576, 0) + " MB";
}

function normalize(raw: unknown): Settings | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const merged = { ...DEFAULTS, ...(raw as Partial<Settings>) };
  if (!Array.isArray(merged.filler_words)) merged.filler_words = [];
  if (!Array.isArray(merged.vocabulary)) merged.vocabulary = [];
  if (!merged.dictionary || typeof merged.dictionary !== "object" || Array.isArray(merged.dictionary)) {
    merged.dictionary = {};
  }
  return merged;
}

function same(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  try {
    return JSON.stringify(a) === JSON.stringify(b);
  } catch {
    return false;
  }
}

let pending: Record<string, unknown> = {};
let inflight = new Set<string>();
let saveTimer: ReturnType<typeof setTimeout> | undefined;
let savedTimer: ReturnType<typeof setTimeout> | undefined;
let chain: Promise<void> = Promise.resolve();
let autoRetry = true;

function applyRemote(raw: unknown, guardInflight: boolean) {
  const remote = normalize(raw);
  if (!remote) return;
  const local = app.settings;
  if (!local) {
    app.settings = remote;
    return;
  }
  const target = local as unknown as Record<string, unknown>;
  const source = remote as unknown as Record<string, unknown>;
  for (const key of Object.keys(source)) {
    if (Object.prototype.hasOwnProperty.call(pending, key)) continue;
    if (guardInflight && inflight.has(key)) continue;
    if (!same(target[key], source[key])) target[key] = source[key];
  }
}

function stage(patch: Patch) {
  const local = app.settings;
  if (!local) return false;
  const target = local as unknown as Record<string, unknown>;
  for (const [key, value] of Object.entries(patch)) {
    if (value === undefined) continue;
    target[key] = value;
    pending[key] = value;
  }
  return true;
}

function markSaved() {
  autoRetry = true;
  app.save = "saved";
  app.saveError = "";
  app.savePulse += 1;
  clearTimeout(savedTimer);
  savedTimer = setTimeout(() => {
    if (app.save === "saved") app.save = "idle";
  }, 2200);
}

function markFailed(e: unknown) {
  autoRetry = describeError(e).trim() === "settings_unreadable";
  clearTimeout(savedTimer);
  app.save = "error";
  app.saveError = errorText(e, describeError(e));
}

async function send() {
  const keys = Object.keys(pending);
  if (keys.length === 0) return;
  const patch = pending;
  pending = {};
  inflight = new Set(keys);
  try {
    const result = await api.updateSettings($state.snapshot(patch) as Patch);
    inflight = new Set();
    applyRemote(result, false);
    markSaved();
  } catch (e) {
    inflight = new Set();
    pending = { ...patch, ...pending };
    markFailed(e);
  }
}

export function flush(): Promise<void> {
  clearTimeout(saveTimer);
  saveTimer = undefined;
  chain = chain.then(send, send);
  return chain;
}

export function commit(patch: Patch): Promise<void> {
  if (!stage(patch)) return Promise.resolve();
  return flush();
}

export function schedule(patch: Patch, delay: number) {
  if (!stage(patch)) return;
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    void flush();
  }, delay);
}

export function hasPending(): boolean {
  return Object.keys(pending).length > 0;
}

export function refreshModels() {
  api
    .modelStatuses()
    .then((m) => {
      if (Array.isArray(m)) app.models = m;
    })
    .catch(() => {});
}

export function refreshLlama() {
  api
    .llamaStatus()
    .then((s) => {
      if (s && typeof s === "object") app.llama = s;
    })
    .catch(() => {});
}

export function refreshDevices() {
  api
    .listAudioDevices()
    .then((d) => {
      if (Array.isArray(d)) app.devices = d;
    })
    .catch(() => {});
}

export function refreshAutostart() {
  api
    .autostartStatus()
    .then((s) => {
      if (s && typeof s === "object") app.autostart = s;
    })
    .catch(() => {});
}

export function refreshStatus() {
  api
    .getStatus()
    .then((s) => {
      if (s && typeof s === "object") app.status = s;
    })
    .catch(() => {});
}

let updateSeq = 0;

function adoptUpdate(u: unknown) {
  if (!u || typeof u !== "object") return;
  updateSeq++;
  app.update = u as UpdateStatus;
}

export function refreshUpdate() {
  const seq = updateSeq;
  api
    .updateStatus()
    .then((u) => {
      if (seq === updateSeq) adoptUpdate(u);
    })
    .catch(() => {});
}

let groqSeq = 0;

function adoptGroq(g: unknown) {
  if (!g || typeof g !== "object" || !Array.isArray((g as GroqModels).models)) return;
  groqSeq++;
  app.groq = g as GroqModels;
}

function loadCachedGroqModels() {
  api
    .groqModels(false)
    .then((g) => {
      if (!app.groq && g && typeof g === "object" && Array.isArray(g.models)) app.groq = g;
    })
    .catch(() => {});
}

export function refreshGroqModels() {
  const seq = groqSeq;
  api
    .groqModels(true)
    .then((g) => {
      if (seq === groqSeq) adoptGroq(g);
    })
    .catch(() => {});
}

export async function checkUpdate(): Promise<void> {
  try {
    await api.checkUpdate();
  } catch {
    refreshUpdate();
  }
}

export async function installUpdate(): Promise<string | null> {
  await flush();
  if (hasPending()) return "settings_unsaved";
  try {
    await api.installUpdate();
    return null;
  } catch (e) {
    refreshUpdate();
    return describeError(e).trim() || "error";
  }
}

export function adoptSettings(raw: unknown) {
  applyRemote(raw, true);
}

export async function testLlm(): Promise<{ ok: boolean; detail: string }> {
  await flush();
  try {
    const out = await api.testLlm();
    return { ok: true, detail: typeof out === "string" ? out : "" };
  } catch (e) {
    return { ok: false, detail: describeError(e) };
  }
}

export function startLlamaSetup() {
  app.llamaRunning = true;
  app.llamaProgress = null;
  api.setupLlamaAuto().catch((e) => {
    app.llamaRunning = false;
    const detail = describeError(e);
    app.llamaProgress = {
      stage: "configure_start",
      pct: 0,
      overall_pct: 0,
      message: detail,
      done: false,
      error: detail,
    };
  });
}

export function markDownloadStart(id: string) {
  app.downloads = {
    ...app.downloads,
    [id]: { id, downloaded: 0, total: 0, pct: 0, done: false, error: null },
  };
}

export function markDownloadError(id: string, e: unknown) {
  app.downloads = {
    ...app.downloads,
    [id]: { id, downloaded: 0, total: 0, pct: 0, done: false, error: describeError(e) },
  };
}

export function isDownloading(id: string): boolean {
  const d = app.downloads[id];
  return !!d && !d.done && !d.error;
}

async function loadSettings(alive: () => boolean) {
  const startedAt = Date.now();
  for (let attempt = 0; alive(); attempt++) {
    try {
      const loaded = await api.getSettings();
      if (!alive()) return;
      if (normalize(loaded)) {
        if (!app.settings) applyRemote(loaded, true);
        app.loadSlow = false;
        return;
      }
    } catch {
      if (Date.now() - startedAt > 8000) app.loadSlow = true;
    }
    await new Promise((r) => setTimeout(r, Math.min(2000, 200 + attempt * 150)));
  }
}

export function start(): () => void {
  let disposed = false;
  const unlisten: UnlistenFn[] = [];
  const alive = () => !disposed;

  const onBlur = () => {
    void flush();
  };
  const onVisibility = () => {
    if (document.visibilityState === "hidden") void flush();
    else onFocus();
  };
  const onFocus = () => {
    refreshAutostart();
    refreshDevices();
    refreshStatus();
    refreshLlama();
    refreshUpdate();
  };
  window.addEventListener("blur", onBlur);
  window.addEventListener("pagehide", onBlur);
  window.addEventListener("focus", onFocus);
  document.addEventListener("visibilitychange", onVisibility);

  (async () => {
    const results = await Promise.allSettled([
      on<Settings>("settings-changed", (e) => {
        applyRemote(e.payload, true);
        if (app.save === "error" && hasPending() && autoRetry) {
          autoRetry = false;
          void flush();
        }
      }),
      on<string>("settings-navigate", (e) => {
        if (isSection(e.payload)) app.section = e.payload;
      }),
      on<StatusPayload>("status-changed", (e) => {
        if (e.payload && typeof e.payload === "object") app.status = e.payload;
      }),
      on<DownloadProgress>("model-download-progress", (e) => {
        const p = e.payload;
        if (!p || typeof p.id !== "string") return;
        app.downloads = { ...app.downloads, [p.id]: p };
        if (p.done || p.error) {
          refreshModels();
          refreshLlama();
        }
      }),
      on<LlamaSetupProgress>("llama-setup-progress", (e) => {
        const p = e.payload;
        if (!p || typeof p !== "object") return;
        app.llamaProgress = p;
        if (p.done || p.error) {
          app.llamaRunning = false;
          refreshModels();
          refreshLlama();
        } else {
          app.llamaRunning = true;
        }
      }),
      on("models-changed", () => {
        refreshModels();
        refreshLlama();
      }),
      on<UpdateStatus>("update-status", (e) => adoptUpdate(e.payload)),
      on<GroqModels>("groq-models", (e) => adoptGroq(e.payload)),
    ]);
    for (const r of results) {
      if (r.status !== "fulfilled") continue;
      if (disposed) {
        try {
          r.value();
        } catch {
          continue;
        }
      } else {
        unlisten.push(r.value);
      }
    }
    if (disposed) return;
    await loadSettings(alive);
    if (disposed) return;
    refreshDevices();
    refreshModels();
    refreshLlama();
    refreshAutostart();
    refreshStatus();
    refreshUpdate();
    loadCachedGroqModels();
    api
      .hardwareInfo()
      .then((h) => {
        if (h && typeof h === "object") app.hw = h;
      })
      .catch(() => {});
  })().catch(() => {});

  return () => {
    disposed = true;
    void flush();
    clearTimeout(savedTimer);
    window.removeEventListener("blur", onBlur);
    window.removeEventListener("pagehide", onBlur);
    window.removeEventListener("focus", onFocus);
    document.removeEventListener("visibilitychange", onVisibility);
    for (const u of unlisten) {
      try {
        u();
      } catch {
        continue;
      }
    }
  };
}
