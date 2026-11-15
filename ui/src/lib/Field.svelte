<script lang="ts">
  import type { Snippet } from 'svelte';
  let { title, hint = '', stacked = false, error = false, children }: { title: string; hint?: string; stacked?: boolean; error?: boolean; children: Snippet } = $props();
</script>

<div class="field" class:stacked class:error>
  <div class="text">
    <div class="title">{title}</div>
    {#if hint}<div class="hint">{hint}</div>{/if}
  </div>
  <div class="ctl">{@render children()}</div>
</div>

<style>
  .field {
    display: flex; align-items: center; justify-content: space-between; gap: 1.25rem;
    padding: .9rem 0; border-bottom: 1px solid var(--border);
  }
  .field:last-child { border-bottom: 0; }
  .stacked { flex-direction: column; align-items: stretch; gap: .6rem; }
  .text { min-width: 0; flex: 1; }
  .title { font-weight: 600; }
  .hint { color: var(--text-muted); font-size: .85rem; margin-top: .1rem; }
  .ctl { display: flex; align-items: center; gap: .6rem; flex-wrap: wrap; justify-content: flex-end; }
  .stacked .ctl { justify-content: flex-start; }
  .error { background: var(--danger-soft); margin: 0 -.75rem; padding-left: .75rem; padding-right: .75rem; border-radius: 9px; }
  @media (max-width: 560px) {
    .field:not(.stacked) { flex-wrap: wrap; }
    .field:not(.stacked) .ctl { justify-content: flex-start; }
  }
</style>
