<script lang="ts">
  import type { CloudStatus } from './api';

  // A clip's cloud state as a small icon: in the cloud, going up (with progress), waiting, or failed.
  let { status, big = false }: { status: CloudStatus | undefined; big?: boolean } = $props();

  const label = $derived(
    !status
      ? ''
      : status.state === 'uploaded'
        ? 'In the cloud'
        : status.state === 'uploading'
          ? `Uploading ${Math.round((status.progress ?? 0) * 100)}%`
          : status.state === 'queued'
            ? 'Waiting to upload'
            : `Upload failed${status.error ? `: ${status.error}` : ''}`,
  );
</script>

{#if status}
  <span class="badge {status.state}" class:big title={label} aria-label={label} role="img">
    <svg viewBox="0 0 24 24" width={big ? 18 : 14} height={big ? 18 : 14} aria-hidden="true">
      <path
        d="M7 18h10.5a4 4 0 0 0 .6-7.96A6 6 0 0 0 6.4 9.1 4.5 4.5 0 0 0 7 18Z"
        fill={status.state === 'uploaded' ? 'currentColor' : 'none'}
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linejoin="round"
      />
      {#if status.state === 'uploading' || status.state === 'queued'}
        <path d="M12 15.5v-5m-2.2 2.1L12 10.4l2.2 2.2" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
      {:else if status.state === 'failed'}
        <path d="M12 10.5v2.8m0 2.2v.1" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
      {/if}
    </svg>
    {#if status.state === 'uploading'}<span class="pct">{Math.round((status.progress ?? 0) * 100)}%</span>{/if}
  </span>
{/if}

<style>
  .badge {
    display: inline-flex; align-items: center; gap: .25rem; padding: .12rem .35rem; border-radius: 6px;
    background: #000b; color: #fff; font-size: .72rem; font-weight: 600; line-height: 1;
  }
  .badge.big { padding: .25rem .5rem; font-size: .8rem; }
  .uploaded { color: #7dd3fc; }
  .queued { opacity: .75; }
  .uploading { color: #fff; }
  .failed { color: var(--danger); }
  .pct { font-variant-numeric: tabular-nums; }
</style>
