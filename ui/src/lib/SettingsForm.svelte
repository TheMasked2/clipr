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
  import CapturePreview from './CapturePreview.svelte';
  import Fader from './Fader.svelte';
  import Mixer from './Mixer.svelte';
  import AudioFilters from './AudioFilters.svelte';
  import DiscordSettings from './DiscordSettings.svelte';
  import CloudSettings from './CloudSettings.svelte';
  import UpdatePanel from './UpdatePanel.svelte';
  import FolderField from './FolderField.svelte';
  import CleanupWarning from './CleanupWarning.svelte';

  let { initial }: { initial: Config } = $props();

  const clone = (c: Config): Config => structuredClone($state.snapshot(c)) as Config;
  const draft = $state<Config>(untrack(() => clone(initial)));
  let savedJson = $state(untrack(() => JSON.stringify(initial)));
  let pinShown = $state(false);
  const dirty = $derived(JSON.stringify(draft) !== savedJson);

  /** A fader saved a volume on its own: take it into the draft and the saved copy, so it isn't a pending
   * change (and Save doesn't put the old value back). */
  function followVolumes(c: Config): void {
    const saved = JSON.parse(savedJson) as Config;
    for (const a of [draft.audio, saved.audio]) {
      a.system_volume = c.audio.system_volume;
      a.mic_volume = c.audio.mic_volume;
      a.volumes = { ...c.audio.volumes };
    }
    savedJson = JSON.stringify(saved);
  }

  const tabs = ['General', 'Video', 'Replay', 'Audio', 'Hotkeys', 'Games', 'Library', 'Recording', 'Discord', 'Cloud', 'Network', 'Appearance'] as const;
  let tab = $state<(typeof tabs)[number]>('General');
  // Simple or Advanced is remembered on this device.
  const readAdvanced = () => {
    try {
      return localStorage.getItem('clipr.videoAdvanced') === '1';
    } catch {
      return false;
    }
  };
  let advanced = $state(readAdvanced());
  function setAdvanced(on: boolean) {
    advanced = on;
    try {
      localStorage.setItem('clipr.videoAdvanced', on ? '1' : '0');
    } catch {
      // Private window or blocked storage: it just isn't remembered.
    }
  }
  let saving = $state(false);
  let error = $state('');
  let justSaved = $state(false);
  let sys = $state<api.SystemInfo | null>(null);
  // Fullscreen programs seen lately that aren't games yet (Games tab), to add one with a click.
  let unlisted = $state<api.UnlistedGame[]>([]);
  // Refreshed while the tab shows, so a game started meanwhile turns up.
  $effect(() => {
    if (tab !== 'Games') return;
    const load = () => api.getUnlistedGames().then((u) => (unlisted = u)).catch(() => {});
    load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  });
  const customLower = $derived(new Set(draft.games.custom.map((c) => c.trim().toLowerCase())));
  const notAdded = $derived(unlisted.filter((u) => !customLower.has(u.exe)));
  let seenList = $state<HTMLElement>();
  const addGame = (exe: string) => {
    if (!customLower.has(exe)) draft.games.custom.push(exe);
    // The row goes away: keep the keyboard in the list, or on the Save button once it's empty.
    queueMicrotask(() =>
      ((seenList?.querySelector('button') ?? document.querySelector('.savebar .primary')) as HTMLElement | null)?.focus(),
    );
  };
  // The monitor being captured, to say when the output height scales the picture.
  const capturedMonitor = $derived(
    (sys?.monitors ?? []).find((m) => (draft.video.monitor ? m.index === draft.video.monitor : m.primary)),
  );
  const scaleNote = $derived.by(() => {
    const m = capturedMonitor;
    const h = draft.video.height;
    if (!m || !h || h === m.height) return '';
    if (h > m.height)
      return `Your monitor is ${m.height}p, so clips are ${m.height}p: clipr never scales up (it would make files bigger without adding detail).`;
    return `Scaled down from your monitor's ${m.height}p to ${h}p: lighter on the GPU and smaller files, but small text looks softer. Pick Native to keep every pixel.`;
  });
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
      const lanChanged = JSON.parse(savedJson).web?.lan !== c.web.lan;
      savedJson = JSON.stringify(c);
      Object.assign(draft, clone(c));
      justSaved = true;
      // The server opens or closes the network port right after saving; show if it couldn't.
      if (lanChanged) setTimeout(() => api.getSystem().then((s) => (sys = s)).catch(() => {}), 500);
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
      toggle_mic: draft.hotkeys.toggle_mic,
    }),
  );
  const addProfile = () => draft.games.profiles.push({ game: '', fps: null, height: null, bitrate_kbps: null });
  // The recorded apps (their filter keys match the server's: the exe name in lower case), and the tracks a
  // clip gets, worked out like the server's plan (crates/audio/src/plan.rs): track 1 is the mix; every app
  // has its own track; Separate tracks also gives system audio and the mic one. Six tracks at most.
  // ASCII-only lower case, like the server's key (to_ascii_lowercase).
  const asciiLower = (s: string) => s.replace(/[A-Z]/g, (c) => c.toLowerCase());
  const appList = $derived(
    draft.audio.apps
      .map((a) => a.trim())
      .filter((a, i, all) => a && all.findIndex((b) => asciiLower(b) === asciiLower(a)) === i)
      .map((a) => ({ key: asciiLower(a), label: a.replace(/\.exe$/i, '') })),
  );
  const tracks = $derived.by(() => {
    const a = draft.audio;
    const mix = [
      ...(a.system ? ['System audio'] : []),
      ...(a.mic ? ['Microphone'] : []),
      ...(a.system ? [] : [...(a.game ? ['Game'] : []), ...appList.map((x) => x.label)]),
    ];
    if (!mix.length && !appList.length && !a.game) return [];
    const own = [
      // The names the clip page and video editors show.
      ...(a.separate_tracks && a.system ? ['System'] : []),
      ...(a.separate_tracks && a.mic ? ['Mic'] : []),
      ...(a.game ? ['Game'] : []),
      ...appList.map((x) => x.label),
    ];
    return [{ name: 'Mix', has: mix.join(' + ') || 'nothing' }, ...own.map((o) => ({ name: o, has: o }))].slice(0, 6);
  });
  const dropped = $derived(
    Math.max(
      0,
      1 + (draft.audio.separate_tracks ? +draft.audio.system + +draft.audio.mic : 0) + +draft.audio.game + appList.length - 6,
    ),
  );
  let syncOpen = $state(false);
  const micMuted = $derived(app.status?.mic_muted ?? false);
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
  const lanUrl = $derived(`http://${sys?.lan_ip ?? "<this PC's IP address>"}:${location.port || draft.web.port}`);
