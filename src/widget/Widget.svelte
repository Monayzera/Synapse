<script lang="ts">
  import { onMount, tick } from "svelte";
  import { api, on, getCurrentWindow, type UnlistenFn } from "../lib/ipc";
  import { currentMonitor } from "@tauri-apps/api/window";
  import { LogicalPosition, LogicalSize } from "@tauri-apps/api/dpi";
  import type {
    StatusPayload,
    CompletePayload,
    PipelineErrorPayload,
    PrivacyKind,
    SectionId,
    Settings,
  } from "../lib/types";
  import Icon from "../lib/Icon.svelte";
  import { t, tOr, setLanguage } from "../lib/i18n.svelte";

  const BAR_COUNT = 18;
  const PILL_HALF = 75;
  const DOCK_NEED = 74;
  const PILL_MIN = 150;
  const PILL_MAX = 440;
  const WINDOW_EXTRA = 170;
  const WINDOW_HEIGHT = 40;

  const INITIAL_STATUS: StatusPayload = {
    status: "idle",
    recording: false,
    cpu_mode: false,
    engine_ready: false,
    model: "",
    audio_available: true,
    vad_active: false,
    error: null,
    starting: true,
    mic_denied: false,
    accessibility_needed: false,
    hotkey_ready: true,
    llm: "off",
    error_code: null,
    autostart_launch: false,
  };

  let status = $state<StatusPayload>({ ...INITIAL_STATUS });

  let levels = $state<number[]>(Array(BAR_COUNT).fill(0));
  let flash = $state<{ text: string; kind: "ok" | "err" } | null>(null);
  let flashTimer: ReturnType<typeof setTimeout> | undefined;
  let dockSide = $state<"left" | "right">("right");
  let hovered = $state(false);
  let elapsed = $state(0);
  let procStart = 0;
  let procTimer: ReturnType<typeof setInterval> | undefined;
  let cancelling = $state(false);
  let pillW = $state(PILL_MIN);
  let measureEl: HTMLElement | undefined = $state();
  let resizeSeq = 0;
  let shrinkTimer: ReturnType<typeof setTimeout> | undefined;

  const mode = $derived(status.status);

  $effect(() => {
    if (mode === "processing") {
      if (!procTimer) {
        procStart = Date.now();
        elapsed = 0;
        procTimer = setInterval(() => {
          elapsed = Math.floor((Date.now() - procStart) / 1000);
        }, 250);
      }
    } else if (procTimer) {
      clearInterval(procTimer);
      procTimer = undefined;
      elapsed = 0;
      cancelling = false;
    }
    return () => {
      if (procTimer) {
        clearInterval(procTimer);
        procTimer = undefined;
      }
    };
  });

  const procLabel = $derived.by(() => {
    if (cancelling) return t("w.cancelling");
    if (elapsed >= 20) return t("w.takingLong", { s: elapsed });
    if (elapsed >= 1) return t("w.transcribingS", { s: elapsed });
    return t("w.transcribing");
  });

  const idle = $derived.by((): {
    text: string;
    section: SectionId | null;
    privacy?: PrivacyKind;
    dim: boolean;
  } => {
    if (status.starting) return { text: t("w.starting"), section: null, dim: true };
    if (status.mic_denied) {
      return { text: t("w.micDenied"), section: null, privacy: "microphone", dim: true };
    }
    if (!status.audio_available) return { text: t("w.noMic"), section: "general", dim: true };
    if (status.accessibility_needed) {
      return { text: t("w.accessibility"), section: null, privacy: "accessibility", dim: true };
    }
    if (!status.engine_ready) {
      if (status.status === "loading" || status.error_code === "engine_loading") {
        return { text: t("w.loading"), section: null, dim: true };
      }
      if (status.error_code === "model_missing") {
        return { text: t("w.downloadModel"), section: "voice", dim: true };
      }
      if (status.status === "error" || status.error_code === "engine_error") {
        return { text: t("w.modelError"), section: "voice", dim: true };
      }
      return { text: t("w.downloadModel"), section: "voice", dim: true };
    }
    if (status.hotkey_ready === false) {
      return { text: t("w.hotkeyUnavailable"), section: null, dim: true };
    }
    return { text: "Synapse", section: null, dim: false };
  });

  const idleText = $derived(idle.text);

  function showFlash(text: string, kind: "ok" | "err") {
    if (!text) return;
    flash = { text: text.length > 220 ? text.slice(0, 219) + "…" : text, kind };
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => (flash = null), kind === "err" ? 7000 : 2600);
  }

  const centerText = $derived.by(() => {
    if (flash) return flash.text;
    if (status.status === "processing") return procLabel;
    return idleText;
  });

  async function resizeWindowTo(targetWin: number, anchorRight: boolean) {
    const win = getCurrentWindow();
    const [pos, size, scale, mon] = await Promise.all([
      win.outerPosition(),
      win.outerSize(),
      win.scaleFactor(),
      currentMonitor(),
    ]);
    const delta = targetWin - size.width / scale;
    if (Math.abs(delta) < 2) return;
    await win.setResizable(true);
    await win.setSize(new LogicalSize(targetWin, WINDOW_HEIGHT));
    if (anchorRight) {
      let nx = pos.x / scale - delta;
      if (mon) nx = Math.max(mon.position.x / scale, nx);
      await win.setPosition(new LogicalPosition(nx, pos.y / scale));
    }
    await win.setResizable(false);
  }

  async function applyWindowWidth(targetPill: number) {
    const seq = ++resizeSeq;
    clearTimeout(shrinkTimer);
    try {
      const grow = targetPill > pillW;
      if (grow) {
        await resizeWindowTo(targetPill + WINDOW_EXTRA, dockSide === "right");
        if (seq === resizeSeq) pillW = targetPill;
      } else {
        pillW = targetPill;
        shrinkTimer = setTimeout(async () => {
          if (seq !== resizeSeq) return;
          try {
            await resizeWindowTo(targetPill + WINDOW_EXTRA, dockSide === "right");
          } catch (_) {}
        }, 400);
      }
    } catch (_) {
      pillW = targetPill;
    }
  }

  $effect(() => {
    const mode = status.status;
    void centerText;
    void (async () => {
      await tick();
      if (mode === "recording") {
        void applyWindowWidth(PILL_MIN);
        return;
      }
      const textW = measureEl ? measureEl.scrollWidth : 0;
      const extra = mode === "processing" ? 25 : 0;
      const target = Math.max(PILL_MIN, Math.min(PILL_MAX, 52 + textW + extra));
      void applyWindowWidth(target);
    })();
  });

  function pushLevel(raw: number) {
    const shaped = Math.min(1, Math.pow(Math.max(0, raw) * 6.5, 0.75));
    const next = levels.slice(1);
    next.push(shaped);
    levels = next;
  }

  function resetLevels() {
    levels = Array(BAR_COUNT).fill(0);
  }

  async function updateDockSide() {
    try {
      const win = getCurrentWindow();
      const [pos, size, scale, mon] = await Promise.all([
        win.outerPosition(),
        win.outerSize(),
        win.scaleFactor(),
        currentMonitor(),
      ]);
      if (!mon) return;
      const pillCenterX = pos.x + size.width / 2;
      const pillRightX = pillCenterX + PILL_HALF * scale;
      const monitorRight = mon.position.x + mon.size.width;
      const need = DOCK_NEED * scale;
      dockSide = monitorRight - pillRightX >= need ? "right" : "left";
    } catch (_) {}
  }

  function applyStatus(payload: unknown) {
    if (!payload || typeof payload !== "object") return;
    const was = status.status;
    status = { ...INITIAL_STATUS, starting: false, ...(payload as Partial<StatusPayload>) };
    if (status.status === "recording" && was !== "recording") resetLevels();
  }

  function pipelineErrorText(payload: PipelineErrorPayload | string | null | undefined): string {
    if (typeof payload === "string") return tOr("err." + payload, payload);
    if (!payload || typeof payload !== "object") return "";
    const message = typeof payload.message === "string" ? payload.message : "";
    const code = typeof payload.code === "string" ? payload.code : "";
    if (code) return tOr("err." + code, message || code);
    return message;
  }

  function openSection(section: SectionId) {
    api.openSettings(section).catch(() => {
      api.openWindow("settings").catch(() => {});
    });
  }

  function openPrivacy(kind: PrivacyKind) {
    api.openPrivacySettings(kind).catch(() => openSection("general"));
  }

  onMount(() => {
    let disposed = false;
    let statusSeq = 0;
    let gapTimer: ReturnType<typeof setTimeout> | undefined;
    let pollTimer: ReturnType<typeof setInterval> | undefined;
    const unlisten: UnlistenFn[] = [];
    const win = getCurrentWindow();

    const clearHover = () => (hovered = false);

    async function fetchStatus(): Promise<boolean> {
      const seq = statusSeq;
      try {
        const next = await api.getStatus();
        if (!disposed && seq === statusSeq) applyStatus(next);
        return true;
      } catch (_) {
        return false;
      }
    }

    function onStatusEvent(payload: unknown) {
      statusSeq++;
      applyStatus(payload);
      clearTimeout(gapTimer);
      gapTimer = setTimeout(() => {
        void fetchStatus();
      }, 1500);
    }

    const onFocus = () => {
      void fetchStatus();
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") void fetchStatus();
    };

    window.addEventListener("blur", clearHover);
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);

    (async () => {
      const results = await Promise.allSettled([
        on<StatusPayload>("status-changed", (e) => onStatusEvent(e.payload)),
        on<number>("audio-level", (e) => {
          if (typeof e.payload === "number") pushLevel(e.payload);
        }),
        on<CompletePayload>("transcription-complete", (e) =>
          showFlash(typeof e.payload?.final_text === "string" ? e.payload.final_text : "", "ok"),
        ),
        on<PipelineErrorPayload | string>("pipeline-error", (e) =>
          showFlash(pipelineErrorText(e.payload), "err"),
        ),
        on("transcription-empty", () => showFlash(t("w.noSpeech"), "err")),
        on("transcription-cancelled", () => showFlash(t("w.cancelled"), "err")),
        on<Settings>("settings-changed", (e) => {
          if (e.payload && typeof e.payload === "object") setLanguage(e.payload.ui_language);
        }),
        win.onMoved(() => {
          hovered = false;
          updateDockSide();
        }),
        win.onResized(() => updateDockSide()),
        win.onScaleChanged(() => updateDockSide()),
      ]);
      for (const r of results) {
        if (r.status !== "fulfilled") continue;
        if (disposed) {
          try {
            r.value();
          } catch (_) {}
        } else {
          unlisten.push(r.value);
        }
      }
      if (disposed) return;
      for (let attempt = 0; attempt < 40 && !disposed; attempt++) {
        if (await fetchStatus()) break;
        await new Promise((r) => setTimeout(r, Math.min(1500, 200 + attempt * 100)));
      }
      if (disposed) return;
      pollTimer = setInterval(() => {
        if (status.starting) void fetchStatus();
      }, 2000);
      for (let attempt = 0; attempt < 20 && !disposed; attempt++) {
        try {
          const s = await api.getSettings();
          if (!disposed && s && typeof s === "object") setLanguage(s.ui_language);
          break;
        } catch (_) {
          await new Promise((r) => setTimeout(r, Math.min(2000, 300 + attempt * 200)));
        }
      }
    })().catch(() => {});

    updateDockSide();

    return () => {
      disposed = true;
      clearTimeout(flashTimer);
      clearTimeout(gapTimer);
      clearInterval(pollTimer);
      window.removeEventListener("blur", clearHover);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
      for (const u of unlisten) {
        try {
          u();
        } catch (_) {}
      }
    };
  });

  function toggle() {
    api.toggleRecording().catch(() => {});
  }

  function cancelProcessing() {
    cancelling = true;
    api.cancelRecording().catch(() => {});
  }

  function handleSeal() {
    if (status.status === "processing") cancelProcessing();
    else toggle();
  }
