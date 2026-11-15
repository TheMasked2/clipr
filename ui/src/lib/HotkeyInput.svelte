<script lang="ts">
  import { capture, isBare } from './hotkey';

  let {
    value = $bindable(''),
    conflict = false,
  }: { value?: string; conflict?: boolean } = $props();
  let listening = $state(false);
  let msg = $state('');

  function onKey(e: KeyboardEvent) {
    if (!listening) return;
    e.preventDefault();
    e.stopPropagation();
    const r = capture(e);
    if (r.kind === 'wait') return;
    if (r.kind === 'cancel') {
      listening = false;
      msg = '';
    } else if (r.kind === 'error') {
      msg = r.message;
    } else {
      value = r.value;
      listening = false;
      msg = '';
    }
  }
</script>

<div class="hk">
  <button
    type="button"
    class="box"
    class:listening
    class:conflict
    onclick={() => {
      listening = true;
      msg = '';
    }}
    onkeydown={onKey}
    onblur={() => {
      listening = false;
      msg = '';
    }}
  >
    {#if listening}
      Press keys… (Esc cancels)
    {:else if value}
      <kbd>{value}</kbd>
    {:else}
      <span class="muted">Not set</span>
    {/if}
  </button>
  {#if value}
    <button
      type="button"
      class="ghost"
      aria-label="Clear hotkey"
      onclick={() => {
        value = '';
        msg = '';
      }}>×</button
    >
  {/if}
  {#if msg}
    <div class="m err">{msg}</div>
  {:else if value && isBare(value)}
    <div class="m">A bare key is lost to games while they run; add Ctrl or Alt.</div>
  {/if}
</div>

<style>
  .hk { display: flex; align-items: center; gap: 0.3rem; flex-wrap: wrap; justify-content: flex-end; }
  .box { min-width: 11rem; justify-content: center; }
  .listening { border-color: var(--accent); background: var(--accent-soft); }
  .conflict { border-color: var(--danger); color: var(--danger); background: var(--danger-soft); }
  .conflict kbd { color: var(--danger); border-color: var(--danger-border); }
  .m { flex-basis: 100%; font-size: 0.8rem; color: var(--text-muted); text-align: right; }
  .m.err { color: var(--danger); }
</style>
