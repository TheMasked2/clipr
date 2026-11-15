<script lang="ts">
  import { DEFAULT_ACCENT, loadAccent, resetAccent, saveAccent } from '../lib/accent';

  let accent = $state(loadAccent());

  function onInput(e: Event) {
    accent = (e.currentTarget as HTMLInputElement).value;
    saveAccent(accent);
  }
  function reset() {
    resetAccent();
    accent = DEFAULT_ACCENT;
  }
</script>

<h1>Settings</h1>
<section class="card">
  <h2>Appearance</h2>
  <label class="row">
    <span>Accent colour</span>
    <input type="color" value={accent} oninput={onInput} />
    <code class="muted">{accent}</code>
  </label>
  <button onclick={reset}>Reset</button>
  <p class="muted note">More settings arrive in later phases.</p>
</section>

<style>
  .row { display: flex; align-items: center; gap: 0.75rem; flex-wrap: wrap; margin-bottom: 1rem; }
  input[type='color'] {
    width: 2.5rem; height: 2rem; padding: 0; border: 1px solid var(--border);
    border-radius: 6px; background: var(--surface-2); cursor: pointer;
  }
  code { font-family: var(--mono); }
  button {
    font: inherit; color: var(--text); background: var(--surface-2);
    border: 1px solid var(--border); border-radius: 8px; padding: 0.35rem 0.9rem; cursor: pointer;
  }
  button:hover { border-color: var(--accent-border); background: var(--accent-soft); }
  .note { margin: 1rem 0 0; }
</style>
