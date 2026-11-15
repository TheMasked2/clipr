<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '../lib/api';
  import { onServerEvent, toastError } from '../lib/store.svelte';
  import { dayLabel } from '../lib/format';
  import ClipCard from '../lib/ClipCard.svelte';

  let clips = $state<api.Clip[]>([]);
  let games = $state<api.GameInfo[]>([]);
  let shelves = $state<string[]>([]);
  let loaded = $state(false);
  let fresh = $state('');

  let q = $state('');
  let game = $state('');
  let shelf = $state('');
  let favorite = $state(false);
  let kind = $state<'' | api.ClipKind>('');

  async function refetch() {
    try {
      clips = await api.listClips({ q: q.trim(), game, shelf, favorite, kind: kind || undefined });
    } catch (e) {
      toastError(e);
    }
    loaded = true;
  }
  function meta() {
    api.getGames().then((g) => (games = g)).catch(() => {});
    api.getShelves().then((s) => (shelves = s)).catch(() => {});
  }

  // Refetch when a filter changes; the search box waits for a pause in typing.
  let first = true;
  $effect(() => {
    void [q, game, shelf, favorite, kind];
    if (first) {
      first = false;
      void refetch();
      return;
    }
    const t = setTimeout(refetch, 250);
    return () => clearTimeout(t);
  });

  onMount(() => {
    meta();
    return onServerEvent((e) => {
      if (e.type === 'clip_saved') {
        clips = [e.clip, ...clips.filter((c) => c.id !== e.clip.id)];
        fresh = e.clip.id;
        setTimeout(() => (fresh = ''), 3000);
        meta();
      } else if (e.type === 'library') {
        void refetch();
        meta();
      }
    });
  });

  interface Group {
    game: string;
    days: { label: string; clips: api.Clip[] }[];
  }
  const groups = $derived.by(() => {
    const out: Group[] = [];
    for (const c of clips) {
      let g = out.find((x) => x.game === c.game);
      if (!g) out.push((g = { game: c.game, days: [] }));
      const label = dayLabel(c.created);
      let d = g.days.find((x) => x.label === label);
      if (!d) g.days.push((d = { label, clips: [] }));
      d.clips.push(c);
    }
    return out;
  });
  const filtered = $derived(!!(q || game || shelf || favorite || kind));
  const upd = (u: api.Clip) => (clips = clips.map((x) => (x.id === u.id ? u : x)));
</script>

<h1>Gallery</h1>

<div class="filters">
  <input type="search" placeholder="Search clips…" bind:value={q} aria-label="Search" />
  <select bind:value={game} aria-label="Game">
    <option value="">All games</option>
    {#each games as g (g.name)}<option value={g.name}>{g.name} ({g.clips})</option>{/each}
  </select>
  <select bind:value={kind} aria-label="Kind">
    <option value="">Clips and recordings</option>
    <option value="clip">Clips</option>
    <option value="recording">Recordings</option>
  </select>
  <button class="chip" class:on={favorite} aria-pressed={favorite} onclick={() => (favorite = !favorite)}>
    ★ Favorites
  </button>
</div>
{#if shelves.length}
  <div class="chips">
    {#each shelves as s (s)}
      <button
        class="chip"
        class:on={shelf === s}
        aria-pressed={shelf === s}
        onclick={() => (shelf = shelf === s ? '' : s)}>{s}</button
      >
    {/each}
  </div>
{/if}

{#if !loaded}
  <p class="muted">Loading…</p>
{:else if !clips.length}
  <p class="muted">{filtered ? 'No clips match these filters.' : 'No clips yet.'}</p>
{/if}

{#each groups as g (g.game)}
  <section>
    <h2 class="game">{g.game}</h2>
    {#each g.days as d (d.label)}
      <h3 class="muted day">{d.label}</h3>
      <div class="grid">
        {#each d.clips as c (c.id)}
          <ClipCard clip={c} showGame={false} highlight={fresh === c.id} onupdate={upd} />
        {/each}
      </div>
    {/each}
  </section>
{/each}

<style>
  .filters { display: flex; flex-wrap: wrap; gap: 0.6rem; margin-bottom: 0.8rem; }
  .filters input { flex: 1 1 14rem; }
  .chips { display: flex; flex-wrap: wrap; gap: 0.4rem; margin-bottom: 1rem; }
  section { margin-top: 1.8rem; }
  .game { font-size: 1.25rem; margin-bottom: 0.2rem; }
  .day { margin: 0.9rem 0 0.5rem; font-weight: 500; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 1rem; }
  @media (max-width: 520px) {
    .grid { grid-template-columns: 1fr; }
    .filters select { flex: 1 1 8rem; }
  }
</style>
