<script lang="ts">
  import Icon from "../lib/Icon.svelte";
  import { t } from "../lib/i18n.svelte";
  import { api } from "../lib/ipc";
  import type { ModelKind, ModelStatus } from "../lib/types";
  import {
    app,
    adoptSettings,
    refreshModels,
    refreshLlama,
    markDownloadStart,
    markDownloadError,
    isDownloading,
    describeError,
    fmtBytes,
  } from "./store.svelte";

  let { kind, recommended }: { kind: ModelKind; recommended: string } = $props();

  let confirmId = $state<string | null>(null);
  let activating = $state<string | null>(null);
  let rowError = $state<{ id: string; text: string; detail: string } | null>(null);

  const list = $derived(app.models.filter((m) => m.info?.kind === kind));

  const noAccel = $derived(
    !app.hw?.build_gpu || (!!app.status?.engine_ready && !!app.status?.cpu_mode),
  );

  function isActive(m: ModelStatus): boolean {
    const s = app.settings;
    if (!s) return false;
    if (kind === "whisper") return s.whisper_model === m.info.id;
    return s.llm_local_model === m.info.filename || s.llm_local_model === m.info.id;
  }

  function isCustom(m: ModelStatus): boolean {
    return m.info.id.startsWith("custom-");
  }

  function displayName(m: ModelStatus): string {
    if (kind === "whisper" && !isCustom(m)) return m.info.id;
    const match = m.info.label.match(/^(.*?)\s*\(([^)]+)\)\s*$/);
    return (match ? match[1] : m.info.label).trim() || m.info.filename;
  }

  function slowHere(m: ModelStatus): boolean {
    if (kind !== "whisper" || isCustom(m) || !app.hw) return false;
    const id = m.info.id;
    const weak = app.hw.tier === "weak";
    const heavy = id.startsWith("large-v3") && !id.includes("turbo") && !id.includes("q5");
    const mid = id === "large-v3-turbo" || id === "medium";
    return (heavy && (weak || noAccel)) || (mid && weak && noAccel);
  }

  function sizeOf(m: ModelStatus): string {
    return fmtBytes(m.info.size_bytes > 0 ? m.info.size_bytes : m.actual_bytes);
  }

  function activeLabel(): { text: string; tone: "ok" | "busy" | "bad" } {
    const st = app.status;
    if (kind === "whisper") {
      if (st?.status === "loading" || st?.error_code === "engine_loading") {
        return { text: t("common.loading"), tone: "busy" };
      }
      if (st?.error_code === "engine_error") return { text: t("model.failed"), tone: "bad" };
      return { text: t("model.active"), tone: "ok" };
    }
    if (app.settings?.llm_backend === "local") {
      if (st?.llm === "starting") return { text: t("model.starting"), tone: "busy" };
      if (st?.llm === "failed") return { text: t("model.failed"), tone: "bad" };
    }
    return { text: t("model.active"), tone: "ok" };
  }

  function download(m: ModelStatus) {
    confirmId = null;
    rowError = null;
    markDownloadStart(m.info.id);
    api.downloadModel(m.info.id, true).catch((e) => markDownloadError(m.info.id, e));
  }

  async function activate(m: ModelStatus) {
    if (activating) return;
    confirmId = null;
    rowError = null;
    activating = m.info.id;
    try {
      const next = await api.activateModel(m.info.id);
      adoptSettings(next);
    } catch (e) {
      rowError = { id: m.info.id, text: t("model.activateFailed"), detail: describeError(e) };
    } finally {
      activating = null;
    }
  }

  async function remove(m: ModelStatus) {
    confirmId = null;
    rowError = null;
    try {
      await api.deleteModel(m.info.id);
    } catch (e) {
      rowError = { id: m.info.id, text: t("model.deleteFailed"), detail: describeError(e) };
    } finally {
      refreshModels();
      refreshLlama();
    }
  }
</script>

<div class="group models">
  {#each list as m (m.info.id)}
    {@const id = m.info.id}
    {@const active = isActive(m)}
    {@const busy = isDownloading(id)}
    {@const dl = app.downloads[id]}
    {@const canDelete = !active && (m.present || isCustom(m))}
    {@const pill = active && m.present ? activeLabel() : null}
    <div class="model-row" class:is-active={active && m.present}>
      <div class="model-main">
        <div class="model-head">
          <span class="model-name" title={m.info.filename}>{displayName(m)}</span>
          {#if id === recommended}
            <span class="tag rec">{t("model.recommended")}</span>
          {/if}
          {#if slowHere(m)}
            <span class="tag warn">{t("voice.slow")}</span>
          {/if}
        </div>
        {#if sizeOf(m)}
          <div class="model-meta tnum">{sizeOf(m)}</div>
        {/if}
        {#if busy && dl}
          <div class="bar"><span style={`width:${Math.max(0, Math.min(100, dl.pct || 0))}%`}></span></div>
        {/if}
        {#if dl?.error && !busy}
          <div class="err-line" title={dl.error}>{t("model.downloadFailed")}</div>
        {:else if rowError?.id === id}
          <div class="err-line" title={rowError.detail}>{rowError.text}</div>
        {/if}
      </div>
      <div class="model-actions">
        {#if confirmId === id}
          <button type="button" class="btn sm danger-solid" onclick={() => remove(m)}>
            {t("common.delete")}
          </button>
          <button
            type="button"
            class="icon-btn"
            aria-label={t("common.cancel")}
            title={t("common.cancel")}
            onclick={() => (confirmId = null)}
          >
            <Icon name="x" size={15} />
          </button>
        {:else}
          {#if busy}
            <span class="pct tnum">{Math.round(dl?.pct || 0)}%</span>
          {:else if pill}
            <span class="state-pill" data-tone={pill.tone}>
              {#if pill.tone === "ok"}<Icon name="check-circle" size={14} />{/if}
              {pill.text}
            </span>
          {:else if m.present}
            <button
              type="button"
              class="btn sm"
              onclick={() => activate(m)}
              disabled={activating !== null}
            >
              {activating === id ? "…" : t("model.use")}
            </button>
          {:else}
            <button type="button" class="btn sm" onclick={() => download(m)}>
              <Icon name="download-simple" size={14} />
              {t("model.download")}
            </button>
          {/if}
          {#if canDelete && !busy}
            <button
              type="button"
              class="icon-btn danger"
              aria-label={t("model.deleteLabel")}
              title={t("model.deleteLabel")}
              onclick={() => (confirmId = id)}
            >
              <Icon name="trash" size={15} />
            </button>
          {/if}
        {/if}
      </div>
    </div>
  {:else}
    <div class="empty">
      <Icon name="cube" size={24} />
      <p>{t("model.empty")}</p>
    </div>
  {/each}
</div>
