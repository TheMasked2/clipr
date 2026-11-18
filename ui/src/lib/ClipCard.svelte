<script lang="ts">
  import { patchClip, type Clip } from './api';
  import { fmtBytes, fmtDateTime, fmtTime } from './format';
  import { clipHref } from './router.svelte';
  import { app, toastError } from './store.svelte';
  import CloudBadge from './CloudBadge.svelte';

  let { clip, highlight = false, onupdate, showGame = true }: { clip: Clip; highlight?: boolean; onupdate?: (c: Clip) => void; showGame?: boolean } = $props();

  let frame = $state(-1);

  function scrub(e: MouseEvent) {
    if (!clip.sprite || clip.sprite_frames < 2) return;
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const x = Math.min(Math.max((e.clientX - r.left) / r.width, 0), 0.9999);
    frame = Math.floor(x * clip.sprite_frames);
  }

  async function star(e: Event) {
    e.preventDefault();
    e.stopPropagation();
    try {
      onupdate?.(await patchClip({ id: clip.id, favorite: !clip.favorite }));
    } catch (err) {
      toastError(err);
    }
  }
</script>

<a class="card clip" class:highlight href={clipHref(clip.id)}>
  <div class="media" onmousemove={scrub} onmouseleave={() => (frame = -1)} role="presentation">
    {#if clip.thumb}
      <img src={clip.thumb} alt="" loading="lazy" />
    {:else}
      <video src={clip.url + '#t=0.5'} preload="metadata" muted tabindex="-1"></video>
    {/if}
    {#if clip.sprite && frame >= 0}
      <div
        class="sprite"
        style:background-image="url({clip.sprite})"
        style:background-size="{clip.sprite_frames * 100}% 100%"
        style:background-position="{(frame / (clip.sprite_frames - 1)) * 100}% 0"
      ></div>
    {/if}
    {#if clip.duration_secs != null}<span class="dur">{fmtTime(clip.duration_secs)}</span>{/if}
    {#if clip.kind === 'recording'}<span class="kind">REC</span>{/if}
    {#if app.cloud[clip.id]}<span class="cloud"><CloudBadge status={app.cloud[clip.id]} /></span>{/if}
    <button class="star" class:on={clip.favorite} aria-label={clip.favorite ? 'Remove favorite' : 'Add favorite'} onclick={star}>{clip.favorite ? '★' : '☆'}</button>
    {#if frame >= 0 && clip.sprite}
      <div class="bar"><i style:width="{((frame + 1) / clip.sprite_frames) * 100}%"></i></div>
    {/if}
  </div>
  <div class="info">
    <div class="title" title={clip.title}>{clip.title}</div>
    <div class="meta">
      {#if showGame}<span>{clip.game}</span><span>·</span>{/if}<span>{fmtDateTime(clip.created)}</span><span>·</span><span>{fmtBytes(clip.size_bytes)}</span>
    </div>
  </div>
</a>

<style>
  .clip { padding: 0; overflow: hidden; display: block; text-decoration: none; transition: border-color .12s, transform .12s; }
  .clip:hover { border-color: var(--accent-border); }
  .highlight { animation: glow 2.5s ease-out; }
  @keyframes glow { 0% { box-shadow: 0 0 0 2px var(--accent), 0 0 24px var(--accent); } 100% { box-shadow: 0 0 0 0 transparent; } }
  .media { position: relative; aspect-ratio: 16 / 9; background: #000; }
  img, video, .sprite { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; }
  .sprite { background-repeat: no-repeat; background-color: #000; }
  .dur, .kind {
    position: absolute; bottom: .4rem; right: .4rem; font-size: .75rem; font-weight: 600;
    background: #000b; color: #fff; padding: .05rem .4rem; border-radius: 5px;
  }
  .kind { right: auto; left: .4rem; background: var(--danger); }
  .cloud { position: absolute; top: .4rem; left: .4rem; display: flex; }
  .star {
    position: absolute; top: .3rem; right: .3rem; padding: .1rem .45rem; font-size: 1.1rem; line-height: 1.2;
    background: #000a; border-color: transparent; color: #fff; opacity: 0; transition: opacity .12s;
  }
  .clip:hover .star, .star.on, .star:focus-visible { opacity: 1; }
  .star.on { color: #fbbf24; }
  .bar { position: absolute; left: 0; right: 0; bottom: 0; height: 3px; background: #fff3; }
  .bar i { display: block; height: 100%; background: var(--accent); }
  .info { padding: .65rem .8rem .8rem; }
  .title { font-weight: 600; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .meta { color: var(--text-muted); font-size: .8rem; display: flex; gap: .3rem; flex-wrap: wrap; }
</style>
