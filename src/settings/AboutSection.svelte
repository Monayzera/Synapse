<script lang="ts">
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { t } from "../lib/i18n.svelte";

  const REPO = "https://github.com/Monayzera/Synapse";

  let version = $state("");

  onMount(() => {
    getVersion()
      .then((v) => {
        version = v;
      })
      .catch(() => {
        version = "";
      });
  });

  function open(url: string) {
    openUrl(url).catch(() => {});
  }
</script>

<div class="group">
  <div class="about-head">
    <span class="about-logo" aria-hidden="true"></span>
    <div class="item-text">
      <span class="about-name display">Synapse</span>
      {#if version}
        <span class="item-sub tnum">{t("about.version", { v: version })}</span>
      {/if}
    </div>
  </div>
  <div class="item">
    <span class="item-label">{t("about.changes")}</span>
    <button type="button" class="btn sm" onclick={() => open(`${REPO}/releases`)}>
      {t("about.open")}
    </button>
  </div>
  <div class="item">
    <span class="item-label">{t("about.source")}</span>
    <button type="button" class="btn sm" onclick={() => open(REPO)}>
      {t("about.open")}
    </button>
  </div>
</div>
