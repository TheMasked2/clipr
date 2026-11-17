<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from './api';
  import type { UpdateInfo } from './api';
  import { toastError } from './store.svelte';
  import { fmtBytes, fmtDateTime } from './format';

  // `remote`: this page is open on another device, which may look but not install (it restarts clipr).
  let { remote }: { remote: boolean } = $props();

  let info = $state<UpdateInfo | null>(null);
  let busy = $state(false);
  let notesOpen = $state(false);

  // Poll faster while something is happening, slowly otherwise.
  const ACTIVE = new Set(['checking', 'downloading', 'waiting', 'installing']);

  async function refresh() {
    try {
      info = await api.getUpdate();
    } catch {
      /* clipr is restarting or unreachable; the next poll tries again */
    }
  }

  onMount(() => {
    refresh();
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      await refresh();
      timer = setTimeout(tick, info && ACTIVE.has(info.state) ? 700 : 5000);
    };
    timer = setTimeout(tick, 700);
    return () => clearTimeout(timer);
  });

  async function check() {
    busy = true;
    try {
      await api.checkUpdate();
      await new Promise((r) => setTimeout(r, 300));
      await refresh();
    } catch (e) {
      toastError(e);
    } finally {
      busy = false;
    }
  }

  async function install() {
    busy = true;
    try {
      await api.applyUpdate();
      await refresh();
    } catch (e) {
      toastError(e);
    } finally {
      busy = false;
    }
  }

  const pct = (p: { done: number; total: number }) => (p.total ? Math.round((p.done / p.total) * 100) : 0);
</script>

<div class="upd">
  {#if !info}
    <p class="muted small">Loading…</p>
  {:else}
    <div class="row">
      <div class="what">
        <div>
          clipr <strong>{info.current}</strong>
          <span class="muted small">{info.installed ? '(installed)' : '(portable)'}</span>
        </div>
        <div class="small" aria-live="polite">
          {#if info.state === 'checking'}
            <span class="muted">Checking GitHub…</span>
          {:else if info.state === 'up_to_date'}
            <span class="ok">You have the latest version.</span>
          {:else if info.state === 'error'}
            <span class="err">{info.error}</span>
          {:else if info.release && info.state === 'downloading' && info.progress}
            <span>Downloading {info.release.version}… {pct(info.progress)} % of {fmtBytes(info.progress.total)}</span>
          {:else if info.release && info.state === 'waiting'}
            <span>{info.release.version} is ready and installs once nothing is running{info.busy ? ` (${info.busy})` : ''}.</span>
          {:else if info.release && info.state === 'installing'}
            <span>Installing {info.release.version}. clipr restarts in a moment…</span>
          {:else if info.release}
            <span>Version <strong>{info.release.version}</strong> is out{info.release.prerelease ? ' (beta)' : ''}.</span>
          {:else}
            <span class="muted">Not checked yet.</span>
          {/if}
          {#if info.checked_at && info.state !== 'checking'}
            <span class="muted"> · Checked {fmtDateTime(info.checked_at)}</span>
          {/if}
        </div>
      </div>
      <div class="actions">
        <button onclick={check} disabled={busy || info.state === 'checking' || info.state === 'downloading' || info.state === 'installing'}>Check now</button>
        {#if info.release && !['downloading', 'waiting', 'installing'].includes(info.state)}
          {#if info.installed && info.release.setup && !remote}
            <button class="primary" onclick={install} disabled={busy || info.busy !== null} title={info.busy ? `Can't update while ${info.busy}` : undefined}>Update now</button>
          {:else if !info.installed}
            <a class="button primary" href={info.release.page} target="_blank" rel="noopener noreferrer">Download</a>
          {/if}
        {/if}
      </div>
    </div>
    {#if info.state === 'downloading' && info.progress}
      <progress max="100" value={pct(info.progress)}></progress>
    {/if}
    {#if info.release && info.busy && info.installed && !['waiting', 'installing'].includes(info.state)}
      <p class="muted small">Can't update while {info.busy}.</p>
    {/if}
    {#if remote && info.release && info.installed}
      <p class="muted small">Install it from the PC running clipr.</p>
    {/if}
    {#if info.release?.notes}
      <button class="link small" onclick={() => (notesOpen = !notesOpen)} aria-expanded={notesOpen}>{notesOpen ? 'Hide' : 'Show'} what's new</button>
      {#if notesOpen}
        <pre class="notes">{info.release.notes}</pre>
        <a class="small" href={info.release.page} target="_blank" rel="noopener noreferrer">Release page on GitHub</a>
      {/if}
    {/if}
  {/if}
</div>

<style>
  .upd { display: flex; flex-direction: column; gap: 0.5rem; padding: 0.8rem; margin-bottom: 0.8rem; border: 1px solid var(--border); border-radius: 10px; }
  .row { display: flex; gap: 0.8rem; align-items: center; flex-wrap: wrap; }
  .what { flex: 1; min-width: 12rem; display: flex; flex-direction: column; gap: 0.2rem; }
  .actions { display: flex; gap: 0.5rem; }
  .button { display: inline-flex; align-items: center; padding: 0.45rem 0.9rem; border-radius: 8px; text-decoration: none; }
  .ok { color: #4ade80; }
  .err { color: var(--danger); }
  progress { width: 100%; accent-color: var(--accent); }
  .link { align-self: flex-start; background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; }
  .notes { white-space: pre-wrap; font: inherit; font-size: 0.85rem; max-height: 16rem; overflow: auto; margin: 0; padding: 0.6rem; border-radius: 8px; background: var(--surface-2, rgba(255, 255, 255, 0.04)); }
</style>
