<script lang="ts">
  import { confirmState } from './store.svelte';
  let dlg: HTMLDialogElement | undefined = $state();

  $effect(() => {
    if (!dlg) return;
    if (confirmState.open && !dlg.open) dlg.showModal();
    if (!confirmState.open && dlg.open) dlg.close();
  });
  function done(v: boolean) {
    confirmState.open = false;
    confirmState.resolve?.(v);
    confirmState.resolve = null;
  }
</script>

<dialog
  bind:this={dlg}
  oncancel={(e) => {
    e.preventDefault();
    done(false);
  }}
>
  <p>{confirmState.text}</p>
  <div class="row end">
    <button onclick={() => done(false)}>Cancel</button>
    <button
      class:danger={confirmState.danger}
      class:solid={confirmState.danger}
      class:primary={!confirmState.danger}
      onclick={() => done(true)}>{confirmState.ok}</button
    >
  </div>
</dialog>

<style>
  dialog {
    background: var(--surface); color: var(--text); border: 1px solid var(--border);
    border-radius: 14px; padding: 1.4rem; max-width: 26rem; width: calc(100vw - 2rem);
  }
  dialog::backdrop { background: #000a; }
  .end { justify-content: flex-end; }
</style>
