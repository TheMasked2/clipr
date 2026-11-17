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

  startConnection();
</script>

{#if app.locked}
  <!-- Another device without a session: only the PIN screen, nothing that would call the API. -->
  <PinScreen />
{:else}
  <TopBar />
  <main>
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
</style>
