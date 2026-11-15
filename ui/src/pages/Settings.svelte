<script lang="ts">
  import { app } from '../lib/store.svelte';
  import SettingsForm from '../lib/SettingsForm.svelte';
</script>

{#if app.config}
  <SettingsForm initial={app.config} />
{:else}
  <h1>Settings</h1>
  <p class="muted">Loading…</p>
{/if}
