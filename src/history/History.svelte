<script lang="ts">
  import { onMount } from "svelte";
  import { fly } from "svelte/transition";
  import { cubicOut } from "svelte/easing";
  import { api, on, getCurrentWindow, type UnlistenFn } from "../lib/ipc";
  import type { HistoryEntry, Settings, Stats } from "../lib/types";
  import Icon from "../lib/Icon.svelte";
  import { t, locale, setLanguage } from "../lib/i18n.svelte";

  let entries = $state<HistoryEntry[]>([]);
  let stats = $state<Stats>({ total_entries: 0, total_words: 0, total_speaking_ms: 0, avg_wpm: 0 });
  let query = $state("");
  let expanded = $state<Set<number>>(new Set());
  let copiedId = $state<number>(0);
  let confirmClear = $state(false);
  let searchTimer: ReturnType<typeof setTimeout> | undefined;

  const reduce =
    typeof matchMedia !== "undefined" && matchMedia("(prefers-reduced-motion: reduce)").matches;

  $effect(() => {
    const title = t("title.history");
    try {
      document.title = title;
    } catch (_) {
      return;
    }
    getCurrentWindow()
      .setTitle(title)
      .catch(() => {});
  });

  onMount(() => {
    let unlisten: UnlistenFn[] = [];
    let disposed = false;
    refresh();
    (async () => {
      try {
        unlisten.push(await on("transcription-complete", () => refresh()));
      } catch (_) {}
      try {
        unlisten.push(
          await on<Settings>("settings-changed", (e) => {
            if (e.payload && typeof e.payload === "object") setLanguage(e.payload.ui_language);
          }),
        );
      } catch (_) {}
      for (let attempt = 0; attempt < 20 && !disposed; attempt++) {
        try {
          const loaded = await api.getSettings();
          if (!disposed && loaded && typeof loaded === "object") setLanguage(loaded.ui_language);
          break;
        } catch (_) {
          await new Promise((r) => setTimeout(r, Math.min(2000, 300 + attempt * 200)));
        }
      }
    })();
    return () => {
      disposed = true;
      clearTimeout(searchTimer);
      unlisten.forEach((u) => {
        try {
          u();
        } catch (_) {}
      });
    };
  });

  function refresh() {
    api.getStats().then((s) => (stats = s)).catch(() => {});
    if (query.trim()) {
      api.searchHistory(query.trim(), 200).then((e) => (entries = e)).catch(() => {});
    } else {
      api.getHistory(200, 0).then((e) => (entries = e)).catch(() => {});
    }
  }

  function onSearch() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(refresh, 180);
  }

  async function copy(id: number) {
    try {
      await api.recopy(id);
      copiedId = id;
      setTimeout(() => (copiedId = 0), 1200);
    } catch (_) {}
  }

  async function remove(id: number) {
    await api.deleteHistory(id).catch(() => {});
    refresh();
  }

  async function doClear() {
    confirmClear = false;
    try {
      await api.clearHistory();
    } catch (_) {}
    refresh();
  }

  function toggle(id: number) {
    const next = new Set(expanded);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    expanded = next;
  }

  async function toDict(entry: HistoryEntry) {
    const phrase = prompt(t("h.promptPhrase"), entry.raw_text);
    if (phrase === null) return;
    const replacement = prompt(t("h.promptReplacement"), entry.final_text);
    if (replacement === null) return;
    await api.addToDictionary(phrase, replacement).catch(() => {});
  }

  function fmtTime(ms: number): string {
    return new Date(ms).toLocaleString(locale(), {
      day: "2-digit",
      month: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  function fmtNum(v: number, digits: number): string {
    return v.toLocaleString(locale(), {
      minimumFractionDigits: digits,
      maximumFractionDigits: digits,
    });
  }

  function fmtDuration(ms: number): string {
    return fmtNum(ms / 1000, 1) + " " + t("h.sec");
  }

  function fmtSpeaking(ms: number): string {
    const min = Math.floor(ms / 60000);
    if (min >= 60) return fmtNum(min / 60, 1) + " " + t("h.hour");
    if (min >= 1) return min + " " + t("h.min");
    return Math.round(ms / 1000) + " " + t("h.sec");
  }

  function wpm(entry: HistoryEntry): number {
    if (entry.duration_ms <= 0) return 0;
    return Math.round((entry.word_count * 60000) / entry.duration_ms);
  }

  function minimize() {
    getCurrentWindow().minimize().catch(() => {});
  }
</script>

<div class="shell">
  <header class="titlebar" data-tauri-drag-region>
    <div class="brand">
      <span class="logo"></span>
      <h1>Synapse <span>· {t("app.history")}</span></h1>
    </div>
    <div class="head-actions">
      {#if confirmClear}
        <span class="confirm-q">{t("h.clearAsk")}</span>
        <button class="btn btn-confirm" onclick={doClear}>{t("h.confirm")}</button>
        <button class="btn ghost" onclick={() => (confirmClear = false)}>{t("common.cancel")}</button>
      {:else}
        <button class="btn danger" onclick={() => (confirmClear = true)}>
          <Icon name="trash" size={15} /> {t("h.clearAll")}
        </button>
      {/if}
      <div class="winbtns">
        <button class="winbtn" title={t("win.minimize")} aria-label={t("win.minimize")} onclick={minimize}>
          <Icon name="minus" size={15} />
        </button>
        <button class="winbtn close" title={t("win.close")} aria-label={t("win.close")} onclick={() => api.hideWindow("history").catch(() => {})}>
          <Icon name="x" size={15} />
        </button>
      </div>
    </div>
  </header>

  <div class="body">
    <div class="stats card">
      <div class="stat">
        <span class="num tnum">{stats.total_words.toLocaleString(locale())}</span>
        <span class="lbl">{t("h.words")}</span>
      </div>
      <div class="stat">
        <span class="num tnum">{fmtSpeaking(stats.total_speaking_ms)}</span>
        <span class="lbl">{t("h.speaking")}</span>
      </div>
      <div class="stat">
        <span class="num tnum">{stats.avg_wpm.toFixed(0)}</span>
        <span class="lbl" title={t("h.wpmTitle")}>{t("h.avgWpm")}</span>
      </div>
      <div class="stat">
        <span class="num tnum">{stats.total_entries.toLocaleString(locale())}</span>
        <span class="lbl">{t("h.entries")}</span>
      </div>
    </div>

    <div class="toolbar">
      <div class="search">
        <Icon name="magnifying-glass" size={17} />
        <input placeholder={t("h.search")} bind:value={query} oninput={onSearch} />
      </div>
      <span class="count tnum">{entries.length} {entries.length === 1 ? t("h.entryOne") : t("h.entryMany")}</span>
    </div>

    <div class="list">
      {#each entries as e, i (e.id)}
        <div class="entry" in:fly={{ y: 8, duration: reduce ? 0 : 300, delay: reduce ? 0 : Math.min(i * 26, 260), easing: cubicOut }}>
          <div
            class="entry-main"
            onclick={() => toggle(e.id)}
            onkeydown={(ev) => {
              if (ev.key === "Enter" || ev.key === " ") toggle(e.id);
            }}
            role="button"
            tabindex="0"
          >
            <div class="final">{e.final_text}</div>
            {#if expanded.has(e.id) && e.raw_text !== e.final_text}
              <div class="raw"><span class="raw-mark">{t("h.raw")}</span>{e.raw_text}</div>
            {/if}
            <div class="meta tnum">
              <span>{fmtTime(e.created_at)}</span>
              <span class="dot">·</span>
              <span>{fmtDuration(e.duration_ms)}</span>
              <span class="dot">·</span>
              <span title={t("h.wpmTitle")}>{wpm(e)} {t("h.wpm")}</span>
              {#if e.cloud}
                <span class="tag cloud">{t("h.cloud")}</span>
              {:else if e.on_gpu}
                <span class="tag gpu">{t("h.gpu")}</span>
              {:else}
                <span class="tag cpu">{t("h.cpu")}</span>
              {/if}
              {#if e.llm_used}<span class="tag llm">{t("h.ai")}</span>{/if}
            </div>
          </div>
          <div class="entry-actions">
            <button class="btn ghost" onclick={() => copy(e.id)}>
              <Icon name={copiedId === e.id ? "check-circle" : "copy"} size={15} />
              {copiedId === e.id ? t("h.copied") : t("h.copy")}
            </button>
            <button class="btn ghost" onclick={() => toDict(e)}>
              <Icon name="book-bookmark" size={15} /> {t("h.dictionary")}
            </button>
            <button class="btn ghost danger" onclick={() => remove(e.id)}>
              <Icon name="trash" size={15} /> {t("h.delete")}
            </button>
          </div>
        </div>
      {/each}
      {#if entries.length === 0}
        <div class="empty">
          <Icon name="bird" size={36} />
          <p>{t("h.empty")}</p>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .shell {
    position: relative;
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--paper);
    border: 1px solid var(--paper-edge);
    border-radius: 12px;
    overflow: hidden;
  }

  .head-actions {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .confirm-q {
    font-size: 13.5px;
    font-weight: 500;
    color: var(--ink-faint);
  }

  .btn-confirm {
    background: var(--danger);
    border-color: var(--danger);
    color: var(--on-accent);
  }

  .btn-confirm:hover {
    background: var(--danger);
    border-color: var(--danger);
    filter: brightness(0.92);
  }

  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .stats {
    flex: 0 0 auto;
    margin: 24px 28px 4px;
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    overflow: hidden;
  }

  .stat {
    padding: 18px 22px;
    display: flex;
    flex-direction: column;
    gap: 5px;
    border-left: 1px solid var(--line);
  }

  .stat:first-child {
    border-left: none;
  }

  .num {
    font-family: var(--font-display);
    font-size: var(--t-figure);
    font-weight: 600;
    line-height: 1;
    letter-spacing: -0.02em;
    color: var(--ink);
  }

  .lbl {
    font-size: var(--t-meta);
    color: var(--ink-faint);
    font-weight: 500;
  }

  .toolbar {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 18px 28px 14px;
  }

  .search {
    position: relative;
    flex: 1;
    display: flex;
    align-items: center;
    min-width: 0;
  }

  .search :global(.ph) {
    position: absolute;
    left: 13px;
    color: var(--ink-ghost);
    pointer-events: none;
  }

  .search input {
    width: 100%;
    padding-left: 40px;
  }

  .count {
    flex: 0 0 auto;
    font-size: var(--t-meta);
    color: var(--ink-faint);
  }

  .list {
    flex: 1;
    overflow-y: auto;
    padding: 0 28px 24px;
    display: flex;
    flex-direction: column;
    gap: 9px;
  }

  .entry {
    display: flex;
    gap: 14px;
    align-items: flex-start;
    padding: 15px 17px;
    background: var(--paper-raised);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    transition: border-color 0.16s ease, box-shadow 0.16s ease;
  }

  .entry:hover {
    border-color: var(--line-strong);
    box-shadow: var(--shadow-sm);
  }

  .entry-main {
    flex: 1;
    min-width: 0;
    cursor: pointer;
  }

  .final {
    font-family: var(--font-body);
    font-size: 15.5px;
    line-height: 1.5;
    color: var(--ink);
    overflow-wrap: anywhere;
  }

  .raw {
    margin-top: 9px;
    padding-left: 12px;
    border-left: 2px solid var(--line-strong);
    font-size: 14px;
    line-height: 1.5;
    color: var(--ink-faint);
    overflow-wrap: anywhere;
  }

  .raw-mark {
    display: inline-block;
    margin-right: 7px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink-ghost);
    vertical-align: 1px;
  }

  .meta {
    margin-top: 11px;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    font-size: var(--t-meta);
    color: var(--ink-faint);
  }

  .meta .dot {
    color: var(--ink-ghost);
  }

  .tag {
    font-family: var(--font-body);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.02em;
    padding: 2px 8px;
    border-radius: var(--radius-xs);
  }

  .tag.gpu {
    background: var(--sage-soft);
    color: var(--sage-text);
  }

  .tag.cpu {
    background: var(--paper-sunk);
    color: var(--ink-faint);
  }

  .tag.cloud {
    background: var(--sky-soft);
    color: var(--sky-text);
  }

  .tag.llm {
    background: var(--terra-soft);
    color: var(--terra-text);
  }

  .entry-actions {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 0 0 auto;
    opacity: 0;
    transform: translateX(5px);
    transition: opacity 0.18s ease, transform 0.18s ease;
  }

  .entry-actions .btn {
    justify-content: flex-start;
    padding: 6px 11px;
    border-color: transparent;
    font-size: 13px;
  }

  .entry:hover .entry-actions,
  .entry:focus-within .entry-actions {
    opacity: 1;
    transform: translateX(0);
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 13px;
    text-align: center;
    color: var(--ink-ghost);
    padding: 52px 40px;
  }

  .empty p {
    margin: 0;
    font-size: 15px;
    color: var(--ink-faint);
  }

  @media (max-width: 620px) {
    .stats {
      grid-template-columns: repeat(2, 1fr);
    }
    .stat:nth-child(3) {
      border-left: none;
    }
    .stat:nth-child(n + 3) {
      border-top: 1px solid var(--line);
    }
    .entry {
      flex-direction: column;
    }
    .entry-actions {
      flex-direction: row;
      opacity: 1;
      transform: none;
    }
  }
</style>
