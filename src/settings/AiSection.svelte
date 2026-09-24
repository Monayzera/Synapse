<script lang="ts">
  import { onMount } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import Icon from "../lib/Icon.svelte";
  import { t, tOr, type TKey } from "../lib/i18n.svelte";
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
    refreshModels,
    refreshLlama,
  } from "./store.svelte";

  let { s }: { s: Settings } = $props();

  onMount(() => {
    refreshModels();
    refreshLlama();
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

  const GROQ_MODELS: { id: string; label: string }[] = [
    { id: "llama-3.1-8b-instant", label: "Llama 3.1 8B" },
    { id: "llama-3.3-70b-versatile", label: "Llama 3.3 70B" },
    { id: "qwen/qwen3-32b", label: "Qwen 3 32B" },
    { id: "openai/gpt-oss-120b", label: "GPT-OSS 120B" },
    { id: "openai/gpt-oss-20b", label: "GPT-OSS 20B" },
    { id: "meta-llama/llama-4-scout-17b-16e-instruct", label: "Llama 4 Scout" },
    { id: "allam-2-7b", label: "Allam 2 7B" },
  ];

  const OTHER_TYPES: { id: LlmBackend; key: TKey }[] = [
    { id: "open_ai_compatible", key: "ai.typeOpenai" },
    { id: "anthropic", key: "ai.typeAnthropic" },
    { id: "ollama", key: "ai.typeOllama" },
  ];

  const RECOMMENDED_LLM = "gemma-3-4b-it";

  const provider = $derived(
    s.llm_backend === "local" ? "local" : s.llm_backend === "groq" ? "groq" : "other",
  );
  const translateValue = $derived(s.translation_enabled ? s.translation_target : "");
  const llamaReady = $derived(!!app.llama && app.llama.binary && app.llama.model_present);
  const recommended = $derived(app.models.find((m) => m.info?.id === RECOMMENDED_LLM) ?? null);
  const progress = $derived(app.llamaProgress);

  function recommendedName(): string {
    const label = recommended?.info.label ?? "";
    const match = label.match(/^(.*?)\s*\(([^)]+)\)\s*$/);
    return (match ? match[1] : label).trim() || "Gemma 3 4B";
  }

  function pickProvider(id: string) {
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
      <label class="item-label" for="ai-groq-model">{t("ai.model")}</label>
      <select
        id="ai-groq-model"
        value={s.groq_llm_model}
        onchange={(e) => void commit({ groq_llm_model: e.currentTarget.value })}
      >
        {#each GROQ_MODELS as m}
          <option value={m.id}>{m.label}</option>
        {/each}
        {#if !GROQ_MODELS.some((m) => m.id === s.groq_llm_model)}
          <option value={s.groq_llm_model}>{s.groq_llm_model}</option>
        {/if}
      </select>
    </div>
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
    <div class="item">
      <ActionButton
        label={t("ai.test")}
        busyLabel={t("ai.testing")}
        okLabel={t("ai.testOk")}
        action={testLlm}
      />
    </div>
  {/if}
</div>

{#if provider === "local" && app.llama}
  {#if llamaReady}
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
          {#if app.llamaRunning}
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
            disabled={app.llamaRunning}
          >
            <Icon name="download-simple" size={14} />
            {progress?.error && !app.llamaRunning ? t("common.retry") : t("ai.install")}
          </button>
        </div>
      </div>
    </div>
  {/if}
{/if}
