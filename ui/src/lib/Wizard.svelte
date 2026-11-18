<script lang="ts">
  import { untrack } from 'svelte';
  import { getSystem, type Config, type SystemInfo } from './api';
  import { app, saveConfig, toastError } from './store.svelte';
  import { applyAccent } from './accent';
  import VideoSimple from './VideoSimple.svelte';
  import HotkeyInput from './HotkeyInput.svelte';
  import Toggle from './Toggle.svelte';
  import Field from './Field.svelte';

  const steps = ['Look', 'Video', 'Hotkeys', 'Audio', 'Done'] as const;
  let step = $state(0);
  let busy = $state(false);
  let sys = $state<SystemInfo | null>(null);
  const draft = $state<Config>(
    untrack(() => structuredClone($state.snapshot(app.config!)) as Config),
  );
  const swatches = ['#8b5cf6', '#3b82f6', '#22d3ee', '#10b981', '#f59e0b', '#ef4444', '#ec4899'];

  getSystem()
    .then((s) => (sys = s))
    .catch(() => {});

  function setAccent(c: string) {
    draft.ui.accent = c;
    applyAccent(c);
  }

  async function finish() {
    busy = true;
    try {
      draft.ui.setup_done = true;
      await saveConfig($state.snapshot(draft) as Config);
    } catch (e) {
      toastError(e);
      busy = false;
    }
  }
  const next = () => (step < steps.length - 1 ? step++ : finish());
  const gpuText = $derived(
    sys?.gpus?.length
      ? 'Detected: ' +
          sys.gpus.map((g) => g.name + (g.encoders?.length ? ` (${g.encoders.join(', ')})` : '')).join('; ') +
          '.'
      : 'Detecting your GPU…',
  );
</script>

<div class="overlay">
  <div class="card wiz" role="dialog" aria-modal="true" aria-label="Setup">
    <div class="dots">
      {#each steps as s, i (s)}<span class:on={i <= step}></span>{/each}
    </div>
    <h1>{step === 4 ? 'You are set' : 'Welcome to clipr'}</h1>

    {#if step === 0}
      <p class="muted">Pick an accent colour. You can change it any time in Settings → Appearance.</p>
      <div class="row">
        {#each swatches as c (c)}
          <button
            type="button"
            class="sw"
            class:sel={draft.ui.accent.toLowerCase() === c}
            style:background={c}
            aria-label={c}
            onclick={() => setAccent(c)}
          ></button>
        {/each}
        <input
          type="color"
          value={draft.ui.accent}
          oninput={(e) => setAccent(e.currentTarget.value)}
          aria-label="Custom accent"
        />
      </div>
    {:else if step === 1}
      <p class="muted">{gpuText} Medium at 1080p is a good start.</p>
      <VideoSimple bind:video={draft.video} />
    {:else if step === 2}
      <p class="muted">Press this combo in a game to save the last {draft.replay.clip_secs} seconds.</p>
      <Field title="Save clip" hint="Click the box, then press the keys. Esc cancels.">
        <HotkeyInput bind:value={draft.hotkeys.save_clip} />
      </Field>
    {:else if step === 3}
      <Field title="Game / system audio" hint="Record what you hear: game, voice chat, music.">
        <Toggle bind:checked={draft.audio.system} label="System audio" />
      </Field>
      <Field title="Microphone" hint="Mix your default microphone into the same track.">
        <Toggle bind:checked={draft.audio.mic} label="Microphone" />
      </Field>
    {:else}
      <p class="muted">
        clipr keeps the last {Math.round(draft.replay.buffer_secs / 60)} minutes while a game runs. Press
        <kbd>{draft.hotkeys.save_clip || 'your hotkey'}</kbd> to keep a clip. Everything else lives in Settings.
      </p>
    {/if}

    <div class="row foot">
      <button class="ghost" onclick={finish} disabled={busy}>Skip setup</button>
      <span class="spacer"></span>
      {#if step > 0 && step < 4}<button onclick={() => step--}>Back</button>{/if}
      {#if step < 4}<button onclick={next}>Skip</button>{/if}
      <button class="primary" onclick={next} disabled={busy}>{step === 4 ? 'Finish' : 'Next'}</button>
    </div>
  </div>
</div>

<style>
  .overlay { position: fixed; inset: 0; background: #000c; display: grid; place-items: center; padding: 1rem; z-index: 40; overflow-y: auto; }
  .wiz { width: min(34rem, 100%); max-height: 100%; overflow-y: auto; }
  .dots { display: flex; gap: 0.35rem; margin-bottom: 1rem; }
  .dots span { height: 4px; flex: 1; border-radius: 2px; background: var(--border); }
  .dots .on { background: var(--accent); }
  .foot { margin-top: 1.25rem; }
  .sw { width: 2rem; height: 2rem; padding: 0; border-radius: 50%; border: 2px solid transparent; }
  .sw.sel { border-color: var(--text); }
</style>
