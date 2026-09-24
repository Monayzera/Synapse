<script lang="ts">
  import { onMount } from "svelte";
  import { fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { api, on, getCurrentWindow, type UnlistenFn } from "../lib/ipc";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import type {
    Settings,
    ModelStatus,
    DownloadProgress,
    ModelKind,
    HfFile,
    LlamaSetupProgress,
    LlamaStatus,
    HardwareInfo,
    StatusPayload,
  } from "../lib/types";
  import Icon from "../lib/Icon.svelte";
  import { getTheme, setTheme, type ThemeMode } from "../lib/theme";

  type Tab = "general" | "audio" | "models" | "llm" | "dictionary";

  let settings = $state<Settings | null>(null);
  let loadSlow = $state(false);
  let loadStartedAt = 0;
  let devices = $state<string[]>([]);
  let models = $state<ModelStatus[]>([]);
  let downloads = $state<Record<string, DownloadProgress>>({});
  let tab = $state<Tab>("general");
  let saving = $state(false);
  let saved = $state(false);
  let savedTimer: ReturnType<typeof setTimeout> | undefined;
  let capturing = $state<null | "hotkey_ptt" | "hotkey_toggle">(null);
  let llmTest = $state<string>("");
  let dictKey = $state("");
  let dictVal = $state("");
  let fillerText = $state("");
  let vocabText = $state("");
  let themeMode = $state<ThemeMode>(getTheme());
  let hw = $state<HardwareInfo | null>(null);
  let runtimeStatus = $state<StatusPayload | null>(null);
  let showGroqKey = $state(false);
  let groqKeyCopied = $state(false);
  let groqCopyTimer: ReturnType<typeof setTimeout> | undefined;
  let showGroqLlmKey = $state(false);

  const GROQ_LLM_MODELS: {
    id: string;
    label: string;
    badges: string[];
    blurb: string;
    good: string;
    bad: string;
  }[] = [
    {
      id: "llama-3.1-8b-instant",
      label: "Llama 3.1 8B Instant",
      badges: ["Fast", "Light"],
      blurb: "Best default for everyday dictation. Near-instant grammar and punctuation cleanup.",
      good: "Lowest latency and the most generous daily limits (14.4K requests/day).",
      bad: "Less precise on complex translation or long, structured text.",
    },
    {
      id: "llama-3.3-70b-versatile",
      label: "Llama 3.3 70B Versatile",
      badges: ["Strong", "Accurate"],
      blurb: "The most capable general model. Best overall correction and translation quality.",
      good: "Excellent grammar, nuance and multilingual translation.",
      bad: "Slower and tighter limits (1K requests/day, 12K tokens/min).",
    },
    {
      id: "qwen/qwen3-32b",
      label: "Qwen 3 32B",
      badges: ["Reasoning", "Multilingual"],
      blurb: "Strong multilingual reasoning model. Great for translation into Asian languages.",
      good: "Excellent on Chinese, Japanese and structured reasoning.",
      bad: "Emits thinking tokens, so it is slower; 1K requests/day.",
    },
    {
      id: "openai/gpt-oss-120b",
      label: "GPT-OSS 120B",
      badges: ["Reasoning", "Heavy"],
      blurb: "Large reasoning model. Highest quality on hard, ambiguous text.",
      good: "Top accuracy on tricky grammar and ambiguous phrasing.",
      bad: "Slower (spends tokens thinking) and low daily limit (1K requests/day).",
    },
    {
      id: "openai/gpt-oss-20b",
      label: "GPT-OSS 20B",
      badges: ["Reasoning", "Balanced"],
      blurb: "Smaller reasoning model. Good quality with less latency than the 120B.",
      good: "Solid accuracy, lighter and faster than the 120B.",
      bad: "Still a reasoning model, slower than instant models; 1K requests/day.",
    },
    {
      id: "meta-llama/llama-4-scout-17b-16e-instruct",
      label: "Llama 4 Scout 17B",
      badges: ["Modern", "Balanced"],
      blurb: "Newer Llama 4 model. A solid all-rounder for correction and translation.",
      good: "Fresh architecture with good quality at moderate speed.",
      bad: "Lower token throughput (30K tokens/min); 1K requests/day.",
    },
    {
      id: "allam-2-7b",
      label: "Allam 2 7B",
      badges: ["Arabic", "Light"],
      blurb: "Specialized for Arabic. Best when you dictate in Arabic.",
      good: "Strong Arabic grammar and fluency, high daily limits.",
      bad: "Not ideal for other languages; pick a Llama model instead.",
    },
  ];

  const groqLlmInfo = $derived(
    GROQ_LLM_MODELS.find((m) => m.id === settings?.groq_llm_model) ?? GROQ_LLM_MODELS[0],
  );

  const noAccel = $derived(
    !hw?.build_gpu || (!!runtimeStatus?.engine_ready && !!runtimeStatus?.cpu_mode),
  );
  const weakHw = $derived(hw?.tier === "weak");
  const modestHw = $derived(hw?.tier === "modest");
  const lightWhisper = $derived(
    !!settings &&
      (settings.whisper_model.includes("q5") ||
        settings.whisper_model === "small" ||
        settings.whisper_model === "base"),
  );
  const heavyWhisper = $derived(
    !!settings &&
      settings.whisper_model.startsWith("large-v3") &&
      !settings.whisper_model.includes("turbo") &&
      !settings.whisper_model.includes("q5"),
  );
  const midWhisper = $derived(
    !!settings &&
      (settings.whisper_model === "large-v3-turbo" || settings.whisper_model === "medium"),
  );
  const warnWhisper = $derived(
    !lightWhisper &&
      ((heavyWhisper && (weakHw || noAccel)) || (midWhisper && weakHw && noAccel)),
  );
  const warnLlm = $derived(
    !!settings && settings.llm_enabled && (weakHw || (modestHw && noAccel)),
  );

  let modelTab = $state<ModelKind>("whisper");
  let hfUrl = $state("");
  let hfBusy = $state(false);
  let hfError = $state("");
  let pending = $state<null | { filename: string; url: string; size: number; kind: ModelKind }>(null);
  let repoFiles = $state<HfFile[]>([]);
  let llamaProgress = $state<LlamaSetupProgress | null>(null);
  let llamaRunning = $state(false);
  let llamaStatus = $state<LlamaStatus | null>(null);
  let deleteError = $state("");

  const llamaConfigured = $derived(
    !!llamaStatus && llamaStatus.binary && llamaStatus.model_present,
  );

  const visibleModels = $derived(models.filter((m) => m.info.kind === modelTab));
  const installedLlm = $derived(models.filter((m) => m.info.kind === "llm" && m.present));
  const customWhisper = $derived(
    models.filter((m) => m.info.kind === "whisper" && m.info.id.startsWith("custom-")),
  );

  const reduce =
    typeof matchMedia !== "undefined" && matchMedia("(prefers-reduced-motion: reduce)").matches;

  const isMac =
    typeof navigator !== "undefined" && /Mac/i.test(navigator.platform || navigator.userAgent);
  const capturePrompt = isMac
    ? "Press the keys…"
    : "Press keys or a mouse button…";

  const themes: { id: ThemeMode; label: string; icon: string }[] = [
    { id: "light", label: "Light", icon: "sun" },
    { id: "system", label: "System", icon: "monitor" },
    { id: "dark", label: "Dark", icon: "moon-stars" },
  ];

  function pickTheme(mode: ThemeMode) {
    themeMode = mode;
    setTheme(mode);
  }

  onMount(() => {
    let unlisten: UnlistenFn[] = [];
    reload();
    (async () => {
      try {
        unlisten.push(
          await on<DownloadProgress>("model-download-progress", (e) => {
            downloads = { ...downloads, [e.payload.id]: e.payload };
            if (e.payload.done) refreshModels();
          }),
        );
        unlisten.push(
          await on<LlamaSetupProgress>("llama-setup-progress", (e) => {
            llamaProgress = e.payload;
            if (e.payload.done || e.payload.error) {
              llamaRunning = false;
              refreshModels();
              reload();
            } else {
              llamaRunning = true;
            }
          }),
        );
        unlisten.push(await on("models-changed", () => refreshModels()));
      } catch (_) {}
    })();
    return () => {
      clearTimeout(savedTimer);
      clearTimeout(groqCopyTimer);
      unlisten.forEach((u) => u());
    };
  });

  async function reload() {
    loadSlow = false;
    if (!settings) loadStartedAt = Date.now();
    for (let attempt = 0; ; attempt++) {
      try {
        const loaded = await api.getSettings();
        settings = loaded;
        loadSlow = false;
        fillerText = loaded.filler_words.join("\n");
        vocabText = loaded.vocabulary.join("\n");
        break;
      } catch (_) {
        if (Date.now() - loadStartedAt > 8000) loadSlow = true;
        const delay = Math.min(2000, 200 + attempt * 150);
        await new Promise((r) => setTimeout(r, delay));
      }
    }
    api.listAudioDevices().then((d) => (devices = d)).catch(() => {});
    api.hardwareInfo().then((h) => (hw = h)).catch(() => {});
    api.getStatus().then((s) => (runtimeStatus = s)).catch(() => {});
    refreshModels();
    refreshLlamaStatus();
  }

  function refreshModels() {
    api.modelStatuses().then((m) => (models = m)).catch(() => {});
  }

  function refreshLlamaStatus() {
    api.llamaStatus().then((s) => (llamaStatus = s)).catch(() => {});
  }

  function reconfigureLlama() {
    if (
      confirm(
        "AI Correction is already set up and working. Re-run the full setup anyway? This re-downloads the server and model.",
      )
    ) {
      setupLlama();
    }
  }

  async function save() {
    if (!settings) return;
    saving = true;
    try {
      settings.filler_words = fillerText
        .split(/[\n,]/)
        .map((s) => s.trim())
        .filter(Boolean);
      settings.vocabulary = vocabText
        .split(/[\n,]/)
        .map((s) => s.trim())
        .filter(Boolean);
      await api.saveSettings($state.snapshot(settings) as Settings);
      saved = true;
      clearTimeout(savedTimer);
      savedTimer = setTimeout(() => (saved = false), 2400);
    } catch (e) {
      llmTest = "Failed to save: " + e;
    } finally {
      saving = false;
    }
  }

  function onKey(e: KeyboardEvent) {
    if (!capturing || !settings) return;
    e.preventDefault();
    if (e.code === "Escape") {
      capturing = null;
      return;
    }
    const accel = accelFromKeyboard(e);
    if (accel) {
      settings[capturing] = accel;
      capturing = null;
    }
  }

  function onMouse(e: MouseEvent) {
    if (!capturing || !settings) return;
    const token = isMac ? null : mouseToken(e.button);
    if (!token) {
      if (e.button === 0) {
        const target = e.target as HTMLElement | null;
        if (!target || !target.closest("button")) capturing = null;
      }
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    settings[capturing] = [...modsFrom(e), token].join("+");
    capturing = null;
  }

  function mouseToken(button: number): string | null {
    switch (button) {
      case 1:
        return "MouseMiddle";
      case 3:
        return "MouseBack";
      case 4:
        return "MouseForward";
      default:
        return null;
    }
  }

  function modsFrom(e: KeyboardEvent | MouseEvent): string[] {
    const mods: string[] = [];
    if (e.ctrlKey) mods.push("Ctrl");
    if (e.shiftKey) mods.push("Shift");
    if (e.altKey) mods.push("Alt");
    if (e.metaKey) mods.push("Super");
    return mods;
  }

  function accelFromKeyboard(e: KeyboardEvent): string | null {
    const key = normalizeKey(e.code);
    if (!key) return null;
    return [...modsFrom(e), key].join("+");
  }

  function normalizeKey(code: string): string | null {
    const modifiers = [
      "ControlLeft",
      "ControlRight",
      "ShiftLeft",
      "ShiftRight",
      "AltLeft",
      "AltRight",
      "MetaLeft",
      "MetaRight",
    ];
    if (modifiers.includes(code)) return null;
    if (code === "Space") return "Space";
    if (code.startsWith("Key")) return code.slice(3);
    if (code.startsWith("Digit")) return code.slice(5);
    if (/^F\d{1,2}$/.test(code)) return code;
    const map: Record<string, string> = {
      Backquote: "`",
      Minus: "-",
      Equal: "=",
      Comma: ",",
      Period: ".",
      Slash: "/",
      Semicolon: ";",
      Quote: "'",
      BracketLeft: "[",
      BracketRight: "]",
      Backslash: "\\",
      Enter: "Enter",
      Tab: "Tab",
      ArrowUp: "Up",
      ArrowDown: "Down",
      ArrowLeft: "Left",
      ArrowRight: "Right",
    };
    return map[code] ?? null;
  }

  function prettyHotkey(accel: string): string {
    if (!accel) return "None";
    return accel
      .split("+")
      .map((part) => {
        switch (part) {
          case "MouseMiddle":
            return "Mouse 3 (middle)";
          case "MouseBack":
            return "Mouse 4 (side)";
          case "MouseForward":
            return "Mouse 5 (side)";
          case "Super":
            return isMac ? "Cmd" : "Win";
          default:
            return part;
        }
      })
      .join(" + ");
  }

  function download(id: string) {
    api.downloadModel(id).catch(() => {});
  }

  async function detectHf() {
    const url = hfUrl.trim();
    hfError = "";
    pending = null;
    repoFiles = [];
    if (!url) return;
    hfBusy = true;
    try {
      const r = await api.hfDetect(url);
      if (r.kind === "invalid") {
        hfError = r.reason;
      } else if (r.kind === "file") {
        pending = { filename: r.filename, url: r.url, size: 0, kind: r.guessed ?? "llm" };
      } else {
        repoFiles = await api.hfListFiles(r.repo);
      }
    } catch (e) {
      hfError = String(e);
    } finally {
      hfBusy = false;
    }
  }

  function pickRepoFile(f: HfFile) {
    pending = { filename: f.filename, url: f.url, size: f.size_bytes, kind: f.guessed ?? "llm" };
    repoFiles = [];
  }

  async function confirmAdd() {
    if (!pending) return;
    hfBusy = true;
    hfError = "";
    try {
      const created = await api.addCustomModel(
        pending.filename,
        pending.kind,
        pending.filename,
        pending.url,
        pending.size,
        true,
      );
      modelTab = created.kind;
      pending = null;
      hfUrl = "";
      refreshModels();
    } catch (e) {
      hfError = String(e);
    } finally {
      hfBusy = false;
    }
  }

  function cancelAdd() {
    pending = null;
    repoFiles = [];
    hfError = "";
  }

  async function removeModel(m: ModelStatus) {
    deleteError = "";
    if (!confirm(`Delete "${m.info.label}"? The file will be removed from disk.`)) return;
    try {
      await api.deleteModel(m.info.id);
      refreshModels();
      reload();
    } catch (e) {
      deleteError = String(e);
    }
  }

  function setupLlama() {
    llamaRunning = true;
    llamaProgress = null;
    api.setupLlamaAuto().catch((e) => {
      llamaRunning = false;
      llamaProgress = {
        stage: "configure_start",
        pct: 0,
        overall_pct: 0,
        message: String(e),
        done: false,
        error: String(e),
      };
    });
  }

  function isBusy(id: string): boolean {
    const d = downloads[id];
    return !!d && !d.done && !d.error;
  }

  async function testLlm() {
    llmTest = "Testing…";
    try {
      const out = await api.testLlm();
      llmTest = "OK: " + out.slice(0, 80);
    } catch (e) {
      llmTest = "Error: " + e;
    }
  }

  function addDict() {
    if (!settings || !dictKey.trim()) return;
    settings.dictionary = { ...settings.dictionary, [dictKey.trim()]: dictVal };
    dictKey = "";
    dictVal = "";
  }

  function removeDict(key: string) {
    if (!settings) return;
    const next = { ...settings.dictionary };
    delete next[key];
    settings.dictionary = next;
  }

  function fmtBytes(n: number): string {
    if (n <= 0) return "0";
    const gb = n / 1_073_741_824;
    if (gb >= 1) return gb.toFixed(2) + " GB";
    return (n / 1_048_576).toFixed(0) + " MB";
  }

  function modelLabel(label: string): { name: string; tag: string | null } {
    const m = label.match(/^(.*?)\s*\(([^)]+)\)\s*$/);
    if (m) return { name: m[1].trim(), tag: m[2].trim() };
    return { name: label.trim(), tag: null };
  }

  function minimize() {
    getCurrentWindow().minimize().catch(() => {});
  }

  function openGroqKeys() {
    openUrl("https://console.groq.com/keys").catch(() => {});
  }

  function copyGroqKey() {
    const key = settings?.groq_api_key ?? "";
    if (!key) return;
    navigator.clipboard
      .writeText(key)
      .then(() => {
        groqKeyCopied = true;
        clearTimeout(groqCopyTimer);
        groqCopyTimer = setTimeout(() => (groqKeyCopied = false), 1500);
      })
      .catch(() => {});
  }

  const tabs: { id: Tab; label: string; icon: string }[] = [
    { id: "general", label: "General", icon: "sliders-horizontal" },
    { id: "audio", label: "Audio", icon: "waveform" },
    { id: "models", label: "Models", icon: "cube" },
    { id: "llm", label: "Translation", icon: "text-aa" },
    { id: "dictionary", label: "Dictionary", icon: "book-open-text" },
  ];

  const active = $derived(tabs.find((t) => t.id === tab) ?? tabs[0]);
</script>

<svelte:window onkeydown={onKey} onmousedown={onMouse} />

<div class="shell">
  <header class="titlebar" data-tauri-drag-region>
    <div class="brand">
      <span class="logo"></span>
      <h1>Synapse <span>· Settings</span></h1>
    </div>
    <div class="head-actions">
      {#if saved}
        <span class="saved-chip"><Icon name="check-circle" size={15} /> Saved</span>
      {/if}
      <button class="btn primary" onclick={save} disabled={saving}>
        {saving ? "Saving…" : "Save"}
      </button>
      <div class="winbtns">
        <button class="winbtn" title="Minimize" aria-label="Minimize" onclick={minimize}>
          <Icon name="minus" size={15} />
        </button>
        <button class="winbtn close" title="Close" aria-label="Close" onclick={() => api.hideWindow("settings")}>
          <Icon name="x" size={15} />
        </button>
      </div>
    </div>
  </header>

  {#if settings}
    <div class="body">
      <nav>
        {#each tabs as t}
          <button class="nav-item" class:active={tab === t.id} onclick={() => (tab = t.id)}>
            <Icon name={t.icon} size={18} />
            <span>{t.label}</span>
          </button>
        {/each}
      </nav>

      <section class="panel">
        {#key tab}
          <div class="panel-inner" in:fly={{ y: 6, duration: reduce ? 0 : 260, easing: cubicOut }}>
            <h2 class="panel-title display">{active.label}</h2>

            {#if tab === "general"}
              <div class="group">
                <span class="group-head">Shortcuts</span>
                <div class="field">
                  <label for="ptt">Push to talk</label>
                  <div class="hotkey">
                    <input id="ptt" readonly value={capturing === "hotkey_ptt" ? capturePrompt : prettyHotkey(settings.hotkey_ptt)} class:capturing={capturing === "hotkey_ptt"} />
                    <button class="btn" onclick={() => (capturing = capturing === "hotkey_ptt" ? null : "hotkey_ptt")}>
                      {capturing === "hotkey_ptt" ? "Cancel" : "Set"}
                    </button>
                  </div>
                </div>
                <div class="field">
                  <label for="tog">Toggle recording (on/off)</label>
                  <div class="hotkey">
                    <input id="tog" readonly value={capturing === "hotkey_toggle" ? capturePrompt : prettyHotkey(settings.hotkey_toggle)} class:capturing={capturing === "hotkey_toggle"} />
                    <button class="btn" onclick={() => (capturing = capturing === "hotkey_toggle" ? null : "hotkey_toggle")}>
                      {capturing === "hotkey_toggle" ? "Cancel" : "Set"}
                    </button>
                  </div>
                </div>
                <p class="hint">
                  {#if isMac}
                    Accepts combinations with Ctrl, Shift, Alt (Option) and Cmd, plus common keys.
                    Press Esc to cancel capture. On macOS, grant the app Accessibility permission
                    for the global shortcut to work.
                  {:else}
                    Accepts combinations with Ctrl, Shift, Alt and Win, common keys, and mouse buttons:
                    middle (Mouse 3) and side (Mouse 4 and 5). Press Esc to cancel capture.
                  {/if}
                </p>
                <div class="field">
                  <label for="mode">Recording mode</label>
                  <select id="mode" bind:value={settings.record_mode}>
                    <option value="push_to_talk">Push to talk</option>
                    <option value="toggle">Toggle (press to start and stop)</option>
                  </select>
                </div>
              </div>

              <div class="group">
                <span class="group-head">Language</span>
                <div class="field">
                  <label for="lang">Speech language</label>
                  <select id="lang" bind:value={settings.language}>
                    <option value="auto">Auto-detect</option>
                    <option value="pt">Portuguese</option>
                    <option value="en">English</option>
                    <option value="es">Spanish</option>
                    <option value="fr">French</option>
                    <option value="de">German</option>
                    <option value="it">Italian</option>
                    <option value="ja">Japanese</option>
                    <option value="zh">Chinese</option>
                    <option value="ru">Russian</option>
                    <option value="ko">Korean</option>
                  </select>
                </div>
              </div>

              <div class="group">
                <span class="group-head">Behavior</span>
                <label class="switch">
                  <input type="checkbox" bind:checked={settings.restore_clipboard} />
                  <span class="track"><span class="thumb"></span></span>
                  <span>Restore clipboard after pasting</span>
                </label>
                <label class="switch">
                  <input type="checkbox" bind:checked={settings.autostart} />
                  <span class="track"><span class="thumb"></span></span>
                  <span>{isMac ? "Start with macOS" : "Start with Windows"}</span>
                </label>
                <div class="field">
                  <label for="delay">Paste delay <em class="tnum">{settings.paste_delay_ms} ms</em></label>
                  <input id="delay" type="range" min="40" max="400" step="10" bind:value={settings.paste_delay_ms} />
                </div>
              </div>

              <div class="group">
                <span class="group-head">Appearance</span>
                <div class="field">
                  <span class="cap">Theme</span>
                  <div class="theme-seg" role="group" aria-label="Theme">
                    {#each themes as th}
                      <button class="seg" class:active={themeMode === th.id} onclick={() => pickTheme(th.id)}>
                        <Icon name={th.icon} size={15} />
                        <span>{th.label}</span>
                      </button>
                    {/each}
                  </div>
                </div>
              </div>
            {/if}

            {#if tab === "audio"}
              <div class="group">
                <span class="group-head">Audio input</span>
                <div class="field">
                  <label for="dev">Microphone</label>
                  <select id="dev" bind:value={settings.audio_device}>
                    <option value={null}>System default</option>
                    {#each devices as d}
                      <option value={d}>{d}</option>
                    {/each}
                  </select>
                </div>
              </div>

              <div class="group">
                <span class="group-head">Speech detection (VAD)</span>
                <label class="switch">
                  <input type="checkbox" bind:checked={settings.vad_enabled} />
                  <span class="track"><span class="thumb"></span></span>
                  <span>Trim silence automatically (Silero VAD)</span>
                </label>
                <div class="field">
                  <label for="thr">Sensitivity <em class="tnum">{settings.vad_threshold.toFixed(2)}</em></label>
                  <input id="thr" type="range" min="0.1" max="0.9" step="0.05" bind:value={settings.vad_threshold} />
                </div>
                <div class="field">
                  <label for="pad">Padding before and after speech <em class="tnum">{settings.speech_pad_ms} ms</em></label>
                  <input id="pad" type="range" min="0" max="400" step="10" bind:value={settings.speech_pad_ms} />
                </div>
                <div class="field">
                  <label for="sil">Minimum silence between segments <em class="tnum">{settings.min_silence_ms} ms</em></label>
                  <input id="sil" type="range" min="50" max="1000" step="50" bind:value={settings.min_silence_ms} />
                </div>
              </div>

              <div class="group">
                <span class="group-head">Text cleanup</span>
                <label class="switch">
                  <input type="checkbox" bind:checked={settings.filler_removal} />
                  <span class="track"><span class="thumb"></span></span>
                  <span>Remove speech fillers (uh, um, like…)</span>
                </label>
                <div class="field">
                  <label for="fill">Words to remove (one per line)</label>
                  <textarea id="fill" rows="5" bind:value={fillerText}></textarea>
                </div>
              </div>
            {/if}

            {#if tab === "models"}
              <p class="hint lead">Models are downloaded from Hugging Face and stored on your computer.
                Nothing is sent to the internet beyond the download itself. The models below are
                recommendations — you can add any model via a Hugging Face link.</p>

              <div class="theme-seg seg-tabs" role="group" aria-label="Model type">
                <button class="seg" class:active={modelTab === "whisper"} onclick={() => (modelTab = "whisper")}>
                  <Icon name="waveform" size={15} />
                  <span>Voice (Whisper)</span>
                </button>
                <button class="seg" class:active={modelTab === "llm"} onclick={() => { modelTab = "llm"; refreshLlamaStatus(); }}>
                  <Icon name="sparkle" size={15} />
                  <span>AI Correction</span>
                </button>
              </div>

              {#if modelTab === "whisper"}
                <div class="group">
                  <span class="group-head">Transcription engine</span>
                  <div class="field">
                    <label for="tbk" class="cap with-info">
                      How transcription runs
                      <span class="info-pop">
                        <Icon name="info" size={15} />
                        <span class="tip">
                          <strong>On this computer</strong> runs Whisper fully offline and private. Nothing leaves your machine.
                          <br /><br />
                          <strong>Groq Cloud</strong> uploads your audio to Groq's servers and transcribes it with the Whisper model you pick below (large-v3-turbo for speed or large-v3 for maximum accuracy). Fast and very accurate, but it needs an internet connection and is not offline. Create a free API key at console.groq.com/keys. The free tier is generous for personal dictation but has daily limits that reset every 24h; if you reach them, transcription returns an error until they reset. Check your live limits at console.groq.com/settings/limits.
                        </span>
                      </span>
                    </label>
                    <select id="tbk" bind:value={settings.transcription_backend}>
                      <option value="local">On this computer (offline, private)</option>
                      <option value="groq">Groq Cloud (free API, very fast)</option>
                    </select>
                  </div>
                  {#if settings.transcription_backend === "groq"}
                    <div class="field">
                      <label for="gk">Groq API key</label>
                      <div class="hf-add">
                        <input id="gk" type={showGroqKey ? "text" : "password"} placeholder="gsk_..." bind:value={settings.groq_api_key} />
                        <button class="btn" title={showGroqKey ? "Hide key" : "Show key"} aria-label="Toggle key visibility" onclick={() => (showGroqKey = !showGroqKey)}>
                          <Icon name={showGroqKey ? "eye-slash" : "eye"} size={15} />
                        </button>
                        <button class="btn" title="Copy key" aria-label="Copy key" onclick={copyGroqKey} disabled={!settings.groq_api_key}>
                          <Icon name={groqKeyCopied ? "check-circle" : "copy"} size={15} />
                        </button>
                        <button class="btn" onclick={openGroqKeys}>
                          <Icon name="arrow-right" size={15} />
                          Get key
                        </button>
                      </div>
                    </div>
                    <div class="field">
                      <label for="gm">Groq model</label>
                      <select id="gm" bind:value={settings.groq_model}>
                        <option value="whisper-large-v3-turbo">large-v3-turbo (faster)</option>
                        <option value="whisper-large-v3">large-v3 (maximum accuracy)</option>
                      </select>
                    </div>
                    {#if settings.groq_model === "whisper-large-v3-turbo" && settings.translation_enabled && settings.translation_target.trim().toLowerCase() === "english"}
                      <div class="cue warn">
                        <Icon name="warning-circle" size={16} />
                        <span>Translation to English uses large-v3 automatically — Groq's turbo model does not support translation.</span>
                      </div>
                    {/if}
                    <p class="hint">Get a free key at console.groq.com/keys. Your audio is sent to Groq's servers, so this mode is not offline. The local Whisper model below is not used while Groq is selected.</p>
                  {/if}
                </div>
                <div class="group">
                  <span class="group-head">Active model</span>
                  <div class="field">
                    <label for="wm">Whisper model</label>
                    <select id="wm" bind:value={settings.whisper_model}>
                      <option value="large-v3-turbo">large-v3-turbo (recommended)</option>
                      <option value="large-v3-turbo-q5">large-v3-turbo Q5 (light, best for CPU)</option>
                      <option value="large-v3">large-v3 (maximum accuracy)</option>
                      <option value="medium">medium</option>
                      <option value="medium-q5">medium Q5 (light)</option>
                      <option value="small">small (fast, good quality)</option>
                      <option value="base">base (very fast, light)</option>
                      {#each customWhisper as m}
                        <option value={m.info.id}>{m.info.label}</option>
                      {/each}
                    </select>
                  </div>
                  {#if warnWhisper}
                    <div class="cue warn">
                      <Icon name="warning-circle" size={16} />
                      <span>
                        {#if heavyWhisper}
                          This machine is weak for this model. Without a GPU, large-v3 (about 3 GB) can be
                          very slow or freeze on every dictation. Reason: transcription on CPU is heavy.
                          Suggestion: use large-v3-turbo Q5 (light), small or base.
                        {:else}
                          No GPU detected and low memory. Expect some delay on every dictation. Reason: the
                          model runs on the CPU. For a faster, lighter option with good quality, switch to
                          large-v3-turbo Q5, small or base.
                        {/if}
                      </span>
                    </div>
                  {/if}
                  <label class="switch">
                    <input type="checkbox" bind:checked={settings.prefer_gpu} />
                    <span class="track"><span class="thumb"></span></span>
                    <span>{isMac ? "Use GPU (Metal) when available" : "Use GPU (CUDA) when available"}</span>
                  </label>
                  {#if hw && !hw.build_gpu}
                    <p class="hint">This build was compiled without GPU support; this option has no effect.</p>
                  {/if}
                  <div class="row">
                    <button class="btn" onclick={() => api.reloadEngine()}>Reload voice model</button>
                  </div>
                </div>
              {/if}

              {#if modelTab === "llm"}
                <div class="group">
                  <span class="group-head">AI Correction</span>
                  <label class="switch">
                    <input type="checkbox" bind:checked={settings.llm_enabled} />
                    <span class="track"><span class="thumb"></span></span>
                    <span>Correct speech with AI (rewrites the text as you meant it)</span>
                  </label>
                  <label class="switch">
                    <input type="checkbox" bind:checked={settings.llm_format_paragraphs} />
                    <span class="track"><span class="thumb"></span></span>
                    <span>Format into paragraphs (organize long text with line breaks instead of one line)</span>
                  </label>
                  <p class="hint">When off, output stays on a single line. When on, the AI groups your speech into clear paragraphs and puts dictated lists on separate lines. Applies to correction and AI translation (English translation is done by Whisper and stays on one line).</p>
                  {#if warnLlm}
                    <div class="cue warn">
                      <Icon name="warning-circle" size={16} />
                      <span>
                        AI correction is heavy. On this machine it can make every dictation much slower or
                        freeze. Reason: the AI model runs on the CPU and uses a lot of memory. On weak
                        hardware we recommend keeping it off and enabling it only when needed.
                      </span>
                    </div>
                  {/if}
                  <div class="field">
                    <label for="bk">Backend</label>
                    <select id="bk" bind:value={settings.llm_backend}>
                      <option value="local">Local (llama-server)</option>
                      <option value="groq">Groq Cloud (free API, very fast)</option>
                      <option value="open_ai_compatible">OpenAI-compatible</option>
                      <option value="anthropic">Anthropic</option>
                      <option value="ollama">Ollama</option>
                    </select>
                  </div>
                  {#if settings.llm_backend === "groq"}
                    <div class="field">
                      <label for="glm">Groq model</label>
                      <select id="glm" bind:value={settings.groq_llm_model}>
                        {#each GROQ_LLM_MODELS as m}
                          <option value={m.id}>{m.label}</option>
                        {/each}
                      </select>
                    </div>
                    {#if groqLlmInfo}
                      <div class="modelcard">
                        <div class="mc-head">
                          <span class="mc-name">{groqLlmInfo.label}</span>
                          {#each groqLlmInfo.badges as b}
                            <span class="mc-badge">{b}</span>
                          {/each}
                        </div>
                        <p class="mc-blurb">{groqLlmInfo.blurb}</p>
                        <div class="mc-pro"><Icon name="check-circle" size={14} /><span>{groqLlmInfo.good}</span></div>
                        <div class="mc-con"><Icon name="warning-circle" size={14} /><span>{groqLlmInfo.bad}</span></div>
                      </div>
                    {/if}
                    <label class="switch">
                      <input type="checkbox" bind:checked={settings.groq_reuse_transcription_key} />
                      <span class="track"><span class="thumb"></span></span>
                      <span>Reuse the Groq API key from transcription</span>
                    </label>
                    {#if !settings.groq_reuse_transcription_key}
                      <div class="field">
                        <label for="glk">Groq API key</label>
                        <div class="hf-add">
                          <input id="glk" type={showGroqLlmKey ? "text" : "password"} placeholder="gsk_..." bind:value={settings.llm_api_key} />
                          <button class="btn" title={showGroqLlmKey ? "Hide key" : "Show key"} aria-label="Toggle key visibility" onclick={() => (showGroqLlmKey = !showGroqLlmKey)}>
                            <Icon name={showGroqLlmKey ? "eye-slash" : "eye"} size={15} />
                          </button>
                          <button class="btn" onclick={openGroqKeys}>
                            <Icon name="arrow-right" size={15} />
                            Get key
                          </button>
                        </div>
                      </div>
                      {#if !settings.llm_api_key}
                        <div class="cue warn">
                          <Icon name="warning-circle" size={16} />
                          <span>No Groq API key entered. AI Correction will not run until you paste a key here, or turn reuse back on.</span>
                        </div>
                      {/if}
                    {:else if !settings.groq_api_key}
                      <div class="cue warn">
                        <Icon name="warning-circle" size={16} />
                        <span>No Groq API key set. Add it in the transcription section, or turn this off to enter a separate key here.</span>
                      </div>
                    {/if}
                    <p class="hint">Used for both AI Correction and Translation. Get a free key at console.groq.com/keys. Your text is sent to Groq's servers, so this mode is not offline.</p>
                  {/if}
                </div>

                {#if settings.llm_backend !== "groq"}
                <div class="group">
                  <span class="group-head">Active model</span>
                  <div class="field">
                    <label for="lm">Local model</label>
                    <select id="lm" bind:value={settings.llm_local_model} disabled={settings.llm_backend !== "local" || installedLlm.length === 0}>
                      {#if installedLlm.length === 0}
                        <option value={settings.llm_local_model}>No model installed — download one below</option>
                      {/if}
                      {#each installedLlm as m}
                        <option value={m.info.filename}>{m.info.label}</option>
                      {/each}
                    </select>
                  </div>
                  <div class="row">
                    <button class="btn" onclick={() => api.restartLlm()} disabled={settings.llm_backend !== "local"}>Restart local server</button>
                  </div>
                  {#if settings.llm_backend !== "local"}
                    <p class="hint">Used when the AI Correction backend is set to Local.</p>
                  {:else if installedLlm.length === 0}
                    <p class="hint">No AI model installed yet. Download one from the list below to enable AI Correction.</p>
                  {/if}
                </div>
                {/if}

                <div class="group">
                  <span class="group-head">Connection</span>
                  {#if settings.llm_backend !== "groq"}
                    <div class="field">
                      <label for="ep">Endpoint</label>
                      <input id="ep" bind:value={settings.llm_endpoint} />
                    </div>
                    <div class="field">
                      <label for="mn">Model name</label>
                      <input id="mn" bind:value={settings.llm_model_name} />
                    </div>
                  {/if}
                  {#if settings.llm_backend !== "local" && settings.llm_backend !== "groq"}
                    <div class="field">
                      <label for="key">API key</label>
                      <input id="key" type="password" bind:value={settings.llm_api_key} />
                    </div>
                  {/if}
                  <div class="field">
                    <label for="to">Timeout <em class="tnum">{settings.llm_timeout_ms} ms</em></label>
                    <input id="to" type="range" min="500" max="20000" step="100" bind:value={settings.llm_timeout_ms} />
                  </div>
                  <div class="field">
                    <label for="tmp">Temperature <em class="tnum">{settings.llm_temperature.toFixed(2)}</em></label>
                    <input id="tmp" type="range" min="0" max="1" step="0.05" bind:value={settings.llm_temperature} />
                  </div>
                  <div class="row">
                    <button class="btn" onclick={testLlm}>Test connection</button>
                    {#if llmTest}<span class="hint">{llmTest}</span>{/if}
                  </div>
                </div>

                {#if settings.llm_backend !== "groq"}
                <div class="group">
                  <span class="group-head">Automatic setup</span>
                  <p class="hint">Downloads the llama server and the recommended model, sets the best
                    correction prompt, and enables everything automatically.</p>
                  {#if llamaRunning && llamaProgress && !llamaProgress.done && !llamaProgress.error}
                    <div class="bar"><span style={`width:${llamaProgress.overall_pct}%`}></span></div>
                    <div class="pct tnum">{llamaProgress.message} · {llamaProgress.overall_pct.toFixed(0)}%</div>
                  {:else if llamaConfigured}
                    <div class="cue ok">
                      <Icon name="check-circle" size={16} />
                      <span>You're all set. AI Correction is already installed{llamaStatus?.ready ? " and running" : ""} — you don't need to run this again.</span>
                    </div>
                    <div class="row">
                      <button class="btn ghost" onclick={reconfigureLlama} disabled={llamaRunning}>Reconfigure anyway</button>
                    </div>
                    {#if llamaProgress?.error}<div class="err-text">{llamaProgress.message}</div>{/if}
                  {:else}
                    <div class="row">
                      <button class="btn primary" onclick={setupLlama} disabled={llamaRunning}>
                        <Icon name="sparkle" size={15} />
                        Download and set up automatically
                      </button>
                      {#if llamaProgress?.done && !llamaProgress?.error}
                        <span class="ok"><Icon name="check-circle" size={14} /> {llamaProgress.message}</span>
                      {/if}
                    </div>
                    {#if llamaProgress?.error}<div class="err-text">{llamaProgress.message}</div>{/if}
                  {/if}
                </div>
                {/if}
              {/if}

              <div class="group">
                {#if visibleModels.length === 0}
                  <div class="empty">
                    <Icon name="cube" size={26} />
                    <p>No models of this type.</p>
                  </div>
                {/if}
                {#each visibleModels as m, i}
                  {@const info = modelLabel(m.info.label)}
                  <div class="model" class:divided={i > 0}>
                    <div class="model-info">
                      <div class="model-head">
                        <span class="model-name">{info.name}</span>
                        {#if info.tag}
                          <span class="model-tag" class:rec={/recommend/i.test(info.tag)}>{info.tag}</span>
                        {/if}
                        {#if m.present}
                          <span class="model-tag inst"><Icon name="check-circle" size={12} /> installed</span>
                        {/if}
                      </div>
                      <div class="model-meta tnum">{m.info.filename} · {fmtBytes(m.info.size_bytes)}</div>
                      {#if isBusy(m.info.id)}
                        <div class="bar"><span style={`width:${downloads[m.info.id].pct}%`}></span></div>
                        <div class="pct tnum">{downloads[m.info.id].pct.toFixed(1)}%</div>
                      {/if}
                      {#if downloads[m.info.id]?.error}
                        <div class="err-text">{downloads[m.info.id].error}</div>
                      {/if}
                    </div>
                    <div class="model-actions">
                      <button class="btn" onclick={() => download(m.info.id)} disabled={isBusy(m.info.id)}>
                        {#if isBusy(m.info.id)}
                          Downloading…
                        {:else}
                          <Icon name="download-simple" size={15} />
                          {m.present ? "Re-download" : "Download"}
                        {/if}
                      </button>
                      {#if m.present || m.info.id.startsWith("custom-")}
                        <button class="btn ghost danger" onclick={() => removeModel(m)} disabled={isBusy(m.info.id)} aria-label="Delete model">
                          <Icon name="trash" size={15} />
                        </button>
                      {/if}
                    </div>
                  </div>
                {/each}
              </div>

              <div class="group">
                <span class="group-head">Add from Hugging Face</span>
                <p class="hint">Paste a link to a file (.gguf / .bin) or a repository. The type is
                  auto-detected — confirm before downloading.</p>
                <div class="hf-add">
                  <input placeholder="https://huggingface.co/..." bind:value={hfUrl} />
                  <button class="btn" onclick={detectHf} disabled={hfBusy || !hfUrl.trim()}>
                    {hfBusy ? "…" : "Detect"}
                  </button>
                </div>
                {#if hfError}<div class="err-text">{hfError}</div>{/if}

                {#if repoFiles.length > 0}
                  <div class="repo-list">
                    {#each repoFiles as f}
                      <button class="repo-file" onclick={() => pickRepoFile(f)}>
                        <span class="rf-name">{f.filename}</span>
                        <span class="rf-meta tnum">
                          {f.guessed === "whisper" ? "Voice" : "AI"}{f.size_bytes > 0 ? " · " + fmtBytes(f.size_bytes) : ""}
                        </span>
                      </button>
                    {/each}
                  </div>
                {/if}

                {#if pending}
                  <div class="pending">
                    <div class="pending-file tnum">{pending.filename}</div>
                    <div class="field">
                      <span class="cap">Model type</span>
                      <div class="theme-seg" role="group" aria-label="Model type">
                        <button class="seg" class:active={pending.kind === "whisper"} onclick={() => pending && (pending.kind = "whisper")}>
                          <Icon name="waveform" size={15} /><span>Voice</span>
                        </button>
                        <button class="seg" class:active={pending.kind === "llm"} onclick={() => pending && (pending.kind = "llm")}>
                          <Icon name="sparkle" size={15} /><span>AI Correction</span>
                        </button>
                      </div>
                    </div>
                    <div class="row">
                      <button class="btn primary" onclick={confirmAdd} disabled={hfBusy}>
                        <Icon name="download-simple" size={15} /> Add and download
                      </button>
                      <button class="btn ghost" onclick={cancelAdd} disabled={hfBusy}>Cancel</button>
                    </div>
                  </div>
                {/if}
              </div>

              {#if deleteError}<div class="err-text">{deleteError}</div>{/if}
            {/if}

            {#if tab === "llm"}
              <div class="group">
                <span class="group-head">Automatic translation</span>
                <label class="switch">
                  <input type="checkbox" bind:checked={settings.translation_enabled} />
                  <span class="track"><span class="thumb"></span></span>
                  <span>Translate speech into another language</span>
                </label>
                <p class="hint">You speak in one language and get the text already translated and
                  corrected in the output language, with perfect grammar. Set up the AI model in the Models tab (AI Correction).</p>
                {#if settings.translation_enabled}
                  <div class="field">
                    <label for="lin">Input language (what you speak)</label>
                    <select id="lin" bind:value={settings.language}>
                      <option value="auto">Auto-detect</option>
                      <option value="pt">Portuguese</option>
                      <option value="en">English</option>
                      <option value="es">Spanish</option>
                      <option value="fr">French</option>
                      <option value="de">German</option>
                      <option value="it">Italian</option>
                      <option value="ja">Japanese</option>
                      <option value="zh">Chinese</option>
                      <option value="ru">Russian</option>
                      <option value="ko">Korean</option>
                    </select>
                  </div>
                  <div class="field">
                    <label for="lout">Output language (translation)</label>
                    <select id="lout" bind:value={settings.translation_target}>
                      <option value="English">English</option>
                      <option value="Spanish">Spanish</option>
                      <option value="Brazilian Portuguese">Portuguese (BR)</option>
                      <option value="French">French</option>
                      <option value="German">German</option>
                      <option value="Italian">Italian</option>
                      <option value="Japanese">Japanese</option>
                      <option value="Simplified Chinese">Chinese (Simplified)</option>
                      <option value="Russian">Russian</option>
                      <option value="Korean">Korean</option>
                    </select>
                  </div>
                  {#if settings.translation_target.trim().toLowerCase() === "english"}
                    <p class="hint">English is the fastest: translation is done by Whisper itself, without using the AI.</p>
                  {:else}
                    <p class="hint">Targets other than English are translated by the AI Correction backend you pick in the Models tab.</p>
                    {#if settings.llm_backend === "local"}
                      <div class="cue warn">
                        <Icon name="warning-circle" size={16} />
                        <span>Translation to {settings.translation_target} needs the AI, but AI Correction is set to <strong>Local</strong>. If no local model is running it will not translate and you only get the raw transcription. For instant cloud translation, open the <strong>Models</strong> tab and set <strong>AI Correction → Backend = Groq</strong>.</span>
                      </div>
                    {/if}
                  {/if}
                {/if}
              </div>
            {/if}

            {#if tab === "dictionary"}
              <p class="hint lead">Exact replacements applied to the recognized text, ideal for proper
                nouns, technical terms and acronyms.</p>
              <div class="dict-add">
                <input placeholder="as spoken" bind:value={dictKey} />
                <span class="arrow"><Icon name="arrow-right" size={16} /></span>
                <input placeholder="exact replacement" bind:value={dictVal} />
                <button class="btn primary" onclick={addDict}>Add</button>
              </div>
              <div class="group">
                {#each Object.entries(settings.dictionary) as [k, v], i}
                  <div class="dict-row" class:divided={i > 0}>
                    <span class="k">{k}</span>
                    <span class="arrow"><Icon name="arrow-right" size={15} /></span>
                    <span class="v">{v}</span>
                    <button class="btn ghost danger" onclick={() => removeDict(k)}>Remove</button>
                  </div>
                {/each}
                {#if Object.keys(settings.dictionary).length === 0}
                  <div class="empty">
                    <Icon name="book-open-text" size={26} />
                    <p>No entries yet.</p>
                  </div>
                {/if}
              </div>

              <div class="group">
                <span class="group-head">AI vocabulary</span>
                <p class="hint">Words or names you use that Whisper often gets wrong (e.g. Claude,
                  Anthropic, Tauri). They guide recognition to spell them correctly, even with AI
                  correction off. One per line.</p>
                <div class="field">
                  <label for="vocab">Vocabulary terms</label>
                  <textarea id="vocab" rows="5" bind:value={vocabText} placeholder="Claude&#10;Anthropic&#10;Tauri"></textarea>
                </div>
              </div>
            {/if}
          </div>
        {/key}
      </section>
    </div>
  {:else}
    <div class="loading">
      {#if loadSlow}
        <Icon name="warning-circle" size={24} />
        <p class="load-msg">Still starting up…</p>
        <p class="load-sub">This can take a little longer right after a Windows restart. It will load on its own.</p>
      {:else}
        <p class="load-msg">Loading…</p>
      {/if}
    </div>
  {/if}
</div>

<style>
  .shell {
    position: relative;
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--paper);
    border: 1px solid var(--paper-edge);
    border-radius: 12px;
    overflow: hidden;
  }

  .head-actions {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .saved-chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--sage-text);
    font-size: 13px;
    font-weight: 600;
    animation: chip-in 0.24s ease;
  }

  @keyframes chip-in {
    from {
      opacity: 0;
      transform: translateX(6px);
    }
    to {
      opacity: 1;
      transform: translateX(0);
    }
  }

  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }

  nav {
    width: 218px;
    flex: 0 0 auto;
    padding: 18px 14px;
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .nav-item {
    display: flex;
    align-items: center;
    gap: 11px;
    width: 100%;
    text-align: left;
    padding: 10px 13px;
    border-radius: var(--radius-sm);
    font-family: var(--font-body);
    font-weight: 500;
    font-size: 14.5px;
    color: var(--ink-soft);
    transition: background 0.16s ease, color 0.16s ease;
  }

  .nav-item :global(.ph) {
    color: var(--ink-faint);
    transition: color 0.16s ease;
  }

  .nav-item:hover {
    background: var(--paper-sunk);
    color: var(--ink);
  }

  .nav-item.active {
    background: var(--terra-soft);
    color: var(--terra-text);
    font-weight: 600;
  }

  .nav-item.active :global(.ph) {
    color: var(--terra);
  }

  .nav-item:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px var(--terra-soft);
  }

  .panel {
    flex: 1;
    overflow-y: auto;
    padding: 28px 34px 36px;
  }

  .panel-inner {
    max-width: 600px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .panel-title {
    margin: 0 0 6px;
    font-size: var(--t-display);
  }

  .group {
    padding: 18px 20px;
    background: var(--paper-raised);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    display: flex;
    flex-direction: column;
    gap: 15px;
  }

  .group-head {
    font-family: var(--font-body);
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--ink-faint);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .field label,
  .field .cap {
    font-family: var(--font-body);
    font-size: 13.5px;
    font-weight: 600;
    color: var(--ink-soft);
    display: flex;
    align-items: baseline;
    gap: 10px;
  }

  .field label em {
    font-style: normal;
    color: var(--terra-text);
    font-weight: 600;
    margin-left: auto;
  }

  .with-info {
    align-items: center;
  }

  .info-pop {
    position: relative;
    display: inline-flex;
    align-items: center;
    color: var(--ink-faint);
    cursor: help;
  }

  .info-pop:hover {
    color: var(--terra);
  }

  .info-pop .tip {
    position: absolute;
    left: 0;
    top: calc(100% + 8px);
    z-index: 20;
    width: 320px;
    padding: 12px 14px;
    border-radius: var(--radius-sm);
    background: var(--paper-raised);
    border: 1px solid var(--line-strong);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.16);
    color: var(--ink-soft);
    font-size: 12.5px;
    font-weight: 400;
    line-height: 1.5;
    opacity: 0;
    visibility: hidden;
    transform: translateY(-4px);
    transition: opacity 0.16s ease, transform 0.16s ease, visibility 0.16s;
    pointer-events: none;
  }

  .info-pop:hover .tip {
    opacity: 1;
    visibility: visible;
    transform: translateY(0);
  }

  .info-pop .tip strong {
    color: var(--ink);
    font-weight: 600;
  }

  .field select,
  .field > input,
  .field textarea {
    max-width: 440px;
  }

  .hotkey {
    display: flex;
    gap: 8px;
    max-width: 440px;
  }

  .hotkey input {
    flex: 1;
    font-feature-settings: "tnum" 1;
  }

  .hotkey input.capturing {
    border-color: var(--terra);
    box-shadow: 0 0 0 3px var(--terra-soft);
    animation: capture-glow 1.3s ease-in-out infinite;
  }

  @keyframes capture-glow {
    0%,
    100% {
      box-shadow: 0 0 0 3px var(--terra-soft);
    }
    50% {
      box-shadow: 0 0 0 5px var(--terra-glow);
    }
  }

  .switch {
    display: flex;
    align-items: center;
    gap: 13px;
    font-size: 14.5px;
    cursor: pointer;
    color: var(--ink);
  }

  .switch input {
    position: absolute;
    opacity: 0;
    width: 0;
    height: 0;
  }

  .switch .track {
    width: 40px;
    height: 23px;
    flex: 0 0 auto;
    border-radius: 999px;
    background: var(--paper-sunk);
    border: 1px solid var(--line-strong);
    padding: 2px;
    display: flex;
    align-items: center;
    transition: background 0.2s ease, border-color 0.2s ease;
  }

  .switch .thumb {
    width: 17px;
    height: 17px;
    border-radius: 50%;
    background: #faf9f5;
    box-shadow: var(--shadow-sm);
    transform: translateX(0);
    transition: transform 0.2s cubic-bezier(0.3, 0.7, 0.4, 1);
  }

  .switch input:checked + .track {
    background: var(--terra);
    border-color: var(--terra);
  }

  .switch input:checked + .track .thumb {
    transform: translateX(17px);
  }

  .switch input:focus-visible + .track {
    box-shadow: 0 0 0 3px var(--terra-soft);
  }

  .theme-seg {
    display: inline-flex;
    gap: 3px;
    background: var(--paper-sunk);
    border: 1px solid var(--line);
    border-radius: 10px;
    padding: 3px;
  }

  .seg-tabs {
    align-self: flex-start;
  }

  .seg {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 7px 14px;
    border-radius: 7px;
    font-family: var(--font-body);
    font-size: 13px;
    font-weight: 600;
    color: var(--ink-faint);
    transition: background 0.16s ease, color 0.16s ease, box-shadow 0.16s ease;
  }

  .seg:hover {
    color: var(--ink);
  }

  .seg.active {
    background: var(--field);
    color: var(--terra-text);
    box-shadow: var(--shadow-sm);
  }

  .seg.active :global(.ph) {
    color: var(--terra);
  }

  .seg:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px var(--terra-soft);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }

  .row .hint {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .hint {
    font-size: 13.5px;
    line-height: 1.6;
    color: var(--ink-faint);
    margin: 0;
  }

  .hint.lead {
    max-width: 520px;
    margin-bottom: 4px;
    color: var(--ink-soft);
  }

  .model {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 14px;
    padding: 2px 0;
  }

  .model.divided {
    border-top: 1px solid var(--line);
    padding-top: 13px;
    margin-top: 1px;
  }

  .model-info {
    min-width: 0;
    flex: 1;
  }

  .model-head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    min-width: 0;
  }

  .model-name {
    font-size: 14px;
    font-weight: 600;
    color: var(--ink);
    line-height: 1.3;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .model-tag {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    flex: 0 0 auto;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
    line-height: 1.4;
    white-space: nowrap;
    color: var(--ink-faint);
    background: var(--paper-sunk);
    border: 1px solid var(--line);
  }

  .model-tag.rec,
  .model-tag.inst {
    color: var(--sage-text);
    background: var(--sage-soft);
    border-color: transparent;
  }

  .model-meta {
    font-size: 11.5px;
    color: var(--ink-faint);
    margin-top: 3px;
  }

  .ok {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--sage-text);
    font-weight: 600;
  }

  .bar {
    margin-top: 11px;
    height: 5px;
    max-width: 360px;
    background: var(--paper-sunk);
    border-radius: 5px;
    overflow: hidden;
  }

  .bar span {
    display: block;
    height: 100%;
    border-radius: 5px;
    background: var(--terra);
    transition: width 0.3s ease;
  }

  .pct {
    margin-top: 6px;
    font-size: 12px;
    color: var(--ink-faint);
  }

  .err-text {
    color: var(--danger);
    font-size: 13px;
    margin-top: 7px;
  }

  .cue {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 11px 13px;
    border-radius: var(--radius-sm);
    background: var(--paper-sunk);
    border: 1px solid var(--line);
    font-size: 13.5px;
    line-height: 1.45;
    color: var(--ink-soft);
  }

  .cue.ok {
    color: var(--sage-text);
  }

  .cue.warn {
    color: var(--terra-text);
    border-color: var(--terra-line);
    background: var(--terra-soft);
  }

  .cue :global(.ph) {
    flex: 0 0 auto;
  }

  .modelcard {
    display: flex;
    flex-direction: column;
    gap: 7px;
    padding: 13px 14px;
    border-radius: var(--radius-sm);
    background: var(--paper-sunk);
    border: 1px solid var(--line);
  }

  .mc-head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 7px;
  }

  .mc-name {
    font-size: 14px;
    font-weight: 600;
    color: var(--ink);
  }

  .mc-badge {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    padding: 2px 8px;
    border-radius: 999px;
    color: var(--ink-soft);
    background: var(--paper);
    border: 1px solid var(--line);
  }

  .mc-blurb {
    margin: 0;
    font-size: 13px;
    line-height: 1.5;
    color: var(--ink-soft);
  }

  .mc-pro,
  .mc-con {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    font-size: 12.5px;
    line-height: 1.45;
  }

  .mc-pro {
    color: var(--sage-text);
  }

  .mc-con {
    color: var(--terra-text);
  }

  .mc-pro :global(.ph),
  .mc-con :global(.ph) {
    flex: 0 0 auto;
    margin-top: 1px;
  }

  .model-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 0 0 auto;
  }

  .hf-add {
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: 520px;
  }

  .hf-add input {
    flex: 1;
    min-width: 0;
  }

  .repo-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 220px;
    overflow-y: auto;
    border: 1px solid var(--line);
    border-radius: var(--radius-sm);
    padding: 6px;
  }

  .repo-file {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 9px 12px;
    border-radius: var(--radius-sm);
    text-align: left;
    color: var(--ink);
    transition: background 0.14s ease;
  }

  .repo-file:hover {
    background: var(--paper-sunk);
  }

  .rf-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
    font-size: 13.5px;
  }

  .rf-meta {
    flex: 0 0 auto;
    color: var(--ink-faint);
    font-size: 12px;
  }

  .pending {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px;
    border: 1px solid var(--terra-line);
    border-radius: var(--radius-sm);
    background: var(--terra-soft);
  }

  .pending-file {
    font-weight: 600;
    font-size: 13.5px;
    color: var(--ink);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dict-add {
    display: flex;
    align-items: center;
    gap: 10px;
    max-width: 600px;
  }

  .dict-add input {
    flex: 1;
    min-width: 0;
  }

  .arrow {
    color: var(--ink-faint);
    flex: 0 0 auto;
    display: inline-flex;
  }

  .dict-row {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 4px 0;
  }

  .dict-row.divided {
    border-top: 1px solid var(--line);
    padding-top: 12px;
    margin-top: 1px;
  }

  .dict-row .k {
    font-weight: 600;
    color: var(--ink);
  }

  .dict-row .v {
    flex: 1;
    color: var(--ink-soft);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    padding: 26px;
    color: var(--ink-ghost);
  }

  .empty p {
    margin: 0;
    font-size: 14px;
    color: var(--ink-faint);
  }

  .loading {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    flex: 1;
    padding: 24px;
    text-align: center;
    color: var(--ink-faint);
    font-size: 16px;
  }

  .load-msg {
    margin: 0;
    color: var(--ink-soft);
    font-weight: 600;
  }

  .load-sub {
    margin: 0;
    max-width: 340px;
    font-size: 13px;
    line-height: 1.5;
    color: var(--ink-faint);
  }

  @media (max-width: 680px) {
    nav {
      width: 188px;
    }
    .panel {
      padding: 22px 22px 30px;
    }
  }
</style>
