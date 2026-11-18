<script lang="ts">
  import { cancelUpload, removeFromCloud, uploadClip } from './api';
  import { app, confirmDialog, toast, toastError } from './store.svelte';
  import CloudBadge from './CloudBadge.svelte';

  // "Cloud" on a clip's page: upload it, copy its link, or take it out of the bucket.
  let { id }: { id: string } = $props();

  const cloud = $derived(app.config?.cloud);
  const ready = $derived(
    !!cloud && cloud.enabled && !!cloud.endpoint && !!cloud.bucket && !!cloud.access_key_id,
  );
  const status = $derived(app.cloud[id]);
  const job = $derived(app.uploads.find((u) => u.clip.id === id && (u.state === 'queued' || u.state === 'uploading')));
  let busy = $state(false);

  async function run(f: () => Promise<unknown>) {
    busy = true;
    try {
      await f();
    } catch (e) {
      toastError(e);
    } finally {
      busy = false;
    }
  }
  async function copy() {
    if (!status?.url) return;
    try {
      await navigator.clipboard.writeText(status.url);
      toast('ok', 'Link copied');
    } catch {
      toast('err', "Couldn't copy; select the link and copy it by hand");
    }
  }
  async function remove() {
    const ok = await confirmDialog(
      'Remove this clip from your bucket? Links to it stop working for everyone. The file on this PC stays.',
      'Remove from cloud',
      true,
    );
    if (ok) await run(() => removeFromCloud(id));
  }
</script>

<div>
  <h3>Cloud <CloudBadge {status} /></h3>
  {#if status?.state === 'uploaded'}
    {#if status.url}
      <div class="row">
        <input type="text" readonly value={status.url} aria-label="Public link" onfocus={(e) => e.currentTarget.select()} />
        <button onclick={copy}>Copy link</button>
      </div>
    {:else}
      <p class="muted small">In your bucket. Add a public address in Settings → Cloud to get a link you can share.</p>
    {/if}
    <div class="row"><button class="danger" disabled={busy} onclick={remove}>Remove from cloud</button></div>
  {:else if job}
    <div class="row">
      <span class="small">{job.state === 'uploading' ? `Uploading ${Math.round((status?.progress ?? 0) * 100)}%` : 'Waiting to upload'}</span>
      {#if job.state === 'queued' && !job.now}<button disabled={busy} onclick={() => run(() => uploadClip(id))}>Upload now</button>{/if}
      <button disabled={busy} onclick={() => run(() => cancelUpload(job.id))}>Cancel</button>
    </div>
  {:else if ready}
    {#if status?.state === 'failed'}<p class="small errt">{status.error ?? 'The upload failed.'}</p>{/if}
    <button disabled={busy} onclick={() => run(() => uploadClip(id))}>{status?.state === 'failed' ? 'Try again' : 'Upload to the cloud'}</button>
  {:else}
    <p class="muted small">Keep a copy in your own bucket and share links: set it up in <a href="#/settings">Settings → Cloud</a>.</p>
  {/if}
</div>

<style>
  h3 { display: flex; align-items: center; gap: 0.5rem; }
  p { margin: 0.4rem 0 0; }
  .row { margin-top: 0.4rem; gap: 0.5rem; flex-wrap: wrap; align-items: center; }
  input { flex: 1; min-width: 12rem; }
  .errt { color: var(--danger); }
</style>
