<script lang="ts">
  import * as api from './api';
  import { toastError } from './store.svelte';

  // A folder path: typed, or picked with the Windows folder picker on the PC itself. Another device can't
  // change folders (the server refuses), so there it's read-only.
  let {
    value = $bindable(),
    placeholder = '',
    title = 'Pick a folder',
    label,
    remote = false,
  }: { value: string; placeholder?: string; title?: string; label: string; remote?: boolean } = $props();

  let picking = $state(false);

  async function browse() {
    picking = true;
    try {
      const r = await api.pickFolder(title, value || '');
      if (r.path) value = r.path;
    } catch (e) {
      toastError(e);
    } finally {
      picking = false;
    }
  }
</script>

<div class="folder">
  <input
    type="text"
    bind:value
    {placeholder}
    aria-label={label}
    readonly={remote}
    title={remote ? 'Folders can only be changed on the PC clipr runs on.' : undefined}
  />
  {#if !remote}
    <button type="button" onclick={browse} disabled={picking}>{picking ? 'Picking…' : 'Browse…'}</button>
  {/if}
</div>
{#if picking}<p class="muted small">The folder picker is open on the PC; it may be behind this window.</p>{/if}

<style>
  .folder { display: flex; gap: 0.5rem; width: 100%; }
  .folder input { flex: 1 1 auto; min-width: 0; }
  .folder button { flex: none; }
  p { margin: 0.3rem 0 0; }
</style>
