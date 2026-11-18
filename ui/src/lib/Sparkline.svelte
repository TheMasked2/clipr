<script lang="ts">
  // A small trend line for a stat tile: the last two minutes, one point a second, with a crosshair and a
  // tooltip on hover. One series, so no legend: the tile's label names it.
  let {
    values,
    times,
    label,
    format,
    max,
  }: { values: number[]; times: number[]; label: string; format: (v: number) => string; max?: number } = $props();

  const W = 160;
  const H = 36;
  const PAD = 3;
  const top = $derived(Math.max(max ?? 0, ...values, 1e-9) * 1.1);
  const x = (i: number) => (values.length < 2 ? W : (i / (values.length - 1)) * W);
  const y = (v: number) => H - PAD - (Math.max(0, v) / top) * (H - 2 * PAD);
  const path = $derived(values.map((v, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(''));
  const area = $derived(values.length ? `${path}L${x(values.length - 1)},${H}L0,${H}Z` : '');

  let hover = $state<number | null>(null);
  let box: SVGSVGElement | undefined = $state();
  function move(e: PointerEvent): void {
    if (!box || values.length === 0) return;
    const r = box.getBoundingClientRect();
    const f = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
    hover = Math.round(f * (values.length - 1));
  }
  const ago = (t: number) => {
    const s = Math.round((Date.now() - t) / 1000);
    return s <= 1 ? 'now' : `${s} s ago`;
  };
</script>

<div class="spark">
  <svg
    bind:this={box}
    viewBox="0 0 {W} {H}"
    preserveAspectRatio="none"
    role="img"
    aria-label="{label}, last {values.length} seconds{values.length ? `, now ${format(values[values.length - 1])}` : ''}"
    onpointermove={move}
    onpointerleave={() => (hover = null)}
  >
    {#if values.length}
      <path d={area} class="area" />
      <path d={path} class="line" vector-effect="non-scaling-stroke" />
      {#if hover !== null}
        <line x1={x(hover)} x2={x(hover)} y1="0" y2={H} class="cross" vector-effect="non-scaling-stroke" />
      {/if}
    {/if}
  </svg>
  {#if values.length}
    <span class="dot" style="left: 100%; top: {(y(values[values.length - 1]) / H) * 100}%"></span>
  {/if}
  {#if hover !== null && values[hover] !== undefined}
    <span class="dot hov" style="left: {(x(hover) / W) * 100}%; top: {(y(values[hover]) / H) * 100}%"></span>
    <div class="tip" class:left={hover > values.length / 2} style="left: {(x(hover) / W) * 100}%">
      <b>{format(values[hover])}</b><span>{ago(times[hover])}</span>
    </div>
  {/if}
</div>

<style>
  .spark { position: relative; height: 36px; margin-top: 0.4rem; }
  svg { width: 100%; height: 100%; display: block; overflow: visible; cursor: crosshair; }
  .line { fill: none; stroke: var(--accent); stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }
  .area { fill: var(--accent); opacity: 0.12; }
  .cross { stroke: var(--text-muted); stroke-width: 1; }
  .dot { position: absolute; width: 8px; height: 8px; margin: -4px 0 0 -4px; border-radius: 50%; background: var(--accent); box-shadow: 0 0 0 2px var(--surface); pointer-events: none; }
  .tip { position: absolute; bottom: calc(100% + 6px); transform: translateX(-10%); display: flex; gap: 0.4rem; align-items: baseline; white-space: nowrap; padding: 0.2rem 0.45rem; border-radius: 0.35rem; background: var(--surface-2); border: 1px solid var(--border); font-size: 0.75rem; color: var(--text); pointer-events: none; z-index: 2; }
  .tip.left { transform: translateX(-90%); }
  .tip span { color: var(--text-muted); }
</style>
