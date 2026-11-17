<script lang="ts">
  import type { AudioFilter, Config, FilterKind } from './api';
  import Toggle from './Toggle.svelte';
  import FilterMeter from './FilterMeter.svelte';
  import { app } from './store.svelte';

  // One source's filter list, like OBS: nothing by default; add, tune, reorder, switch off or remove.
  let { audio, source, label }: { audio: Config['audio']; source: string; label: string } = $props();

  type Kind = FilterKind['kind'];
  interface Param {
    key: string;
    label: string;
    min: number;
    max: number;
    step: number;
    unit: string;
  }

  // What a new filter starts with; the same values as the server's FilterKind::defaults.
  const DEFAULTS: { [K in Kind]: Extract<FilterKind, { kind: K }> } = {
    noise_suppression: { kind: 'noise_suppression', strength: 100 },
    noise_gate: { kind: 'noise_gate', open_db: -32, close_db: -40, attack_ms: 10, hold_ms: 150, release_ms: 100 },
    compressor: { kind: 'compressor', threshold_db: -20, ratio: 3, attack_ms: 5, release_ms: 80, makeup_db: 4 },
    limiter: { kind: 'limiter', ceiling_db: -1, release_ms: 100 },
    gain: { kind: 'gain', db: 0 },
  };

  const KINDS: { kind: Kind; name: string; text: string; params: Param[] }[] = [
    {
      kind: 'noise_suppression',
      name: 'Noise suppression',
      text: 'Removes steady background noise: fans, hum, keyboard. Lower strength keeps more of the original sound.',
      params: [{ key: 'strength', label: 'Strength', min: 0, max: 100, step: 5, unit: '%' }],
    },
    {
      kind: 'noise_gate',
      name: 'Noise gate',
      text: 'Mutes the source while it is quiet. It opens above the open level and closes below the close level, after the hold time. Set the levels against the live meter above.',
      params: [
        { key: 'open_db', label: 'Open level', min: -96, max: 0, step: 1, unit: 'dB' },
        { key: 'close_db', label: 'Close level', min: -96, max: 0, step: 1, unit: 'dB' },
        { key: 'attack_ms', label: 'Attack', min: 1, max: 500, step: 1, unit: 'ms' },
        { key: 'hold_ms', label: 'Hold', min: 0, max: 5000, step: 10, unit: 'ms' },
        { key: 'release_ms', label: 'Release', min: 1, max: 5000, step: 10, unit: 'ms' },
      ],
    },
    {
      kind: 'compressor',
      name: 'Compressor',
      text: 'Evens out loud and quiet parts. Above the threshold, every "ratio" dB in comes out as 1 dB.',
      params: [
        { key: 'threshold_db', label: 'Threshold', min: -60, max: 0, step: 1, unit: 'dB' },
        { key: 'ratio', label: 'Ratio', min: 1, max: 20, step: 1, unit: ':1' },
        { key: 'attack_ms', label: 'Attack', min: 1, max: 500, step: 1, unit: 'ms' },
        { key: 'release_ms', label: 'Release', min: 1, max: 5000, step: 10, unit: 'ms' },
        { key: 'makeup_db', label: 'Output gain', min: -30, max: 30, step: 1, unit: 'dB' },
      ],
    },
    {
      kind: 'limiter',
      name: 'Limiter',
      text: 'Never lets a peak past the ceiling, so shouting or a loud explosion does not distort. Put it last, and keep the source volume at 100 % or lower: the volume applies after the filters.',
      params: [
        { key: 'ceiling_db', label: 'Ceiling', min: -30, max: 0, step: 1, unit: 'dB' },
        { key: 'release_ms', label: 'Release', min: 1, max: 5000, step: 10, unit: 'ms' },
      ],
    },
    {
      kind: 'gain',
      name: 'Gain',
      text: 'Turns the source up or down.',
      params: [{ key: 'db', label: 'Gain', min: -30, max: 30, step: 1, unit: 'dB' }],
    },
  ];
  const info = (k: Kind) => KINDS.find((x) => x.kind === k)!;

  const list = $derived(audio.filters[source] ?? []);
  // The meter measures the saved filters (the volume too, since it applies after them).
  const dirty = $derived.by(() => {
    const saved = app.config?.audio;
    if (!saved) return false;
    const vol = (a: Config['audio']) => (source === 'system' ? a.system_volume : source === 'mic' ? a.mic_volume : 100);
    return JSON.stringify(saved.filters[source] ?? []) !== JSON.stringify(list) || vol(saved) !== vol(audio);
  });
  let adding = $state<Kind | ''>('');

  function add() {
    if (!adding) return;
    // Create the list first, then push through the state proxy: pushing onto the plain array that `??=`
    // returns would be lost, because the stored copy is a separate proxy.
    if (!audio.filters[source]) audio.filters[source] = [];
    audio.filters[source].push({ enabled: true, ...structuredClone(DEFAULTS[adding]) } as AudioFilter);
    adding = '';
  }
  function move(i: number, by: number) {
    const l = audio.filters[source];
    const j = i + by;
    if (!l || j < 0 || j >= l.length) return;
    [l[i], l[j]] = [l[j], l[i]];
  }
  function remove(i: number) {
    audio.filters[source]?.splice(i, 1);
    // No empty list left behind: the form stays unchanged after adding and removing a filter.
    if (audio.filters[source]?.length === 0) delete audio.filters[source];
  }
  // Params are read and written by name; the kind decides which exist.
  const get = (f: AudioFilter, key: string) => (f as unknown as Record<string, number>)[key];
  const set = (f: AudioFilter, key: string, v: number) => ((f as unknown as Record<string, number>)[key] = v);
