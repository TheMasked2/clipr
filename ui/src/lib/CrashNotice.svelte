<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from './api';
  import type { CrashReport } from './api';
  import { toast, toastError } from './store.svelte';
  import { fmtDateTime } from './format';

  // Crash reports from earlier runs (only with Settings → General → Crash reports on). Nothing is sent by
  // itself: the user reviews the report and files it on GitHub, or dismisses it.
  let reports = $state<CrashReport[]>([]);
  let open = $state(false);
  let busy = $state(false);
  // Another device can't open Explorer on this PC.
  let remote = $state(true);

  const r = $derived(reports[0]);

  async function load() {
    try {
      reports = await api.getCrashes();
      if (reports.length) remote = (await api.getSystem()).remote ?? false;
    } catch {
      /* not important enough to complain about */
    }
  }

  onMount(load);

  async function dismiss() {
    if (!r) return;
    busy = true;
    try {
      await api.dismissCrash(r.id);
      open = false;
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      busy = false;
    }
  }

  async function copy() {
    if (!r) return;
    try {
      await navigator.clipboard.writeText(r.report);
      toast('ok', 'Crash report copied.');
    } catch {
      open = true;
      toast('err', "Couldn't copy: select the report text and copy it yourself.");
    }
  }

  async function reveal() {
    if (!r) return;
    try {
      await api.revealCrash(r.id);
    } catch (e) {
      toastError(e);
    }
  }
</script>

{#if r}
  <section class="card crash" aria-labelledby="crash-h">
    <div class="top">
      <h2 id="crash-h">clipr crashed</h2>
      <span class="muted small">{fmtDateTime(r.time)} · {r.version}{reports.length > 1 ? ` · ${reports.length - 1} more` : ''}</span>
    </div>
    <p class="what">{r.summary}</p>
    <p class="muted small">
      Sorry about that. Reporting it helps fix it: <strong>Report on GitHub</strong> opens a new issue with this
      report filled in, for you to read and send. Your user name, PC name and profile folder are taken out, but read it
      before you send it. Nothing is sent by itself.
    </p>
    <div class="actions">
      <a class="button primary" href={r.issue_url} target="_blank" rel="noopener noreferrer">Report on GitHub</a>
      <button onclick={() => (open = !open)} aria-expanded={open}>{open ? 'Hide report' : 'Show report'}</button>
      <button onclick={copy}>Copy report</button>
      {#if r.has_dump && !remote}
        <button onclick={reveal} title="A minidump helps with crashes inside drivers. Attach it to the issue if you like.">Show dump file</button>
      {/if}
      <span class="spacer"></span>
      <button onclick={dismiss} disabled={busy}>Dismiss</button>
    </div>
    {#if open}
      <pre class="report">{r.report}</pre>
    {/if}
  </section>
{/if}

<style>
  .crash { border-color: color-mix(in srgb, var(--danger) 45%, var(--border)); }
  .top { display: flex; align-items: baseline; gap: 0.6rem; flex-wrap: wrap; }
  h2 { margin: 0; font-size: 1.1rem; }
  .what { margin: 0.4rem 0; font-weight: 600; overflow-wrap: anywhere; }
  .actions { display: flex; gap: 0.5rem; flex-wrap: wrap; align-items: center; margin-top: 0.6rem; }
  .spacer { flex: 1; }
  .button { display: inline-flex; align-items: center; padding: 0.45rem 0.9rem; border-radius: 8px; text-decoration: none; }
  .report { white-space: pre-wrap; overflow-wrap: anywhere; font-size: 0.8rem; max-height: 22rem; overflow: auto; margin: 0.6rem 0 0; padding: 0.6rem; border-radius: 8px; background: var(--surface-2, rgba(255, 255, 255, 0.04)); }
</style>