</script>

<h1>Settings</h1>

<div class="layout">
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
    <Field title="Update automatically" hint="Check GitHub releases once each time clipr starts, like Steam, and install a new version by itself, only when no game runs and nothing records. Opt-in, off by default."><Toggle bind:checked={draft.general.auto_update} label="Auto update" /></Field>
    <Field title="Beta versions" hint="Offer beta versions too, not only full releases."><Toggle bind:checked={draft.general.update_beta} label="Beta versions" /></Field>
    <UpdatePanel remote={sys?.remote ?? false} />
    <Field title="Crash reports" hint="If clipr crashes, keep a report (and a minidump) on this PC and show it on Home next time, so you can send it as a GitHub issue. Your user name, PC name and profile folder are taken out, and nothing is ever sent by itself. Opt-in, off by default."><Toggle bind:checked={draft.privacy.crash_reports} label="Crash reports" /></Field>
  {:else if tab === 'Video'}
    <Field title="Preview" hint="What clipr captures right now, a few frames a second. Changes here show once you save them." stacked><CapturePreview /></Field>
    <div class="seg">
      <button class="chip" class:on={!advanced} onclick={() => setAdvanced(false)}>Simple</button>
      <button class="chip" class:on={advanced} onclick={() => setAdvanced(true)}>Advanced</button>
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
      <Field title="GPU priority" hint="Let clipr's capture go a little ahead of the game on the GPU. It helps on some PCs and makes clips skip frames on others: if your clips stutter, try it both ways. Off by default."><Toggle bind:checked={draft.video.gpu_priority} label="GPU priority" /></Field>
    {/if}
    {#if scaleNote}<p class="small note scale">{scaleNote}</p>{/if}
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
    <Field title="Sound on save" hint="Play a short chime when a clip is saved (a low tone if saving fails). It plays through your speakers, so with system audio on it can be heard in a clip saved right after."><Toggle bind:checked={draft.replay.sound_on_save} label="Sound on save" /></Field>
    <Field title="Sound volume" hint="How loud the save sound and the mic mute blip play (the blip plays even with Sound on save off). 0 % = silent.">
      <div class="vol">
        <input type="range" min="0" max="100" step="5" bind:value={draft.replay.sound_volume} aria-label="Sound volume" />
        <span class="pctv">{draft.replay.sound_volume}%</span>
        <button onclick={() => api.testSound(draft.replay.sound_volume).catch(() => {})} disabled={!draft.replay.sound_volume}>Test</button>
      </div>
    </Field>
    <Field title="Note on save" hint="Show a small &quot;Clip saved&quot; note in the top-right corner. It never shows up in your clips. Games in exclusive fullscreen hide it (the sound still plays)."><Toggle bind:checked={draft.replay.toast_on_save} label="Note on save" /></Field>
  {:else if tab === 'Audio'}
    <div class="group">
    <h3 class="sub">Tracks</h3>
    <p class="muted small note">What each clip records. Players and Discord play track 1; video editors show every track, so you can mute voice chat or your mic afterwards. <a href="https://kdenlive.org" target="_blank" rel="noreferrer">Kdenlive</a> (free) opens all tracks as separate audio.</p>
    {#if tracks.length}
      <ol class="tracks">
        {#each tracks as t, i (i)}
          <li><span class="tn">Track {i + 1}</span><strong>{t.name}</strong>{#if i === 0}<span class="muted small">{t.has}</span>{/if}</li>
        {/each}
      </ol>
      {#if dropped}<p class="warnt small">Only 6 tracks fit: the last {dropped === 1 ? 'one is' : `${dropped} are`} left out (still in the mix).</p>{/if}
    {:else}
      <p class="warnt small">No audio: turn on a source below.</p>
    {/if}
    <Field title="Separate tracks" hint="Also give system audio and the microphone a track of their own. Apps below always get one."><Toggle bind:checked={draft.audio.separate_tracks} label="Separate tracks" /></Field>
    <Field title="Mixer" hint="What each source records right now, after its filters, from -60 to 0 dB, with its volume. Faders apply and save right away, even while recording; other changes show once saved. Shown here even when nothing records, so you can set levels first (the mic is opened for it, if it's on). CLIP means the source is already distorting before clipr gets it: lower its level in Windows." stacked><Mixer onsaved={followVolumes} /></Field>
    </div>
    <div class="group">
    <h3 class="sub">System audio</h3>
    <Field title="Record system audio" hint="Everything you hear (game, voice chat, music): the default output device."><Toggle bind:checked={draft.audio.system} label="System audio" /></Field>
    {#if draft.audio.system}
      <Field title="Volume" hint="0–200 %, applied after the filters. 100 % leaves it unchanged. Applies and saves right away, even while recording."><Fader source="system" label="System audio" onsaved={followVolumes} /></Field>
      <Field title="Filters" stacked><AudioFilters audio={draft.audio} source="system" label="System audio" /></Field>
    {/if}
    </div>
    <div class="group">
    <h3 class="sub">Microphone</h3>
    <Field title="Record the microphone" hint="Your default microphone (Windows Sound settings)."><Toggle bind:checked={draft.audio.mic} label="Microphone" /></Field>
    {#if draft.audio.mic}
      <Field title="Volume" hint="0–200 %, applied after the filters. 100 % leaves it unchanged. Applies and saves right away, even while recording."><Fader source="mic" label="Microphone" onsaved={followVolumes} /></Field>
      <Field title="Push-to-talk key" hint="Record the mic only while this key is held: your voice chat's push-to-talk key, like V or Mouse4. Empty = always on.">
        <input type="text" bind:value={draft.audio.push_to_talk} placeholder="V, Mouse4, Mouse5…" aria-label="Push-to-talk key" />
      </Field>
      {#if draft.audio.push_to_talk.trim()}
        <Field title="Push-to-talk release (ms)" hint="Keep the mic open this long after you let go, so the end of a word isn't cut off."><input type="number" min="0" max="5000" step="10" bind:value={draft.audio.ptt_release_ms} /></Field>
      {/if}
      <Field title="Mute in clips" hint={`Silences your mic in clips (not in your voice chat) until you unmute it. Set a hotkey for it under Hotkeys${draft.hotkeys.toggle_mic ? ` (now ${draft.hotkeys.toggle_mic})` : ''}. clipr always starts unmuted.`}>
        <div class="row">
          {#if micMuted}<span class="warnt small">Muted</span>{/if}
          <button onclick={() => api.setMicMuted(!micMuted).catch(() => {})}>{micMuted ? 'Unmute' : 'Mute now'}</button>
        </div>
      </Field>
      <Field title="Filters" hint="Like OBS: none by default. A good start for a mic: noise suppression, noise gate, compressor, limiter." stacked><AudioFilters audio={draft.audio} source="mic" label="Microphone" /></Field>
    {/if}
    </div>
    <div class="group">
    <h3 class="sub">Game audio</h3>
    <Field title="Game on its own track" hint="Also record only the game's sound (no voice chat, no music) on a track of its own, so you can turn it up or down later in an editor. Follows whichever game clipr detects (if the game is also under App tracks, that track has it instead). Needs Windows 10 2004 or newer."><Toggle bind:checked={draft.audio.game} label="Game audio" /></Field>
    {#if draft.audio.game}
      <Field title="Filters" stacked><AudioFilters audio={draft.audio} source="game" label="Game" /></Field>
    {/if}
    </div>
    <div class="group">
    <h3 class="sub">App tracks</h3>
    <Field title="Apps on their own track" hint="Record programs separately (up to 4), like discord.exe, so you can mute voice chat later in an editor. Needs Windows 10 2004 or newer." stacked><ListEditor bind:items={draft.audio.apps} placeholder="discord.exe" /></Field>
    {#each appList as app (app.key)}
      <Field title="{app.label} filters" stacked><AudioFilters audio={draft.audio} source={app.key} label={app.label} /></Field>
    {/each}

    <details class="sync" bind:open={syncOpen}>
      <summary>Audio out of sync with the video?</summary>
      <p class="muted small">Only needed if a source plays early or late in your clips. Positive = later.</p>
      <Field title="Microphone offset (ms)"><input type="number" min="-5000" max="5000" step="5" bind:value={draft.audio.mic_offset_ms} /></Field>
      <Field title="System audio offset (ms)"><input type="number" min="-5000" max="5000" step="5" bind:value={draft.audio.system_offset_ms} /></Field>
    </details>
    </div>
    <div class="group">
    <h3 class="sub">Quality</h3>
    <Field title="Audio quality (AAC)" hint="AAC bitrate in kbit/s per track. 160 is plenty for game audio.">
      <select bind:value={draft.audio.bitrate_kbps}>{#each [96, 128, 160, 192] as b (b)}<option value={b}>{b} kbps</option>{/each}</select>
    </Field>
    </div>
  {:else if tab === 'Hotkeys'}
    <p class="muted small note">Click a box, then press the keys, like <kbd>Alt+F10</kbd>. Esc cancels, × clears. While clipr runs, a hotkey is taken from games, so they will not see it.</p>
    {#if bad.size}<div class="banner err">Two actions share the same hotkey. Change one of the rows marked in red.</div>{/if}
    <Field title="Save clip" hint="Saves the last clip-length seconds. Empty turns it off." error={bad.has('save_clip')}><HotkeyInput bind:value={draft.hotkeys.save_clip} conflict={bad.has('save_clip')} /></Field>
    <Field title="Save long clip" hint="Saves the whole buffer. Empty turns it off." error={bad.has('save_long_clip')}><HotkeyInput bind:value={draft.hotkeys.save_long_clip} conflict={bad.has('save_long_clip')} /></Field>
    <Field title="Pause / resume buffer" hint="Pauses or resumes the replay buffer. Empty (the default) turns it off." error={bad.has('toggle_buffer')}><HotkeyInput bind:value={draft.hotkeys.toggle_buffer} conflict={bad.has('toggle_buffer')} /></Field>
    <Field title="Start / stop recording" hint="Starts or stops a full recording." error={bad.has('toggle_recording')}><HotkeyInput bind:value={draft.hotkeys.toggle_recording} conflict={bad.has('toggle_recording')} /></Field>
    <Field title="Mark moment" hint="Marks this moment; markers show on the clip's timeline." error={bad.has('mark')}><HotkeyInput bind:value={draft.hotkeys.mark} conflict={bad.has('mark')} /></Field>
    <Field title="Mute / unmute microphone" hint="Mutes your mic in clips (not in your voice chat), with a short blip. Press again to unmute. Empty (the default) turns it off." error={bad.has('toggle_mic')}><HotkeyInput bind:value={draft.hotkeys.toggle_mic} conflict={bad.has('toggle_mic')} /></Field>
  {:else if tab === 'Games'}
    <Field title="Mode" hint="Game only buffers while a known game runs and does nothing otherwise. Desktop buffers your monitor all the time, game or not.">
      <div class="row">
        <button class="chip" class:on={draft.games.mode === 'auto'} onclick={() => (draft.games.mode = 'auto')}>Game only</button>
        <button class="chip" class:on={draft.games.mode === 'always'} onclick={() => (draft.games.mode = 'always')}>Desktop</button>
      </div>
    </Field>
    {#if draft.games.mode === 'auto'}
      <Field title="Capture" hint="Game window records only the game, so alt-tabbing, notifications and your other screens stay out of clips. Pick Whole monitor if a game's clips come out black.">
        <div class="row">
          <button class="chip" class:on={draft.games.capture === 'window'} onclick={() => (draft.games.capture = 'window')}>Game window</button>
          <button class="chip" class:on={draft.games.capture === 'monitor'} onclick={() => (draft.games.capture = 'monitor')}>Whole monitor</button>
        </div>
      </Field>
    {/if}
    <p class="muted small note">{app.status?.game ? `Detected right now: ${app.status.game}` : 'No game detected right now.'}</p>
    <p class="muted small note">{sys?.known_games != null ? `clipr knows ${sys.known_games.toLocaleString()} games.` : 'Loading the known games list…'}</p>
    <Field title="Find new games" hint="clipr knows games from Discord's list, which can miss brand-new and small games. On, a program that runs fullscreen from a game library (Steam, Epic, Xbox, GOG, EA, Ubisoft, Riot, Amazon, itch) counts as a game too, named after its folder. Launchers, installers and helpers don't, nor do blocked programs."><Toggle bind:checked={draft.games.detect_unlisted} label="Find new games" /></Field>
    <Field title="Extra games" hint="Games clipr should always record: the exe name (mygame.exe), or the game's name (Trust Your Gut). A name counts when it's the game's folder in a game library, or starts the window title of a fullscreen program." stacked><ListEditor bind:items={draft.games.custom} placeholder="mygame.exe or Game Name" /></Field>
    {#if notAdded.length}
      <div class="seen">
        <p class="muted small">Ran fullscreen lately, not recorded as a game. Add yours (then Save):</p>
        <ul bind:this={seenList}>
          {#each notAdded as u (u.exe)}
            <li><span class="t">{u.title || u.exe}</span> <code>{u.exe}</code> <button class="small" onclick={() => addGame(u.exe)} aria-label={`Add ${u.title || u.exe} as a game`}>Add as a game</button></li>
          {/each}
        </ul>
      </div>
    {/if}
    <Field title="Blocked programs" hint="Exe names never treated as games, even if the known list has them." stacked><ListEditor bind:items={draft.games.blocked} placeholder="program.exe" /></Field>
    <Field title="Auto-record" hint="Start a full recording by itself when one of these games starts (exe names)." stacked><ListEditor bind:items={draft.games.auto_record} /></Field>
  {:else if tab === 'Library'}
    <Field title="Clips folder" hint="Where clips go. Empty = Videos\clipr. Each game gets its own subfolder." stacked><FolderField bind:value={draft.clips.folder} placeholder="Videos\clipr" label="Clips folder" title="Where should clips go?" remote={sys?.remote ?? false} /></Field>
    <Field title="Recordings folder" hint="Where full recordings go, also in game folders. Empty = the clips folder. Handy for a big second drive. It can't be inside the clips folder or the other way round." stacked><FolderField bind:value={draft.recording.folder} placeholder="Same as the clips folder" label="Recordings folder" title="Where should recordings go?" remote={sys?.remote ?? false} /></Field>
    <Field title="Delete after (days)" hint="Delete clips older than this many days (favorites never). 0 = keep forever."><input type="number" min="0" bind:value={draft.clips.max_age_days} /></Field>
    <Field title="Maximum total size (GB)" hint="Keep the clips folder under this by deleting the oldest (favorites never). 0 = no limit."><input type="number" min="0" bind:value={draft.clips.max_total_gb} /></Field>
    <CleanupWarning limits={draft.clips} />
    <Field title="Minimum free space (GB)" hint="Stop recording and warn when the clips or recordings drive has less than this free."><input type="number" min="0" bind:value={draft.clips.min_free_gb} /></Field>
    <Field title="Clean up now" hint="Runs the age and size limits above on the saved settings (it also runs hourly). Save first if you changed them.">
      <button onclick={cleanNow} disabled={cleaning}>{cleaning ? 'Cleaning…' : 'Clean up now'}</button>
      {#if cleanMsg}<span class="muted small">{cleanMsg}</span>{/if}
    </Field>
  {:else if tab === 'Recording'}
    <Field title="Maximum length (minutes)" hint="Stop a recording after this many minutes. 0 = no limit."><input type="number" min="0" bind:value={draft.recording.max_minutes} /></Field>
  {:else if tab === 'Discord'}
    <DiscordSettings discord={draft.discord} remote={sys?.remote ?? false} cloudReady={draft.cloud.enabled && !!draft.cloud.public_url} onCloud={() => (tab = 'Cloud')} />
  {:else if tab === 'Cloud'}
    <CloudSettings cloud={draft.cloud} remote={sys?.remote ?? false} />
  {:else if tab === 'Network'}
    <Field title="Allow access from my local network" hint="Open this page to other devices at home (your phone). They must enter a PIN. Off by default."><Toggle bind:checked={draft.web.lan} label="LAN access" /></Field>
    {#if draft.web.lan}
      <div class="banner warn">Anyone on your network with the PIN can see and delete your clips. Only turn this on on a network you trust. The connection is not encrypted.</div>
      <Field title="Live preview on other devices" hint="Let devices with the PIN see the live capture preview. It shows your whole screen (other windows, notifications, messages), not just games. Off by default."><Toggle bind:checked={draft.web.lan_preview} label="Live preview on other devices" /></Field>
    {/if}
    {#if app.config?.web.lan && app.config.web.lan_pin}
      <div class="pin">
        <div class="muted small">PIN for other devices</div>
        <!-- Hidden until asked, so clicking through Settings while recording doesn't put it in a clip. -->
        <div class="pinv">{pinShown ? app.config.web.lan_pin : '••••••'}</div>
        <button class="link small" onclick={() => (pinShown = !pinShown)}>{pinShown ? 'Hide PIN' : 'Show PIN'}</button>
        <div class="muted small">Open <code>{lanUrl}</code> on the other device and enter it.</div>
      </div>
      {#if sys?.lan_error}
        <div class="banner warn">Other devices can't connect: {sys.lan_error}</div>
      {/if}
    {:else if draft.web.lan}
      <p class="muted small note">Save to generate the PIN.</p>
    {/if}
    <p class="muted small note">
      <strong>Away from home?</strong> clipr doesn't open itself to the internet. Don't forward its port on your router: the
      connection isn't encrypted. To reach it from outside, put your phone and PC on a private network such as
      <a href="https://tailscale.com" target="_blank" rel="noreferrer">Tailscale</a>, turn on local network access and
      use the PC's address in that network. To share single clips, use Cloud.
    </p>
    <Field title="First port" hint="First port to try for the web UI. If it is taken, clipr uses the next free one. Applies after a restart."><input type="number" min="1024" max="65535" bind:value={draft.web.port} /></Field>
  {:else if tab === 'Appearance'}
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
</div>

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
  .layout { display: grid; grid-template-columns: 10.5rem 1fr; gap: 1rem; align-items: start; }
  .layout > .card { min-width: 0; }
  .tabs { display: flex; flex-direction: column; gap: 0.2rem; position: sticky; top: 4.2rem; }
  .tab { background: transparent; border-color: transparent; color: var(--text-muted); white-space: nowrap; text-align: left; }
  .tracks { list-style: none; margin: 0.4rem 0 0.6rem; padding: 0; display: flex; flex-direction: column; gap: 0.3rem; }
  .tracks li { display: flex; align-items: baseline; gap: 0.6rem; flex-wrap: wrap; padding: 0.35rem 0.6rem; border: 1px solid var(--border); border-radius: 0.5rem; background: var(--surface-2); }
  .tn { font-size: 0.75rem; color: var(--text-muted); font-variant-numeric: tabular-nums; min-width: 3.6rem; }
  .sync { margin-top: 1.4rem; border: 1px solid var(--border); border-radius: 0.6rem; padding: 0.5rem 0.8rem; }
  .sync summary { cursor: pointer; color: var(--text-muted); }
  .tab.active { color: var(--text); background: var(--accent-soft); border-color: var(--accent-border); }
  .tabsel { display: none; width: 100%; margin-bottom: 1rem; }
  @media (max-width: 640px) {
    .layout { display: block; }
    .tabs { display: none; }
    .tabsel { display: block; }
  }
  section { padding-top: 0.5rem; padding-bottom: 0.5rem; }
  .seg { display: flex; gap: 0.4rem; padding: 0.8rem 0 0.2rem; }
  .note { margin: 0.8rem 0 0; }
  .scale { padding: 0.5rem 0.7rem; border-left: 3px solid #f5b942; background: color-mix(in srgb, #f5b942 8%, transparent); border-radius: 4px; }
  .group { margin-top: 1rem; padding: 0.2rem 1rem 0.8rem; border: 1px solid var(--border); border-radius: var(--radius); background: color-mix(in srgb, var(--text) 2%, transparent); }
  .group > .sub:first-child { margin-top: 0.8rem; }
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
  .seen { margin: -.3rem 0 .9rem; }
  .seen ul { list-style: none; margin: .3rem 0 0; padding: 0; display: flex; flex-direction: column; gap: .3rem; }
  .seen li { display: flex; align-items: center; gap: .5rem; flex-wrap: wrap; }
  .seen .t { font-weight: 600; }
</style>
