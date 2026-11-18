<script lang="ts">
  import { volumeOf } from './api';
  import { app, setSourceVolume, toastError } from './store.svelte';
  import type { Config } from './api';

  // A live volume fader for one audio source, like OBS: it applies while you drag (no restart, no Save) and
  // is saved right away. `onsaved` hears the saved settings, so a settings form can follow along.
  let {
    source,
    label,
    onsaved,
    vertical = false,
  }: { source: string; label: string; onsaved?: (c: Config) => void; vertical?: boolean } = $props();

  const saved = $derived(app.config ? volumeOf(app.config.audio, source) : 100);
  /** The value while dragging; the saved one otherwise. */
  let dragging = $state<number | null>(null);
  const value = $derived(dragging ?? saved);

  let timer: ReturnType<typeof setTimeout> | undefined;
  let sending = Promise.resolve();
  function send(v: number): void {
    // One request at a time, in order, so the last value wins.
    sending = sending.then(() =>
      setSourceVolume(source, v).then(
        (c) => onsaved?.(c),
        (e) => toastError(e),
      ),
    );
  }
  /** Which drag is going on, so a finished one doesn't reset a newer one. */
  let drag = 0;
  let active = false;
  function input(v: number): void {
    if (!active) {
      active = true;
      drag++;
    }
    dragging = v;
    if (timer === undefined) {
      timer = setTimeout(() => {
        timer = undefined;
        if (dragging !== null) send(dragging);
      }, 120);
    }
  }
  function change(v: number): void {
    clearTimeout(timer);
    timer = undefined;
    if (!active) drag++;
    active = false;
    dragging = v;
    const mine = drag;
    send(v);
    void sending.then(() => {
      if (mine === drag) dragging = null;
    });
  }
</script>

<div class="fader" class:vertical>
  <input
    type="range"
    min="0"
    max="200"
    step="5"
    {value}
    oninput={(e) => input(+e.currentTarget.value)}
    onchange={(e) => change(+e.currentTarget.value)}
    ondblclick={() => change(100)}
    aria-label="{label} volume"
    title="Double-click for 100 %"
  />
  <span class="v">{value}%</span>
</div>

<style>
  .fader { display: flex; align-items: center; gap: 0.6rem; min-width: 0; width: min(16rem, 100%); }
  input { flex: 1; min-width: 4rem; }
  /* Upright, like an OBS mixer strip: 0 % at the bottom, the value under it. */
  .fader.vertical { flex-direction: column; width: auto; height: 100%; gap: .3rem; }
  .vertical input { writing-mode: vertical-lr; direction: rtl; flex: 1; min-width: 0; min-height: 3rem; width: 1.2rem; margin: 0; }
  .vertical .v { width: auto; text-align: center; font-size: .72rem; }
  .v { width: 3rem; text-align: right; font-size: 0.8rem; color: var(--text-muted); font-variant-numeric: tabular-nums; }
</style>
