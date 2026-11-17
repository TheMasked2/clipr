<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import * as api from './api';
  import type { Config } from './api';
  import { app, saveConfig } from './store.svelte';
  import { router } from './router.svelte';
  import { applyAccent, DEFAULT_ACCENT } from './accent';
  import { conflicts } from './hotkey';
  import { fmtBytes } from './format';
  import Field from './Field.svelte';
  import Toggle from './Toggle.svelte';
  import ListEditor from './ListEditor.svelte';
  import HotkeyInput from './HotkeyInput.svelte';
  import VideoSimple from './VideoSimple.svelte';
  import LevelMeters from './LevelMeters.svelte';
  import AudioFilters from './AudioFilters.svelte';
  import DiscordSettings from './DiscordSettings.svelte';
  import UpdatePanel from './UpdatePanel.svelte';

  let { initial }: { initial: Config } = $props();

  const clone = (c: Config): Config => structuredClone($state.snapshot(c)) as Config;
  const draft = $state<Config>(untrack(() => clone(initial)));
  let savedJson = $state(untrack(() => JSON.stringify(initial)));
  const dirty = $derived(JSON.stringify(draft) !== savedJson);

  const tabs = ['General', 'Video', 'Replay', 'Audio', 'Hotkeys', 'Games', 'Library', 'Recording', 'Discord', 'Network', 'Look'] as const;
  let tab = $state<(typeof tabs)[number]>('General');
  let advanced = $state(false);
  let saving = $state(false);
  let error = $state('');
  let justSaved = $state(false);
  let sys = $state<api.SystemInfo | null>(null);
  let cleanMsg = $state('');
  let cleaning = $state(false);

  onMount(() => {
    api.getSystem().then((s) => (sys = s)).catch(() => {});
    router.guard = () => dirty;
    const warn = (e: BeforeUnloadEvent) => {
      if (dirty) e.preventDefault();
    };
    window.addEventListener('beforeunload', warn);
    return () => {
      router.guard = null;
      window.removeEventListener('beforeunload', warn);
      // Drop an unsaved accent preview.
      if (app.config) applyAccent(app.config.ui.accent);
    };
  });

  async function save() {
    saving = true;
    error = '';
    justSaved = false;
    try {
      const c = await saveConfig(clone(draft));
      savedJson = JSON.stringify(c);
      Object.assign(draft, clone(c));
      justSaved = true;
      setTimeout(() => (justSaved = false), 3000);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }
  function revert() {
    Object.assign(draft, clone(JSON.parse(savedJson)));
    applyAccent(draft.ui.accent);
    error = '';
  }

  const bad = $derived(
    conflicts({
      save_clip: draft.hotkeys.save_clip,
      save_long_clip: draft.hotkeys.save_long_clip,
      toggle_buffer: draft.hotkeys.toggle_buffer,
      toggle_recording: draft.hotkeys.toggle_recording,
      mark: draft.hotkeys.mark,
    }),
  );
  const addProfile = () => draft.games.profiles.push({ game: '', fps: null, height: null, bitrate_kbps: null });
  // The sources that can have filters: their keys match the server's (`system`, `mic`, the app's name).
  // Apps are only recorded with system audio off or separate tracks on (otherwise they're in the mix).
  const appsRecorded = $derived(!draft.audio.system || draft.audio.separate_tracks);
  const filterSources = $derived([
    ...(draft.audio.system ? [{ key: 'system', label: 'System audio' }] : []),
    ...(draft.audio.mic ? [{ key: 'mic', label: 'Microphone' }] : []),
    ...(appsRecorded ? draft.audio.apps.map((a) => ({ key: a.trim().toLowerCase(), label: a.trim() })) : []),
  ]);
  const ramMb = $derived(Math.round((draft.video.bitrate_kbps * draft.replay.buffer_secs) / 8 / 1000));

  const codecs: { v: Config['video']['codec']; name: string; text: string; enc: string }[] = [
    { v: 'h264', name: 'H.264', text: 'Plays everywhere (Discord, phones, browsers). The default.', enc: 'H.264' },
    { v: 'hevc', name: 'HEVC (H.265)', text: 'Smaller files at the same quality; not every browser or phone plays it.', enc: 'HEVC' },
    { v: 'av1', name: 'AV1', text: 'Smallest files; needs a recent GPU (RTX 40, RX 7000, Arc) and a recent player.', enc: 'AV1' },
  ];
  const supported = (enc: string) =>
    !sys?.gpus?.length || sys.gpus.some((g) => !g.encoders || g.encoders.includes(enc));

  async function cleanNow() {
    cleaning = true;
    cleanMsg = '';
    try {
      const r = await api.cleanup();
      cleanMsg = r.removed
        ? `Removed ${r.removed} clip${r.removed === 1 ? '' : 's'}, freed ${fmtBytes(r.freed_bytes)}.`
        : 'Nothing to clean up.';
    } catch (e) {
      cleanMsg = e instanceof Error ? e.message : String(e);
    } finally {
      cleaning = false;
    }
  }

  const themes = [
    { v: 'dark-sleek', t: 'Dark sleek', on: true },
    { v: 'minimal', t: 'Minimal', on: false },
    { v: 'playful', t: 'Playful', on: false },
    { v: 'system', t: 'System (light / dark)', on: false },
  ];
  const lanUrl = $derived(`http://<this PC's IP address>:${location.port || draft.web.port}`);
</script>

<h1>Settings</h1>

<nav class="tabs" aria-label="Settings sections">
  {#each tabs as t (t)}
    <button class="tab" class:active={tab === t} aria-current={tab === t ? 'page' : undefined} onclick={() => (tab = t)}>{t}</button>
  {/each}
</nav>
<select class="tabsel" bind:value={tab} aria-label="Settings section">
  {#each tabs as t (t)}<option value={t}>{t}</option>{/each}
</select>

<section class="card">
  {#if tab === 'General'}
    <Field title="Start with Windows" hint="Start clipr when Windows starts."><Toggle bind:checked={draft.general.start_with_windows} label="Start with Windows" /></Field>
    <Field title="Open the UI on start" hint="Open this page when you start clipr yourself, so you can see it is running. Never happens when Windows starts it."><Toggle bind:checked={draft.general.open_ui_on_start} label="Open UI on start" /></Field>
    <Field title="Update automatically" hint="Check GitHub releases every few hours and install a new version by itself, only when no game runs and nothing records. Opt-in, off by default."><Toggle bind:checked={draft.general.auto_update} label="Auto update" /></Field>
    <Field title="Beta versions" hint="Offer beta versions too, not only full releases."><Toggle bind:checked={draft.general.update_beta} label="Beta versions" /></Field>
    <UpdatePanel remote={sys?.remote ?? false} />
    <Field title="Send crash reports" hint="Send anonymous crash reports. Opt-in, off by default."><Toggle bind:checked={draft.privacy.crash_reports} label="Crash reports" /></Field>
  {:else if tab === 'Video'}
    <div class="seg">
      <button class="chip" class:on={!advanced} onclick={() => (advanced = false)}>Simple</button>
      <button class="chip" class:on={advanced} onclick={() => (advanced = true)}>Advanced</button>
    </div>
    {#if !advanced}
      <VideoSimple bind:video={draft.video} />
    {:else}
      <Field title="Codec" hint="What the video is compressed with. Pick by who needs to play it." stacked>
        <div class="codecs">
          {#each codecs as c (c.v)}
            <button class="codec" class:on={draft.video.codec === c.v} onclick={() => (draft.video.codec = c.v)}>
              <b>{c.name}</b>
              <span class="muted small">{c.text}</span>
              {#if !supported(c.enc)}<span class="small warnt">Not reported by your GPU</span>{/if}
            </button>
          {/each}
        </div>
      </Field>
      <Field title="Bitrate (kbit/s)" hint="Higher looks better and makes bigger files (15000 ≈ 56 MB per 30 s)."><input type="number" min="1000" max="100000" step="500" bind:value={draft.video.bitrate_kbps} /></Field>
      <Field title="Frames per second" hint="Capture delivers up to 60 for now."><input type="number" min="1" max="60" bind:value={draft.video.fps} /></Field>
      <Field title="Output height (px)" hint="Width follows the monitor's shape. 0 = the monitor's own resolution."><input type="number" min="0" max="4320" step="1" bind:value={draft.video.height} /></Field>
      <Field title="Monitor" hint="Which monitor to capture: primary, or a specific one in Windows' order.">
        <select bind:value={draft.video.monitor}>
          <option value={0}>Primary monitor</option>
          {#each sys?.monitors ?? [] as m (m.index)}
            <option value={m.index}>{m.index}: {m.name} ({m.width}×{m.height}){m.primary ? ' · primary' : ''}</option>
          {/each}
          {#if draft.video.monitor > 0 && !(sys?.monitors ?? []).some((m) => m.index === draft.video.monitor)}
            <option value={draft.video.monitor}>Monitor {draft.video.monitor}</option>
          {/if}
        </select>
      </Field>
    {/if}
    <Field title="Per-game profiles" hint="Use other video settings while a particular game runs. Match by the game's name or exe (cs2.exe), any case. Leave a box empty to keep the normal value above. Capture restarts when the game changes." stacked>
      <div class="profiles">
        {#each draft.games.profiles as p, i (i)}
          <div class="profile">
            <input type="text" class="pgame" bind:value={p.game} placeholder="game.exe or name" aria-label="Game" />
            <input type="number" min="10" max="240" bind:value={p.fps} placeholder="FPS" aria-label="Frames per second" />
            <input type="number" min="0" max="4320" bind:value={p.height} placeholder="Height" aria-label="Output height" />
            <input type="number" min="1000" max="150000" step="500" bind:value={p.bitrate_kbps} placeholder="kbit/s" aria-label="Bitrate" />
            <button aria-label="Remove profile" onclick={() => draft.games.profiles.splice(i, 1)}>×</button>
          </div>
        {/each}
        <div><button onclick={addProfile}>Add a profile</button></div>
      </div>
    </Field>
  {:else if tab === 'Replay'}
    <Field title="Replay buffer" hint="Keep the replay buffer running, so the save-clip hotkey has something to save."><Toggle bind:checked={draft.replay.enabled} label="Replay buffer" /></Field>
    <Field title="Clip length (seconds)" hint="How many seconds a saved clip goes back."><input type="number" min="1" max="3600" bind:value={draft.replay.clip_secs} /></Field>
    <Field title="Buffer length (seconds)" hint={`How much the buffer keeps in memory; "save long clip" saves all of it. Estimated RAM use at your bitrate: about ${ramMb} MB.`}><input type="number" min="1" max="3600" bind:value={draft.replay.buffer_secs} /></Field>
    <Field title="Sound on save" hint="Play a short sound when a clip is saved."><Toggle bind:checked={draft.replay.sound_on_save} label="Sound on save" /></Field>
  {:else if tab === 'Audio'}
    <Field title="Live levels" hint="What each source picks up right now, before its filters, from -60 to 0 dB. CLIP means the source is already distorting before clipr gets it: lower its level in Windows. Shows while the buffer runs; save to apply changes." stacked><LevelMeters /></Field>
    <h3 class="sub">Sources</h3>
    <Field title="System audio" hint="Record what you hear (game, voice chat, music): the default output device."><Toggle bind:checked={draft.audio.system} label="System audio" /></Field>
    <Field title="System audio volume" hint="0–200 %. 100 % leaves it unchanged.">
      <div class="vol">
        <input type="range" min="0" max="200" step="5" bind:value={draft.audio.system_volume} disabled={!draft.audio.system} aria-label="System audio volume" />
        <span class="pctv">{draft.audio.system_volume}%</span>
      </div>
    </Field>
    <Field title="Microphone" hint="Record your default microphone."><Toggle bind:checked={draft.audio.mic} label="Microphone" /></Field>
    <Field title="Microphone volume" hint="0–200 %, applied after the mic's filters. 100 % leaves it unchanged.">
      <div class="vol">
        <input type="range" min="0" max="200" step="5" bind:value={draft.audio.mic_volume} disabled={!draft.audio.mic} aria-label="Microphone volume" />
        <span class="pctv">{draft.audio.mic_volume}%</span>
      </div>
    </Field>
    <Field title="Push-to-talk key" hint="Record the mic only while this key is held: your voice chat's push-to-talk key, like V or Mouse4. Empty = always on.">
      <input type="text" bind:value={draft.audio.push_to_talk} placeholder="V, Mouse4, Mouse5…" disabled={!draft.audio.mic} aria-label="Push-to-talk key" />
    </Field>
    <Field title="Push-to-talk release (ms)" hint="Keep the mic open this long after you let go, so the end of a word isn't cut off."><input type="number" min="0" max="5000" step="10" bind:value={draft.audio.ptt_release_ms} disabled={!draft.audio.mic || !draft.audio.push_to_talk.trim()} /></Field>
    <Field title="Apps on their own track" hint="Record these programs separately (up to 4), like discord.exe, so you can mute voice chat later in an editor. With system audio on, this needs Separate tracks (the app is already in the mix). Needs Windows 10 2004 or newer." stacked><ListEditor bind:items={draft.audio.apps} placeholder="discord.exe" /></Field>
    <Field title="Separate tracks" hint="Also write every source on its own track. Track 1 is always the full mix, so clips still play normally."><Toggle bind:checked={draft.audio.separate_tracks} label="Separate tracks" /></Field>
    <Field title="Microphone offset (ms)" hint="Shift the mic in time if it's out of sync with the video. Positive = later."><input type="number" min="-5000" max="5000" step="5" bind:value={draft.audio.mic_offset_ms} /></Field>
    <Field title="System audio offset (ms)" hint="Shift system audio in time. Positive = later."><input type="number" min="-5000" max="5000" step="5" bind:value={draft.audio.system_offset_ms} /></Field>
    <h3 class="sub">Filters</h3>
    <p class="muted small note">Like OBS: each source starts with no filters. Add the ones you want; they run top to bottom.</p>
    {#each filterSources as src (src.key)}
      <Field title={src.label} stacked><AudioFilters audio={draft.audio} source={src.key} label={src.label} /></Field>
    {:else}
      <p class="muted small">Turn on a source above to give it filters.</p>
    {/each}
    {#if draft.audio.apps.length && !appsRecorded}
      <p class="muted small">The apps above are part of system audio right now, so they get its filters. Turn on Separate tracks to give each app its own.</p>
    {/if}
    <h3 class="sub">Quality</h3>
    <Field title="Audio quality (AAC)" hint="AAC bitrate in kbit/s per track. 160 is plenty for game audio.">
      <select bind:value={draft.audio.bitrate_kbps}>{#each [96, 128, 160, 192] as b (b)}<option value={b}>{b} kbps</option>{/each}</select>
    </Field>
  {:else if tab === 'Hotkeys'}
    <p class="muted small note">Click a box, then press the keys, like <kbd>Alt+F10</kbd>. Esc cancels, × clears. While clipr runs, a hotkey is taken from games, so they will not see it.</p>
    {#if bad.size}<div class="banner err">Two actions share the same hotkey. Change one of the rows marked in red.</div>{/if}
    <Field title="Save clip" hint="Saves the last clip-length seconds. Empty turns it off." error={bad.has('save_clip')}><HotkeyInput bind:value={draft.hotkeys.save_clip} conflict={bad.has('save_clip')} /></Field>
    <Field title="Save long clip" hint="Saves the whole buffer. Empty turns it off." error={bad.has('save_long_clip')}><HotkeyInput bind:value={draft.hotkeys.save_long_clip} conflict={bad.has('save_long_clip')} /></Field>
    <Field title="Pause / resume buffer" hint="Pauses or resumes the replay buffer. Empty (the default) turns it off." error={bad.has('toggle_buffer')}><HotkeyInput bind:value={draft.hotkeys.toggle_buffer} conflict={bad.has('toggle_buffer')} /></Field>
    <Field title="Start / stop recording" hint="Starts or stops a full recording." error={bad.has('toggle_recording')}><HotkeyInput bind:value={draft.hotkeys.toggle_recording} conflict={bad.has('toggle_recording')} /></Field>
    <Field title="Mark moment" hint="Marks this moment; markers show on the clip's timeline." error={bad.has('mark')}><HotkeyInput bind:value={draft.hotkeys.mark} conflict={bad.has('mark')} /></Field>
  {:else if tab === 'Games'}
    <Field title="Mode" hint="Auto buffers only while a known game runs and idles otherwise. Always buffers the monitor, game or not.">
      <div class="row">
        <button class="chip" class:on={draft.games.mode === 'auto'} onclick={() => (draft.games.mode = 'auto')}>Auto</button>
        <button class="chip" class:on={draft.games.mode === 'always'} onclick={() => (draft.games.mode = 'always')}>Always</button>
      </div>
    </Field>
    <p class="muted small note">{sys?.known_games != null ? `clipr knows ${sys.known_games.toLocaleString()} games.` : 'Loading the known games list…'}</p>
    <Field title="Extra games" hint="Games to detect by exe name (mygame.exe) on top of the known list." stacked><ListEditor bind:items={draft.games.custom} /></Field>
    <Field title="Blocked programs" hint="Exe names never treated as games, even if the known list has them." stacked><ListEditor bind:items={draft.games.blocked} placeholder="program.exe" /></Field>
    <Field title="Auto-record" hint="Start a full recording by itself when one of these games starts (exe names)." stacked><ListEditor bind:items={draft.games.auto_record} /></Field>
  {:else if tab === 'Library'}
    <Field title="Clips folder" hint="Where clips go. Empty = Videos\clipr. Each game gets its own subfolder." stacked><input type="text" bind:value={draft.clips.folder} placeholder="Videos\clipr" /></Field>
    <Field title="Delete after (days)" hint="Delete clips older than this many days (favorites never). 0 = keep forever."><input type="number" min="0" bind:value={draft.clips.max_age_days} /></Field>
    <Field title="Maximum total size (GB)" hint="Keep the clips folder under this by deleting the oldest (favorites never). 0 = no limit."><input type="number" min="0" bind:value={draft.clips.max_total_gb} /></Field>
    <Field title="Minimum free space (GB)" hint="Stop recording and warn when the clips drive has less than this free."><input type="number" min="0" bind:value={draft.clips.min_free_gb} /></Field>
    <Field title="Clean up now" hint="Runs the age and size limits above on the saved settings (it also runs hourly). Save first if you changed them.">
      <button onclick={cleanNow} disabled={cleaning}>{cleaning ? 'Cleaning…' : 'Clean up now'}</button>
      {#if cleanMsg}<span class="muted small">{cleanMsg}</span>{/if}
    </Field>
  {:else if tab === 'Recording'}
    <Field title="Maximum length (minutes)" hint="Stop a recording after this many minutes. 0 = no limit."><input type="number" min="0" bind:value={draft.recording.max_minutes} /></Field>
  {:else if tab === 'Discord'}
    <DiscordSettings discord={draft.discord} remote={sys?.remote ?? false} />
  {:else if tab === 'Network'}
    <Field title="Allow access from my local network" hint="Open this page to other devices at home (your phone). They must enter a PIN. Off by default."><Toggle bind:checked={draft.web.lan} label="LAN access" /></Field>
    {#if draft.web.lan}
      <div class="banner warn">Anyone on your network with the PIN can see and delete your clips. Only turn this on on a network you trust. The connection is not encrypted.</div>
    {/if}
    {#if app.config?.web.lan && app.config.web.lan_pin}
      <div class="pin">
        <div class="muted small">PIN for other devices</div>
        <div class="pinv">{app.config.web.lan_pin}</div>
        <div class="muted small">Open <code>{lanUrl}</code> on the other device and enter it.</div>
      </div>
    {:else if draft.web.lan}
      <p class="muted small note">Save to generate the PIN.</p>
    {/if}
    <Field title="First port" hint="First port to try for the web UI. If it is taken, clipr uses the next free one. Applies after a restart."><input type="number" min="1024" max="65535" bind:value={draft.web.port} /></Field>
  {:else if tab === 'Look'}
    <Field title="Accent colour" hint="The one colour used for buttons, highlights and the timeline.">
      <input type="color" value={draft.ui.accent} oninput={(e) => { draft.ui.accent = e.currentTarget.value; applyAccent(draft.ui.accent); }} aria-label="Accent colour" />
      <code class="muted">{draft.ui.accent}</code>
      <button onclick={() => { draft.ui.accent = DEFAULT_ACCENT; applyAccent(DEFAULT_ACCENT); }}>Reset</button>
    </Field>
    <Field title="Theme" hint="More looks are coming later.">
      <select bind:value={draft.ui.theme}>
        {#each themes as t (t.v)}<option value={t.v} disabled={!t.on}>{t.t}{t.on ? '' : ' (coming later)'}</option>{/each}
      </select>
    </Field>
  {/if}
</section>

<div class="savebar" class:dirty>
  <span class="msg">
    {#if error}<span class="errt">{error}</span>
    {:else if justSaved}<span class="okt">Saved</span>
    {:else if dirty}You have unsaved changes
    {/if}
  </span>
  <button onclick={revert} disabled={!dirty || saving}>Revert</button>
  <button class="primary" onclick={save} disabled={!dirty || saving || bad.size > 0}>{saving ? 'Saving…' : 'Save'}</button>
</div>

<style>
  .tabs { display: flex; gap: 0.3rem; overflow-x: auto; margin-bottom: 1rem; padding-bottom: 0.2rem; scrollbar-width: thin; }
  .tab { background: transparent; border-color: transparent; color: var(--text-muted); white-space: nowrap; }
  .tab.active { color: var(--text); background: var(--accent-soft); border-color: var(--accent-border); }
  .tabsel { display: none; width: 100%; margin-bottom: 1rem; }
  @media (max-width: 640px) {
    .tabs { display: none; }
    .tabsel { display: block; }
  }
  section { padding-top: 0.5rem; padding-bottom: 0.5rem; }
  .seg { display: flex; gap: 0.4rem; padding: 0.8rem 0 0.2rem; }
  .note { margin: 0.8rem 0 0; }
  .sub { margin: 1.4rem 0 0.2rem; font-size: 0.8rem; font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; color: var(--text-muted); }
  .profiles { display: flex; flex-direction: column; gap: 0.5rem; width: 100%; }
  .profile { display: flex; flex-wrap: wrap; gap: 0.4rem; }
  .profile input[type='number'] { width: 6.5rem; }
  .pgame { flex: 1; min-width: 9rem; }
  .codecs { display: grid; grid-template-columns: repeat(auto-fit, minmax(13rem, 1fr)); gap: 0.6rem; }
  .codec { flex-direction: column; align-items: flex-start; text-align: left; gap: 0.25rem; padding: 0.8rem; }
  .codec.on { border-color: var(--accent); background: var(--accent-soft); }
  .warnt { color: #f5b942; }
  .vol { display: flex; align-items: center; gap: 0.7rem; width: min(16rem, 100%); }
  .pctv { width: 3rem; text-align: right; font-variant-numeric: tabular-nums; }
  .pin { margin: 0.8rem 0; padding: 1rem; border: 1px dashed var(--accent-border); border-radius: 10px; text-align: center; }
  .pinv { font-family: var(--mono); font-size: 2rem; letter-spacing: 0.3em; margin: 0.2rem 0; }
  .savebar {
    position: sticky; bottom: 0; display: flex; align-items: center; gap: 0.6rem; justify-content: flex-end;
    margin-top: 1rem; padding: 0.8rem 1rem; background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius);
  }
  .savebar.dirty { border-color: var(--accent-border); }
  .msg { flex: 1; font-size: 0.9rem; color: var(--text-muted); }
  .errt { color: var(--danger); }
  .okt { color: #4ade80; }
</style>
