<script lang="ts">
  import { untrack } from 'svelte';
  import type { CloudConfig, CloudProvider, UploadTiming } from './api';
  import { testCloud } from './api';
  import Field from './Field.svelte';
  import Toggle from './Toggle.svelte';

  // Your own bucket. `cloud` is the settings draft, changed in place. `remote`: this page is open on another
  // device, which never sees the secret and can't change the bucket.
  let { cloud, remote }: { cloud: CloudConfig; remote: boolean } = $props();

  const PROVIDERS: { v: CloudProvider; name: string; text: string }[] = [
    { v: 'r2', name: 'Cloudflare R2', text: 'Recommended: 10 GB free, and no fees when people watch your clips.' },
    { v: 'b2', name: 'Backblaze B2', text: '10 GB free; cheap storage.' },
    { v: 's3', name: 'Amazon S3', text: 'Pay for storage and for every view.' },
    { v: 'other', name: 'Other', text: 'Any S3-compatible service (MinIO, Wasabi, …).' },
  ];
  const TIMINGS: { v: UploadTiming; name: string; text: string }[] = [
    { v: 'after_game', name: 'After the game', text: 'New clips go up once the game closes, so uploading never costs frames.' },
    { v: 'now', name: 'Right away', text: 'New clips go up as soon as they are saved. Set a speed limit so games stay smooth.' },
    { v: 'manual', name: 'Only by hand', text: 'Only clips you upload from their page.' },
  ];
  const PLACEHOLDER: Record<CloudProvider, { endpoint: string; region: string; public: string }> = {
    r2: { endpoint: 'https://<account id>.r2.cloudflarestorage.com', region: 'auto', public: 'https://pub-….r2.dev' },
    b2: { endpoint: 'https://s3.us-west-004.backblazeb2.com', region: 'us-west-004', public: 'https://s3.us-west-004.backblazeb2.com/<bucket>' },
    s3: { endpoint: 'https://s3.eu-west-1.amazonaws.com', region: 'eu-west-1', public: 'https://<bucket>.s3.eu-west-1.amazonaws.com' },
    other: { endpoint: 'https://s3.example.com', region: 'us-east-1', public: 'https://clips.example.com' },
  };

  const filled = $derived(
    !!cloud.endpoint.trim() && !!cloud.bucket.trim() && !!cloud.access_key_id.trim() && (remote || !!cloud.secret_access_key),
  );
  // Open at first while nothing is filled in; after that it's the user's to open or close.
  let stepsOpen = $state(untrack(() => !filled && !remote));
  let testing = $state(false);
  let result = $state<{ ok: boolean; text: string } | null>(null);

  function pick(p: CloudProvider) {
    cloud.provider = p;
    if (p === 'r2') cloud.region = 'auto';
    else if (cloud.region === 'auto') cloud.region = '';
  }
  // B2 and S3 endpoints name the region: fill it in.
  function endpointChanged() {
    const m = cloud.endpoint.match(/^https:\/\/s3\.([a-z0-9-]+)\.(backblazeb2|amazonaws)\.com/);
    if (m) cloud.region = m[1];
  }
  async function test() {
    testing = true;
    result = null;
    try {
      result = { ok: true, text: (await testCloud($state.snapshot(cloud) as CloudConfig)).message };
    } catch (e) {
      result = { ok: false, text: e instanceof Error ? e.message : String(e) };
    } finally {
      testing = false;
    }
  }
</script>

<p class="muted small note">
  Copy clips to your own bucket, so they're safe if this PC isn't, and so Discord gets a link to the full clip instead
  of a smaller copy. clipr has no server: it talks to your bucket with your key. The files on this PC stay.
