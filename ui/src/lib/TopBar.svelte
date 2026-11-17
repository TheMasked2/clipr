<script lang="ts">
  import { router } from './router.svelte';
  import StatusPill from './StatusPill.svelte';

  const links = [
    { route: 'home', label: 'Home', href: '#/' },
    { route: 'gallery', label: 'Gallery', href: '#/gallery' },
    { route: 'settings', label: 'Settings', href: '#/settings' },
  ] as const;
  const GUIDE = 'https://github.com/TheMasked2/clipr/blob/main/docs/guide/README.md';
</script>

<header>
  <div class="inner">
    <a class="brand" href="#/">clipr</a>
    <StatusPill />
    <nav>
      {#each links as l (l.route)}
        <a
          href={l.href}
          class:active={router.route === l.route || (l.route === 'gallery' && router.route === 'clip')}
          aria-current={router.route === l.route ? 'page' : undefined}>{l.label}</a
        >
      {/each}
      <a href={GUIDE} target="_blank" rel="noopener noreferrer" title="The user guide, on GitHub">Help</a>
    </nav>
  </div>
</header>

<style>
  header {
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    position: sticky;
    top: 0;
    z-index: 1;
  }
  .inner {
    max-width: 960px;
    margin: 0 auto;
    padding: 0.6rem 1rem;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem 0.9rem;
  }
  .brand {
    font-weight: 800;
    font-size: 1.25rem;
    letter-spacing: -0.02em;
    text-decoration: none;
  }
  nav {
    margin-left: auto;
    display: flex;
    gap: 0.25rem;
  }
  nav a {
    text-decoration: none;
    color: var(--text-muted);
    padding: 0.35rem 0.7rem;
    border-radius: 8px;
  }
  nav a:hover { color: var(--text); background: var(--surface-2); }
  nav a.active { color: var(--text); background: var(--accent-soft); }
</style>
