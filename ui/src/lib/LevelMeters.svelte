<script lang="ts">
  import { app } from './store.svelte';
  import { FLOOR, fmtDb, pct } from './meter';

  /** The meters show the raw input, so a peak this close to 0 dB means it clipped before clipr got it. */
  const CLIP_DB = -0.5;
  /** How long CLIP stays lit after the last clipped peak, so a short one is still seen. */
  const CLIP_HOLD_MS = 2000;

  // Updated on every levels message (10 a second), which also lets the badge expire. When levels stop the
  // store clears them, and the meters (with their badges) go away.
  let clipping = $state<Set<string>>(new Set());
  const lastClip = new Map<string, number>();
  $effect(() => {
    const now = Date.now();
    const on = new Set<string>();
    for (const l of app.levels) {
      if (l.db >= CLIP_DB) lastClip.set(l.name, now);
      const at = lastClip.get(l.name);
      if (at !== undefined && now - at < CLIP_HOLD_MS) on.add(l.name);
    }
    clipping = on;
  });
</script>

<div class="meters" aria-label="Audio levels">
  {#each app.levels as l (l.name)}
    <div class="m">
      <span class="name">{l.name}</span>
      <div class="bar" role="meter" aria-label={l.name} aria-valuemin={FLOOR} aria-valuemax={0} aria-valuenow={Math.round(l.db)}>
        <div class="fill" class:hot={l.db > -12} class:clip={l.db > -3} style="width: {pct(l.db)}%"></div>
      </div>
      {#if clipping.has(l.name)}
        <span class="badge" title="This source reached full scale: its sound is distorting before clipr records it. Lower its level (for a mic: Windows Sound settings → input volume or boost).">CLIP</span>
      {:else}
        <span class="db">{fmtDb(l.db)}</span>
      {/if}
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
  .badge { justify-self: end; padding: 0.05rem 0.4rem; border-radius: 0.3rem; background: var(--danger); color: #fff; font-size: 0.72rem; font-weight: 700; letter-spacing: 0.04em; }
  p { margin: 0; }
</style>
