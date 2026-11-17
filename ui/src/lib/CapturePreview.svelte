<script lang="ts">
  import { onMount } from 'svelte';
  import { app } from './store.svelte';

  // A small live picture of what clipr captures, a few frames a second, like OBS's preview. It only runs
  // while the page is visible and the preview isn't hidden: clipr takes snapshots only while someone asks.
  let { hideable = false }: { hideable?: boolean } = $props();

  const KEY = 'clipr.preview.hidden';
  const POLL_MS = 250;

  let src = $state<string | null>(null);
  let hidden = $state(read());
  let failed = $state(false);

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
        if (!capturing) show(null);
        timer = setTimeout(poll, 1000);
        return;
      }
      try {
        const r = await fetch('/api/preview', { cache: 'no-store' });
        if (!alive) return;
        if (r.status === 200) {
          show(URL.createObjectURL(await r.blob()));
          failed = false;
        } else if (r.status === 204) {
          show(null);
        } else {
          failed = true;
        }
      } catch {
        failed = true;
      }
      if (alive) timer = setTimeout(poll, POLL_MS);
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
          {:else if failed}
            Can't load the preview.
          {:else}
            Waiting for the screen to change…
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
