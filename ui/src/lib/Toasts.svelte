<script lang="ts">
  import { toasts, dismissToast } from './store.svelte';
</script>

<div class="stack" aria-live="polite">
  {#each toasts as t (t.id)}
    <div class="toast" class:err={t.kind === 'err'}>
      <span class="txt">{t.text}</span>
      {#if t.href}<a href={t.href} onclick={() => dismissToast(t.id)}>{t.linkText ?? 'Open'}</a>{/if}
      <button class="ghost" aria-label="Dismiss" onclick={() => dismissToast(t.id)}>×</button>
    </div>
  {/each}
</div>

<style>
  .stack {
    position: fixed; right: 1rem; bottom: 1rem; display: flex; flex-direction: column; gap: 0.5rem;
    z-index: 50; max-width: min(24rem, calc(100vw - 2rem));
  }
  .toast {
    display: flex; align-items: center; gap: 0.7rem; padding: 0.65rem 0.8rem 0.65rem 1rem;
    background: var(--surface-2); border: 1px solid var(--accent-border); border-radius: 10px;
    box-shadow: 0 8px 30px #0008; animation: in 0.2s ease-out;
  }
  .toast.err { border-color: var(--danger-border); background: color-mix(in srgb, var(--danger) 14%, var(--surface-2)); }
  .txt { flex: 1; min-width: 0; overflow-wrap: anywhere; font-size: 0.9rem; }
  a { color: var(--accent); font-weight: 600; text-decoration: none; }
  .ghost { padding: 0 0.3rem; font-size: 1.1rem; }
  @keyframes in { from { transform: translateY(8px); opacity: 0; } }
</style>
