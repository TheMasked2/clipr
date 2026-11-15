<script lang="ts">
  import { app } from './store.svelte';

  const FLOOR = -60;
  const pct = (db: number) => Math.round((Math.min(0, Math.max(FLOOR, db)) - FLOOR) * (100 / -FLOOR));
</script>

<div class="meters" aria-label="Audio levels">
  {#each app.levels as l (l.name)}
    <div class="m">
      <span class="name">{l.name}</span>
      <div class="bar" role="meter" aria-label={l.name} aria-valuemin={FLOOR} aria-valuemax={0} aria-valuenow={Math.round(l.db)}>
        <div class="fill" class:hot={l.db > -12} class:clip={l.db > -3} style="width: {pct(l.db)}%"></div>
      </div>
      <span class="db">{l.db <= FLOOR ? '-∞' : Math.round(l.db)} dB</span>
    </div>
  {:else}
    <p class="muted small">No audio is being captured right now. Levels show while the buffer runs (for example while a game is on).</p>
  {/each}
</div>

<style>
  .meters { display: flex; flex-direction: column; gap: 0.5rem; width: 100%; }
  .m { display: grid; grid-template-columns: 7rem 1fr 4rem; align-items: center; gap: 0.7rem; }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 0.9rem; }
  .bar { height: 0.6rem; background: var(--surface-2); border: 1px solid var(--border); border-radius: 999px; overflow: hidden; }
  .fill { height: 100%; background: var(--accent); transition: width 90ms linear; }
  .fill.hot { background: #f5b942; }
  .fill.clip { background: var(--danger); }
  .db { text-align: right; font-size: 0.8rem; color: var(--text-muted); font-variant-numeric: tabular-nums; }
  p { margin: 0; }
</style>
