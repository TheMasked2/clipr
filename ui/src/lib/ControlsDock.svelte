<script lang="ts">
  import * as api from './api';
  import { app, toast, toastError } from './store.svelte';
  import { HEIGHTS, QUALITY } from './video';
  import QuickCapture from './QuickCapture.svelte';

  // The actions, like OBS's Controls dock: one column of equal buttons with their hotkeys, the main three on
  // top. Less used ones sit under More, and Output opens the quality presets.
  const st = $derived(app.status);
  const hk = $derived(app.config?.hotkeys);
  const v = $derived(app.config?.video);
  const paused = $derived(st?.paused ?? false);
  const recording = $derived(st?.state === 'recording');
  const run = (p: Promise<unknown>) => p.catch(toastError);
  // Connecting, live or reconnecting: the button ends it. A failed stream shows its reason in the status bar.
  const streaming = $derived(!!st?.stream && st.stream.state !== 'failed');
  async function goLive() {
    if (streaming) return run(api.setStreaming(false));
    // This page never sees the key on another device (it's blank there): only on this PC can a blank one mean
    // there is none.
    if (!app.config?.stream.key) {
      const remote = await api.getSystem().then((s) => s.remote ?? false, () => false);
      if (!remote) return toast('err', 'Add your stream key first.', '#/settings/stream', 'Settings → Stream');
    }
    return run(api.setStreaming(true));
  }

  const output = $derived.by(() => {
    if (!v) return '';
    const q = QUALITY.find((x) => x.kbps === v.bitrate_kbps)?.name ?? `${(v.bitrate_kbps / 1000).toFixed(1)} Mbit/s`;
    const h = v.height ? (HEIGHTS.find((x) => x.v === v.height)?.t ?? `${v.height}p`) : 'Native';
    return `${q} · ${h} · ${v.fps} fps`;
  });

  // More and Output open a panel under (or over) their button: a disclosure, so Tab moves through it as usual.
  let menu = $state<'' | 'more' | 'output'>('');
  let root: HTMLElement | undefined = $state();
  let moreBtn: HTMLButtonElement | undefined = $state();
  let outBtn: HTMLButtonElement | undefined = $state();
  function toggle(m: 'more' | 'output') {
    menu = menu === m ? '' : m;
  }
  // A click elsewhere closes an open panel; Esc closes it and returns to its button; so does tabbing out of it.
  $effect(() => {
    if (!menu) return;
    const click = (e: MouseEvent) => {
      if (root && !root.contains(e.target as Node)) menu = '';
    };
    const key = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      // Handled: the in-game overlay doesn't close too.
      e.preventDefault();
      (menu === 'more' ? moreBtn : outBtn)?.focus();
      menu = '';
    };
    window.addEventListener('pointerdown', click);
    window.addEventListener('keydown', key);
    return () => {
      window.removeEventListener('pointerdown', click);
      window.removeEventListener('keydown', key);
    };
  });
</script>

