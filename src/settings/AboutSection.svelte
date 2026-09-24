<script lang="ts">
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { t, locale } from "../lib/i18n.svelte";
  import type { Settings, UpdateStatus } from "../lib/types";
  import Switch from "./Switch.svelte";
  import { app, commit, checkUpdate, installUpdate, fmtBytes } from "./store.svelte";

  const REPO = "https://github.com/Monayzera/Synapse";

  type Action = "check" | "checking" | "update" | "install" | null;
  type Hint = "busy" | "unsaved" | null;

  let { s }: { s: Settings } = $props();

  let version = $state("");
  let now = $state(Date.now());
  let acting = $state(false);
  let hint = $state<Hint>(null);
  let hintTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    getVersion()
      .then((v) => {
        version = v;
      })
      .catch(() => {
        version = "";
      });
    const clock = setInterval(() => {
      now = Date.now();
    }, 30_000);
    return () => {
      clearInterval(clock);
      clearTimeout(hintTimer);
    };
  });

  function open(url: string) {
    openUrl(url).catch(() => {});
  }

  function checkedText(ms: number): string {
    const seconds = Math.min(0, Math.round((ms - Math.max(now, Date.now())) / 1000));
    const abs = Math.abs(seconds);
    if (abs < 60) return t("about.checkedNow");
    try {
      const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: "auto" });
      let when: string;
      if (abs < 3600) when = rtf.format(Math.round(seconds / 60), "minute");
      else if (abs < 86400) when = rtf.format(Math.round(seconds / 3600), "hour");
      else when = rtf.format(Math.round(seconds / 86400), "day");
      return t("about.checked", { when });
    } catch {
      return t("about.checked", { when: new Date(ms).toLocaleString(locale()) });
    }
  }

  function percent(u: UpdateStatus): number {
    if (!Number.isFinite(u.total) || u.total <= 0) return 0;
    return Math.max(0, Math.min(100, Math.floor((u.downloaded / u.total) * 100)));
  }

  function errorLabel(u: UpdateStatus): string {
    switch (u.error) {
      case "network":
        return t("about.errNetwork");
      case "unavailable":
        return t("about.errUnavailable");
      case "download":
      case "download_limit":
        return t("about.errDownload");
      case "disk":
        return t("about.errDisk");
      case "signature":
        return t("about.errSignature");
      case "not_completed":
        return t("about.errNotCompleted", { v: u.version ?? "" });
      default:
        return t("about.errUnknown");
    }
  }

  const view = $derived.by(() => {
    const u = app.update;
    if (!u) return null;
    const v = u.version ?? "";
    const checked = u.last_checked ? checkedText(u.last_checked) : "";
    const none: Action = null;
    switch (u.phase) {
      case "unsupported":
        return { label: t("about.unsupported"), sub: "", err: false, action: none, progress: -1 };
      case "idle":
        return { label: t("about.notChecked"), sub: "", err: false, action: "check" as Action, progress: -1 };
      case "checking":
        return { label: t("about.checking"), sub: checked, err: false, action: "checking" as Action, progress: -1 };
      case "up_to_date":
        return { label: t("about.upToDate"), sub: checked, err: false, action: "check" as Action, progress: -1 };
      case "available":
        if (u.error === "location_move") {
          return { label: t("about.available", { v }), sub: t("about.errMove"), err: true, action: none, progress: -1 };
        }
        return { label: t("about.available", { v }), sub: checked, err: false, action: "update" as Action, progress: -1 };
      case "downloading": {
        const p = percent(u);
        const done = fmtBytes(u.downloaded);
        const total = fmtBytes(u.total);
        let sub = done;
        if (total) sub = done ? `${p}% · ${done} / ${total}` : `${p}% · ${total}`;
        return { label: t("about.downloading", { v }), sub, err: false, action: none, progress: p };
      }
      case "ready": {
        let sub = "";
        let err = false;
        let action: Action = "install";
        if (u.error === "location_move") {
          sub = t("about.errMove");
          err = true;
          action = null;
        } else if (u.error === "location_admin") {
          sub = t("about.errLocation");
          err = true;
        } else if (u.blocked && s.auto_update) {
          sub = t("about.readyBlocked");
          err = true;
        } else if (u.error === "install") {
          sub = t("about.readyRetry");
          err = true;
        } else if (s.auto_update) {
          sub = t("about.readyAuto");
        }
        return { label: t("about.ready", { v }), sub, err, action, progress: -1 };
      }
      case "installing":
        return { label: t("about.installing", { v }), sub: t("about.reopen"), err: false, action: none, progress: -1 };
      default: {
        const transient = u.error === "network" || u.error === "unavailable" || u.error === "download";
        const sub = transient && s.auto_update ? t("about.retryAuto") : checked;
        return { label: errorLabel(u), sub, err: false, action: "check" as Action, progress: -1 };
      }
    }
  });

  const hintText = $derived.by(() => {
    if (!hint || !view || (view.action !== "install" && view.action !== "update")) return "";
    return hint === "unsaved" ? t("about.unsaved") : t("about.busy");
  });

  function actionLabel(action: Action): string {
    switch (action) {
      case "update":
        return t("about.updateNow");
      case "install":
        return t("about.installNow");
      default:
        return t("about.checkNow");
    }
  }

  function showHint(next: Hint) {
    clearTimeout(hintTimer);
    hint = next;
    if (next) {
      hintTimer = setTimeout(() => {
        hint = null;
      }, 6000);
    }
  }

  async function run(action: Action) {
    if (acting || !action || action === "checking") return;
    acting = true;
    showHint(null);
    try {
      if (action === "check") {
        await checkUpdate();
        return;
      }
      const refused = await installUpdate();
      if (refused === "update_busy") showHint("busy");
      else if (refused === "settings_unsaved") showHint("unsaved");
    } finally {
      acting = false;
    }
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
  {#if view}
    <span class="group-head">{t("about.updates")}</span>
    <div class="item">
      <div class="item-text model-main">
        <span class="item-label" title={app.update?.detail || undefined}>{view.label}</span>
        {#if hintText}
          <span class="item-sub err">{hintText}</span>
        {:else if view.sub}
          <span class="item-sub tnum" class:err={view.err} title={app.update?.detail || undefined}>{view.sub}</span>
        {/if}
        {#if view.progress >= 0}
          <div class="bar"><span style={`width:${view.progress}%`}></span></div>
        {/if}
      </div>
      {#if view.action}
        <button
          type="button"
          class="btn sm"
          disabled={acting || view.action === "checking"}
          onclick={() => void run(view.action)}
        >
          {actionLabel(view.action)}
        </button>
      {/if}
    </div>
    {#if app.update?.phase !== "unsupported"}
      <Switch
        label={t("about.autoUpdate")}
        checked={s.auto_update}
        onchange={(v) => void commit({ auto_update: v })}
      />
    {/if}
  {/if}
</div>

<div class="group">
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
