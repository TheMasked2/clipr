<script lang="ts">
  let { items = $bindable([]), placeholder = 'game.exe' }: { items?: string[]; placeholder?: string } = $props();
  let text = $state('');
  function add() {
    const v = text.trim();
    if (v && !items.some((i) => i.toLowerCase() === v.toLowerCase())) items.push(v);
    text = '';
  }
</script>

<div class="list">
  {#if items.length}
    <div class="chips">
      {#each items as it, i (it)}
        <span class="chip on">{it}<button class="x" aria-label="Remove {it}" onclick={() => items.splice(i, 1)}>×</button></span>
      {/each}
    </div>
  {/if}
  <form onsubmit={(e) => { e.preventDefault(); add(); }} class="row">
    <input type="text" bind:value={text} {placeholder} />
    <button type="submit" disabled={!text.trim()}>Add</button>
  </form>
</div>

<style>
  .list { display: flex; flex-direction: column; gap: .6rem; width: 100%; }
  .chips { display: flex; flex-wrap: wrap; gap: .4rem; }
  .x { background: none; border: 0; padding: 0 .1rem; color: inherit; font-size: 1rem; line-height: 1; }
  .x:hover:not(:disabled) { background: none; color: var(--danger); }
  input { flex: 1; min-width: 8rem; }
</style>
