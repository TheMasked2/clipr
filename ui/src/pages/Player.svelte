<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '../lib/api';
  import { clipHref, go, replaceClipRoute, router } from '../lib/router.svelte';
  import { confirmDialog, onServerEvent, toast, toastError } from '../lib/store.svelte';
  import { fmtBytes, fmtDateTime, fmtTime, fmtTimePrecise } from '../lib/format';
  import ShareDiscord from '../lib/ShareDiscord.svelte';
  import CloudShare from '../lib/CloudShare.svelte';

  type AudioTracks = ArrayLike<{ label: string; language: string; enabled: boolean }>;

  let clip = $state<api.Clip | null>(null);
  let missing = $state(false);
  let busy = $state(false);
  let reloadKey = $state(0);
  let allShelves = $state<string[]>([]);

  let video: HTMLVideoElement | undefined = $state();
  let tl: HTMLDivElement | undefined = $state();
  let dur = $state(0);
  let cur = $state(0);
  let trimStart = $state(0);
  let trimEnd = $state(0);
  let tracks = $state<{ label: string; enabled: boolean }[]>([]);
  let titleDraft = $state('');
  let newShelf = $state('');

  async function load(id: string, quiet = false) {
    try {
      const f = (await api.listClips()).find((c) => c.id === id);
      if (f) {
        clip = f;
        missing = false;
        titleDraft = f.title;
      } else if (!quiet || !busy) {
        missing = true;
        if (!quiet) clip = null;
      }
    } catch (e) {
      toastError(e);
    }
  }

  $effect(() => {
    const id = router.id;
    if (clip?.id !== id) {
      dur = 0;
      cur = 0;
      trimStart = 0;
      trimEnd = 0;
      tracks = [];
      void load(id);
    }
  });

  onMount(() => {
    api.getShelves().then((s) => (allShelves = s)).catch(() => {});
    return onServerEvent((e) => {
      if (e.type === 'library') void load(router.id, true);
    });
  });

  function setDur() {
    if (!video) return;
    const d = isFinite(video.duration) ? video.duration : (clip?.duration_secs ?? 0);
    if (!d) return;
    if (!trimEnd || trimEnd > d || dur !== d) trimEnd = d;
    dur = d;
  }
  function audioTracks(): AudioTracks | undefined {
    return (video as unknown as { audioTracks?: AudioTracks } | undefined)?.audioTracks;
  }
  // The player's volume and mute carry over from clip to clip (on this device).
  const VOLUME_KEY = 'clipr.player.volume';
  function restoreVolume(): void {
    if (!video) return;
    try {
      const saved = JSON.parse(localStorage.getItem(VOLUME_KEY) ?? 'null') as { volume?: number; muted?: boolean } | null;
      if (saved && typeof saved.volume === 'number') video.volume = Math.min(1, Math.max(0, saved.volume));
      if (saved && typeof saved.muted === 'boolean') video.muted = saved.muted;
    } catch {
      // Blocked storage or a bad value: the browser's default volume.
    }
  }
  function saveVolume(): void {
    if (!video) return;
    try {
      localStorage.setItem(VOLUME_KEY, JSON.stringify({ volume: video.volume, muted: video.muted }));
    } catch {
      // Private window: it just isn't remembered.
    }
  }

  function onMeta() {
    restoreVolume();
    setDur();
    // Chromium hides audioTracks behind a flag, so this may stay empty.
    const at = audioTracks();
    tracks = at
      ? Array.from({ length: at.length }, (_, i) => ({
          label: at[i].label || at[i].language || `Track ${i + 1}`,
          enabled: at[i].enabled,
        }))
      : [];
  }
  function pickTrack(i: number) {
    const at = audioTracks();
    if (!at) return;
    for (let k = 0; k < at.length; k++) at[k].enabled = k === i;
    tracks = tracks.map((t, k) => ({ ...t, enabled: k === i }));
  }

  // Timeline: pointer drag for seeking and for both trim handles.
  let drag: 'seek' | 'start' | 'end' | null = null;
  const clamp = (v: number, a: number, b: number) => Math.min(Math.max(v, a), b);
  const pct = (t: number) => (dur ? (clamp(t, 0, dur) / dur) * 100 : 0);
  function timeAt(e: PointerEvent): number {
    const r = tl!.getBoundingClientRect();
    return clamp((e.clientX - r.left) / r.width, 0, 1) * dur;
  }
  function down(e: PointerEvent, which: 'seek' | 'start' | 'end') {
    if (!dur) return;
    e.stopPropagation();
    drag = which;
    tl!.setPointerCapture(e.pointerId);
    move(e);
  }
  function move(e: PointerEvent) {
    if (!drag || !video) return;
    const t = timeAt(e);
    if (drag === 'seek') video.currentTime = t;
    else if (drag === 'start') {
      trimStart = clamp(t, 0, trimEnd - 0.5);
      video.currentTime = trimStart;
    } else {
      trimEnd = clamp(t, trimStart + 0.5, dur);
      video.currentTime = Math.max(trimEnd - 0.5, trimStart);
    }
  }
  const up = () => (drag = null);
  function nudge(e: KeyboardEvent, which: 'start' | 'end') {
    const d = e.key === 'ArrowLeft' ? -0.5 : e.key === 'ArrowRight' ? 0.5 : 0;
    if (!d) return;
    e.preventDefault();
    if (which === 'start') trimStart = clamp(trimStart + d, 0, trimEnd - 0.5);
    else trimEnd = clamp(trimEnd + d, trimStart + 0.5, dur);
  }

  const trimmed = $derived(dur > 0 && (trimStart > 0.05 || trimEnd < dur - 0.05));

  async function update(patch: { title?: string; favorite?: boolean; shelves?: string[] }) {
    if (!clip) return;
    busy = true;
    try {
      const old = clip.id;
      const c = await api.patchClip({ id: old, ...patch });
      clip = c;
      titleDraft = c.title;
      if (c.id !== old) replaceClipRoute(c.id);
      if (patch.shelves) api.getShelves().then((s) => (allShelves = s)).catch(() => {});
    } catch (e) {
      toastError(e);
      if (clip) titleDraft = clip.title;
    } finally {
      busy = false;
    }
  }
  function commitTitle() {
    const t = titleDraft.trim();
    if (!clip || !t || t === clip.title) {
      if (clip) titleDraft = clip.title;
      return;
    }
    void update({ title: t });
  }
  function addShelf() {
    const s = newShelf.trim();
    newShelf = '';
    if (!clip || !s || clip.shelves.some((x) => x.toLowerCase() === s.toLowerCase())) return;
    void update({ shelves: [...clip.shelves, s] });
  }

  async function remove() {
    if (!clip) return;
    if (!(await confirmDialog(`Delete "${clip.title}"? It goes to the Recycle Bin.`, 'Delete', true))) return;
    try {
      await api.deleteClip(clip.id);
      go('#/gallery');
    } catch (e) {
      toastError(e);
    }
  }

  async function trim(replace: boolean) {
    if (!clip) return;
    if (
      replace &&
      !(await confirmDialog(
        'Replace the original file with the trimmed version? This cannot be undone.',
        'Replace original',
        true,
      ))
    )
      return;
    busy = true;
    try {
      const old = clip.id;
      const c = await api.trimClip(old, trimStart, trimEnd, replace);
      toast('ok', replace ? 'Original replaced with the trimmed clip.' : 'Saved as a new clip.');
      if (replace) {
        clip = c;
        titleDraft = c.title;
        reloadKey++;
        dur = 0;
        trimStart = 0;
        trimEnd = 0;
        if (c.id !== old) replaceClipRoute(c.id);
      } else {
        go(clipHref(c.id));
      }
    } catch (e) {
      toastError(e);
    } finally {
      busy = false;
    }
  }

  const fileName = $derived(clip ? clip.id.split('/').pop() : '');