</script>

{#snippet dockButtons()}
  <button class="icon" aria-label={t("w.settings")} onclick={() => api.openWindow("settings").catch(() => {})}>
    <Icon name="gear-six" size={15} />
  </button>
  <button class="icon" aria-label={t("w.history")} onclick={() => api.openWindow("history").catch(() => {})}>
    <Icon name="clock-counter-clockwise" size={15} />
  </button>
{/snippet}

<div
  class="stage"
  role="toolbar"
  tabindex="-1"
  aria-label="Synapse"
  onpointerenter={() => (hovered = true)}
  onpointerleave={() => (hovered = false)}
  onpointercancel={() => (hovered = false)}
>
  <div class="dock left" class:active={dockSide === "left"} class:show={hovered && dockSide === "left"}>
    {@render dockButtons()}
  </div>

  <span class="measure" bind:this={measureEl}>{centerText}</span>

  <div class="pill" data-state={status.status} data-tauri-drag-region style={`width:${pillW}px`}>
    <button
      class="seal"
      data-state={status.status}
      onclick={handleSeal}
      aria-label={status.status === "recording"
        ? t("w.stop")
        : status.status === "processing"
          ? t("w.cancelTx")
          : t("w.start")}
    >
      <span class="core"></span>
      <span class="halo"></span>
    </button>

    <div class="center" data-tauri-drag-region>
      {#if status.status === "recording"}
        <div class="wave" aria-label={t("w.level")}>
          {#each levels as value}
            <span class="bar" style={`height:${(2 + value * 13).toFixed(1)}px`}></span>
          {/each}
        </div>
      {:else if flash}
        <span class="flash" class:err={flash.kind === "err"}>{flash.text}</span>
      {:else if status.status === "processing"}
        <div class="proc-row">
          <span class="brand shimmer proc-text" class:slow={elapsed >= 20}>{procLabel}</span>
          <button class="proc-cancel" onclick={cancelProcessing} aria-label={t("w.cancelTx")} title={t("w.cancelTx")}>
            <Icon name="x" size={11} />
          </button>
        </div>
      {:else if idle.privacy}
        {@const kind = idle.privacy}
        <button class="brand dim link" onclick={() => openPrivacy(kind)}>
          {idleText}
        </button>
      {:else if idle.section}
        {@const target = idle.section}
        <button class="brand dim link" onclick={() => openSection(target)}>
          {idleText}
        </button>
      {:else}
        <span class="brand" class:dim={idle.dim}>
          {idleText}
        </span>
      {/if}
    </div>
  </div>

  <div class="dock right" class:active={dockSide === "right"} class:show={hovered && dockSide === "right"}>
    {@render dockButtons()}
  </div>
</div>

<style>
  :global(html),
  :global(body) {
    background: transparent !important;
    overflow: hidden;
    user-select: none;
  }

  .stage {
    height: 40px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    padding: 0 3px;
  }

  .dock {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 0 0 auto;
    width: 60px;
    opacity: 0;
    pointer-events: none;
    transition:
      opacity 0.2s ease,
      transform 0.36s cubic-bezier(0.22, 0.7, 0.3, 1);
  }

  .dock.left {
    justify-content: flex-end;
    transform: translateX(12px);
  }

  .dock.right {
    justify-content: flex-start;
    transform: translateX(-12px);
  }

  .dock.show {
    opacity: 1;
    transform: translateX(0);
    pointer-events: auto;
  }

  .dock.show .icon:nth-child(2) {
    transition-delay: 0.04s;
  }

  .pill {
    position: relative;
    display: flex;
    align-items: center;
    gap: 9px;
    flex: 0 0 auto;
    width: 150px;
    height: 32px;
    padding: 0 13px 0 6px;
    border-radius: 16px;
    background: var(--paper-raised);
    border: 1px solid var(--line-strong);
    box-shadow: var(--shadow);
    color: var(--ink);
    transition:
      border-color 0.25s ease,
      width 0.36s cubic-bezier(0.22, 0.7, 0.3, 1);
  }

  .measure {
    position: absolute;
    visibility: hidden;
    white-space: nowrap;
    font-family: var(--font-body);
    font-size: 12.5px;
    font-weight: 600;
    letter-spacing: -0.005em;
    pointer-events: none;
  }

  .pill[data-state="recording"] {
    border-color: var(--terra-line);
  }

  :global(:root[data-theme="dark"]) .pill {
    box-shadow: none;
  }

  @media (prefers-color-scheme: dark) {
    :global(:root:not([data-theme="light"])) .pill {
      box-shadow: none;
    }
  }

  .seal {
    position: relative;
    width: 22px;
    height: 22px;
    flex: 0 0 auto;
    border-radius: 50%;
    z-index: 1;
  }

  .core {
    position: absolute;
    inset: 0;
    margin: auto;
    width: 13px;
    height: 13px;
    border-radius: 50%;
    background: var(--terra);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.18);
    transition: transform 0.25s ease, background 0.3s ease;
  }

  .halo {
    position: absolute;
    inset: 0;
    margin: auto;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    border: 1.5px solid transparent;
    box-sizing: border-box;
    transition: border-color 0.3s ease;
  }

  .seal:hover .core {
    transform: scale(1.1);
  }

  .seal[data-state="recording"] .core {
    background: var(--terra-bright);
  }

  .seal[data-state="recording"] .halo {
    border-color: var(--terra);
    animation: pulse 1.5s ease-out infinite;
  }

  .seal[data-state="processing"] .halo {
    border-color: transparent;
    border-top-color: var(--terra);
    animation: spin 0.85s linear infinite;
  }

  .seal[data-state="loading"] .core {
    animation: breathe 1.6s ease-in-out infinite;
  }

  .seal[data-state="error"] .core {
    background: var(--danger);
  }

  @keyframes pulse {
    0% {
      transform: scale(0.78);
      opacity: 0.8;
    }
    100% {
      transform: scale(1.4);
      opacity: 0;
    }
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  @keyframes breathe {
    0%,
    100% {
      transform: scale(0.84);
      opacity: 0.65;
    }
    50% {
      transform: scale(1.08);
      opacity: 1;
    }
  }

  .center {
    flex: 1 1 auto;
    min-width: 0;
    height: 100%;
    display: flex;
    align-items: center;
    cursor: grab;
    overflow: hidden;
    z-index: 1;
  }

  .center:active {
    cursor: grabbing;
  }

  .brand {
    font-family: var(--font-body);
    font-size: 12.5px;
    font-weight: 600;
    letter-spacing: -0.005em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--ink);
    pointer-events: none;
  }

  .brand.dim {
    color: var(--ink-faint);
    font-weight: 500;
  }

  .brand.link {
    min-width: 0;
    padding: 0;
    text-align: left;
    pointer-events: auto;
    cursor: pointer;
    text-decoration: underline;
    text-decoration-color: transparent;
    text-underline-offset: 3px;
    transition: color 0.16s ease, text-decoration-color 0.16s ease;
  }

  .brand.link:hover {
    color: var(--terra-text);
    text-decoration-color: currentColor;
  }

  .brand.link:focus-visible {
    outline: none;
    color: var(--terra-text);
    text-decoration-color: currentColor;
  }

  .brand.shimmer {
    background: linear-gradient(90deg, var(--ink-ghost) 20%, var(--terra) 48%, var(--ink-ghost) 72%);
    background-size: 220% 100%;
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
    animation: shimmer 1.6s linear infinite;
  }

  .proc-row {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }

  .proc-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .proc-text.slow {
    animation: none;
    background: none;
    -webkit-background-clip: border-box;
    background-clip: border-box;
    color: var(--terra);
  }

  .proc-cancel {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--terra-soft);
    color: var(--terra-text);
    border: 1px solid var(--terra-line);
    pointer-events: auto;
    cursor: pointer;
    transition: background 0.18s ease;
  }

  .proc-cancel:hover {
    background: var(--terra);
    color: #fff;
  }

  @keyframes shimmer {
    from {
      background-position: 120% 0;
    }
    to {
      background-position: -120% 0;
    }
  }

  .flash {
    font-family: var(--font-body);
    font-size: 12px;
    font-weight: 500;
    color: var(--sage-text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    pointer-events: none;
    animation: fadein 0.24s ease;
  }

  .flash.err {
    color: var(--danger);
  }

  @keyframes fadein {
    from {
      opacity: 0;
      transform: translateY(2px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .wave {
    display: flex;
    align-items: center;
    gap: 1.6px;
    height: 17px;
    width: 100%;
    pointer-events: none;
  }

  .bar {
    flex: 1 1 auto;
    min-height: 2px;
    max-width: 3px;
    border-radius: 2px;
    background: var(--terra);
    transition: height 60ms ease-out;
  }

  .icon {
    width: 28px;
    height: 28px;
    flex: 0 0 auto;
    border-radius: 50%;
    display: grid;
    place-items: center;
    color: var(--ink-soft);
    background: var(--paper-raised);
    border: 1px solid var(--line-strong);
    box-shadow: var(--shadow-sm);
    transition: color 0.16s ease, border-color 0.16s ease, transform 0.14s ease;
  }

  .icon:hover {
    color: var(--terra);
    border-color: var(--terra-line);
    transform: translateY(-1px);
  }

  .icon:active {
    transform: translateY(0);
  }

  .icon:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px var(--terra-soft);
  }

  @media (prefers-reduced-motion: reduce) {
    .icon,
    .bar,
    .dock {
      transition: none;
    }
    .halo,
    .core,
    .brand.shimmer {
      animation: none !important;
    }
  }
</style>
