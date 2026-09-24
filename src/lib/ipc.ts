import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn, type EventCallback } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type {
  Settings,
  StatusPayload,
  HistoryEntry,
  Stats,
  ModelStatus,
  ModelInfo,
  ModelKind,
  HfParse,
  HfFile,
  LlamaStatus,
  HardwareInfo,
  AutostartStatus,
  PrivacyKind,
  SectionId,
  UpdateNotice,
  UpdateStatus,
} from "./types";

export const api = {
  getStatus: () => invoke<StatusPayload>("get_status"),
  getSettings: () => invoke<Settings>("get_settings"),
  updateSettings: (patch: Partial<Settings>) => invoke<Settings>("update_settings", { patch }),
  listAudioDevices: () => invoke<string[]>("list_audio_devices"),
  toggleRecording: () => invoke<void>("toggle_recording"),
  cancelRecording: () => invoke<void>("cancel_recording"),
  getHistory: (limit: number, offset: number) =>
    invoke<HistoryEntry[]>("get_history", { limit, offset }),
  searchHistory: (query: string, limit: number) =>
    invoke<HistoryEntry[]>("search_history", { query, limit }),
  deleteHistory: (id: number) => invoke<void>("delete_history", { id }),
  clearHistory: () => invoke<void>("clear_history"),
  getStats: () => invoke<Stats>("get_stats"),
  recopy: (id: number) => invoke<void>("recopy", { id }),
  addToDictionary: (phrase: string, replacement: string) =>
    invoke<void>("add_to_dictionary", { phrase, replacement }),
  modelStatuses: () => invoke<ModelStatus[]>("model_statuses"),
  downloadModel: (id: string, activate: boolean) =>
    invoke<void>("download_model", { id, activate }),
  activateModel: (id: string) => invoke<Settings>("activate_model", { id }),
  hfDetect: (url: string) => invoke<HfParse>("hf_detect", { url }),
  hfListFiles: (repo: string) => invoke<HfFile[]>("hf_list_files", { repo }),
  addCustomModel: (
    label: string,
    kind: ModelKind,
    filename: string,
    url: string,
    sizeBytes: number,
    downloadNow: boolean,
  ) =>
    invoke<ModelInfo>("add_custom_model", {
      label,
      kind,
      filename,
      url,
      sizeBytes,
      downloadNow,
    }),
  deleteModel: (id: string) => invoke<void>("delete_model", { id }),
  setupLlamaAuto: () => invoke<void>("setup_llama_auto"),
  llamaStatus: () => invoke<LlamaStatus>("llama_status"),
  hardwareInfo: () => invoke<HardwareInfo>("hardware_info"),
  reloadEngine: () => invoke<void>("reload_engine"),
  restartLlm: () => invoke<void>("restart_llm"),
  testLlm: () => invoke<string>("test_llm"),
  autostartStatus: () => invoke<AutostartStatus>("autostart_status"),
  setAutostart: (enabled: boolean) => invoke<AutostartStatus>("set_autostart", { enabled }),
  openPrivacySettings: (kind: PrivacyKind) => invoke<void>("open_privacy_settings", { kind }),
  updateStatus: () => invoke<UpdateStatus>("update_status"),
  checkUpdate: () => invoke<void>("check_update"),
  installUpdate: () => invoke<void>("install_update"),
  takeUpdateNotice: () => invoke<UpdateNotice | null>("take_update_notice"),
  openSettings: (section: SectionId | null) =>
    invoke<void>("open_settings", { section }),
  openWindow: (label: string) => invoke<void>("open_window", { label }),
  hideWindow: (label: string) => invoke<void>("hide_window", { label }),
};

export function on<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
  return listen<T>(event, handler);
}

export { getCurrentWindow };
export type { UnlistenFn };
