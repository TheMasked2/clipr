<script lang="ts">
  import { shareClip } from './api';
  import { app, toast, toastError } from './store.svelte';

  // "Share to Discord" on a clip's page: pick a channel, post it now (game or not).
  let { id }: { id: string } = $props();

  const hooks = $derived(app.config?.discord.webhooks ?? []);
  let pick = $state('');
  let busy = $state(false);
  const target = $derived(hooks.some((w) => w.name === pick) ? pick : (hooks[0]?.name ?? ''));
  const mine = $derived(app.posts.filter((j) => j.clip.id === id).slice(0, 3));

  async function share() {
    busy = true;
    try {
      await shareClip(id, target);
      toast('ok', `Posting to ${target}…`);
    } catch (e) {
      toastError(e);
    } finally {
      busy = false;
    }
  }
</script>

<div>
  <h3>Share to Discord</h3>
  {#if hooks.length}
    <div class="row">
      {#if hooks.length > 1}
        <select value={target} onchange={(e) => (pick = e.currentTarget.value)} aria-label="Discord channel">
          {#each hooks as w (w.name)}<option value={w.name}>{w.name}</option>{/each}
        </select>
      {/if}
      <button onclick={share} disabled={busy || !target}>{busy ? 'Sharing…' : `Post to ${hooks.length > 1 ? 'Discord' : target}`}</button>
    </div>
    {#each mine as j (j.id)}
      <p class="muted small">{j.webhook}: {j.state}{j.error ? ` (${j.error})` : ''}</p>
    {/each}
  {:else}
    <p class="muted small">Add a Discord channel in <a href="#/settings">Settings → Discord</a> to post clips there.</p>
  {/if}
</div>

<style>
  p { margin: 0.4rem 0 0; }
</style>
