<script lang="ts">
  import { app } from './store.svelte';
  import { FLOOR, fmtDb, pct } from './meter';

  // A source's live level before and after its filters, so the effect of each filter can be seen while
  // tuning it. The server measures the saved settings: unsaved changes show once they are saved.
  let { source, label, dirty }: { source: string; label: string; dirty: boolean } = $props();

  const level = $derived(app.levels.find((l) => l.source === source));
  const rows = $derived(
    level
      ? [
          { name: 'Before', db: level.db, title: `${label} as the device delivers it` },
          { name: 'After', db: level.after, title: `${label} as it is recorded: after the filters, push-to-talk and volume` },
        ]
      : [],
  );
</script>

{#if level}
  <div class="meter" aria-label="{label} level before and after filters">
    {#each rows as r (r.name)}
      <div class="m" title={r.title}>
        <span class="name">{r.name}</span>
        <div class="bar" role="meter" aria-label="{label} {r.name.toLowerCase()} filters" aria-valuemin={FLOOR} aria-valuemax={0} aria-valuenow={Math.round(r.db)}>
          <div class="fill" class:hot={r.db > -12} class:clip={r.db > -3} style="width: {pct(r.db)}%"></div>
        </div>
        <span class="db">{fmtDb(r.db)}</span>
      </div>
    {/each}
    {#if dirty}
      <p class="muted small">Save to hear your changes: the After meter follows the saved filters.</p>
    {/if}
  </div>
{:else}
  <p class="muted small">While {label} is captured (for example while a game is on), a meter here shows its level before and after the filters.</p>
{/if}

<style>
  .meter { display: flex; flex-direction: column; gap: 0.35rem; }
  .m { display: grid; grid-template-columns: 4rem 1fr 4rem; align-items: center; gap: 0.6rem; }
  .name { font-size: 0.85rem; color: var(--text-muted); }
  .bar { height: 0.55rem; background: var(--surface-2); border: 1px solid var(--border); border-radius: 999px; overflow: hidden; }
  .fill { height: 100%; background: var(--accent); transition: width 90ms linear; }
  .fill.hot { background: #f5b942; }
  .fill.clip { background: var(--danger); }
  .db { text-align: right; font-size: 0.8rem; color: var(--text-muted); font-variant-numeric: tabular-nums; }
  p { margin: 0; }
</style>
