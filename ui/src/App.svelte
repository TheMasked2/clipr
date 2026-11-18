<script lang="ts">
  import { router } from './lib/router.svelte';
  import { app, startConnection } from './lib/store.svelte';
  import TopBar from './lib/TopBar.svelte';
  import Toasts from './lib/Toasts.svelte';
  import ConfirmDialog from './lib/ConfirmDialog.svelte';
  import PinScreen from './lib/PinScreen.svelte';
  import Home from './pages/Home.svelte';
  import Gallery from './pages/Gallery.svelte';
  import Player from './pages/Player.svelte';
  import Settings from './pages/Settings.svelte';
  import { closeOverlay, inOverlay } from './lib/overlay';

  startConnection();

  // In the in-game overlay the page is a panel over the dimmed game: the page itself is see-through.
  if (inOverlay) document.documentElement.classList.add('overlay');
  const overlayKey = $derived(app.config?.hotkeys.overlay ?? '');

  /** Esc closes the overlay, unless it's closing something else first (a dialog, a menu, a hotkey box). */
  function onKey(e: KeyboardEvent) {
    if (!inOverlay || e.key !== 'Escape' || e.defaultPrevented) return;
    if (document.querySelector('dialog[open]')) return;
    const t = e.target as HTMLElement | null;
    if (t?.closest('input, textarea, select, [contenteditable="true"]')) return;
    closeOverlay();
  }
</script>

<svelte:window onkeydown={onKey} />

{#snippet pages()}
  <TopBar />
  <!-- Home (an overview in two columns) and the Gallery (filters beside the clips) get more room. -->
  <main class:wide={(router.route === 'home' || router.route === 'gallery')}>
    {#if router.route === 'gallery'}
      <Gallery />
    {:else if router.route === 'clip'}
      <!-- Not keyed: Player reloads by itself when the clip id changes, and a rename keeps the page. -->
      <Player />
    {:else if router.route === 'settings'}
      <Settings />
    {:else}
      <Home />
    {/if}
  </main>
{/snippet}

{#if app.locked}
  <!-- Another device without a session: only the PIN screen, nothing that would call the API. -->
  <PinScreen />
{:else if inOverlay}
  <!-- A click beside the panel closes the overlay, like Esc. -->
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="ov-backdrop" onclick={(e) => e.target === e.currentTarget && closeOverlay()}>
    <div class="ov-panel" role="dialog" aria-label="clipr overlay">
      <button class="ov-close" onclick={closeOverlay} title="Back to the game (Esc{overlayKey ? ` or ${overlayKey}` : ''})">
        Back to game {#if overlayKey}<kbd>{overlayKey}</kbd>{/if} ×
      </button>
      {@render pages()}
    </div>
  </div>
{:else}
  {@render pages()}
  <!-- Not under the PIN screen: its own refused calls would only stack "enter the PIN" notes. -->
  <Toasts />
{/if}
<ConfirmDialog />

<style>
  main {
    max-width: 960px;
    margin: 0 auto;
    padding: 1.25rem 1rem 3rem;
  }
  main.wide {
    max-width: 1280px;
  }
  /* The overlay: the game shows dimmed around a panel with the usual pages in it. */
  :global(html.overlay), :global(html.overlay body) { background: transparent; }
  .ov-backdrop { position: fixed; inset: 0; background: rgb(5 7 10 / 72%); display: grid; place-items: center; }
  .ov-panel {
    position: relative; width: min(1400px, 92vw); height: 90vh; overflow: auto; background: var(--bg);
    border: 1px solid var(--border); border-radius: 14px; box-shadow: 0 20px 60px #000a;
  }
  .ov-close {
    position: sticky; top: .6rem; float: right; z-index: 20; margin: .6rem .8rem 0 0; display: inline-flex;
    align-items: center; gap: .45rem; font-size: .85rem;
  }
  .ov-close kbd { font-size: .75rem; }
</style>
