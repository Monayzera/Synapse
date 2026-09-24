<script lang="ts">
  import Icon from "../lib/Icon.svelte";

  type Option = { id: string; label: string; icon?: string };

  let {
    value,
    options,
    onchange,
    label,
    iconOnly = false,
  }: {
    value: string;
    options: Option[];
    onchange: (id: string) => void;
    label: string;
    iconOnly?: boolean;
  } = $props();
</script>

<div class="seg-group" role="radiogroup" aria-label={label}>
  {#each options as o (o.id)}
    <button
      type="button"
      class="seg"
      class:active={value === o.id}
      class:icon-only={iconOnly}
      role="radio"
      aria-checked={value === o.id}
      aria-label={o.label}
      title={iconOnly ? o.label : undefined}
      onclick={() => {
        if (value !== o.id) onchange(o.id);
      }}
    >
      {#if o.icon}<Icon name={o.icon} size={15} />{/if}
      {#if !iconOnly}<span>{o.label}</span>{/if}
    </button>
  {/each}
</div>
