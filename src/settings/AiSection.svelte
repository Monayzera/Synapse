<script lang="ts">
  import { onMount } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import Icon from "../lib/Icon.svelte";
  import { api } from "../lib/ipc";
  import { t, tOr, locale, type TKey } from "../lib/i18n.svelte";
  import type { LlmBackend, Settings } from "../lib/types";
  import Switch from "./Switch.svelte";
  import Segmented from "./Segmented.svelte";
  import TextField from "./TextField.svelte";
  import ModelList from "./ModelList.svelte";
  import ActionButton from "./ActionButton.svelte";
  import {
    app,
    commit,
    startLlamaSetup,
    fmtBytes,
    testLlm,
    testOkLabel,
    refreshModels,
    refreshLlama,
    refreshLocalAi,
    refreshGroqModels,
    repairLocalAi,
  } from "./store.svelte";

  let { s }: { s: Settings } = $props();

  onMount(() => {
    refreshModels();
    refreshLlama();
    refreshLocalAi();
    refreshGroqModels();
  });

  const TARGETS = [
    "English",
    "Spanish",
    "Brazilian Portuguese",
    "French",
    "German",
    "Italian",
    "Japanese",
    "Simplified Chinese",
    "Russian",
    "Korean",
  ];

  const OTHER_TYPES: { id: LlmBackend; key: TKey }[] = [
    { id: "open_ai_compatible", key: "ai.typeOpenai" },
    { id: "anthropic", key: "ai.typeAnthropic" },
    { id: "ollama", key: "ai.typeOllama" },
  ];

  const RECOMMENDED_LLM = "gemma-3-4b-it";

  const LIMIT_KEYS: Record<string, TKey> = {
    otpm: "ai.groqLimitTpm",
    tpm: "ai.groqLimitTpm",
    itpm: "ai.groqLimitTpm",
    rpm: "ai.groqLimitRpm",
    tpd: "ai.groqLimitTpd",
    rpd: "ai.groqLimitRpd",
  };

  let test = $state<{ provider: string; ms: number; text: string } | null>(null);
  let restarting = $state(false);

  const provider = $derived(
    s.llm_backend === "local" ? "local" : s.llm_backend === "groq" ? "groq" : "other",
  );
  const translateValue = $derived(s.translation_enabled ? s.translation_target : "");
  const llamaReady = $derived(
    app.localAi
      ? app.localAi.installed && app.localAi.model_present
      : !!app.llama && app.llama.binary && app.llama.model_present,
  );
  const recommended = $derived(app.models.find((m) => m.info?.id === RECOMMENDED_LLM) ?? null);
  const progress = $derived(app.llamaProgress);
  const groqModels = $derived(app.groq?.models ?? []);
  const groqError = $derived.by(() => {
    const code = app.groq?.error;
    if (!code || code === "key_missing" || !s.groq_llm_api_key.trim()) return "";
    if (code === "invalid_key") return t("ai.groqKeyInvalid");
    if (code === "network_blocked") return t("ai.groqNetworkBlocked");
    return t("ai.modelsFailed");
  });
  const groqLimitNote = $derived.by(() => {
    if (!s.groq_llm_api_key.trim()) return "";
    const list = app.groq?.limits;
    if (!Array.isArray(list)) return "";
    let key: TKey | null = null;
    let limit = 0;
    let newest = -Infinity;
    for (const entry of list) {
      if (!entry || typeof entry !== "object" || entry.model !== s.groq_llm_model) continue;
      const kind = typeof entry.kind === "string" ? entry.kind.toLowerCase() : "";
      if (!Object.prototype.hasOwnProperty.call(LIMIT_KEYS, kind)) continue;
      if (typeof entry.limit !== "number" || !Number.isFinite(entry.limit) || entry.limit <= 0) continue;
      const at = typeof entry.at === "number" && Number.isFinite(entry.at) ? entry.at : 0;
      if (at < newest) continue;
      newest = at;
      key = LIMIT_KEYS[kind];
      limit = entry.limit;
    }
    return key ? t(key, { limit: Math.round(limit).toLocaleString(locale()) }) : "";
  });
  const setupBusy = $derived(app.llamaRunning || !!app.localAi?.setup_running);
  const localState = $derived(app.localAi?.state ?? app.status?.llm ?? null);
  const canRepair = $derived(
    !setupBusy &&
      !!app.localAi?.repairable &&
      (localState === "failed" || localState === "ready"),
  );
  const installError = $derived(
    !setupBusy && progress?.error ? app.localAi?.last_error || progress.error : "",
  );
  let keepInstalled = $state(false);
  $effect(() => {
    if (!setupBusy) keepInstalled = false;
    else if (llamaReady) keepInstalled = true;
  });
  const localInstalled = $derived(llamaReady || (setupBusy && keepInstalled));
  const localLine = $derived.by(() => {
    const ai = app.localAi;
    if (localState === "ready") {
      if (ai?.device === "gpu") {
        return ai.device_name
          ? t("ai.localReadyGpu", { name: ai.device_name })
          : t("ai.localReadyGpuPlain");
      }
      if (ai?.repairable) return t("ai.localCpuRepair");
      if (ai?.device === "cpu") return t("ai.localReadyCpu");
      return t("ai.localReady");
    }
    if (localState === "starting") return t("model.starting");
    if (localState === "off" && !s.llm_enabled && !s.translation_enabled) return t("ai.localOff");
    return "";
  });
  const showLocalStatus = $derived(
    setupBusy || localState === "failed" || !!localLine || !!installError,
  );
  const testOk = $derived(testOkLabel(test && test.provider === provider ? test.ms : 0));

  async function runTest(): Promise<{ ok: boolean; detail: string; title?: string }> {
    const which = provider;
    test = null;
    const result = await testLlm();
    if (result.ok) test = { provider: which, ms: result.report?.ms ?? 0, text: result.detail.trim() };
    return result;
  }

  function restartLocal() {
    if (restarting) return;
    restarting = true;
    void api
      .restartLlm()
      .catch(() => {})
      .finally(() => {
        restarting = false;
        refreshLocalAi();
        refreshLlama();
      });
  }

  function shortId(id: string): string {
    const tail = id.split("/").pop()?.trim();
    return tail || id;
  }

  function recommendedName(): string {
    const label = recommended?.info.label ?? "";
    const match = label.match(/^(.*?)\s*\(([^)]+)\)\s*$/);
    return (match ? match[1] : label).trim() || "Gemma 3 4B";
  }

  function pickProvider(id: string) {
    if (id !== provider) test = null;
    if (id === "local") void commit({ llm_backend: "local" });
    else if (id === "groq") void commit({ llm_backend: "groq" });
    else if (provider !== "other") void commit({ llm_backend: "open_ai_compatible" });
  }

  function pickTarget(value: string) {
    if (!value) void commit({ translation_enabled: false });
    else void commit({ translation_enabled: true, translation_target: value });
  }

  function openGroqKeys() {
    openUrl("https://console.groq.com/keys").catch(() => {});
  }
