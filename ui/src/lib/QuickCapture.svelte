<script lang="ts">
  import type { Config } from './api';
  import { app, quickEdit, toastError } from './store.svelte';
  import { HEIGHTS, QUALITY } from './video';

  // The output presets (Home → Controls → Output): quality, resolution and frame rate, one click each. Each
  // click saves at once (Settings → Video has the details; what to record is in the Video source).
  const cfg = $derived(app.config);
  /** Clicks waiting to be saved; each is saved in turn (none are dropped). */
  let saving = $state(0);

  async function change(edit: (c: Config) => void): Promise<void> {
    saving++;
    try {
      await quickEdit(edit);
    } catch (e) {
      toastError(e);
    } finally {
      saving--;
    }
  }
</script>

{#if cfg}
  <div class="quick" class:busy={saving > 0}>
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
    <p class="muted small">Changes save at once and restart capture, so the replay buffer starts over. <a href="#/settings">More in Settings → Video</a></p>
  </div>
{/if}

<style>
  .quick { display: flex; flex-direction: column; gap: 0.6rem; }
  .quick.busy { opacity: 0.7; }
  .line { display: flex; align-items: center; gap: 0.5rem 0.8rem; flex-wrap: wrap; }
  .label { width: 5.2rem; font-size: 0.85rem; color: var(--text-muted); }
  .chips { display: flex; gap: 0.35rem; flex-wrap: wrap; }
  p { margin: 0.2rem 0 0; }
</style>
