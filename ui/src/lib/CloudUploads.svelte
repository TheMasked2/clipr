<script lang="ts">
  import { cancelUpload, uploadClip, type Upload } from './api';
  import { app, toastError } from './store.svelte';
  import { clipHref } from './router.svelte';
  import { fmtBytes } from './format';

  // The cloud queue on Home: uploads on their way, and the last few that failed.
  const RECENT = 3;
  const active = $derived(app.uploads.filter((u) => u.state === 'queued' || u.state === 'uploading'));
  const failed = $derived(
    app.uploads.filter((u) => u.state === 'failed' && app.cloud[u.clip.id]?.state === 'failed').slice(0, RECENT),
  );
  const waitsForGame = $derived(!!app.status?.game && app.config?.cloud.upload === 'after_game');
  let busy = $state<number | null>(null);

  async function run(u: Upload, f: () => Promise<unknown>) {
    busy = u.id;
    try {
      await f();
    } catch (e) {
      toastError(e);
    } finally {
      busy = null;
    }
  }
  function label(u: Upload): string {
    if (u.state === 'uploading') {
      const pct = u.clip.size_bytes ? Math.min(100, Math.round((u.done_bytes / u.clip.size_bytes) * 100)) : 0;
      return `${pct}% of ${fmtBytes(u.clip.size_bytes)}`;
    }
    if (!u.now && app.config?.cloud.upload === 'manual') return 'Waiting';
    return !u.now && waitsForGame ? 'After the game' : 'Waiting';
  }
</script>

{#if active.length || failed.length}
  <section class="card">
    <h2>Cloud</h2>
    {#each active as u (u.id)}
      <div class="job">
        <a href={clipHref(u.clip.id)} class="t">{u.clip.title}</a>
        <span class="spacer"></span>
        <span class="small">{label(u)}</span>
        {#if u.state === 'queued' && !u.now}
          <button disabled={busy === u.id} onclick={() => run(u, () => uploadClip(u.clip.id))}>Upload now</button>
        {/if}
        <button disabled={busy === u.id} onclick={() => run(u, () => cancelUpload(u.id))}>Cancel</button>
      </div>
      {#if u.state === 'uploading'}
        <div class="bar"><i style:width="{u.clip.size_bytes ? Math.min(100, (u.done_bytes / u.clip.size_bytes) * 100) : 0}%"></i></div>
      {/if}
    {/each}
    {#each failed as u (u.id)}
      <div class="job">
        <a href={clipHref(u.clip.id)} class="t">{u.clip.title}</a>
        <span class="spacer"></span>
        <span class="small errt">Failed</span>
        <button disabled={busy === u.id} onclick={() => run(u, () => uploadClip(u.clip.id))}>Retry</button>
      </div>
      {#if u.error}<p class="small errt err">{u.error}</p>{/if}
    {/each}
  </section>
{/if}

<style>
  h2 { margin: 0 0 0.4rem; font-size: 1.1rem; }
  .job { display: flex; align-items: center; gap: 0.6rem; padding: 0.35rem 0; flex-wrap: wrap; }
  .t { font-weight: 600; text-decoration: none; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 22rem; }
  .spacer { flex: 1; }
  .errt { color: var(--danger); }
  .err { margin: 0 0 0.3rem; }
  .bar { height: 4px; border-radius: 2px; background: var(--border); overflow: hidden; margin-bottom: 0.3rem; }
  .bar i { display: block; height: 100%; background: var(--accent); transition: width 0.4s; }
</style>
