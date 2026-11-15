<script lang="ts">
  import type { Config } from './api';
  import Field from './Field.svelte';

  let { video = $bindable() }: { video: Config['video'] } = $props();

  const presets = [
    { name: 'Low', kbps: 8000 },
    { name: 'Medium', kbps: 15000 },
    { name: 'High', kbps: 25000 },
    { name: 'Ultra', kbps: 40000 },
  ];
  const heights = [
    { v: 720, t: '720p' },
    { v: 1080, t: '1080p' },
    { v: 1440, t: '1440p' },
    { v: 0, t: 'Native (monitor resolution)' },
  ];

  const exact = $derived(presets.findIndex((p) => p.kbps === video.bitrate_kbps));
  const nearest = $derived(
    exact >= 0
      ? exact
      : presets.reduce((b, p, i) => (Math.abs(p.kbps - video.bitrate_kbps) < Math.abs(presets[b].kbps - video.bitrate_kbps) ? i : b), 0),
  );
  const heightKnown = $derived(heights.some((h) => h.v === video.height));
</script>

<Field title="Quality" hint="Higher looks better and makes bigger files. Medium is a good start." stacked>
  <div class="slider">
    <input type="range" min="0" max="3" step="1" value={nearest} aria-label="Quality preset" oninput={(e) => (video.bitrate_kbps = presets[+e.currentTarget.value].kbps)} />
    <div class="labels">
      {#each presets as p, i (p.name)}<span class:on={exact === i}>{p.name}<small>{p.kbps / 1000} Mbit/s</small></span>{/each}
    </div>
    {#if exact < 0}<div class="muted small">Custom bitrate ({video.bitrate_kbps} kbps) set in Advanced; moving the slider replaces it.</div>{/if}
  </div>
</Field>
<Field title="Resolution" hint="Output height; the width follows your monitor's shape. Lower is lighter on the GPU and the file size.">
  <select bind:value={video.height}>
    {#each heights as h (h.v)}<option value={h.v}>{h.t}</option>{/each}
    {#if !heightKnown}<option value={video.height}>{video.height}p (custom)</option>{/if}
  </select>
</Field>
<Field title="Frame rate" hint="60 fps looks smoother for fast games; 30 fps halves the work.">
  <div class="row">
    {#each [30, 60] as f (f)}<button type="button" class="chip" class:on={video.fps === f} onclick={() => (video.fps = f)}>{f} fps</button>{/each}
  </div>
</Field>

<style>
  .slider { width: 100%; }
  .labels { display: flex; justify-content: space-between; margin-top: .3rem; font-size: .85rem; color: var(--text-muted); }
  .labels span { display: flex; flex-direction: column; align-items: center; }
  .labels small { font-size: .72rem; }
  .labels .on { color: var(--accent); font-weight: 600; }
</style>
