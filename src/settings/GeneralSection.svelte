<script lang="ts">
  import { t, type TKey } from "../lib/i18n.svelte";
  import { api } from "../lib/ipc";
  import { getTheme, setTheme, type ThemeMode } from "../lib/theme";
  import type { RecordMode, Settings } from "../lib/types";
  import Switch from "./Switch.svelte";
  import Segmented from "./Segmented.svelte";
  import { app, commit, describeError } from "./store.svelte";

  let { s }: { s: Settings } = $props();

  const isMac =
    typeof navigator !== "undefined" && /Mac/i.test(navigator.platform || navigator.userAgent);

  const SPEECH_LANGS = ["auto", "pt", "en", "es", "fr", "de", "it", "ja", "zh", "ru", "ko"];

  let capturing = $state(false);
  let themeMode = $state<ThemeMode>(getTheme());
  let autostartBusy = $state(false);
  let autostartError = $state("");

  const keyParts = $derived(prettyHotkey(s.hotkey_ptt));

  function prettyHotkey(accel: string): string[] {
    if (!accel) return [];
    return accel
      .split("+")
      .filter(Boolean)
      .map((part) => {
        switch (part) {
          case "MouseMiddle":
            return t("key.mouseMiddle");
          case "MouseBack":
            return t("key.mouseBack");
          case "MouseForward":
            return t("key.mouseForward");
          case "Super":
            return isMac ? "Cmd" : "Win";
          case "Space":
            return t("key.space");
          case "Up":
            return t("key.up");
          case "Down":
            return t("key.down");
          case "Left":
            return t("key.left");
          case "Right":
            return t("key.right");
          default:
            return part;
        }
      });
  }

  function modsFrom(e: KeyboardEvent | MouseEvent): string[] {
    const mods: string[] = [];
    if (e.ctrlKey) mods.push("Ctrl");
    if (e.shiftKey) mods.push("Shift");
    if (e.altKey) mods.push("Alt");
    if (e.metaKey) mods.push("Super");
    return mods;
  }

  function normalizeKey(code: string): string | null {
    const modifiers = [
      "ControlLeft",
      "ControlRight",
      "ShiftLeft",
      "ShiftRight",
      "AltLeft",
      "AltRight",
      "MetaLeft",
      "MetaRight",
    ];
    if (modifiers.includes(code)) return null;
    if (code === "Space") return "Space";
    if (code.startsWith("Key")) return code.slice(3);
    if (code.startsWith("Digit")) return code.slice(5);
    if (/^F\d{1,2}$/.test(code)) return code;
    const map: Record<string, string> = {
      Backquote: "`",
      Minus: "-",
      Equal: "=",
      Comma: ",",
      Period: ".",
      Slash: "/",
      Semicolon: ";",
      Quote: "'",
      BracketLeft: "[",
      BracketRight: "]",
      Backslash: "\\",
      Enter: "Enter",
      Tab: "Tab",
      ArrowUp: "Up",
      ArrowDown: "Down",
      ArrowLeft: "Left",
      ArrowRight: "Right",
    };
    return map[code] ?? null;
  }

  function mouseToken(button: number): string | null {
    switch (button) {
      case 1:
        return "MouseMiddle";
      case 3:
        return "MouseBack";
      case 4:
        return "MouseForward";
      default:
        return null;
    }
  }

  function setHotkey(accel: string) {
    capturing = false;
    if (accel && accel !== s.hotkey_ptt) void commit({ hotkey_ptt: accel });
  }

  function onKey(e: KeyboardEvent) {
    if (!capturing) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.code === "Escape") {
      capturing = false;
      return;
    }
    const key = normalizeKey(e.code);
    if (!key) return;
    const mods = modsFrom(e);
    if (mods.length === 0 && !/^F\d{1,2}$/.test(key)) return;
    setHotkey([...mods, key].join("+"));
  }

  function onMouse(e: MouseEvent) {
    if (!capturing) return;
    const token = isMac ? null : mouseToken(e.button);
    if (!token) {
      if (e.button === 0) {
        const target = e.target as HTMLElement | null;
        if (!target || !target.closest(".hotkey-change")) capturing = false;
      }
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    setHotkey([...modsFrom(e), token].join("+"));
  }

  function pickTheme(id: string) {
    const mode: ThemeMode = id === "light" || id === "dark" ? id : "system";
    themeMode = mode;
    setTheme(mode);
  }

  async function toggleAutostart(enabled: boolean) {
    if (!app.settings || autostartBusy) return;
    const previous = app.settings.autostart;
    autostartBusy = true;
    autostartError = "";
    app.settings.autostart = enabled;
    try {
      const status = await api.setAutostart(enabled);
      if (app.settings) app.settings.autostart = enabled;
      if (status && typeof status === "object") {
        app.autostart = status;
        if (status.error) autostartError = status.error;
      }
    } catch (e) {
      if (app.settings) app.settings.autostart = previous;
      autostartError = describeError(e) || "error";
    } finally {
      autostartBusy = false;
    }
  }

  const autostartSub = $derived.by(() => {
    if (autostartError) return { text: t("gen.autostartFailed"), err: true, title: autostartError };
    if (s.autostart && app.autostart?.disabled_by_windows) {
      return { text: t("gen.disabledByWindows"), err: false, title: "" };
    }
    return { text: "", err: false, title: "" };
  });
</script>

<svelte:window
  onkeydown={onKey}
  onmousedown={onMouse}
  onblur={() => (capturing = false)}
/>

<div class="group">
  <div class="item">
    <span class="item-label">{t("gen.shortcut")}</span>
    <div class="item-control hotkey">
      <div class="keys" class:capturing aria-live="polite">
        {#if capturing}
          <span class="keys-prompt">{isMac ? t("gen.pressKeys") : t("gen.pressKeysMouse")}</span>
        {:else if keyParts.length === 0}
          <span class="keys-prompt">{t("gen.none")}</span>
        {:else}
          {#each keyParts as part, i}
            {#if i > 0}<span class="plus">+</span>{/if}
            <kbd>{part}</kbd>
          {/each}
        {/if}
      </div>
      <button
        type="button"
        class="btn sm hotkey-change"
        onclick={() => (capturing = !capturing)}
      >
        {capturing ? t("common.cancel") : t("gen.change")}
      </button>
    </div>
  </div>
  <div class="item">
    <span class="item-label">{t("gen.mode")}</span>
    <Segmented
      label={t("gen.mode")}
      value={s.record_mode}
      options={[
        { id: "push_to_talk", label: t("gen.modeHold") },
        { id: "toggle", label: t("gen.modeTap") },
      ]}
      onchange={(id) => void commit({ record_mode: id as RecordMode })}
    />
  </div>
</div>

<div class="group">
  <div class="item">
    <label class="item-label" for="gen-lang">{t("gen.speechLang")}</label>
    <select
      id="gen-lang"
      value={s.language}
      onchange={(e) => void commit({ language: e.currentTarget.value })}
    >
      {#each SPEECH_LANGS as code}
        <option value={code}>{t(`sl.${code}` as TKey)}</option>
      {/each}
      {#if !SPEECH_LANGS.includes(s.language)}
        <option value={s.language}>{s.language}</option>
      {/if}
    </select>
  </div>
  <div class="item">
    <label class="item-label" for="gen-mic">{t("gen.mic")}</label>
    <select
      id="gen-mic"
      value={s.audio_device ?? ""}
      onchange={(e) => {
        const v = e.currentTarget.value;
        void commit({ audio_device: v === "" ? null : v });
      }}
    >
      <option value="">{t("gen.micDefault")}</option>
      {#each app.devices as d}
        <option value={d}>{d}</option>
      {/each}
      {#if s.audio_device && !app.devices.includes(s.audio_device)}
        <option value={s.audio_device}>{s.audio_device}</option>
      {/if}
    </select>
  </div>
</div>

<div class="group">
  <Switch
    label={isMac ? t("gen.startMac") : t("gen.startWin")}
    checked={s.autostart}
    disabled={autostartBusy}
    sub={autostartSub.text}
    subError={autostartSub.err}
    subTitle={autostartSub.title}
    onchange={(v) => void toggleAutostart(v)}
  />
  <div class="item">
    <span class="item-label">{t("gen.theme")}</span>
    <Segmented
      label={t("gen.theme")}
      iconOnly
      value={themeMode}
      options={[
        { id: "light", label: t("gen.themeLight"), icon: "sun" },
        { id: "system", label: t("gen.themeSystem"), icon: "monitor" },
        { id: "dark", label: t("gen.themeDark"), icon: "moon-stars" },
      ]}
      onchange={pickTheme}
    />
  </div>
  <div class="item">
    <label class="item-label" for="gen-ui">{t("gen.appLang")}</label>
    <select
      id="gen-ui"
      value={s.ui_language}
      onchange={(e) => {
        const v = e.currentTarget.value;
        void commit({ ui_language: v === "pt" || v === "en" ? v : "auto" });
      }}
    >
      <option value="auto">{t("gen.appLangAuto")}</option>
      <option value="pt">{t("lang.pt")}</option>
      <option value="en">{t("lang.en")}</option>
    </select>
  </div>
</div>
