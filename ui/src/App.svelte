<script lang="ts">
  import { router } from './lib/router.svelte';
  import { startConnection } from './lib/store.svelte';
  import TopBar from './lib/TopBar.svelte';
  import Home from './pages/Home.svelte';
  import Gallery from './pages/Gallery.svelte';
  import Settings from './pages/Settings.svelte';

  startConnection();
</script>

<TopBar />
<main>
  {#if router.route === 'gallery'}
    <Gallery />
  {:else if router.route === 'settings'}
    <Settings />
  {:else}
    <Home />
  {/if}
</main>

<style>
  main {
    max-width: 960px;
    margin: 0 auto;
    padding: 1.25rem 1rem 3rem;
  }
</style>
