<script lang="ts">
  import { onMount } from "svelte";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { t } from "../lib/i18n.svelte";
  import type { Settings } from "../lib/types";
  import Switch from "./Switch.svelte";
  import Segmented from "./Segmented.svelte";
  import TextField from "./TextField.svelte";
  import ModelList from "./ModelList.svelte";
  import { commit, refreshModels } from "./store.svelte";

  let { s }: { s: Settings } = $props();

  onMount(() => {
    refreshModels();
  });

  function openGroqKeys() {
    openUrl("https://console.groq.com/keys").catch(() => {});
  }
</script>

<div class="group">
  <div class="item">
    <span class="item-label">{t("voice.engine")}</span>
    <Segmented
      label={t("voice.engine")}
      value={s.transcription_backend}
      options={[
        { id: "local", label: t("voice.onPc") },
        { id: "groq", label: t("voice.groq") },
      ]}
      onchange={(id) => void commit({ transcription_backend: id === "groq" ? "groq" : "local" })}
    />
  </div>
</div>

{#if s.transcription_backend === "groq"}
  <div class="group">
    <TextField
      key="groq_api_key"
      label={t("voice.groqKey")}
      secret
      placeholder="gsk_..."
      link={{ label: t("voice.getKey"), onclick: openGroqKeys }}
    />
    <div class="item">
      <span class="item-label">{t("voice.model")}</span>
      <Segmented
        label={t("voice.model")}
        value={s.groq_model}
        options={[
          { id: "whisper-large-v3-turbo", label: t("voice.groqTurbo") },
          { id: "whisper-large-v3", label: t("voice.groqLarge") },
        ]}
        onchange={(id) => void commit({ groq_model: id })}
      />
    </div>
  </div>
{:else}
  <ModelList kind="whisper" recommended="large-v3-turbo" />
{/if}

<div class="group">
  <Switch
    label={t("voice.fillers")}
    checked={s.filler_removal}
    onchange={(v) => void commit({ filler_removal: v })}
  />
</div>
