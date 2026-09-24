<script lang="ts">
  import Icon from "../lib/Icon.svelte";
  import { t } from "../lib/i18n.svelte";
  import { describeError } from "./store.svelte";

  let {
    label,
    busyLabel,
    okLabel,
    action,
  }: {
    label: string;
    busyLabel: string;
    okLabel: string;
    action: () => Promise<{ ok: boolean; detail: string }>;
  } = $props();

  let busy = $state(false);
  let result = $state<{ ok: boolean; detail: string } | null>(null);

  async function run() {
    if (busy) return;
    busy = true;
    result = null;
    try {
      result = await action();
    } catch (e) {
      result = { ok: false, detail: describeError(e) };
    } finally {
      busy = false;
    }
  }
</script>

<div class="action-row">
  <button type="button" class="btn sm" onclick={run} disabled={busy}>
    {busy ? busyLabel : label}
  </button>
  {#if result}
    {#if result.ok}
      <span class="ok-line"><Icon name="check-circle" size={14} />{okLabel}</span>
    {:else}
      <span class="err-line one-line" title={result.detail}>
        {t("common.failed")}{result.detail ? ": " + result.detail : ""}
      </span>
    {/if}
  {/if}
</div>
