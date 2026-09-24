<script lang="ts">
  import { app, schedule, flush, type ListKey } from "./store.svelte";

  let {
    key,
    label,
    hint = "",
    placeholder = "",
    rows = 5,
  }: {
    key: ListKey;
    label: string;
    hint?: string;
    placeholder?: string;
    rows?: number;
  } = $props();

  let draft = $state("");
  let focused = $state(false);

  const stored = $derived.by(() => {
    const list = app.settings ? app.settings[key] : [];
    return Array.isArray(list) ? list.join("\n") : "";
  });

  $effect(() => {
    const value = stored;
    if (!focused) draft = value;
  });

  function parse(text: string): string[] {
    return text
      .split(/[\n,]/)
      .map((s) => s.trim())
      .filter(Boolean);
  }

  function onInput(e: Event & { currentTarget: HTMLTextAreaElement }) {
    draft = e.currentTarget.value;
    schedule({ [key]: parse(draft) }, 700);
  }

  function onBlur() {
    focused = false;
    void flush();
  }
</script>

<div class="field">
  <label class="field-label" for={`lf-${key}`}>
    <span>{label}</span>
    {#if hint}<span class="field-hint">{hint}</span>{/if}
  </label>
  <textarea
    id={`lf-${key}`}
    {rows}
    spellcheck="false"
    {placeholder}
    value={draft}
    oninput={onInput}
    onfocus={() => (focused = true)}
    onblur={onBlur}
  ></textarea>
</div>
