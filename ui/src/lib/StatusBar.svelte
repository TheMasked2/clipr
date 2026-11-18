<script lang="ts">
  import type * as api from './api';
  import { app } from './store.svelte';
  import { fmtBytes } from './format';
  import { clipHref } from './router.svelte';
  import PerfPanel from './PerfPanel.svelte';

  // The few numbers that matter while playing, in one line along the bottom like OBS's status bar. The full
  // graphs open from Graphs.
  let {
    samples,
    disk,
    latest,
    fresh = false,
  }: { samples: api.PerfSample[]; disk: api.Disk | null; latest: api.Clip | null; fresh?: boolean } = $props();

  const last = $derived(samples[samples.length - 1]);
  const capturing = $derived(last?.capturing ?? false);
  const target = $derived(app.config?.video.fps ?? 60);
  const avg = (pick: (s: api.PerfSample) => number, n = 5) => {
    const r = samples.slice(-n);
    return r.length ? r.reduce((a, s) => a + pick(s), 0) / r.length : 0;
  };
  const fps = $derived(avg((s) => s.fps));
  const lastMinute = $derived(samples.slice(-60));
  const lost = $derived(lastMinute.reduce((a, s) => a + s.late + s.dropped, 0));
  const cpu = $derived(avg((s) => s.cpu));
  /** Under 10 GB free, the cleanup's free-space limit is near and long recordings may stop. */
  const lowDisk = $derived(!!disk && disk.free_bytes < 10 * 1024 ** 3);

  let dlg: HTMLDialogElement | undefined = $state();
  let graphs = $state(false);
  $effect(() => {
    if (!dlg) return;
    if (graphs && !dlg.open) dlg.showModal();
    if (!graphs && dlg.open) dlg.close();
  });
</script>

<footer class="status" aria-label="Status">
  <span title="Frames encoded per second (average of the last 5 s), against the target">
    <span class="k">FPS</span>
    <span class:warnt={capturing && fps < target * 0.9}>{capturing ? `${Math.round(fps)} / ${target}` : '–'}</span>
  </span>
  <span title="Late and skipped frames in the last minute">
    <span class="k">Dropped</span>
    <span class:warnt={lost > 0}>{capturing ? lost : '–'}</span>
  </span>
  <span title="clipr's share of all CPU cores">
    <span class="k">CPU</span> {last ? `${cpu.toFixed(1)}%` : '–'}
  </span>
  {#if disk}
    <span title="Free space on the clips drive · clips use {fmtBytes(disk.clips_bytes)}">
      <span class="k">Disk</span> <span class:warnt={lowDisk}>{fmtBytes(disk.free_bytes)} free</span>
    </span>
  {/if}
  <span class="grow"></span>
  {#if latest}
    <a class="last" class:fresh href={clipHref(latest.id)} title="{latest.game} · open the clip">
      <span class="k">{fresh ? 'Just saved' : 'Last clip'}</span> <span class="t">{latest.title}</span>
    </a>
  {/if}
  <button class="link small" onclick={() => (graphs = true)}>Graphs ▸</button>
</footer>

<dialog
  bind:this={dlg}
  aria-label="Performance"
  oncancel={(e) => {
    e.preventDefault();
    graphs = false;
  }}
>
  {#if graphs}
    <header><h2>Performance</h2><button class="x" onclick={() => (graphs = false)} aria-label="Close">×</button></header>
    <div class="content"><PerfPanel /></div>
  {/if}
</dialog>

<style>
  .status {
    display: flex; align-items: center; flex-wrap: wrap; gap: .2rem 1.2rem; padding: .35rem .8rem;
    background: var(--surface-2); border: 1px solid var(--border); border-radius: var(--radius);
    font-size: .8rem; font-variant-numeric: tabular-nums;
  }
  .k { color: var(--text-muted); margin-right: .2rem; }
  .warnt { color: #f5b942; }
  .grow { flex: 1; }
  .last { text-decoration: none; display: flex; min-width: 0; max-width: 22rem; gap: .2rem; }
  .last .t { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .last.fresh .k { color: var(--accent); }
  .last:hover .t { text-decoration: underline; }
  dialog {
    background: var(--surface); color: var(--text); border: 1px solid var(--border); border-radius: 14px;
    padding: 0; width: min(56rem, calc(100vw - 2rem)); max-height: calc(100dvh - 3rem);
  }
  dialog::backdrop { background: #000a; }
  header { display: flex; align-items: center; justify-content: space-between; padding: .8rem 1.2rem; border-bottom: 1px solid var(--border); }
  h2 { margin: 0; }
  .x { background: none; border: none; font-size: 1.4rem; line-height: 1; padding: 0 .3rem; color: var(--text-muted); }
  .content { padding: 1rem 1.2rem; overflow: auto; }
</style>
