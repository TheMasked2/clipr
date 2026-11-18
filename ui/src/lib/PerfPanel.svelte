<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from './api';
  import Sparkline from './Sparkline.svelte';
  import { app } from './store.svelte';

  // Performance numbers, like OBS's stats dock: what the capture does right now, with the last two minutes as
  // a trend. Polled once a second while shown (and the tab is visible).
  let samples = $state<api.PerfSample[]>([]);
  let showTable = $state(false);

  onMount(() => {
    let stop = false;
    const poll = async () => {
      if (stop) return;
      if (document.visibilityState === 'visible') {
        try {
          samples = await api.getPerf();
        } catch {
          // Disconnected: keep the last numbers; the status card says so.
        }
      }
      if (!stop) setTimeout(poll, 1000);
    };
    void poll();
    return () => (stop = true);
  });

  const last = $derived(samples[samples.length - 1]);
  const times = $derived(samples.map((s) => s.at_ms));
  const capturing = $derived(last?.capturing ?? false);
  /** The average of the last few seconds, so a tile doesn't flicker. */
  const recent = (pick: (s: api.PerfSample) => number, n = 5) => {
    const r = samples.slice(-n);
    return r.length ? r.reduce((a, s) => a + pick(s), 0) / r.length : 0;
  };
  const lastMinute = $derived(samples.slice(-60));
  const late = $derived(lastMinute.reduce((a, s) => a + s.late, 0));
  const dropped = $derived(lastMinute.reduce((a, s) => a + s.dropped, 0));
  const target = $derived(app.config?.video.fps ?? 60);
  const freshPct = $derived.by(() => {
    const fps = recent((s) => s.fps);
    return fps > 0 ? (recent((s) => s.fresh) / fps) * 100 : 0;
  });

  const f1 = (v: number) => (v >= 100 ? Math.round(v).toLocaleString() : v.toFixed(1));
  const tiles = $derived([
    {
      label: 'Frame rate',
      value: capturing ? `${Math.round(recent((s) => s.fps))}` : '–',
      unit: capturing ? `of ${target} fps` : 'not capturing',
      series: samples.map((s) => s.fps),
      fmt: (v: number) => `${Math.round(v)} fps`,
      max: target,
      hint: 'Frames encoded per second. Below the target means clipr fell behind.',
    },
    {
      label: 'New pictures',
      value: capturing ? `${Math.round(freshPct)}%` : '–',
      unit: 'of frames',
      series: samples.map((s) => (s.fps > 0 ? (s.fresh / s.fps) * 100 : 0)),
      fmt: (v: number) => `${Math.round(v)}%`,
      max: 100,
      hint: 'Frames with a new picture from Windows. A still desktop repeats frames by design; in a game, a low number means the game delivers fewer frames than clipr records.',
    },
    {
      label: 'Frame time',
      value: capturing ? f1(recent((s) => s.frame_ms)) : '–',
      unit: `ms (budget ${(1000 / target).toFixed(1)})`,
      series: samples.map((s) => s.frame_ms),
      fmt: (v: number) => `${v.toFixed(2)} ms`,
      max: undefined,
      hint: "How long clipr's capture thread spends on a frame: scaling it and handing it to the GPU encoder. It has to stay well under the budget (1000 ms ÷ fps).",
    },
    {
      label: 'Bitrate',
      value: capturing ? (recent((s) => s.kbps) / 1000).toFixed(1) : '–',
      unit: 'Mbit/s',
      series: samples.map((s) => s.kbps / 1000),
      fmt: (v: number) => `${v.toFixed(1)} Mbit/s`,
      max: undefined,
      hint: 'Video data actually written. Still pictures need less than the setting.',
    },
    {
      label: 'CPU',
      value: f1(recent((s) => s.cpu)),
      unit: '% of this PC',
      series: samples.map((s) => s.cpu),
      fmt: (v: number) => `${v.toFixed(1)}%`,
      max: 5,
      hint: "clipr's share of all CPU cores. Encoding runs on the GPU, so this stays low.",
    },
    {
      label: 'Memory',
      value: last ? `${Math.round(last.mem_mb)}` : '–',
      unit: last && last.buffer_mb ? `MB (buffer ${Math.round(last.buffer_mb)} MB, ${Math.round(last.buffer_secs)} s)` : 'MB',
      series: samples.map((s) => s.mem_mb),
      fmt: (v: number) => `${Math.round(v)} MB`,
      max: undefined,
      hint: 'clipr in memory, most of it the replay buffer.',
    },
  ]);
  const ago = (t: number) => `${Math.max(0, Math.round((Date.now() - t) / 1000))} s ago`;
</script>

<div class="perf">
  <div class="tiles">
    {#each tiles as t (t.label)}
      <div class="tile" title={t.hint}>
        <span class="label">{t.label}</span>
        <span class="value">{t.value}<small>{t.unit}</small></span>
        <Sparkline values={t.series} {times} label={t.label} format={t.fmt} max={t.max} />
      </div>
    {/each}
  </div>
  <div class="foot">
    <span class="small" class:warnt={late + dropped > 0} class:muted={late + dropped === 0}>
      {#if !capturing}Not capturing: frame numbers show while the buffer or a recording runs.{:else if late + dropped > 0}Last minute: {late} late, {dropped} skipped frames. Try a lower resolution or frame rate, or GPU priority under Video.{:else}No late or skipped frames in the last minute.{/if}
    </span>
    <button class="link small" onclick={() => (showTable = !showTable)}>{showTable ? 'Hide numbers' : 'Show numbers'}</button>
  </div>
  {#if showTable}
    <div class="table">
      <table>
        <thead><tr><th>When</th><th>fps</th><th>New</th><th>Late</th><th>Skipped</th><th>Frame ms</th><th>Mbit/s</th><th>CPU %</th><th>MB</th></tr></thead>
        <tbody>
          {#each samples.slice(-15).reverse() as s (s.at_ms)}
            <tr><td>{ago(s.at_ms)}</td><td>{s.fps.toFixed(1)}</td><td>{s.fresh.toFixed(1)}</td><td>{s.late}</td><td>{s.dropped}</td><td>{s.frame_ms.toFixed(2)}</td><td>{(s.kbps / 1000).toFixed(1)}</td><td>{s.cpu.toFixed(1)}</td><td>{Math.round(s.mem_mb)}</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<style>
  .tiles { display: grid; grid-template-columns: repeat(auto-fill, minmax(10.5rem, 1fr)); gap: 0.8rem; }
  .tile { display: flex; flex-direction: column; padding: 0.7rem 0.8rem 0.5rem; border: 1px solid var(--border); border-radius: 0.6rem; background: var(--surface-2); min-width: 0; }
  .label { font-size: 0.8rem; color: var(--text-muted); }
  .value { font-size: 1.35rem; font-weight: 650; font-variant-numeric: tabular-nums; display: flex; align-items: baseline; gap: 0.35rem; flex-wrap: wrap; }
  .value small { font-size: 0.72rem; font-weight: 400; color: var(--text-muted); }
  .foot { display: flex; justify-content: space-between; gap: 0.8rem; align-items: baseline; margin-top: 0.7rem; flex-wrap: wrap; }
  .table { overflow-x: auto; margin-top: 0.6rem; }
  table { border-collapse: collapse; font-size: 0.78rem; font-variant-numeric: tabular-nums; width: 100%; }
  th, td { padding: 0.2rem 0.5rem; text-align: right; border-bottom: 1px solid var(--border); white-space: nowrap; }
  th:first-child, td:first-child { text-align: left; }
  th { color: var(--text-muted); font-weight: 500; }
  .warnt { color: #f5b942; }
</style>
