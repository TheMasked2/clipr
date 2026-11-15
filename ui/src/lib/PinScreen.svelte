<script lang="ts">
  import { ApiError, login } from './api';
  let pin = $state('');
  let error = $state('');
  let busy = $state(false);

  async function submit(e: Event) {
    e.preventDefault();
    if (!/^\d{6}$/.test(pin)) {
      error = 'Enter the 6 digit PIN.';
      return;
    }
    busy = true;
    error = '';
    try {
      await login(pin);
      location.reload();
    } catch (err) {
      if (err instanceof ApiError && err.status === 429) error = 'Too many tries, wait a minute.';
      else if (err instanceof ApiError && err.status === 401) error = 'Wrong PIN.';
      else error = err instanceof Error ? err.message : String(err);
      busy = false;
    }
  }
</script>

<div class="wrap">
  <form class="card" onsubmit={submit}>
    <div class="brand">clipr</div>
    <p class="muted">This clipr is protected. Enter the 6 digit PIN shown in Settings → Network on the PC.</p>
    <input
      type="password"
      inputmode="numeric"
      autocomplete="one-time-code"
      maxlength="6"
      placeholder="••••••"
      bind:value={pin}
      aria-label="PIN"
    />
    {#if error}<div class="banner err">{error}</div>{/if}
    <button class="primary big" disabled={busy}>Unlock</button>
  </form>
</div>

<style>
  .wrap { min-height: 100vh; display: grid; place-items: center; padding: 1rem; }
  form { width: min(22rem, 100%); display: flex; flex-direction: column; gap: 1rem; padding: 1.75rem; }
  .brand { font-weight: 800; font-size: 1.6rem; letter-spacing: -0.02em; }
  p { margin: 0; }
  input { font-size: 1.8rem; letter-spacing: 0.5em; text-align: center; }
</style>