</p>
{#if remote}
  <div class="banner warn">The bucket can only be set up or changed on the PC running clipr. Here you can turn uploads on or off and change when they go.</div>
{/if}

<Field title="Service" stacked>
  <div class="row chips">
    {#each PROVIDERS as p (p.v)}
      <button class="chip" class:on={cloud.provider === p.v} onclick={() => pick(p.v)} disabled={remote}>{p.name}</button>
    {/each}
  </div>
  <p class="muted small">{PROVIDERS.find((p) => p.v === cloud.provider)?.text}</p>
</Field>

<details class="steps" bind:open={stepsOpen}>
  <summary>How to set it up</summary>
  {#if cloud.provider === 'r2'}
    <ol>
      <li>In the <a href="https://dash.cloudflare.com/?to=/:account/r2/overview" target="_blank" rel="noreferrer">Cloudflare dashboard</a>, open <b>R2 Object Storage</b> and click <b>Create bucket</b>. Name it, for example <code>clipr-clips</code>.</li>
      <li>In the bucket's <b>Settings</b>, under <b>Public access</b>, allow the <b>R2.dev subdomain</b> (or connect your own domain). Copy that address into <b>Public address</b> below.</li>
      <li>Back in R2, open <b>Manage API tokens</b> → <b>Create API token</b>. Pick <b>Object Read &amp; Write</b> and apply it to <b>this bucket only</b>, then create it.</li>
      <li>Copy the <b>Access Key ID</b>, the <b>Secret Access Key</b> and the endpoint (<code>https://…r2.cloudflarestorage.com</code>) into the fields below. Cloudflare shows the secret only once.</li>
      <li>Click <b>Test</b>, turn on <b>Copy clips to the cloud</b>, and Save.</li>
    </ol>
  {:else if cloud.provider === 'b2'}
    <ol>
      <li>In <a href="https://secure.backblaze.com/b2_buckets.htm" target="_blank" rel="noreferrer">Backblaze B2</a>, create a <b>Public</b> bucket.</li>
      <li>The bucket shows its <b>Endpoint</b> (<code>s3.us-west-004.backblazeb2.com</code>): put <code>https://</code> in front of it below. The public address is that endpoint plus <code>/</code> and the bucket name.</li>
      <li>Under <b>Application Keys</b>, add a key for <b>this bucket only</b> with <b>Read and Write</b>. Copy the <b>keyID</b> and the <b>applicationKey</b> below (B2 shows it only once).</li>
      <li>Click <b>Test</b>, turn on <b>Copy clips to the cloud</b>, and Save.</li>
    </ol>
  {:else if cloud.provider === 's3'}
    <ol>
      <li>Create a bucket in the <a href="https://console.aws.amazon.com/s3/" target="_blank" rel="noreferrer">S3 console</a>. For links in Discord, allow public reads with a bucket policy for <code>s3:GetObject</code>.</li>
      <li>In IAM, make a user with <code>s3:PutObject</code>, <code>s3:GetObject</code>, <code>s3:DeleteObject</code> and <code>s3:AbortMultipartUpload</code> on that bucket, and create an access key for it.</li>
      <li>The endpoint is <code>https://s3.&lt;region&gt;.amazonaws.com</code>; the public address <code>https://&lt;bucket&gt;.s3.&lt;region&gt;.amazonaws.com</code>.</li>
      <li>Click <b>Test</b>, turn on <b>Copy clips to the cloud</b>, and Save.</li>
    </ol>
  {:else}
    <ol>
      <li>Create a bucket and a key that may read, write and delete in it.</li>
      <li>Fill in the service's S3 endpoint and region, and the bucket's public address if it has one.</li>
      <li>Click <b>Test</b>, turn on <b>Copy clips to the cloud</b>, and Save.</li>
    </ol>
  {/if}
</details>

<div class="grid">
  <label>Endpoint
    <input type="text" bind:value={cloud.endpoint} oninput={endpointChanged} placeholder={PLACEHOLDER[cloud.provider].endpoint} spellcheck="false" disabled={remote} />
  </label>
  {#if cloud.provider !== 'r2'}
    <label>Region
      <input type="text" bind:value={cloud.region} placeholder={PLACEHOLDER[cloud.provider].region} spellcheck="false" disabled={remote} />
    </label>
  {/if}
  <label>Bucket
    <input type="text" bind:value={cloud.bucket} placeholder="clipr-clips" spellcheck="false" disabled={remote} />
  </label>
  <label>Access key ID
    <input type="text" bind:value={cloud.access_key_id} autocomplete="off" spellcheck="false" disabled={remote} />
  </label>
  {#if !remote}
    <label>Secret access key
      <input type="password" bind:value={cloud.secret_access_key} autocomplete="off" spellcheck="false" />
    </label>
  {/if}
  <label>Public address
    <input type="text" bind:value={cloud.public_url} placeholder={PLACEHOLDER[cloud.provider].public} spellcheck="false" disabled={remote} />
  </label>
  <label>Folder in the bucket
    <input type="text" bind:value={cloud.prefix} placeholder="clipr/" spellcheck="false" disabled={remote} />
  </label>
</div>
{#if !remote}
  <div class="row test">
    <button onclick={test} disabled={testing || !filled}>{testing ? 'Testing…' : 'Test'}</button>
    {#if result}<span class="small" class:okt={result.ok} class:errt={!result.ok}>{result.text}</span>{/if}
  </div>
  <p class="muted small note">The key is kept in clipr's settings file on this PC. Give it access to this one bucket only.</p>
{/if}

<Field title="Copy clips to the cloud" hint={filled ? 'New clips go to your bucket; Discord gets links to them.' : 'Fill in the bucket above first.'}>
  <Toggle bind:checked={cloud.enabled} label="Copy clips to the cloud" disabled={!filled && !cloud.enabled} />
</Field>
<Field title="When to upload" stacked>
  <div class="row chips">
    {#each TIMINGS as t (t.v)}
      <button class="chip" class:on={cloud.upload === t.v} onclick={() => (cloud.upload = t.v)}>{t.name}</button>
    {/each}
  </div>
  <p class="muted small">{TIMINGS.find((t) => t.v === cloud.upload)?.text}</p>
</Field>
<Field title="Speed limit" hint="Mbit/s; 0 = no limit. Keeps your connection free for games and voice chat while clips go up.">
  <input type="number" min="0" max="10000" step="1" bind:value={cloud.max_mbps} class="num" />
</Field>
<Field title="Upload recordings too" hint="Full recordings can be gigabytes. Off: only clips go up by themselves; you can still upload a recording from its page.">
  <Toggle bind:checked={cloud.recordings} label="Upload recordings too" />
</Field>

<style>
  .note { margin: 0 0 0.8rem; }
  .chips { gap: 0.4rem; flex-wrap: wrap; }
  .steps { margin: 0 0 1rem; padding: 0.6rem 0.9rem; border: 1px solid var(--border); border-radius: 10px; }
  .steps summary { cursor: pointer; font-weight: 600; }
  .steps ol { margin: 0.6rem 0 0.2rem; padding-left: 1.2rem; display: flex; flex-direction: column; gap: 0.35rem; }
  .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr)); gap: 0.7rem 1rem; margin-bottom: 0.7rem; }
  .grid label { display: flex; flex-direction: column; gap: 0.25rem; font-size: 0.85rem; font-weight: 600; }
  .grid input { font-weight: 400; }
  .test { gap: 0.7rem; align-items: center; margin-bottom: 0.4rem; flex-wrap: wrap; }
  .num { width: 7rem; }
  .okt { color: #4ade80; }
  .errt { color: var(--danger); }
</style>
