<script lang="ts">
  import { onMount } from "svelte";
  import { fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { api, getCurrentWindow } from "../lib/ipc";
  import type { SectionId } from "../lib/types";
  import Icon from "../lib/Icon.svelte";
  import { t, setLanguage, type TKey } from "../lib/i18n.svelte";
  import { app, start, flush } from "./store.svelte";
  import GeneralSection from "./GeneralSection.svelte";
  import VoiceSection from "./VoiceSection.svelte";
  import AiSection from "./AiSection.svelte";
  import DictionarySection from "./DictionarySection.svelte";
  import AdvancedSection from "./AdvancedSection.svelte";
  import AboutSection from "./AboutSection.svelte";

  const reduce =
    typeof matchMedia !== "undefined" && matchMedia("(prefers-reduced-motion: reduce)").matches;

  const NAV: { id: SectionId; key: TKey; icon: string }[] = [
    { id: "general", key: "nav.general", icon: "gear-six" },
    { id: "voice", key: "nav.voice", icon: "microphone" },
    { id: "ai", key: "nav.ai", icon: "sparkle" },
    { id: "dictionary", key: "nav.dictionary", icon: "book-open-text" },
    { id: "advanced", key: "nav.advanced", icon: "sliders-horizontal" },
    { id: "about", key: "nav.about", icon: "info" },
  ];

  const current = $derived(NAV.find((n) => n.id === app.section) ?? NAV[0]);

  $effect(() => {
    setLanguage(app.settings?.ui_language ?? "auto");
  });

  $effect(() => {
    const title = t("title.settings");
    try {
      document.title = title;
    } catch {
      return;
    }
    getCurrentWindow()
      .setTitle(title)
      .catch(() => {});
  });

  onMount(() => start());

  function go(id: SectionId) {
    if (app.section === id) return;
    void flush();
    app.section = id;
  }

  function minimize() {
    getCurrentWindow()
      .minimize()
      .catch(() => {});
  }

  function close() {
    void flush();
    api.hideWindow("settings").catch(() => {});
  }
</script>

<div class="shell">
  <header class="titlebar" data-tauri-drag-region>
    <div class="brand">
      <span class="logo"></span>
      <h1>Synapse <span>· {t("app.settings")}</span></h1>
    </div>
    <div class="head-actions">
      {#if app.save === "error"}
        <span class="save-chip err" title={app.saveError || undefined}>
          <Icon name="warning-circle" size={14} />
          <span>{t("save.failed")}</span>
          <button type="button" class="chip-btn" onclick={() => void flush()}>
            {t("common.retry")}
          </button>
        </span>
      {:else if app.save === "saved"}
        {#key app.savePulse}
          <span class="save-chip ok" aria-live="polite">
            <Icon name="check-circle" size={14} />
            <span>{t("save.saved")}</span>
          </span>
        {/key}
      {/if}
      <div class="winbtns">
        <button
          type="button"
          class="winbtn"
          title={t("win.minimize")}
          aria-label={t("win.minimize")}
          onclick={minimize}
        >
          <Icon name="minus" size={15} />
        </button>
        <button
          type="button"
          class="winbtn close"
          title={t("win.close")}
          aria-label={t("win.close")}
          onclick={close}
        >
          <Icon name="x" size={15} />
        </button>
      </div>
    </div>
  </header>

  {#if app.settings}
    {@const s = app.settings}
    <div class="body">
      <nav class="sidebar">
        {#each NAV as n (n.id)}
          {#if n.id === "advanced"}
            <div class="nav-divider" role="separator"></div>
          {/if}
          <button
            type="button"
            class="nav-item"
            class:active={app.section === n.id}
            aria-current={app.section === n.id ? "page" : undefined}
            onclick={() => go(n.id)}
          >
            <Icon name={n.icon} size={18} />
            <span>{t(n.key)}</span>
          </button>
        {/each}
      </nav>

      <section class="panel">
        {#key app.section}
          <div class="panel-inner" in:fly={{ y: 6, duration: reduce ? 0 : 240, easing: cubicOut }}>
            <h2 class="panel-title display">{t(current.key)}</h2>
            {#if app.section === "general"}
              <GeneralSection {s} />
            {:else if app.section === "voice"}
              <VoiceSection {s} />
            {:else if app.section === "ai"}
              <AiSection {s} />
            {:else if app.section === "dictionary"}
              <DictionarySection {s} />
            {:else if app.section === "advanced"}
              <AdvancedSection {s} />
            {:else}
              <AboutSection {s} />
            {/if}
          </div>
        {/key}
      </section>
    </div>
  {:else}
    <div class="loading">
      {#if app.loadSlow}
        <Icon name="warning-circle" size={24} />
        <p class="load-msg">{t("load.slow")}</p>
        <p class="load-sub">{t("load.slowSub")}</p>
      {:else}
        <p class="load-msg">{t("common.loading")}</p>
      {/if}
    </div>
  {/if}
</div>
