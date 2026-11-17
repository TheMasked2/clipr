<script lang="ts">
  import { answerPost, type Job } from './api';
  import { app, toastError } from './store.svelte';
  import { clipHref } from './router.svelte';

  // The Discord queue on Home: clips waiting for a yes, posts on their way, and the last few results.
  const RECENT = 5;
  const asks = $derived(app.posts.filter((j) => j.state === 'ask'));
  const active = $derived(app.posts.filter((j) => j.state === 'queued' || j.state === 'posting'));
  const recent = $derived(
    app.posts.filter((j) => j.state === 'posted' || j.state === 'failed' || j.state === 'skipped').slice(0, RECENT),
  );
  const inGame = $derived(!!app.status?.game && !app.config?.discord.post_during_game);
  let busy = $state<number | null>(null);

  async function answer(j: Job, yes: boolean) {
    busy = j.id;
    try {
      await answerPost(j.id, yes);
    } catch (e) {
      toastError(e);
    } finally {
      busy = null;
    }
  }
  const what: Record<Job['state'], string> = {
    ask: 'Waiting for you',
    queued: 'Waiting',
    posting: 'Posting…',
    posted: 'Posted',
    skipped: 'Skipped',
    failed: 'Failed',
  };
</script>

{#if app.posts.length}
  <section class="card">
    <h2>Discord</h2>
    {#if asks.length}
      <h3 class="sub">Post these?</h3>
      {#each asks as j (j.id)}
        <div class="job">
          <a href={clipHref(j.clip.id)} class="t">{j.clip.title}</a>
          <span class="muted small">{j.clip.game} → {j.webhook}</span>
          <span class="spacer"></span>
          <button class="primary" disabled={busy === j.id} onclick={() => answer(j, true)}>Post</button>
          <button disabled={busy === j.id} onclick={() => answer(j, false)}>Skip</button>
        </div>
      {/each}
    {/if}
    {#if active.length}
      <h3 class="sub">On the way</h3>
      {#each active as j (j.id)}
        <div class="job">
          <a href={clipHref(j.clip.id)} class="t">{j.clip.title}</a>
          <span class="muted small">→ {j.webhook}</span>
          <span class="spacer"></span>
          <span class="small">{j.state === 'queued' && inGame && !j.now ? 'After the game' : what[j.state]}</span>
          {#if j.state === 'queued'}<button disabled={busy === j.id} onclick={() => answer(j, false)}>Cancel</button>{/if}
        </div>
      {/each}
    {/if}
    {#if recent.length}
      <h3 class="sub">Recent</h3>
      {#each recent as j (j.id)}
        <div class="job">
          <a href={clipHref(j.clip.id)} class="t">{j.clip.title}</a>
          <span class="muted small">→ {j.webhook}</span>
          <span class="spacer"></span>
          <span class="small" class:okt={j.state === 'posted'} class:errt={j.state === 'failed'} title={j.error ?? ''}>{what[j.state]}</span>
          {#if j.state === 'failed'}<button disabled={busy === j.id} onclick={() => answer(j, true)}>Retry</button>{/if}
        </div>
        {#if j.state === 'failed' && j.error}<p class="small errt err">{j.error}</p>{/if}
      {/each}
    {/if}
  </section>
{/if}

<style>
  h2 { margin: 0 0 0.4rem; font-size: 1.1rem; }
  .sub { margin: 0.8rem 0 0.3rem; font-size: 0.85rem; color: var(--text-muted); font-weight: 600; }
  .job { display: flex; align-items: center; gap: 0.6rem; padding: 0.35rem 0; flex-wrap: wrap; }
  .t { font-weight: 600; text-decoration: none; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 22rem; }
  .spacer { flex: 1; }
  .okt { color: #4ade80; }
  .errt { color: var(--danger); }
  .err { margin: 0 0 0.3rem; }
</style>
