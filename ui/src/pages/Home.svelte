<script lang="ts">
  import { app } from '../lib/store.svelte';

  function fmt(secs: number): string {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    return h ? `${h}h ${m}m` : m ? `${m}m ${secs % 60}s` : `${secs}s`;
  }
  const s = $derived(app.status);
</script>

<h1>Status</h1>
<section class="card">
  {#if !s}
    <p class="muted">{app.connected ? 'Loading...' : 'Waiting for clipr...'}</p>
  {:else}
    <dl>
      <dt>State</dt><dd>{s.state}</dd>
      <dt>Game</dt><dd>{s.game ?? '-'}</dd>
      <dt>Uptime</dt><dd>{fmt(s.uptime_secs)}</dd>
      <dt>Version</dt><dd>{s.version}</dd>
    </dl>
  {/if}
</section>

<p class="hint muted">
  clipr lives in your system tray (the <strong>c</strong> icon near the clock, maybe under the ^ arrow).
  Click it to come back here; right-click for quick actions. Closing this tab doesn't stop clipr.
</p>

<style>
  dl {
    margin: 0;
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.5rem 1.25rem;
  }
  dt { color: var(--text-muted); }
  dd { margin: 0; overflow-wrap: anywhere; }
  .hint { margin-top: 1rem; font-size: 0.9rem; line-height: 1.5; }
</style>
