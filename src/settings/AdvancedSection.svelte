<script lang="ts">
  import Icon from "../lib/Icon.svelte";
  import { t, locale } from "../lib/i18n.svelte";
  import { api } from "../lib/ipc";
  import type { HfFile, ModelKind, SectionId, Settings } from "../lib/types";
  import Switch from "./Switch.svelte";
  import Segmented from "./Segmented.svelte";
  import RangeField from "./RangeField.svelte";
  import LinesField from "./LinesField.svelte";
  import ActionButton from "./ActionButton.svelte";
  import {
    app,
    commit,
    testLlm,
    fmtBytes,
    describeError,
    refreshModels,
    refreshLlama,
  } from "./store.svelte";

  let { s }: { s: Settings } = $props();

  let hfUrl = $state("");
  let hfBusy = $state(false);
  let hfError = $state<{ text: string; detail: string } | null>(null);
  let repoFiles = $state<HfFile[] | null>(null);
  let pending = $state<{ filename: string; url: string; size: number; kind: ModelKind } | null>(null);
  let added = $state<ModelKind | null>(null);

  const num = (v: number, digits: number) =>
    v.toLocaleString(locale(), { minimumFractionDigits: digits, maximumFractionDigits: digits });

  async function restartAi(): Promise<{ ok: boolean; detail: string }> {
    try {
      await api.restartLlm();
      return { ok: true, detail: "" };
    } catch (e) {
      return { ok: false, detail: describeError(e) };
    }
  }

  function resetHf() {
    hfError = null;
    repoFiles = null;
    pending = null;
    added = null;
  }

  async function detect() {
    if (hfBusy) return;
    const url = hfUrl.trim();
    resetHf();
    if (!url) return;
    hfBusy = true;
    try {
      const r = await api.hfDetect(url);
      if (!r || typeof r !== "object") {
        hfError = { text: t("hf.failed"), detail: "" };
      } else if (r.kind === "invalid") {
        hfError = { text: t("hf.invalid"), detail: r.reason ?? "" };
      } else if (r.kind === "file") {
        pending = { filename: r.filename, url: r.url, size: 0, kind: r.guessed ?? "llm" };
      } else {
        const files = await api.hfListFiles(r.repo);
        repoFiles = Array.isArray(files) ? files : [];
      }
    } catch (e) {
      hfError = { text: t("hf.failed"), detail: describeError(e) };
    } finally {
      hfBusy = false;
    }
  }

  function pickFile(f: HfFile) {
    pending = { filename: f.filename, url: f.url, size: f.size_bytes, kind: f.guessed ?? "llm" };
    repoFiles = null;
  }

  async function confirmAdd() {
    const req = pending;
    if (!req || hfBusy) return;
    hfBusy = true;
    hfError = null;
    try {
      const created = await api.addCustomModel(
        req.filename,
        req.kind,
        req.filename,
        req.url,
        req.size,
        true,
      );
      added = created?.kind === "whisper" || created?.kind === "llm" ? created.kind : req.kind;
      pending = null;
      hfUrl = "";
      refreshModels();
      refreshLlama();
    } catch (e) {
      hfError = { text: t("hf.addFailed"), detail: describeError(e) };
    } finally {
      hfBusy = false;
    }
  }

  function go(section: SectionId) {
    app.section = section;
  }
</script>

