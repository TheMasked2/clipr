<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '../lib/api';
  import { app, onServerEvent, toastError } from '../lib/store.svelte';
  import { fmtTime } from '../lib/format';
  import Wizard from '../lib/Wizard.svelte';
  import DiscordPosts from '../lib/DiscordPosts.svelte';
  import CloudUploads from '../lib/CloudUploads.svelte';
  import CrashNotice from '../lib/CrashNotice.svelte';
  import CleanupWarning from '../lib/CleanupWarning.svelte';
  import CapturePreview from '../lib/CapturePreview.svelte';
  import Dock from '../lib/Dock.svelte';
  import SourcesDock from '../lib/SourcesDock.svelte';
  import MixerStrips from '../lib/MixerStrips.svelte';
  import ControlsDock from '../lib/ControlsDock.svelte';
  import StatusBar from '../lib/StatusBar.svelte';

  // Home works like OBS's main window, with everything in a fixed place: the live picture with the capture
  // state on it, then three docks (Sources: what's recorded and its settings; Mixer; Controls), and a status
  // bar with the numbers that matter. The Gallery has the clips.
  let latest = $state<api.Clip | null>(null);
  let count = $state<number | null>(null);
  let disk = $state<api.Disk | null>(null);
  let fresh = $state('');
  let now = $state(Date.now());
  let elevated = $state(false);
  let samples = $state<api.PerfSample[]>([]);

  const st = $derived(app.status);
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
    api.getSystem().then((s) => (elevated = !!s.elevated)).catch(() => {});
    const tick = setInterval(() => (now = Date.now()), 500);
    // Performance samples for the status bar and the buffer fill, once a second while the tab is visible.
    let stop = false;
    const poll = async () => {
      if (stop) return;
      if (document.visibilityState === 'visible') {
        try {
          samples = await api.getPerf();
        } catch {
          // Disconnected: keep the last numbers; the badge says so.
        }
      }
      if (!stop) setTimeout(poll, 1000);
    };
    void poll();
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
      stop = true;
      clearInterval(tick);
      off();
    };
  });

  const last = $derived(samples[samples.length - 1]);
  /** How full the replay buffer is, in seconds, against its length. */
  const filled = $derived(Math.min(last?.buffer_secs ?? 0, app.config?.replay.buffer_secs ?? Infinity));
</script>

<!-- What clipr is doing, at the start of the status bar: "Recording (game) 0:12:07", "Clipping (game) · …". -->
{#snippet doing()}
  <span class="doing" class:rec={st?.state === 'recording'} class:buf={st?.state === 'buffering'} title={st?.encoder ? `Encoder: ${st.encoder}` : undefined} role="status">
    {#if !app.connected && !st}
      <span class="dot"></span>Connecting…
    {:else if st?.state === 'recording'}
      <span class="dot"></span><b>Recording</b>{#if st.game}<span class="dim">({st.game})</span>{/if}<span class="mono">{fmtTime(elapsed)}</span>
    {:else if st?.state === 'buffering'}
      <span class="dot"></span><b>Clipping</b>{#if st.game}<span class="dim">({st.game})</span>{/if}<span class="dim mono" title="How much of the replay buffer is filled: the most a clip can hold right now">· {fmtTime(filled)} of {fmtTime(app.config?.replay.buffer_secs ?? 0)} ready</span>
    {:else if paused}
      <span class="dot"></span><b>Paused</b><span class="dim">· resume to save clips</span>
    {:else if app.config?.games.mode === 'auto'}
      <span class="dot"></span><b>Waiting for a game</b>
    {:else}
      <span class="dot"></span><b>Not clipping</b>
    {/if}
  </span>
{/snippet}

<h1 class="sr-only">Home</h1>

<div class="home">
  {#if st?.error}<div class="banner err">{st.error}</div>{/if}
  {#if elevated}<div class="banner warn">clipr is running as administrator. Windows can then block game capture, and your settings may belong to another account. Quit clipr and start it normally.</div>{/if}
  {#if !app.connected && st}<div class="banner warn">Disconnected from clipr, retrying…</div>{/if}
  <CrashNotice />
  <CleanupWarning />
  <DiscordPosts />
  <CloudUploads />

  <CapturePreview hideable stage />

  <div class="docks">
    <div class="cell sources"><Dock title="Sources" flush><SourcesDock /></Dock></div>
    <div class="cell mixer"><Dock title="Mixer" flush><MixerStrips /></Dock></div>
    <div class="cell controls"><Dock title="Controls" menus><ControlsDock /></Dock></div>
  </div>

  <div class="bar"><StatusBar {samples} {disk} {latest} fresh={!!latest && fresh === latest.id} {doing} /></div>
</div>

{#if app.config && !app.config.ui.setup_done}<Wizard />{/if}

<style>
  .sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
  .home { display: flex; flex-direction: column; gap: .7rem; --stage-h: max(14rem, calc(100dvh - 28rem)); }
  /* The notices bring their own bottom margin; the gap spaces them here. */
  .home > :global(section), .home > :global(div) { margin-bottom: 0; }
  .home :global(.screen.stage) { max-height: var(--stage-h); }
  .docks { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.35fr) minmax(0, .9fr); gap: .7rem; height: 17rem; }
  .cell { min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .cell > :global(.dock) { flex: 1; }
  .bar { position: sticky; bottom: .6rem; z-index: 5; }
  @media (max-width: 900px) {
    /* One column: the buttons first, then what's recorded and the levels. */
    .docks { grid-template-columns: minmax(0, 1fr); height: auto; }
    .controls { order: 0; }
    .sources { order: 1; }
    .mixer { order: 2; height: 14rem; }
    /* On a phone the bar wraps to a few lines: it stays at the end instead of covering the docks. */
    .bar { position: static; }
  }
  .doing { display: inline-flex; align-items: center; gap: .4rem; white-space: nowrap; min-width: 0; overflow: hidden; }
  .doing .dot { width: .55rem; height: .55rem; border-radius: 50%; background: #888; flex: none; }
  .doing.buf .dot { background: #3fb950; box-shadow: 0 0 6px #3fb950; }
  .doing.rec b { color: var(--danger); }
  .doing.rec .dot { background: var(--danger); animation: pulse 1.2s infinite; }
  @keyframes pulse { 50% { opacity: .35; } }
  .mono { font-family: var(--mono); font-variant-numeric: tabular-nums; }
  .dim { opacity: .75; overflow: hidden; text-overflow: ellipsis; }
</style>
