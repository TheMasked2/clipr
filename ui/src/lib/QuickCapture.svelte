<script lang="ts">
  import type { Config } from './api';
  import { app, saveConfig, toastError } from './store.svelte';
  import { HEIGHTS, QUALITY } from './video';

  // The capture presets at a glance, like OBS's main window: what to record and how, one click each. Each
  // click saves at once (Settings has the details).
  const cfg = $derived(app.config);
  let saving = $state(false);

  async function change(edit: (c: Config) => void): Promise<void> {
    if (!app.config || saving) return;
    const next = structuredClone($state.snapshot(app.config)) as Config;
    edit(next);
    saving = true;
    try {
      await saveConfig(next);
    } catch (e) {
      toastError(e);
    } finally {
      saving = false;
    }
  }
</script>

{#if cfg}
  <div class="quick" class:busy={saving}>
    <div class="line">
      <span class="label">Record</span>
      <div class="chips">
        <button class="chip" class:on={cfg.games.mode === 'auto'} onclick={() => change((c) => (c.games.mode = 'auto'))} title="Buffer only while a game runs">Games</button>
        <button class="chip" class:on={cfg.games.mode === 'always'} onclick={() => change((c) => (c.games.mode = 'always'))} title="Buffer the desktop too when no game runs">Always</button>
      </div>
      {#if cfg.games.mode === 'auto'}
        <div class="chips">
          <button class="chip" class:on={cfg.games.capture === 'window'} onclick={() => change((c) => (c.games.capture = 'window'))} title="Just the game's window">Game window</button>
          <button class="chip" class:on={cfg.games.capture === 'monitor'} onclick={() => change((c) => (c.games.capture = 'monitor'))} title="The game's whole monitor">Its monitor</button>
        </div>
      {/if}
    </div>
    <div class="line">
      <span class="label">Quality</span>
      <div class="chips">
        {#each QUALITY as q (q.name)}
          <button class="chip" class:on={cfg.video.bitrate_kbps === q.kbps} onclick={() => change((c) => (c.video.bitrate_kbps = q.kbps))} title="{q.kbps / 1000} Mbit/s">{q.name}</button>
        {/each}
      </div>
    </div>
    <div class="line">
      <span class="label">Resolution</span>
      <div class="chips">
        {#each HEIGHTS as h (h.v)}
          <button class="chip" class:on={cfg.video.height === h.v} onclick={() => change((c) => (c.video.height = h.v))}>{h.v ? h.t : 'Native'}</button>
        {/each}
      </div>
    </div>
    <div class="line">
      <span class="label">Frame rate</span>
      <div class="chips">
        {#each [30, 60] as f (f)}
          <button class="chip" class:on={cfg.video.fps === f} onclick={() => change((c) => (c.video.fps = f))}>{f} fps</button>
        {/each}
      </div>
    </div>
    <p class="muted small">Changes save at once and restart capture, so the replay buffer starts over. <a href="#/settings">More in Settings</a></p>
  </div>
{/if}

<style>
  .quick { display: flex; flex-direction: column; gap: 0.6rem; }
  .quick.busy { opacity: 0.7; }
  .line { display: flex; align-items: center; gap: 0.5rem 0.8rem; flex-wrap: wrap; }
  .label { width: 5.5rem; font-size: 0.85rem; color: var(--text-muted); }
  .chips { display: flex; gap: 0.35rem; flex-wrap: wrap; }
  p { margin: 0.2rem 0 0; }
</style>
