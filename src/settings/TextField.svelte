<script lang="ts">
  import Icon from "../lib/Icon.svelte";
  import { t } from "../lib/i18n.svelte";
  import { app, schedule, flush, type TextKey } from "./store.svelte";

  let {
    key,
    label,
    secret = false,
    placeholder = "",
    link = null,
  }: {
    key: TextKey;
    label: string;
    secret?: boolean;
    placeholder?: string;
    link?: { label: string; onclick: () => void } | null;
  } = $props();

  let draft = $state("");
  let focused = $state(false);
  let reveal = $state(false);

  const stored = $derived(app.settings ? String(app.settings[key] ?? "") : "");

  $effect(() => {
    const value = stored;
    if (!focused) draft = value;
  });

  function onInput(e: Event & { currentTarget: HTMLInputElement }) {
    draft = e.currentTarget.value;
    schedule({ [key]: draft.trim() }, 700);
  }

  function onBlur() {
    focused = false;
    void flush();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      void flush();
    }
  }
</script>

<div class="field">
  <label class="field-label" for={`tf-${key}`}><span>{label}</span></label>
  <div class="input-row">
    <input
      id={`tf-${key}`}
      type={secret && !reveal ? "password" : "text"}
      autocomplete="off"
      spellcheck="false"
      {placeholder}
      value={draft}
      oninput={onInput}
      onfocus={() => (focused = true)}
      onblur={onBlur}
      onkeydown={onKey}
    />
    {#if secret}
      <button
        type="button"
        class="icon-btn"
        title={reveal ? t("secret.hide") : t("secret.show")}
        aria-label={reveal ? t("secret.hide") : t("secret.show")}
        onclick={() => (reveal = !reveal)}
      >
        <Icon name={reveal ? "eye-slash" : "eye"} size={16} />
      </button>
    {/if}
    {#if link}
      <button type="button" class="btn sm" onclick={link.onclick}>
        {link.label}
        <Icon name="arrow-right" size={14} />
      </button>
    {/if}
  </div>
</div>
