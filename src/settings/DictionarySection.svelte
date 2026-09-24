<script lang="ts">
  import Icon from "../lib/Icon.svelte";
  import { t } from "../lib/i18n.svelte";
  import type { Settings } from "../lib/types";
  import LinesField from "./LinesField.svelte";
  import { commit } from "./store.svelte";

  let { s }: { s: Settings } = $props();

  let spoken = $state("");
  let replacement = $state("");

  const entries = $derived(Object.entries(s.dictionary ?? {}));

  function current(): Record<string, string> {
    return { ...($state.snapshot(s.dictionary) ?? {}) };
  }

  function add() {
    const key = spoken.trim();
    if (!key) return;
    const next = current();
    next[key] = replacement;
    spoken = "";
    replacement = "";
    void commit({ dictionary: next });
  }

  function remove(key: string) {
    const next = current();
    delete next[key];
    void commit({ dictionary: next });
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      add();
    }
  }
</script>

<div class="group">
  <div class="dict-add">
    <input
      placeholder={t("dict.spoken")}
      aria-label={t("dict.spoken")}
      spellcheck="false"
      bind:value={spoken}
      onkeydown={onKey}
    />
    <span class="arrow"><Icon name="arrow-right" size={16} /></span>
    <input
      placeholder={t("dict.replacement")}
      aria-label={t("dict.replacement")}
      spellcheck="false"
      bind:value={replacement}
      onkeydown={onKey}
    />
    <button type="button" class="btn primary sm" onclick={add} disabled={!spoken.trim()}>
      {t("dict.add")}
    </button>
  </div>
  {#each entries as [k, v] (k)}
    <div class="dict-row">
      <span class="k">{k}</span>
      <span class="arrow"><Icon name="arrow-right" size={15} /></span>
      <span class="v">{v}</span>
      <button
        type="button"
        class="icon-btn danger"
        aria-label={t("dict.remove")}
        title={t("dict.remove")}
        onclick={() => remove(k)}
      >
        <Icon name="trash" size={15} />
      </button>
    </div>
  {:else}
    <div class="empty">
      <Icon name="book-open-text" size={24} />
      <p>{t("dict.empty")}</p>
    </div>
  {/each}
</div>

<div class="group">
  <LinesField
    key="vocabulary"
    label={t("dict.vocab")}
    hint={t("dict.vocabHint")}
    placeholder={"Claude\nAnthropic\nTauri"}
  />
</div>