<div class="controls" bind:this={root} onfocusout={(e) => { if (menu && !root?.contains(e.relatedTarget as Node | null)) menu = ''; }}>
  <button class="primary act" onclick={() => run(api.saveClip())}>Save clip{#if hk?.save_clip}<kbd>{hk.save_clip}</kbd>{/if}</button>
  <button class="act" onclick={() => run(api.saveClip(app.config?.replay.buffer_secs))}>Save long clip{#if hk?.save_long_clip}<kbd>{hk.save_long_clip}</kbd>{/if}</button>
  <button class="act" class:danger={recording} class:solid={recording} onclick={() => run(api.setRecording(!recording))}>
    {recording ? 'Stop recording' : 'Start recording'}{#if hk?.toggle_recording}<kbd>{hk.toggle_recording}</kbd>{/if}
  </button>
  <button class="act" class:danger={streaming} class:solid={streaming} onclick={goLive} title={streaming ? 'End the live stream' : 'Stream to Twitch, YouTube or your own server (Settings → Stream)'}>
    <span class="lbl">{#if !streaming}<span class="dot" aria-hidden="true"></span>{/if}{streaming ? 'End stream' : 'Go live'}</span>{#if hk?.toggle_stream}<kbd>{hk.toggle_stream}</kbd>{/if}
  </button>
  {#if paused}
    <!-- A paused buffer can't save clips: resuming it is the next thing to do, so it comes out of More. -->
    <button class="act warn" onclick={() => run(api.setBuffer(true))}>Resume buffer{#if hk?.toggle_buffer}<kbd>{hk.toggle_buffer}</kbd>{/if}</button>
  {/if}

  <div class="menu-wrap">
    <button class="act" bind:this={moreBtn} aria-expanded={menu === 'more'} aria-controls="more-panel" onclick={() => toggle('more')}>More<span class="caret">▾</span></button>
    {#if menu === 'more'}
      <div class="menu" id="more-panel">
        {#if !paused}
          <button class="act" onclick={() => { menu = ''; run(api.setBuffer(false)); }}>Pause buffer{#if hk?.toggle_buffer}<kbd>{hk.toggle_buffer}</kbd>{/if}</button>
        {/if}
        <button class="act" onclick={() => { menu = ''; run(api.markMoment()); }}>Mark moment{#if hk?.mark}<kbd>{hk.mark}</kbd>{/if}</button>
        {#if app.config?.audio.mic || st?.mic_muted}
          <button class="act" onclick={() => { menu = ''; run(api.setMicMuted(!st?.mic_muted)); }}>{st?.mic_muted ? 'Unmute mic' : 'Mute mic'}{#if hk?.toggle_mic}<kbd>{hk.toggle_mic}</kbd>{/if}</button>
        {/if}
      </div>
    {/if}
  </div>

  <div class="menu-wrap out">
    <button class="output" bind:this={outBtn} aria-expanded={menu === 'output'} aria-controls="output-panel" onclick={() => toggle('output')} title="Quality, resolution and frame rate">
      <span class="muted">Output</span><span class="val">{output}</span><span class="caret">▾</span>
    </button>
    {#if menu === 'output'}
      <div class="menu wide up" id="output-panel"><QuickCapture /></div>
    {/if}
  </div>
</div>

<style>
  .controls { display: flex; flex-direction: column; gap: .4rem; height: 100%; }
  .act { width: 100%; justify-content: space-between; padding: .4rem .7rem; border-radius: 7px; }
  .act kbd { margin-left: auto; font-size: .68rem; padding: 0 .35rem; }
  .primary.act kbd { background: transparent; color: inherit; opacity: .75; border-color: color-mix(in srgb, currentColor 40%, transparent); }
  .danger.solid.act kbd { background: transparent; color: inherit; border-color: #fff6; }
  .lbl { display: inline-flex; align-items: center; gap: .45rem; }
  .dot { width: .5rem; height: .5rem; border-radius: 50%; background: #e5484d; }
  .warn { color: #f5b942; border-color: #f5b94266; }
  .caret { margin-left: auto; font-size: .7rem; color: var(--text-muted); }
  .menu-wrap { position: relative; }
  .out { margin-top: auto; }
  .menu {
    position: absolute; z-index: 20; left: 0; right: 0; top: calc(100% + .25rem);
    display: flex; flex-direction: column; gap: .3rem; padding: .4rem;
    background: var(--surface); border: 1px solid var(--border); border-radius: 9px; box-shadow: 0 8px 24px #0008;
  }
  .menu.up { top: auto; bottom: calc(100% + .25rem); }
  .menu.wide { left: auto; right: 0; width: min(26rem, calc(100vw - 2rem)); padding: .8rem; }
  .output {
    width: 100%; justify-content: flex-start; gap: .5rem; padding: .35rem .6rem; font-size: .8rem;
    background: none; border-radius: 7px;
  }
  .output .val { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
