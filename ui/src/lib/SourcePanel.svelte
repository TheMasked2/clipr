<script lang="ts">
  import * as api from './api';
  import type { Config } from './api';
  import { app, saveConfig, toastError } from './store.svelte';
  import Field from './Field.svelte';
  import Fader from './Fader.svelte';
  import AudioFilters from './AudioFilters.svelte';

  // A source's settings, opened from its ⚙ in the Sources dock, like OBS's Properties window: everything about
  // that one source in one place. Volume applies and saves while you drag; the rest is saved with Save.
  let { source, onclose }: { source: string | null; onclose: () => void } = $props();

  const clone = (c: Config): Config => structuredClone($state.snapshot(c)) as Config;
  let draft = $state<Config | null>(null);
  let base = $state('');
  let saving = $state(false);
  let sys = $state<api.SystemInfo | null>(null);
  let dlg: HTMLDialogElement | undefined = $state();

  const dirty = $derived(!!draft && JSON.stringify(draft) !== base);
  const isApp = $derived(!!source && !['video', 'system', 'mic', 'game'].includes(source));
  const title = $derived(
    source === 'video'
      ? 'Video'
      : source === 'system'
        ? 'System audio'
        : source === 'mic'
          ? 'Microphone'
          : source === 'game'
            ? 'Game audio'
            : (source ?? '').replace(/\.exe$/i, ''),
  );

  // Open: a fresh copy of the saved settings. Closed: drop it.
  $effect(() => {
    if (!dlg) return;
    if (source && app.config) {
      if (!dlg.open) {
        draft = clone(app.config);
        base = JSON.stringify(draft);
        dlg.showModal();
        if (source === 'video' && !sys) api.getSystem().then((s) => (sys = s)).catch(() => {});
      }
    } else if (dlg.open) {
      dlg.close();
    }
  });

  /** The fader saved a volume on its own: take it in, so it isn't a pending change. */
  function followVolumes(c: Config): void {
    if (!draft) return;
    const b = JSON.parse(base) as Config;
    for (const a of [draft.audio, b.audio]) {
      a.system_volume = c.audio.system_volume;
      a.mic_volume = c.audio.mic_volume;
      a.volumes = { ...c.audio.volumes };
    }
    base = JSON.stringify(b);
  }

  async function save() {
    if (!draft) return;
    saving = true;
    try {
      await saveConfig(clone(draft));
      onclose();
    } catch (e) {
      toastError(e);
    } finally {
      saving = false;
    }
  }

  function removeApp() {
    if (!draft || !source) return;
    const key = source;
    draft.audio.apps = draft.audio.apps.filter((a) => a.trim().toLowerCase() !== key);
    void save();
  }
</script>

<dialog
  bind:this={dlg}
  aria-label="{title} settings"
  oncancel={(e) => {
    e.preventDefault();
    onclose();
  }}
