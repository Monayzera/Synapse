<script lang="ts">
  import { app, schedule, flush, type NumberKey } from "./store.svelte";

  let {
    key,
    label,
    min,
    max,
    step,
    format,
  }: {
    key: NumberKey;
    label: string;
    min: number;
    max: number;
    step: number;
    format: (value: number) => string;
  } = $props();

  const value = $derived.by(() => {
    const v = app.settings ? Number(app.settings[key]) : min;
    return Number.isFinite(v) ? Math.min(max, Math.max(min, v)) : min;
  });

  function onInput(e: Event & { currentTarget: HTMLInputElement }) {
    const next = Number(e.currentTarget.value);
    if (!Number.isFinite(next)) return;
    schedule({ [key]: next }, 250);
  }
</script>

<div class="field range">
  <label class="field-label" for={`rg-${key}`}>
    <span>{label}</span>
    <em class="tnum">{format(value)}</em>
  </label>
  <input
    id={`rg-${key}`}
    type="range"
    {min}
    {max}
    {step}
    {value}
    oninput={onInput}
    onchange={() => void flush()}
  />
</div>
