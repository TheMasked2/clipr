<script lang="ts">
  import * as api from './api';
  import Fader from './Fader.svelte';
  import { channels, FLOOR, fmtDb, pct } from './meter';
  import { app, toastError, watchLevelsWhileShown } from './store.svelte';

  // The audio mixer, like OBS's: each source's live level (what gets recorded: after its filters,
  // push-to-talk and volume) with a fader that applies while you drag. It runs while shown, even with
  // nothing being captured, so levels can be set before recording.
  let { onsaved }: { onsaved?: (c: api.Config) => void } = $props();

  $effect(() => watchLevelsWhileShown());

  /** CLIP looks at the raw input: a peak this close to 0 dB means it clipped before clipr got it. */
  const CLIP_DB = -0.5;
  /** How long CLIP stays lit after the last clipped peak, so a short one is still seen. */
  const CLIP_HOLD_MS = 2000;

  // Updated on every levels message (10 a second), which also lets the badge expire.
  let clipping = $state<Set<string>>(new Set());
  const lastClip = new Map<string, number>();
  $effect(() => {
    const now = Date.now();
    const on = new Set<string>();
    for (const l of app.levels) {
      if (l.db >= CLIP_DB) lastClip.set(l.source, now);
      const at = lastClip.get(l.source);
      if (at !== undefined && now - at < CLIP_HOLD_MS) on.add(l.source);
    }
    clipping = on;
  });

  const audio = $derived(app.config?.audio);
  const anySource = $derived(!!audio && (audio.system || audio.mic || audio.game || audio.apps.length > 0));
  const muted = (source: string) => api.isMuted(app.status, source);
  function toggle(source: string) {
    const m = muted(source);
    (source === 'mic' ? api.setMicMuted(!m) : api.setMuted(source, !m)).catch(toastError);
  }
</script>

<div class="mixer" aria-label="Audio mixer">
  {#each app.levels as l (l.source)}
    <div class="row">
      <span class="name" title={l.name}>{l.name}</span>
      <div class="bar" role="meter" aria-label="{l.name} level" aria-valuemin={FLOOR} aria-valuemax={0} aria-valuenow={Math.round(muted(l.source) ? l.db : l.after)} aria-valuetext={muted(l.source) ? `Muted in clips, coming in at ${fmtDb(l.db)}` : fmtDb(l.after)} title="{muted(l.source) ? 'Muted in clips. Coming in' : 'Before filters'}: {fmtDb(l.db)}, recorded: {fmtDb(l.after)}">
        <!-- Muted, it shows what comes in, greyed out, like the Home mixer. -->
        {#each muted(l.source) ? channels(l.db_lr, l.db) : channels(l.after_lr, l.after) as v, i (i)}
          <div class="fill" class:hot={v > -12} class:clip={v > -3} class:muted={muted(l.source)} style="width: {pct(v)}%"></div>
        {/each}
      </div>
      {#if clipping.has(l.source)}
        <span class="badge" title="This source reached full scale: its sound is distorting before clipr records it. Lower its level (for a mic: Windows Sound settings → input volume or boost).">CLIP</span>
      {:else}
        <span class="db">{fmtDb(l.after)}</span>
      {/if}
      <Fader source={l.source} label={l.name} {onsaved} />
      <button class="mute small" class:on={muted(l.source)} aria-pressed={muted(l.source)} aria-label="Mute {l.name}" onclick={() => toggle(l.source)} title={l.source === 'mic' ? 'Mute the microphone in clips (the mute hotkey does the same)' : `Mute ${l.name} in clips (you still hear it)`}>{muted(l.source) ? 'Muted' : 'Mute'}</button>
    </div>
  {:else}
    <p class="muted small">
      {#if !app.config}Loading…{:else if anySource}Starting the meters…{:else}No audio source is on. Turn one on under Settings → Audio.{/if}
    </p>
  {/each}
</div>

<style>
  .mixer { display: flex; flex-direction: column; gap: 0.55rem; width: 100%; container-type: inline-size; }
  .row { display: grid; grid-template-columns: 6.5rem minmax(4rem, 1fr) 3.4rem minmax(8rem, 12rem) 4.2rem; align-items: center; gap: 0.7rem; }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 0.9rem; }
  /* Left over right, like OBS. */
  .bar { height: 0.7rem; background: var(--surface-2); border: 1px solid var(--border); border-radius: 4px; overflow: hidden; display: flex; flex-direction: column; gap: 1px; }
  .fill { flex: 1; background: var(--accent); transition: width 90ms linear; }
  .fill.hot { background: #f5b942; }
  .fill.clip { background: var(--danger); }
  .fill.muted { filter: grayscale(1); opacity: 0.45; }
  .db { text-align: right; font-size: 0.8rem; color: var(--text-muted); font-variant-numeric: tabular-nums; }
  .badge { justify-self: end; padding: 0.05rem 0.4rem; border-radius: 0.3rem; background: var(--danger); color: #fff; font-size: 0.72rem; font-weight: 700; letter-spacing: 0.04em; }
  .mute { padding: 0.2rem 0.5rem; }
  .mute.on { border-color: var(--danger); color: var(--danger); }
  p { margin: 0; }
  /* Narrow (Home's side column, a phone): the fader gets a line of its own under the meter. */
  @container (max-width: 34rem) {
    .row { grid-template-columns: 5.5rem minmax(3rem, 1fr) 3.2rem 4.2rem; row-gap: 0.25rem; }
    .row > :global(.fader) { grid-column: 1 / -1; grid-row: 2; width: 100%; }
  }
</style>