</script>

<div class="filters" aria-label="{label} filters">
  <FilterMeter {source} {label} {dirty} />
  {#each list as f, i (i)}
    {@const k = info(f.kind)}
    <div class="filter" class:off={!f.enabled}>
      <div class="head">
        <Toggle bind:checked={f.enabled} label="{k.name} on" />
        <strong>{k.name}</strong>
        <span class="grow"></span>
        <button class="icon" aria-label="Move {k.name} up" disabled={i === 0} onclick={() => move(i, -1)}>↑</button>
        <button class="icon" aria-label="Move {k.name} down" disabled={i === list.length - 1} onclick={() => move(i, 1)}>↓</button>
        <button class="icon" aria-label="Remove {k.name}" onclick={() => remove(i)}>×</button>
      </div>
      <p class="muted small">{k.text}</p>
      {#each k.params as p (p.key)}
        <label class="param">
          <span>{p.label}</span>
          <input
            type="range"
            min={p.min}
            max={p.max}
            step={p.step}
            value={get(f, p.key)}
            oninput={(e) => set(f, p.key, Number(e.currentTarget.value))}
            aria-label="{k.name} {p.label}"
          />
          <span class="val">{get(f, p.key)} {p.unit}</span>
        </label>
      {/each}
      {#if f.kind === 'noise_gate' && f.close_db > f.open_db}
        <p class="warn small">The close level is above the open level; the gate uses the open level for both.</p>
      {/if}
    </div>
  {:else}
    <p class="muted small">No filters: {label} is recorded as it is.</p>
  {/each}
  <div class="row">
    <select bind:value={adding} aria-label="Filter to add to {label}">
      <option value="">Add a filter…</option>
      {#each KINDS as k (k.kind)}<option value={k.kind}>{k.name}</option>{/each}
    </select>
    <button onclick={add} disabled={!adding}>Add</button>
  </div>
</div>

<style>
  .filters { display: flex; flex-direction: column; gap: 0.6rem; width: 100%; }
  .filter { border: 1px solid var(--border); border-radius: 0.6rem; padding: 0.6rem 0.8rem; display: flex; flex-direction: column; gap: 0.4rem; background: var(--surface-2); }
  .filter.off { opacity: 0.6; }
  .head { display: flex; align-items: center; gap: 0.5rem; }
  .grow { flex: 1; }
  .icon { padding: 0.1rem 0.5rem; }
  .param { display: grid; grid-template-columns: 7rem 1fr 5rem; align-items: center; gap: 0.6rem; font-size: 0.9rem; }
  .val { text-align: right; font-variant-numeric: tabular-nums; color: var(--text-muted); font-size: 0.85rem; }
  .warn { color: var(--danger); }
  p { margin: 0; }
  @media (max-width: 520px) {
    .param { grid-template-columns: 1fr 4.5rem; }
    .param input { grid-column: 1 / -1; grid-row: 2; }
  }
</style>
