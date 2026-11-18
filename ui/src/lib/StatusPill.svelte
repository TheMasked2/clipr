<script lang="ts">
  import { app } from './store.svelte';
  import { fmtTime } from './format';

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

  // The live stream has its own pill, with how long it's been live.
  const stream = $derived(app.connected ? app.status?.stream : null);
  let now = $state(Date.now() / 1000);
  $effect(() => {
    if (stream?.state !== 'live') return;
    now = Date.now() / 1000;
    const t = setInterval(() => (now = Date.now() / 1000), 1000);
    return () => clearInterval(t);
  });
  const live = $derived.by(() => {
    if (!stream) return null;
    switch (stream.state) {
      case 'live':
        return { cls: 'live', text: `● LIVE ${stream.since ? fmtTime(now - stream.since) : ''}`.trim(), title: 'Streaming' };
      case 'connecting':
        return { cls: 'going', text: 'Going live…', title: 'Connecting to the stream server' };
      case 'reconnecting':
        return { cls: 'warnp', text: 'Stream reconnecting…', title: stream.error ?? '' };
      default:
        return { cls: 'warnp', text: 'Stream failed', title: stream.error ?? '' };
    }
  });
</script>

<span class="pill {view.cls}" role="status">{view.text}</span>
{#if live}<span class="pill {live.cls} gap" role="status" title={live.title}>{live.text}</span>{/if}
{#if app.status?.mic_muted}<span class="pill muted" role="status" title="Your microphone is muted in clips (not in your voice chat).">Mic muted</span>{/if}

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
  .muted { color: #f5b942; border-color: #f5b94266; margin-left: 0.4rem; }
  .gap { margin-left: 0.4rem; }
  .live { color: #fff; background: var(--danger); border-color: var(--danger); font-variant-numeric: tabular-nums; }
  .going { color: var(--danger); background: var(--danger-soft); border-color: var(--danger-border); }
  .warnp { color: #f5b942; border-color: #f5b94266; }
</style>