</script>

{#snippet testRow()}
  <div class="item">
    <div class="item-text">
      <ActionButton
        label={t("ai.test")}
        busyLabel={t("ai.testing")}
        okLabel={testOk}
        action={runTest}
      />
      {#if test && test.provider === provider && test.text}
        <span class="item-sub" title={test.text}>{test.text}</span>
      {/if}
    </div>
  </div>
{/snippet}

<div class="group">
  <Switch
    label={t("ai.correct")}
    checked={s.llm_enabled}
    onchange={(v) => void commit({ llm_enabled: v })}
  />
  <div class="item">
    <label class="item-label" for="ai-target">{t("ai.translate")}</label>
    <select id="ai-target" value={translateValue} onchange={(e) => pickTarget(e.currentTarget.value)}>
      <option value="">{t("tt.none")}</option>
      {#each TARGETS as target}
        <option value={target}>{tOr(`tt.${target}`, target)}</option>
      {/each}
      {#if translateValue && !TARGETS.includes(translateValue)}
        <option value={translateValue}>{translateValue}</option>
      {/if}
    </select>
  </div>
  <Switch
    label={t("ai.paragraphs")}
    checked={s.llm_format_paragraphs}
    onchange={(v) => void commit({ llm_format_paragraphs: v })}
  />
</div>

<div class="group">
  <div class="item">
    <span class="item-label">{t("ai.provider")}</span>
    <Segmented
      label={t("ai.provider")}
      value={provider}
      options={[
        { id: "local", label: t("ai.local") },
        { id: "groq", label: t("ai.groq") },
        { id: "other", label: t("ai.other") },
      ]}
      onchange={pickProvider}
    />
  </div>

  {#if provider === "groq"}
    <TextField
      key="groq_llm_api_key"
      label={t("ai.groqKey")}
      secret
      placeholder="gsk_..."
      link={{ label: t("voice.getKey"), onclick: openGroqKeys }}
    />
    <div class="item">
      {#if groqError || groqLimitNote}
        <div class="item-text fill">
          <label class="item-label" for="ai-groq-model">{t("ai.model")}</label>
          {#if groqError}
            <span class="item-sub err" title={groqError}>{groqError}</span>
          {/if}
          {#if groqLimitNote}
            <span class="field-hint">{groqLimitNote}</span>
          {/if}
        </div>
      {:else}
        <label class="item-label" for="ai-groq-model">{t("ai.model")}</label>
      {/if}
      <select
        id="ai-groq-model"
        value={s.groq_llm_model}
        onchange={(e) => void commit({ groq_llm_model: e.currentTarget.value })}
      >
        {#each groqModels as m}
          <option value={m.id}>{m.label}</option>
        {/each}
        {#if !groqModels.some((m) => m.id === s.groq_llm_model)}
          <option value={s.groq_llm_model}>{shortId(s.groq_llm_model)}</option>
        {/if}
      </select>
    </div>
    {@render testRow()}
  {:else if provider === "other"}
    <div class="item">
      <label class="item-label" for="ai-type">{t("ai.type")}</label>
      <select
        id="ai-type"
        value={s.llm_backend}
        onchange={(e) => {
          const v = e.currentTarget.value;
          const hit = OTHER_TYPES.find((o) => o.id === v);
          if (hit) void commit({ llm_backend: hit.id });
        }}
      >
        {#each OTHER_TYPES as o}
          <option value={o.id}>{t(o.key)}</option>
        {/each}
      </select>
    </div>
    <TextField key="llm_endpoint" label={t("ai.endpoint")} placeholder="https://" />
    <TextField key="llm_model_name" label={t("ai.modelName")} />
    <TextField key="llm_api_key" label={t("ai.key")} secret />
    {@render testRow()}
  {:else if provider === "local" && localInstalled}
    {#if showLocalStatus}
      <div class="model-row">
        <div class="model-main">
          <span class="item-label">{t("ai.status")}</span>
          {#if setupBusy}
            <div class="bar">
              <span style={`width:${Math.max(0, Math.min(100, progress?.overall_pct ?? 0))}%`}></span>
            </div>
            <div class="model-meta tnum">
              {progress ? tOr(`stage.${progress.stage}`, t("common.loading")) : t("common.loading")}
              {#if progress}· {Math.round(progress.overall_pct || 0)}%{/if}
            </div>
          {:else}
            {#if localState === "failed"}
              <div class="err-line" title={app.localAi?.last_error ?? undefined}>{t("ai.localStopped")}</div>
            {:else if localLine}
              <div class="model-meta">{localLine}</div>
            {/if}
            {#if installError}
              <div class="err-line" title={installError}>{t("ai.installFailed")}</div>
            {/if}
          {/if}
        </div>
        {#if !setupBusy && (localState === "failed" || canRepair)}
          <div class="model-actions">
            {#if localState === "failed"}
              <button type="button" class="btn sm" onclick={restartLocal} disabled={restarting}>
                {restarting ? t("adv.restarting") : t("ai.restart")}
              </button>
            {/if}
            {#if canRepair}
              <button type="button" class="btn sm" title={t("ai.repairTitle")} onclick={repairLocalAi}>
                {t("ai.repair")}
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
    {@render testRow()}
  {/if}
</div>

{#if provider === "local" && app.llama}
  {#if localInstalled}
    <ModelList kind="llm" recommended={RECOMMENDED_LLM} />
  {:else}
    <div class="group">
      <div class="model-row">
        <div class="model-main">
          <div class="model-head">
            <span class="model-name">{recommendedName()}</span>
            <span class="tag rec">{t("model.recommended")}</span>
          </div>
          {#if recommended && fmtBytes(recommended.info.size_bytes)}
            <div class="model-meta tnum">{fmtBytes(recommended.info.size_bytes)}</div>
          {/if}
          {#if setupBusy}
            <div class="bar">
              <span style={`width:${Math.max(0, Math.min(100, progress?.overall_pct ?? 0))}%`}></span>
            </div>
            <div class="model-meta tnum">
              {progress ? tOr(`stage.${progress.stage}`, t("common.loading")) : t("common.loading")}
              {#if progress}· {Math.round(progress.overall_pct || 0)}%{/if}
            </div>
          {:else if progress?.error}
            <div class="err-line" title={progress.error}>{t("ai.installFailed")}</div>
          {/if}
        </div>
        <div class="model-actions">
          <button
            type="button"
            class="btn primary sm"
            onclick={startLlamaSetup}
            disabled={setupBusy}
          >
            <Icon name="download-simple" size={14} />
            {progress?.error && !setupBusy ? t("common.retry") : t("ai.install")}
          </button>
        </div>
      </div>
    </div>
  {/if}
{/if}

<style>
  .fill {
    flex: 1 1 0;
  }
</style>
