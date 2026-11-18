<script lang="ts">
  import { onMount } from 'svelte';
  import { app } from './store.svelte';

  // A small live picture of what clipr captures, a few frames a second, like OBS's preview. It only runs
  // while the page is visible and the preview isn't hidden: clipr takes snapshots only while someone asks.
  let { hideable = false }: { hideable?: boolean } = $props();

  const KEY = 'clipr.preview.hidden';
  const POLL_MS = 250;
  const RETRY_MS = 3000;

  let src = $state<string | null>(null);
  let hidden = $state(read());
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
      let wait = POLL_MS;
      try {
        const r = await fetch('/api/preview', { cache: 'no-store' });
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
  {#if hideable}
    <div class="bar">
      <span class="muted small">Live preview</span>
      <button class="link small" onclick={() => setHidden(!hidden)}>{hidden ? 'Show' : 'Hide'}</button>
    </div>
  {/if}
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
  .bar { display: flex; justify-content: space-between; align-items: baseline; }
  .link { background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; }
  .screen {
    width: 100%; max-width: 640px; aspect-ratio: 16 / 9;
    display: grid; place-items: center; text-align: center; padding: 1rem;
    background: #000; border: 1px solid var(--border); border-radius: .5rem; overflow: hidden;
  }
  .screen.live { padding: 0; }
  .screen img { width: 100%; height: 100%; object-fit: contain; display: block; }
</style>
