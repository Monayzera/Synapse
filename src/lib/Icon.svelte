<script lang="ts">
  import { ICONS } from "./icons";

  let {
    name,
    size = 18,
  }: {
    name: string;
    size?: number;
  } = $props();
</script>

<svg
  class="ph"
  viewBox="0 0 256 256"
  width={size}
  height={size}
  fill="currentColor"
  aria-hidden="true"
  focusable="false"
>
  <path d={ICONS[name] ?? ""} />
</svg>

<style>
  .ph {
    display: block;
    flex: 0 0 auto;
  }
</style>
