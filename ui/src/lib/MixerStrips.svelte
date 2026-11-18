<script lang="ts">
  import * as api from './api';
  import Fader from './Fader.svelte';
  import { channels, FLOOR, fmtDb, pct, RED_AT, YELLOW_AT } from './meter';
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
  const muted = (source: string) => api.isMuted(app.status, source);
  function toggle(source: string) {
    const m = muted(source);
    (source === 'mic' ? api.setMicMuted(!m) : api.setMuted(source, !m)).catch(toastError);
  }
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
        <!-- Left and right, like OBS. Muted, it greys out and shows what comes in, so you still see it's live. -->
        <div class="meters" class:off={muted(l.source)} role="meter" aria-label="{l.name} level" aria-valuemin={FLOOR} aria-valuemax={0} aria-valuenow={Math.round(muted(l.source) ? l.db : l.after)} aria-valuetext={muted(l.source) ? `Muted in clips, coming in at ${fmtDb(l.db)}` : fmtDb(l.after)} title="{muted(l.source) ? 'Muted in clips. Coming in' : 'Before filters'}: {fmtDb(l.db)}, recorded: {fmtDb(l.after)}" style="--y: {YELLOW_AT}%; --r: {RED_AT}%">
          {#each muted(l.source) ? channels(l.db_lr, l.db) : channels(l.after_lr, l.after) as v, i (i)}
            <div class="bar"><div class="cover" style="height: {100 - pct(v)}%"></div></div>
          {/each}
        </div>
        <Fader source={l.source} label={l.name} vertical />
      </div>
      <button class="mute" class:on={muted(l.source)} aria-pressed={muted(l.source)} aria-label="Mute {l.name}" onclick={() => toggle(l.source)} title={l.source === 'mic' ? 'Mute the microphone in clips, not in your voice chat (the mute hotkey does the same)' : `Mute ${l.name} in clips (you still hear it)`}>{muted(l.source) ? 'Muted' : 'Mute'}</button>
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
  .meters { display: flex; gap: 2px; }
  /* The colours sit on the bar; a cover hides the part above the level. */
  .bar {
    width: .38rem; border-radius: 2px; overflow: hidden; position: relative; border: 1px solid var(--border);
    background: linear-gradient(to top, #3fb950 0 var(--y), #d29922 var(--y) var(--r), #e5484d var(--r) 100%);
  }
  .cover { position: absolute; top: 0; left: 0; right: 0; background: var(--surface-2); transition: height 90ms linear; }
  .meters.off .bar { filter: grayscale(1); opacity: .45; }
  .mute { padding: .05rem .45rem; font-size: .72rem; border-radius: 6px; }
  .mute.on { border-color: var(--danger); color: var(--danger); background: var(--danger-soft); }
  .empty { margin: 0; padding: .7rem; }
</style>
