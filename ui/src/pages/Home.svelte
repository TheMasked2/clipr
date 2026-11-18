<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '../lib/api';
  import { app, onServerEvent, toastError } from '../lib/store.svelte';
  import { fmtBytes, fmtDateTime, fmtTime } from '../lib/format';
  import Wizard from '../lib/Wizard.svelte';
  import DiscordPosts from '../lib/DiscordPosts.svelte';
  import CloudUploads from '../lib/CloudUploads.svelte';
  import CrashNotice from '../lib/CrashNotice.svelte';
  import CapturePreview from '../lib/CapturePreview.svelte';
  import { clipHref } from '../lib/router.svelte';

  // Home is quick info: the newest clip and the library size. The Gallery has the clips.
  let latest = $state<api.Clip | null>(null);
  let count = $state<number | null>(null);
  let disk = $state<api.Disk | null>(null);
  let fresh = $state('');
  let now = $state(Date.now());

  const st = $derived(app.status);
  const hk = $derived(app.config?.hotkeys);
  const paused = $derived(st?.paused ?? false);
  const elapsed = $derived(st?.state === 'recording' && st.recording_since ? now / 1000 - st.recording_since : 0);

  // Only the newest request's answer counts, so an older list can't undo a clip that was just saved.
  let seq = 0;
  async function refetch() {
    const mine = ++seq;
    try {
      const all = await api.listClips();
      if (mine !== seq) return;
      latest = all[0] ?? null;
      count = all.length;
    } catch (e) {
      if (mine === seq && count === null) toastError(e);
    }
  }
  const loadDisk = () => api.getDisk().then((d) => (disk = d)).catch(() => {});

  onMount(() => {
    void refetch();
    void loadDisk();
    const tick = setInterval(() => (now = Date.now()), 500);
    const off = onServerEvent((e) => {
      if (e.type === 'clip_saved') {
        seq++; // drops a list request still on its way
        if (latest?.id !== e.clip.id) count = (count ?? 0) + 1;
        latest = e.clip;
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
    {#if app.config?.audio.mic || st?.mic_muted}
      <div class="act">
        <button class:warn={st?.mic_muted} onclick={() => run(api.setMicMuted(!st?.mic_muted))}>{st?.mic_muted ? 'Unmute mic' : 'Mute mic'}</button>
        {#if hk?.toggle_mic}<kbd>{hk.toggle_mic}</kbd>{/if}
      </div>
    {/if}
  </div>
  <CapturePreview hideable />
</section>

<CrashNotice />
<DiscordPosts />
<CloudUploads />

<section class="card info">
  {#if latest}
    <a class="latest" class:fresh={fresh === latest.id} href={clipHref(latest.id)}>
      <span class="muted small">{fresh === latest.id ? 'Just saved' : 'Last clip'}</span>
      <b>{latest.title}</b>
      <span class="muted small">{latest.game} · {fmtDateTime(latest.created)}{latest.duration_secs ? ` · ${fmtTime(latest.duration_secs)}` : ''}</span>
    </a>
  {:else if count === 0}
    <p class="muted">No clips yet. Press {label(hk?.save_clip) || 'the save-clip button'} while a game runs.</p>
  {/if}
  <div class="row facts">
    {#if count}<a href="#/gallery">{count.toLocaleString()} {count === 1 ? 'clip' : 'clips'} →</a>{/if}
    {#if disk}<span class="muted small">{fmtBytes(disk.free_bytes)} free of {fmtBytes(disk.total_bytes)} · clips use {fmtBytes(disk.clips_bytes)}</span>{/if}
  </div>
</section>

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
  .act button.warn { color: #f5b942; border-color: #f5b94266; }
  .info { display: flex; flex-direction: column; gap: .8rem; padding: 1.2rem 1.6rem; }
  .info p { margin: 0; }
  .latest { display: flex; flex-direction: column; gap: .15rem; text-decoration: none; color: inherit; border-radius: .5rem; padding: .4rem .6rem; margin: -.4rem -.6rem; transition: background .2s; }
  .latest:hover { background: var(--surface-2); }
  .latest.fresh { background: var(--accent-soft, var(--surface-2)); }
  .latest b { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .facts { gap: .4rem 1.2rem; flex-wrap: wrap; align-items: baseline; }
  .facts a { text-decoration: none; }
</style>
