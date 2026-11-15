<script lang="ts">
  import { app } from './store.svelte';

  const view = $derived.by(() => {
    if (!app.connected) return { cls: 'down', text: 'Disconnected' };
    const s = app.status;
    if (!s) return { cls: 'idle', text: 'Connecting...' };
    if (s.state === 'recording') return { cls: 'rec', text: '● REC' };
    if (s.state === 'buffering') {
      return { cls: 'buf', text: s.game ? `● Buffering ${s.game}` : '● Buffering' };
    }
    if (s.paused) return { cls: 'idle', text: 'Buffer paused' };
    return { cls: 'idle', text: 'Idle' };
  });
</script>

<span class="pill {view.cls}" role="status">{view.text}</span>

<style>
  .pill {
    font-size: 0.8rem;
    font-weight: 600;
    padding: 0.2rem 0.7rem;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text-muted);
    max-width: 14rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .buf { color: var(--accent); background: var(--accent-soft); border-color: var(--accent-border); }
  .rec { color: var(--danger); background: var(--danger-soft); border-color: var(--danger-border); }
  .down { border-style: dashed; }
</style>
