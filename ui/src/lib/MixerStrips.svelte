<script lang="ts">
  import * as api from './api';
  import Fader from './Fader.svelte';
  import { FLOOR, fmtDb, pct } from './meter';
  import { app, toastError, watchLevelsWhileShown } from './store.svelte';

  // The audio mixer as upright strips, like OBS's: per source its live level (what gets recorded: after its
  // filters, push-to-talk and volume) beside a fader that applies while you drag. Strips sit side by side, so
  // levels compare at a glance. It runs while shown, even with nothing being captured.
  $effect(() => watchLevelsWhileShown());

  /** CLIP looks at the raw input: a peak this close to 0 dB means it clipped before clipr got it. */
  const CLIP_DB = -0.5;
  const CLIP_HOLD_MS = 2000;
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
  const micMuted = $derived(app.status?.mic_muted ?? false);
</script>

<div class="strips" aria-label="Audio mixer">
  {#each app.levels as l (l.source)}
    <div class="strip">
      <span class="name" title={l.name}>{l.name}</span>
      {#if clipping.has(l.source)}
        <span class="badge" title="This source reached full scale: its sound is distorting before clipr records it. Lower its level (for a mic: Windows Sound settings → input volume or boost).">CLIP</span>
      {:else}
        <span class="db">{fmtDb(l.after)}</span>
      {/if}
      <div class="ctl">
        <div class="bar" role="meter" aria-label="{l.name} level" aria-valuemin={FLOOR} aria-valuemax={0} aria-valuenow={Math.round(l.after)} title="Before filters: {fmtDb(l.db)}, recorded: {fmtDb(l.after)}">
          <div class="fill" class:hot={l.after > -12} class:clip={l.after > -3} class:muted={l.source === 'mic' && micMuted} style="height: {pct(l.after)}%"></div>
        </div>
        <Fader source={l.source} label={l.name} vertical />
      </div>
      {#if l.source === 'mic'}
        <button class="mute" class:on={micMuted} onclick={() => api.setMicMuted(!micMuted).catch(toastError)} title="Mute the microphone in clips (the mute hotkey does the same)">{micMuted ? 'Muted' : 'Mute'}</button>
      {:else}
        <span class="mute-gap"></span>
      {/if}
    </div>
  {:else}
    <p class="muted small empty">
      {#if !app.config}Loading…{:else if anySource}Starting the meters…{:else}No audio source is on. Turn one on in Sources.{/if}
    </p>
  {/each}
</div>

<style>
  .strips { display: flex; height: 100%; min-height: 11rem; overflow-x: auto; }
  .strip {
    flex: 1 0 4.6rem; max-width: 7rem; display: flex; flex-direction: column; align-items: center; gap: .2rem;
    padding: .45rem .3rem; border-right: 1px solid var(--border); min-height: 0;
  }
  .strip:last-child { border-right: 0; }
  .name { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: .78rem; font-weight: 600; }
  .db { font-size: .7rem; color: var(--text-muted); font-variant-numeric: tabular-nums; }
  .badge { padding: 0 .3rem; border-radius: .25rem; background: var(--danger); color: #fff; font-size: .66rem; font-weight: 700; letter-spacing: .04em; }
  .ctl { flex: 1; min-height: 0; display: flex; gap: .45rem; align-items: stretch; padding: .2rem 0; }
  .bar { width: .45rem; background: var(--surface-2); border: 1px solid var(--border); border-radius: 999px; overflow: hidden; display: flex; flex-direction: column-reverse; }
  .fill { width: 100%; background: var(--accent); transition: height 90ms linear; }
  .fill.hot { background: #f5b942; }
  .fill.clip { background: var(--danger); }
  .fill.muted { opacity: .35; }
  .mute { padding: .05rem .45rem; font-size: .72rem; border-radius: 6px; }
  .mute.on { border-color: var(--danger); color: var(--danger); background: var(--danger-soft); }
  .mute-gap { height: 1.45rem; }
  .empty { margin: 0; padding: .7rem; }
</style>
