<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '../lib/api';
  import { app, onServerEvent, toastError } from '../lib/store.svelte';
  import { fmtBytes, fmtTime } from '../lib/format';
  import ClipCard from '../lib/ClipCard.svelte';
  import Wizard from '../lib/Wizard.svelte';
  import DiscordPosts from '../lib/DiscordPosts.svelte';
  import CrashNotice from '../lib/CrashNotice.svelte';

  let clips = $state<api.Clip[]>([]);
  let disk = $state<api.Disk | null>(null);
  let fresh = $state('');
  let now = $state(Date.now());

  const st = $derived(app.status);
  const hk = $derived(app.config?.hotkeys);
  const paused = $derived(st?.paused ?? false);
  const elapsed = $derived(st?.state === 'recording' && st.recording_since ? now / 1000 - st.recording_since : 0);

  async function refetch() {
    try {
      clips = (await api.listClips()).slice(0, 6);
    } catch (e) {
      if (!clips.length) toastError(e);
    }
  }
  const loadDisk = () => api.getDisk().then((d) => (disk = d)).catch(() => {});

  onMount(() => {
    void refetch();
    void loadDisk();
    const tick = setInterval(() => (now = Date.now()), 500);
    const off = onServerEvent((e) => {
      if (e.type === 'clip_saved') {
        clips = [e.clip, ...clips.filter((c) => c.id !== e.clip.id)].slice(0, 6);
        fresh = e.clip.id;
        setTimeout(() => (fresh = ''), 3000);
        void loadDisk();
      } else if (e.type === 'library') {
        void refetch();
        void loadDisk();
      }
    });
    return () => {
      clearInterval(tick);
      off();
    };
  });

  const run = (p: Promise<unknown>) => p.catch(toastError);
  const label = (k?: string) => (k ? k : '');
</script>

<h1>Home</h1>

<section class="card status" class:rec={st?.state === 'recording'} class:buf={st?.state === 'buffering'}>
  <div class="state">
    {#if !app.connected && !st}
      <span class="big-state">Connecting…</span>
    {:else if st?.state === 'recording'}
      <span class="dot rec"></span><span class="big-state">Recording</span><span class="timer">{fmtTime(elapsed)}</span>
    {:else if st?.state === 'buffering'}
      <span class="dot buf"></span><span class="big-state">Buffering</span>
    {:else if paused}
      <span class="dot"></span><span class="big-state">Buffer paused</span>
    {:else}
      <span class="dot"></span><span class="big-state">Not buffering</span>
    {/if}
  </div>
  <div class="muted">
    {#if st?.game}Game: <b>{st.game}</b>{:else if paused}The replay buffer is paused; resume it to save clips.{:else if st?.state === 'idle'}Waiting for a game to start.{:else}No game detected.{/if}
    {#if st?.encoder}<span class="sep">·</span>Encoder: {st.encoder}{/if}
  </div>
  {#if st?.error}<div class="banner err">{st.error}</div>{/if}
  {#if !app.connected && st}<div class="banner warn">Disconnected from clipr, retrying…</div>{/if}

  <div class="actions">
    <div class="act">
      <button class="primary big" onclick={() => run(api.saveClip())}>Save clip</button>
      {#if hk?.save_clip}<kbd>{hk.save_clip}</kbd>{/if}
    </div>
    <div class="act">
      <button onclick={() => run(api.saveClip(app.config?.replay.buffer_secs))}>Save long clip</button>
      {#if hk?.save_long_clip}<kbd>{hk.save_long_clip}</kbd>{/if}
    </div>
    <div class="act">
      <button class:danger={st?.state === 'recording'} onclick={() => run(api.setRecording(st?.state !== 'recording'))}>
        {st?.state === 'recording' ? 'Stop recording' : 'Start recording'}
      </button>
      {#if hk?.toggle_recording}<kbd>{hk.toggle_recording}</kbd>{/if}
    </div>
    <div class="act">
      <button onclick={() => run(api.setBuffer(paused))}>{paused ? 'Resume buffer' : 'Pause buffer'}</button>
      {#if hk?.toggle_buffer}<kbd>{hk.toggle_buffer}</kbd>{/if}
    </div>
    <div class="act">
      <button onclick={() => run(api.markMoment())}>Mark moment</button>
      {#if hk?.mark}<kbd>{hk.mark}</kbd>{/if}
    </div>
  </div>
</section>

<CrashNotice />
<DiscordPosts />

<div class="row head">
  <h2>Newest clips</h2>
  <span class="spacer"></span>
  <a href="#/gallery" class="muted">All clips →</a>
</div>
{#if clips.length}
  <div class="grid">
    {#each clips as c (c.id)}
      <ClipCard clip={c} highlight={fresh === c.id} onupdate={(u) => (clips = clips.map((x) => (x.id === u.id ? u : x)))} />
    {/each}
  </div>
{:else}
  <p class="muted">No clips yet. Press {label(hk?.save_clip) || 'the save-clip button'} while a game runs.</p>
{/if}

{#if disk}
  <p class="muted small disk">
    {fmtBytes(disk.free_bytes)} free of {fmtBytes(disk.total_bytes)} · clips use {fmtBytes(disk.clips_bytes)}
  </p>
{/if}

{#if app.config && !app.config.ui.setup_done}<Wizard />{/if}

<style>
  .status { display: flex; flex-direction: column; gap: .7rem; padding: 1.6rem; margin-bottom: 2rem; }
  .status.rec { border-color: var(--danger-border); }
  .status.buf { border-color: var(--accent-border); }
  .state { display: flex; align-items: center; gap: .7rem; }
  .big-state { font-size: 1.9rem; font-weight: 800; letter-spacing: -.02em; }
  .timer { font-family: var(--mono); font-size: 1.4rem; color: var(--danger); }
  .dot { width: .8rem; height: .8rem; border-radius: 50%; background: var(--text-muted); }
  .dot.buf { background: var(--accent); box-shadow: 0 0 10px var(--accent); }
  .dot.rec { background: var(--danger); box-shadow: 0 0 10px var(--danger); animation: pulse 1.2s infinite; }
  @keyframes pulse { 50% { opacity: .35; } }
  .sep { margin: 0 .4rem; }
  .actions { display: flex; flex-wrap: wrap; gap: 1rem 1.4rem; margin-top: .6rem; }
  .act { display: flex; align-items: center; gap: .5rem; }
  .head { margin-bottom: .8rem; }
  .head h2 { margin: 0; font-size: 1.2rem; }
  .head a { text-decoration: none; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); gap: 1rem; }
  .disk { margin-top: 1.5rem; }
</style>