<div class="group">
  <span class="group-head">{t("adv.audio")}</span>
  <Switch
    label={t("adv.vad")}
    checked={s.vad_enabled}
    onchange={(v) => void commit({ vad_enabled: v })}
  />
  {#if s.vad_enabled}
    <RangeField
      key="vad_threshold"
      label={t("adv.sensitivity")}
      min={0.1}
      max={0.9}
      step={0.05}
      format={(v) => num(v, 2)}
    />
    <RangeField
      key="speech_pad_ms"
      label={t("adv.padding")}
      min={0}
      max={400}
      step={10}
      format={(v) => `${num(v, 0)} ms`}
    />
    <RangeField
      key="min_silence_ms"
      label={t("adv.minSilence")}
      min={50}
      max={1000}
      step={50}
      format={(v) => `${num(v, 0)} ms`}
    />
  {/if}
</div>

<div class="group">
  <span class="group-head">{t("adv.paste")}</span>
  <Switch
    label={t("adv.restoreClipboard")}
    checked={s.restore_clipboard}
    onchange={(v) => void commit({ restore_clipboard: v })}
  />
  <RangeField
    key="paste_delay_ms"
    label={t("adv.pasteDelay")}
    min={40}
    max={1000}
    step={10}
    format={(v) => `${num(v, 0)} ms`}
  />
</div>

{#if app.hw?.build_gpu}
  <div class="group">
    <span class="group-head">{t("adv.performance")}</span>
    <Switch
      label={t("adv.gpu")}
      checked={s.prefer_gpu}
      onchange={(v) => void commit({ prefer_gpu: v })}
    />
  </div>
{/if}

<div class="group">
  <span class="group-head">{t("adv.ai")}</span>
  <RangeField
    key="llm_timeout_ms"
    label={t("adv.timeout")}
    min={500}
    max={20000}
    step={100}
    format={(v) => `${num(v / 1000, 1)} s`}
  />
  <RangeField
    key="llm_temperature"
    label={t("adv.temperature")}
    min={0}
    max={1}
    step={0.05}
    format={(v) => num(v, 2)}
  />
  <div class="item actions">
    <ActionButton
      label={t("adv.testConn")}
      busyLabel={t("ai.testing")}
      okLabel={t("ai.testOk")}
      action={testLlm}
    />
    <ActionButton
      label={t("adv.restartAi")}
      busyLabel={t("adv.restarting")}
      okLabel={t("common.done")}
      action={restartAi}
    />
  </div>
</div>

<div class="group">
  <LinesField
    key="filler_words"
    label={t("adv.fillers")}
    hint={t("adv.fillersHint")}
    rows={4}
  />
</div>

<div class="group">
  <span class="group-head">{t("adv.extraModels")}</span>
  <div class="field">
    <div class="input-row">
      <input
        placeholder={t("hf.placeholder")}
        aria-label={t("hf.placeholder")}
        spellcheck="false"
        bind:value={hfUrl}
        onkeydown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            void detect();
          }
        }}
      />
      <button type="button" class="btn sm" onclick={detect} disabled={hfBusy || !hfUrl.trim()}>
        {hfBusy && !pending ? "…" : t("hf.detect")}
      </button>
    </div>
    {#if hfError}
      <div class="err-line" title={hfError.detail || undefined}>{hfError.text}</div>
    {/if}
    {#if added}
      <button type="button" class="ok-line link" onclick={() => added && go(added === "whisper" ? "voice" : "ai")}>
        <Icon name="check-circle" size={14} />
        {added === "whisper" ? t("hf.addedVoice") : t("hf.addedAi")}
        <Icon name="arrow-right" size={13} />
      </button>
    {/if}
  </div>

  {#if repoFiles}
    {#if repoFiles.length === 0}
      <div class="err-line">{t("hf.noFiles")}</div>
    {:else}
      <div class="repo-list">
        {#each repoFiles as f}
          <button type="button" class="repo-file" onclick={() => pickFile(f)}>
            <span class="rf-name">{f.filename}</span>
            <span class="rf-meta tnum">
              {f.guessed === "whisper" ? t("hf.kindVoice") : t("hf.kindAi")}{fmtBytes(f.size_bytes) ? " · " + fmtBytes(f.size_bytes) : ""}
            </span>
          </button>
        {/each}
      </div>
    {/if}
  {/if}

  {#if pending}
    <div class="pending">
      <div class="pending-file tnum" title={pending.filename}>{pending.filename}</div>
      <div class="pending-row">
        <Segmented
          label={t("ai.type")}
          value={pending.kind}
          options={[
            { id: "whisper", label: t("hf.kindVoice"), icon: "waveform" },
            { id: "llm", label: t("hf.kindAi"), icon: "sparkle" },
          ]}
          onchange={(id) => {
            if (pending) pending.kind = id === "whisper" ? "whisper" : "llm";
          }}
        />
        <div class="pending-actions">
          <button type="button" class="btn ghost sm" onclick={resetHf} disabled={hfBusy}>
            {t("common.cancel")}
          </button>
          <button type="button" class="btn primary sm" onclick={confirmAdd} disabled={hfBusy}>
            <Icon name="download-simple" size={14} />
            {t("hf.add")}
          </button>
        </div>
      </div>
    </div>
  {/if}
</div>
