<script lang="ts">
  let {
    label,
    checked,
    onchange,
    disabled = false,
    sub = "",
    subError = false,
    subTitle = "",
  }: {
    label: string;
    checked: boolean;
    onchange: (value: boolean) => void;
    disabled?: boolean;
    sub?: string;
    subError?: boolean;
    subTitle?: string;
  } = $props();
</script>

<label class="item switch" class:disabled>
  <span class="item-text">
    <span class="item-label">{label}</span>
    {#if sub}
      <span class="item-sub" class:err={subError} title={subTitle || undefined}>{sub}</span>
    {/if}
  </span>
  <input
    type="checkbox"
    {checked}
    {disabled}
    onchange={(e) => onchange(e.currentTarget.checked)}
  />
  <span class="track" aria-hidden="true"><span class="thumb"></span></span>
</label>