>
  {#if draft && source}
    <header>
      <h2>{title}</h2>
      <button class="x" onclick={onclose} aria-label="Close">×</button>
    </header>
    <div class="content">
      {#if source === 'video'}
        <Field title="Record" hint="Games buffers only while a known game runs. Always buffers the desktop too when no game runs.">
          <div class="row">
            <button class="chip" class:on={draft.games.mode === 'auto'} onclick={() => draft && (draft.games.mode = 'auto')}>Games</button>
            <button class="chip" class:on={draft.games.mode === 'always'} onclick={() => draft && (draft.games.mode = 'always')}>Always</button>
          </div>
        </Field>
        <Field title="Game capture" hint="Just the game's window, or the whole monitor it's on. If a window can't be captured, clipr records its monitor instead.">
          <div class="row">
            <button class="chip" class:on={draft.games.capture === 'window'} onclick={() => draft && (draft.games.capture = 'window')}>Game window</button>
            <button class="chip" class:on={draft.games.capture === 'monitor'} onclick={() => draft && (draft.games.capture = 'monitor')}>Its monitor</button>
          </div>
        </Field>
        <Field title="Desktop monitor" hint="What's recorded while no game runs (with Always).">
          <select bind:value={draft.video.monitor}>
            <option value={0}>Primary monitor</option>
            {#each sys?.monitors ?? [] as m (m.index)}
              <option value={m.index}>{m.index}: {m.name} ({m.width}×{m.height}){m.primary ? ' · primary' : ''}</option>
            {/each}
            {#if draft.video.monitor > 0 && !(sys?.monitors ?? []).some((m) => m.index === draft?.video.monitor)}
              <option value={draft.video.monitor}>Monitor {draft.video.monitor}</option>
            {/if}
          </select>
        </Field>
        <p class="muted small">Quality, resolution and frame rate are under Output in Controls; the rest in <a href="#/settings" onclick={onclose}>Settings → Video</a>.</p>
      {:else}
        <Field title="Volume" hint="0–200 %, after the filters. Applies and saves right away, even while recording.">
          <Fader {source} label={title} onsaved={followVolumes} />
        </Field>
        {#if source === 'mic'}
          <Field title="Push-to-talk key" hint="Record the mic only while this key is held, like V or Mouse4. Empty = always on.">
            <input type="text" bind:value={draft.audio.push_to_talk} placeholder="V, Mouse4, Mouse5…" aria-label="Push-to-talk key" />
          </Field>
          {#if draft.audio.push_to_talk.trim()}
            <Field title="Push-to-talk release (ms)" hint="Keep the mic open this long after you let go."><input type="number" min="0" max="5000" step="10" bind:value={draft.audio.ptt_release_ms} /></Field>
          {/if}
        {/if}
        <Field title="Filters" hint={source === 'mic' ? 'None by default. A good start for a mic: noise suppression, noise gate, compressor, limiter.' : undefined} stacked>
          <AudioFilters audio={draft.audio} {source} label={title} />
        </Field>
        {#if source === 'mic'}
          <Field title="Offset (ms)" hint="Only if your voice is early or late in clips. Positive = later."><input type="number" min="-5000" max="5000" step="5" bind:value={draft.audio.mic_offset_ms} /></Field>
        {:else if source === 'system'}
          <Field title="Offset (ms)" hint="Only if the sound is early or late in clips. Positive = later."><input type="number" min="-5000" max="5000" step="5" bind:value={draft.audio.system_offset_ms} /></Field>
        {/if}
      {/if}
    </div>
    <footer>
      {#if isApp}<button class="danger" onclick={removeApp} disabled={saving}>Remove</button>{/if}
      <span class="muted small note">{dirty ? 'Saving restarts capture: the replay buffer starts over.' : ''}</span>
      <button onclick={onclose}>{dirty ? 'Cancel' : 'Close'}</button>
      <button class="primary" onclick={save} disabled={!dirty || saving}>{saving ? 'Saving…' : 'Save'}</button>
    </footer>
  {/if}
</dialog>

<style>
  dialog {
    background: var(--surface); color: var(--text); border: 1px solid var(--border); border-radius: 14px;
    padding: 0; width: min(40rem, calc(100vw - 2rem)); max-height: calc(100dvh - 3rem);
  }
  dialog[open] { display: flex; flex-direction: column; }
  dialog::backdrop { background: #000a; }
  header { display: flex; align-items: center; justify-content: space-between; padding: .8rem 1.2rem; border-bottom: 1px solid var(--border); }
  h2 { margin: 0; }
  .x { background: none; border: none; font-size: 1.4rem; line-height: 1; padding: 0 .3rem; color: var(--text-muted); }
  .content { padding: .6rem 1.2rem 1rem; overflow: auto; flex: 1; }
  footer { display: flex; align-items: center; gap: .6rem; padding: .7rem 1.2rem; border-top: 1px solid var(--border); }
  .note { flex: 1; }
  p { margin: .6rem 0 0; }
</style>
