<script lang="ts">
  import type { Snippet } from 'svelte';

  // One panel of Home, like an OBS dock: a title bar (with optional tools on the right) over a body. Every dock
  // has the same frame, so the page reads as one grid.
  let {
    title,
    tools,
    children,
    flush = false,
    menus = false,
  }: { title: string; tools?: Snippet; children: Snippet; flush?: boolean; menus?: boolean } = $props();
  // `menus`: the body's pop-up menus may reach outside the dock (it doesn't scroll then).

  const id = $derived(`dock-${title.toLowerCase().replace(/\W+/g, '-')}`);
</script>

<section class="dock" class:menus aria-labelledby={id}>
  <header>
    <h2 {id}>{title}</h2>
    {#if tools}<div class="tools">{@render tools()}</div>{/if}
  </header>
  <div class="body" class:flush class:menus>{@render children()}</div>
</section>

<style>
  .dock {
    display: flex; flex-direction: column; min-width: 0; min-height: 0;
    background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius); overflow: hidden;
  }
  header {
    display: flex; align-items: center; justify-content: space-between; gap: .5rem;
    padding: .3rem .7rem; background: var(--surface-2); border-bottom: 1px solid var(--border);
  }
  h2 { margin: 0; font-size: .78rem; font-weight: 700; text-transform: uppercase; letter-spacing: .06em; color: var(--text-muted); }
  .tools { display: flex; align-items: center; gap: .4rem; }
  .body { flex: 1; min-height: 0; overflow: auto; padding: .6rem .7rem; }
  .body.flush { padding: 0; }
  .dock.menus, .body.menus { overflow: visible; }
  .dock.menus header { border-radius: var(--radius) var(--radius) 0 0; }
</style>
