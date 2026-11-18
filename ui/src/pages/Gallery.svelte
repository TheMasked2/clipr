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
        // With filters on, the new clip may not match them: ask the server.
        if (filtered) void refetch();
        else clips = [e.clip, ...clips.filter((c) => c.id !== e.clip.id)];
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
  const total = $derived(games.reduce((a, g) => a + g.clips, 0));
  function clear(): void {
    q = '';
    game = '';
    shelf = '';
    favorite = false;
    kind = '';
  }
  const kinds: { v: '' | api.ClipKind; t: string }[] = [
    { v: '', t: 'Everything' },
    { v: 'clip', t: 'Clips' },
    { v: 'recording', t: 'Recordings' },
  ];
  const upd = (u: api.Clip) => (clips = clips.map((x) => (x.id === u.id ? u : x)));
</script>

<h1>Gallery</h1>

<div class="layout">
<!-- Filters in a column, like Settings' sections; on a phone they fold into a row above the clips. -->
<aside class="side" aria-label="Filters">
  <input type="search" placeholder="Search clips…" bind:value={q} aria-label="Search" />
  <div class="group">
    <span class="gh">Show</span>
    {#each kinds as k (k.v)}
      <button class="item" class:active={kind === k.v} aria-pressed={kind === k.v} onclick={() => (kind = k.v)}>{k.t}</button>
    {/each}
    <button class="item" class:active={favorite} aria-pressed={favorite} onclick={() => (favorite = !favorite)}>★ Favorites</button>
  </div>
  <div class="group games">
    <span class="gh">Games</span>
    <button class="item" class:active={game === ''} aria-pressed={game === ''} onclick={() => (game = '')}><span>All games</span>{#if total}<small>{total}</small>{/if}</button>
    {#each games as g (g.name)}
      <button class="item" class:active={game === g.name} aria-pressed={game === g.name} onclick={() => (game = game === g.name ? '' : g.name)} title={g.name}><span>{g.name}</span><small>{g.clips}</small></button>
    {/each}
  </div>
  {#if shelves.length}
    <div class="group">
      <span class="gh">Shelves</span>
      {#each shelves as s (s)}
        <button class="item" class:active={shelf === s} aria-pressed={shelf === s} onclick={() => (shelf = shelf === s ? '' : s)}><span>{s}</span></button>
      {/each}
    </div>
  {/if}
  {#if filtered}<button class="link small clear" onclick={clear}>Clear filters</button>{/if}
</aside>

<!-- On a phone: the same filters as two selects and chips. -->
<div class="compact">
  <select bind:value={game} aria-label="Game">
    <option value="">All games</option>
    {#each games as g (g.name)}<option value={g.name}>{g.name} ({g.clips})</option>{/each}
  </select>
  <select bind:value={kind} aria-label="Kind">
    {#each kinds as k (k.v)}<option value={k.v}>{k.t}</option>{/each}
  </select>
  <button class="chip" class:on={favorite} aria-pressed={favorite} onclick={() => (favorite = !favorite)}>★ Favorites</button>
  {#each shelves as s (s)}
    <button class="chip" class:on={shelf === s} aria-pressed={shelf === s} onclick={() => (shelf = shelf === s ? '' : s)}>{s}</button>
  {/each}
  {#if filtered}<button class="link small" onclick={clear}>Clear filters</button>{/if}
</div>

<div class="clips">
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
</div>
</div>

<style>
  .layout { display: grid; grid-template-columns: 13rem minmax(0, 1fr); gap: 1.4rem; align-items: start; }
  .side { display: flex; flex-direction: column; gap: 1rem; position: sticky; top: 4.2rem; max-height: calc(100vh - 5rem); overflow-y: auto; padding-right: 0.2rem; }
  .side input { width: 100%; }
  .group { display: flex; flex-direction: column; gap: 0.15rem; }
  .gh { font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-muted); margin: 0 0 0.25rem 0.6rem; }
  .item { display: flex; justify-content: space-between; align-items: baseline; gap: 0.5rem; background: transparent; border-color: transparent; color: var(--text-muted); text-align: left; padding: 0.35rem 0.6rem; }
  .item span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .item small { font-variant-numeric: tabular-nums; opacity: 0.8; }
  .item:hover { color: var(--text); }
  .item.active { color: var(--text); background: var(--accent-soft); border-color: var(--accent-border); }
  .clear { align-self: flex-start; margin-left: 0.6rem; }
  .compact { display: none; }
  .clips > section:first-child { margin-top: 0; }
  section { margin-top: 1.8rem; }
  .game { font-size: 1.25rem; margin-bottom: 0.2rem; }
  .day { margin: 0.9rem 0 0.5rem; font-weight: 500; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 1rem; }
  @media (max-width: 760px) {
    .layout { display: block; }
    .side { position: static; max-height: none; overflow: visible; margin-bottom: 0.6rem; padding: 0; }
    .side .group, .side .clear { display: none; }
    .compact { display: flex; flex-wrap: wrap; gap: 0.5rem; margin-bottom: 1rem; }
    .compact select { flex: 1 1 8rem; }
  }
  @media (max-width: 520px) {
    .grid { grid-template-columns: 1fr; }
  }
</style>
