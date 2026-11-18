<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from './api';
  import type { CleanupPreview } from './api';
  import { fmtBytes } from './format';
  import { clipHref } from './router.svelte';

  // Warns before the automatic cleanup (Settings → Library: age and size limits) deletes clips. On Home it
  // checks the saved limits once and can be dismissed until a different set of clips is due; in Settings it
  // follows the unsaved limits as they're typed.
  let {
    limits,
  }: { limits?: { max_age_days: number; max_total_gb: number } } = $props();

  const HOURS = 24;
  const KEY = 'clipr.cleanupDismissed';
  let preview = $state<CleanupPreview | null>(null);
  let dismissed = $state('');

  const ids = $derived(preview ? preview.clips.map((c) => c.id).join('\n') + `\n${preview.count}` : '');

  function readDismissed() {
    try {
      dismissed = localStorage.getItem(KEY) ?? '';
    } catch {
      dismissed = '';
    }
  }

  function dismiss() {
    dismissed = ids;
    try {
      localStorage.setItem(KEY, ids);
    } catch {
      /* the banner just comes back next time */
    }
  }

  async function load(l?: { max_age_days: number; max_total_gb: number }) {
    try {
      preview = await api.cleanupPreview(l, HOURS);
    } catch {
      preview = null;
    }
  }

  onMount(() => {
    if (!limits) {
      readDismissed();
      void load();
    }
  });

  // Settings: follow the draft, after a pause in typing.
  $effect(() => {
    if (!limits) return;
    const l = {
      max_age_days: Math.max(0, Math.floor(Number(limits.max_age_days) || 0)),
      max_total_gb: Math.max(0, Math.floor(Number(limits.max_total_gb) || 0)),
    };
    const t = setTimeout(() => void load(l), 400);
    return () => clearTimeout(t);
  });

  const names = $derived.by(() => {
    if (!preview) return '';
    const shown = preview.clips.map((c) => c.title).filter(Boolean);
    const more = preview.count - shown.length;
    return shown.join(', ') + (more > 0 ? ` and ${more} more` : '');
  });
  const what = $derived(
    preview ? `${preview.count} ${preview.count === 1 ? 'clip' : 'clips'} (${fmtBytes(preview.bytes)})` : '',
  );
</script>

{#if limits}
  {#if preview && preview.count > 0}
    <p class="inline warn" role="status">
      ⚠ With these limits, the cleanup deletes <strong>{what}</strong> within {HOURS} hours, oldest first:
      {names}. Favorite a clip to keep it.
    </p>
  {:else if preview && (limits.max_age_days > 0 || limits.max_total_gb > 0)}
    <p class="inline muted small" role="status">Nothing is due for deletion in the next {HOURS} hours.</p>
  {/if}
{:else if preview && preview.count > 0 && dismissed !== ids}
  <section class="card cleanup" aria-labelledby="cleanup-h">
    <h2 id="cleanup-h">Clips will be deleted soon</h2>
    <p>
      The automatic cleanup deletes <strong>{what}</strong> in the next {HOURS} hours, by the limits in
      Settings → Library:
    </p>
    <ul>
      {#each preview.clips as c (c.id)}
        <li><a href={clipHref(c.id)}>{c.title || c.id}</a> <span class="muted small">{c.game} · {fmtBytes(c.size_bytes)}</span></li>
      {/each}
      {#if preview.count > preview.clips.length}<li class="muted">and {preview.count - preview.clips.length} more</li>{/if}
    </ul>
    <p class="muted small">Favorite a clip to keep it, or raise the limits in <a href="#/settings">Settings → Library</a>.</p>
    <div class="actions"><button onclick={dismiss}>Dismiss</button></div>
  </section>
{/if}

<style>
  .cleanup { border-color: color-mix(in srgb, #f5b942 55%, var(--border)); }
  h2 { margin: 0 0 0.3rem; font-size: 1.1rem; }
  p { margin: 0.3rem 0; }
  ul { margin: 0.3rem 0; padding-left: 1.2rem; }
  li { overflow-wrap: anywhere; }
  .actions { display: flex; justify-content: flex-end; }
  .inline { margin: 0.2rem 0 0.8rem; }
  .warn { padding: 0.5rem 0.7rem; border-left: 3px solid #f5b942; background: color-mix(in srgb, #f5b942 8%, transparent); border-radius: 4px; }
</style>