</script>

<p class="back"><a href="#/gallery" class="muted">← Gallery</a></p>

{#if missing && !clip}
  <div class="card">
    <p>This clip no longer exists.</p>
    <a class="btn" href="#/gallery">Back to the gallery</a>
  </div>
{:else if !clip}
  <p class="muted">Loading…</p>
{:else}
  <div class="head row">
    <input
      class="title"
      type="text"
      bind:value={titleDraft}
      aria-label="Title"
      onblur={commitTitle}
      onkeydown={(e) => {
        if (e.key === 'Enter') e.currentTarget.blur();
        if (e.key === 'Escape') {
          titleDraft = clip!.title;
          e.currentTarget.blur();
        }
      }}
    />
    <button
      class="star"
      class:on={clip.favorite}
      aria-label={clip.favorite ? 'Remove favorite' : 'Add favorite'}
      aria-pressed={clip.favorite}
      onclick={() => update({ favorite: !clip!.favorite })}>{clip.favorite ? '★' : '☆'}</button
    >
  </div>
  <p class="muted small">
    {clip.game} · {fmtDateTime(clip.created)} · {fmtBytes(clip.size_bytes)}{#if clip.width && clip.height}
      · {clip.width}×{clip.height}{/if}{clip.kind === 'recording' ? ' · recording' : ''}
  </p>

  {#key clip.url + reloadKey}
    <video
      bind:this={video}
      src={clip.url}
      controls
      preload="metadata"
      onloadedmetadata={onMeta}
      ondurationchange={setDur}
      ontimeupdate={() => (cur = video?.currentTime ?? 0)}
      onvolumechange={saveVolume}
    >
      <track kind="captions" />
    </video>
  {/key}

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="tl"
    bind:this={tl}
    onpointerdown={(e) => down(e, 'seek')}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={up}
  >
    <div class="track"></div>
    <div class="range" style:left="{pct(trimStart)}%" style:width="{pct(trimEnd) - pct(trimStart)}%"></div>
    {#each clip.markers as m (m)}
      <button
        class="mark"
        style:left="{pct(m)}%"
        title="Marker {fmtTime(m)}"
        aria-label="Seek to marker at {fmtTime(m)}"
        onpointerdown={(e) => e.stopPropagation()}
        onclick={() => video && (video.currentTime = m)}
      ></button>
    {/each}
    <div class="head-pos" style:left="{pct(cur)}%"></div>
    <div
      class="handle"
      style:left="{pct(trimStart)}%"
      role="slider"
      tabindex="0"
      aria-label="Trim start"
      aria-valuemin={0}
      aria-valuemax={dur}
      aria-valuenow={trimStart}
      onpointerdown={(e) => down(e, 'start')}
      onkeydown={(e) => nudge(e, 'start')}
    ></div>
    <div
      class="handle end"
      style:left="{pct(trimEnd)}%"
      role="slider"
      tabindex="0"
      aria-label="Trim end"
      aria-valuemin={0}
      aria-valuemax={dur}
      aria-valuenow={trimEnd}
      onpointerdown={(e) => down(e, 'end')}
      onkeydown={(e) => nudge(e, 'end')}
    ></div>
  </div>
  <div class="times muted small"><span>{fmtTime(cur)}</span><span>{fmtTime(dur)}</span></div>

  <section class="card stack2">
    <div>
      <h3>Trim</h3>
      <p class="muted small">
        Drag the two handles on the timeline. Trimming is lossless, so the start snaps back to the keyframe before it.
      </p>
      <div class="row">
        <span>Start <b>{fmtTimePrecise(trimStart)}</b></span>
        <span>End <b>{fmtTimePrecise(trimEnd)}</b></span>
        <span class="muted">Length {fmtTime(trimEnd - trimStart)}</span>
      </div>
      {#if trimmed}
        <p class="muted small snap">Starts at the keyframe before {fmtTimePrecise(trimStart)}.</p>
      {/if}
      <div class="row btns">
        <button class="primary" disabled={!trimmed || busy} onclick={() => trim(false)}>Save as new clip</button>
        <button class="danger" disabled={!trimmed || busy} onclick={() => trim(true)}>Replace original</button>
        <button
          class="ghost"
          disabled={!trimmed}
          onclick={() => {
            trimStart = 0;
            trimEnd = dur;
          }}>Reset</button
        >
      </div>
    </div>

    <div>
      <h3>Shelves</h3>
      <p class="muted small">Your own tags, to group clips (funny, clutches…).</p>
      <div class="row">
        {#each clip.shelves as s, i (s)}
          <span class="chip on"
            >{s}<button
              class="x"
              aria-label="Remove {s}"
              onclick={() => update({ shelves: clip!.shelves.filter((_, k) => k !== i) })}>×</button
            ></span
          >
        {/each}
        <form
          class="row"
          onsubmit={(e) => {
            e.preventDefault();
            addShelf();
          }}
        >
          <input type="text" list="shelf-list" placeholder="Add shelf" bind:value={newShelf} aria-label="New shelf" />
          <datalist id="shelf-list">{#each allShelves as s (s)}<option value={s}></option>{/each}</datalist>
          <button type="submit" disabled={!newShelf.trim()}>Add</button>
        </form>
      </div>
    </div>

    {#if tracks.length > 1}
      <div>
        <h3>Audio track</h3>
        <select onchange={(e) => pickTrack(+e.currentTarget.value)} aria-label="Audio track">
          {#each tracks as t, i (i)}<option value={i} selected={t.enabled}>{t.label}</option>{/each}
        </select>
      </div>
    {/if}

    <ShareDiscord id={clip.id} />
    <CloudShare id={clip.id} />

    <div class="row">
      <a class="btn" href={clip.url} download={fileName}>Download</a>
      <button onclick={() => api.reveal(clip!.id).catch(toastError)}>Show in folder</button>
      <span class="spacer"></span>
      <button class="danger" onclick={remove}>Delete</button>
    </div>
  </section>
{/if}

<style>
  .back { margin: 0 0 0.6rem; }
  .back a { text-decoration: none; }
  .head { flex-wrap: nowrap; }
  .title {
    flex: 1; font-size: 1.5rem; font-weight: 700; letter-spacing: -0.01em;
    background: transparent; border-color: transparent; padding: 0.2rem 0.5rem; margin-left: -0.5rem;
  }
  .title:hover, .title:focus { background: var(--surface-2); border-color: var(--border); }
  .star { font-size: 1.3rem; padding: 0.2rem 0.7rem; }
  .star.on { color: #fbbf24; }
  video { width: 100%; max-height: 70vh; background: #000; border-radius: var(--radius); display: block; }

  .tl { position: relative; height: 2.4rem; margin-top: 0.7rem; cursor: pointer; touch-action: none; user-select: none; }
  .track {
    position: absolute; left: 0; right: 0; top: 50%; height: 8px; margin-top: -4px;
    background: var(--surface-2); border: 1px solid var(--border); border-radius: 4px;
  }
  .range {
    position: absolute; top: 50%; height: 8px; margin-top: -4px; background: var(--accent-soft);
    border: 1px solid var(--accent-border); border-radius: 4px; pointer-events: none;
  }
  .head-pos { position: absolute; top: 4px; bottom: 4px; width: 2px; margin-left: -1px; background: var(--text); pointer-events: none; }
  .mark {
    position: absolute; top: 3px; width: 10px; height: 10px; margin-left: -5px; padding: 0;
    background: #fbbf24; border: 0; border-radius: 2px; transform: rotate(45deg);
  }
  .mark:hover:not(:disabled) { background: #fde68a; }
  .handle {
    position: absolute; top: 2px; bottom: 2px; width: 14px; margin-left: -14px; background: var(--accent);
    border-radius: 6px 0 0 6px; cursor: ew-resize;
  }
  .handle.end { margin-left: 0; border-radius: 0 6px 6px 0; }
  .times { display: flex; justify-content: space-between; margin-bottom: 1rem; }

  .stack2 { display: flex; flex-direction: column; gap: 1.4rem; }
  .btns { margin-top: 0.6rem; }
  .snap { margin: 0.4rem 0 0; }
  .x { background: none; border: 0; padding: 0 0.1rem; color: inherit; font-size: 1rem; line-height: 1; }
  .x:hover:not(:disabled) { background: none; color: var(--danger); }
</style>
