<script lang="ts">
  import { onMount } from 'svelte';
  import { app } from './store.svelte';

  // A small live picture of what clipr captures, a few frames a second, like OBS's preview. It only runs
  // while the page is visible and the preview isn't hidden: clipr takes snapshots only while someone asks.
  let { hideable = false }: { hideable?: boolean } = $props();

  const KEY = 'clipr.preview.hidden';
  const FPS_KEY = 'clipr.preview.fps';
  const RATES = [2, 5, 15, 30];
  const RETRY_MS = 3000;

  let src = $state<string | null>(null);
  let hidden = $state(read());
  // Pictures a second, remembered on this device. Higher is smoother and costs clipr a little more.
  let fps = $state(readFps());

  function readFps(): number {
    try {
      const v = Number(localStorage.getItem(FPS_KEY));
      return RATES.includes(v) ? v : 5;
    } catch {
      return 5;
    }
  }

  function setFps(v: number) {
    fps = v;
    try {
      localStorage.setItem(FPS_KEY, String(v));
    } catch {
      /* private window: only for this visit */
    }
  }
  // Why there's no picture (the server said, or it can't be reached); empty when there's no problem.
  let problem = $state('');

  const capturing = $derived(app.status?.state === 'buffering' || app.status?.state === 'recording');

  function read(): boolean {
    try {
      return localStorage.getItem(KEY) === '1';
    } catch {
      return false;
    }
  }

  function setHidden(v: boolean) {
    hidden = v;
    try {
      localStorage.setItem(KEY, v ? '1' : '0');
    } catch {
      /* private window: only for this visit */
    }
  }

  function show(next: string | null) {
    if (src) URL.revokeObjectURL(src);
    src = next;
  }

  onMount(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    async function poll() {
      if (!alive) return;
      if ((hideable && hidden) || document.hidden || !capturing) {
        // Let the last picture go: it's stale by the time the preview shows again.
        show(null);
        timer = setTimeout(poll, 1000);
        return;
      }
      // Ask a little faster than the rate, so a picture is never missed.
      const started = performance.now();
      let wait = 0;
      try {
        const r = await fetch(`/api/preview?fps=${fps}`, { cache: 'no-store' });
        if (r.status === 200) {
          const blob = await r.blob();
          if (!alive) return;
          show(URL.createObjectURL(blob));
          problem = '';
        } else if (r.status === 204) {
          show(null);
          problem = '';
        } else {
          let msg = `The preview isn't available (${r.status}).`;
          try {
            const j = await r.json();
            if (j && typeof j.error === 'string') msg = j.error;
          } catch {
            /* no JSON body */
          }
          show(null);
          problem = msg;
          wait = RETRY_MS;
        }
      } catch {
        show(null);
        problem = 'Cannot reach clipr. Is it running?';
        wait = RETRY_MS;
      }
      if (!wait) wait = Math.max(10, 1000 / fps - (performance.now() - started));
      if (alive) timer = setTimeout(poll, wait);
    }
    poll();
    return () => {
      alive = false;
      clearTimeout(timer);
      show(null);
    };
  });
</script>

<div class="preview">
  <div class="bar">
    <span class="muted small">{hideable ? 'Live preview' : ''}</span>
    <span class="ctl">
      {#if !(hideable && hidden)}
        <select class="small" value={fps} onchange={(e) => setFps(+e.currentTarget.value)} aria-label="Preview pictures a second">
          {#each RATES as r (r)}<option value={r}>{r} fps</option>{/each}
        </select>
      {/if}
      {#if hideable}<button class="link small" onclick={() => setHidden(!hidden)}>{hidden ? 'Show' : 'Hide'}</button>{/if}
    </span>
  </div>
  {#if !(hideable && hidden)}
    <div class="screen" class:live={!!src}>
      {#if src}
        <img {src} alt="What clipr captures now" />
      {:else}
        <span class="muted small">
          {#if !capturing}
            Not capturing. The preview shows while the replay buffer or a recording runs.
          {:else if problem}
            {problem}
          {:else}
            Starting the preview…
          {/if}
        </span>
      {/if}
    </div>
  {/if}
</div>

<style>
  .preview { display: flex; flex-direction: column; gap: .4rem; }
  .bar { display: flex; justify-content: space-between; align-items: center; max-width: 640px; }
  .ctl { display: flex; align-items: center; gap: .6rem; }
  .ctl select { padding: .1rem .3rem; font-size: .8rem; }
  .link { background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; }
  .screen {
    width: 100%; max-width: 640px; aspect-ratio: 16 / 9;
    display: grid; place-items: center; text-align: center; padding: 1rem;
    background: #000; border: 1px solid var(--border); border-radius: .5rem; overflow: hidden;
  }
  .screen.live { padding: 0; }
  .screen img { width: 100%; height: 100%; object-fit: contain; display: block; }
</style>
