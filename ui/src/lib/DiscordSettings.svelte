<script lang="ts">
  import type { DiscordConfig, PostMode } from './api';
  import { testWebhook } from './api';
  import { app } from './store.svelte';
  import Field from './Field.svelte';
  import Toggle from './Toggle.svelte';
  import ListEditor from './ListEditor.svelte';

  // The Discord channels clips go to, and when. `discord` is the settings draft, changed in place.
  // `remote`: this page is open on another device, which never sees or sets webhook URLs.
  let { discord, remote }: { discord: DiscordConfig; remote: boolean } = $props();

  const MODES: { v: PostMode; name: string; text: string }[] = [
    { v: 'ask', name: 'Ask', text: 'New clips wait on the Home page for your yes.' },
    { v: 'auto', name: 'Auto', text: 'New clips are posted by themselves.' },
    { v: 'off', name: 'Off', text: 'Only clips you share by hand.' },
  ];
  const LIMITS = [
    { v: 10, t: '10 MB (free)' },
    { v: 50, t: '50 MB (Nitro Basic)' },
    { v: 500, t: '500 MB (Nitro)' },
  ];

  // Other devices get the URLs blanked: a saved webhook with an empty URL is kept as it is on the PC. Its
  // name is how the PC finds the URL, so other devices can't rename it.
  const savedNames = $derived(new Set((app.config?.discord.webhooks ?? []).map((w) => w.name)));

  let testing = $state<number | null>(null);
  let results = $state<Record<number, { ok: boolean; text: string }>>({});

  function add() {
    let n = discord.webhooks.length + 1;
    let name = n === 1 ? '#clips' : `#clips-${n}`;
    while (discord.webhooks.some((w) => w.name.toLowerCase() === name.toLowerCase())) name = `#clips-${++n}`;
    discord.webhooks.push({ name, url: '', mode: 'ask', games: [] });
  }
  function remove(i: number) {
    discord.webhooks.splice(i, 1);
    results = {};
  }

  async function test(i: number) {
    const w = discord.webhooks[i];
    testing = i;
    try {
      await testWebhook(w.url.trim() ? { url: w.url.trim() } : { name: w.name.trim() });
      results[i] = { ok: true, text: 'Sent a test message. Check the channel.' };
    } catch (e) {
      results[i] = { ok: false, text: e instanceof Error ? e.message : String(e) };
    } finally {
      testing = null;
    }
  }
</script>

<Field title="Show in my Discord status" hint="While clipr clips or records, your Discord profile shows the game with its icon (&quot;Playing clipr · VALORANT · Clipping with clipr&quot;). Only games, never the desktop. Needs the Discord app on this PC; nothing goes online.">
  <Toggle bind:checked={discord.presence} label="Show in my Discord status" />
</Field>

<p class="muted small note">
  Post clips to a Discord channel through a webhook: in Discord, open the channel's settings → Integrations → Webhooks →
  New Webhook → Copy Webhook URL, and paste it here. Anyone with the URL can post to that channel, so keep it to yourself.
</p>
{#if remote}
  <div class="banner warn">Webhook URLs stay on the PC running clipr: paste or change them there. Here you can add channels and change when they post.</div>
{/if}

{#each discord.webhooks as w, i (i)}
  <div class="hook">
    <div class="head">
      <input type="text" class="name" bind:value={w.name} maxlength="50" placeholder="#clips" aria-label="Channel name" disabled={remote && savedNames.has(w.name)} />
      <button onclick={() => test(i)} disabled={testing !== null || (!w.url.trim() && !savedNames.has(w.name))}>{testing === i ? 'Sending…' : 'Test'}</button>
      <button aria-label="Remove {w.name}" onclick={() => remove(i)}>×</button>
    </div>
    {#if !remote}<input
      type="password"
      autocomplete="off"
      spellcheck="false"
      bind:value={w.url}
      placeholder="https://discord.com/api/webhooks/…"
      aria-label="Webhook URL for {w.name}"
    />{:else if !savedNames.has(w.name)}<p class="muted small">Paste this channel's webhook URL on the PC running clipr.</p>{/if}
    {#if results[i]}<p class="small" class:okt={results[i].ok} class:errt={!results[i].ok}>{results[i].text}</p>{/if}
    <div class="row modes">
      {#each MODES as m (m.v)}
        <button class="chip" class:on={w.mode === m.v} onclick={() => (w.mode = m.v)}>{m.name}</button>
      {/each}
      <span class="muted small">{MODES.find((m) => m.v === w.mode)?.text}</span>
    </div>
    <div class="games">
      <span class="muted small">Only these games (as clipr names them). Empty = every game.</span>
      <ListEditor bind:items={w.games} placeholder="VALORANT" />
    </div>
  </div>
{/each}
<div><button onclick={add} disabled={discord.webhooks.length >= 10}>Add a channel</button></div>

<Field title="Post while a game runs" hint="Off: posts wait until the game closes, so uploading never costs frames. Clips you share by hand go right away.">
  <Toggle bind:checked={discord.post_during_game} label="Post while a game runs" />
</Field>
<Field title="Upload limit" hint="Your Discord upload limit. A bigger clip is sent as a smaller copy that fits; the clip itself never changes.">
  <select bind:value={discord.upload_limit_mb}>
    {#each LIMITS as l (l.v)}<option value={l.v}>{l.t}</option>{/each}
    {#if !LIMITS.some((l) => l.v === discord.upload_limit_mb)}<option value={discord.upload_limit_mb}>{discord.upload_limit_mb} MB</option>{/if}
  </select>
</Field>
<p class="muted small note">Recordings are never posted by themselves; share one from its page if you want to.</p>

<style>
  .hook { display: flex; flex-direction: column; gap: 0.5rem; padding: 0.8rem; margin-bottom: 0.8rem; border: 1px solid var(--border); border-radius: 10px; }
  .head { display: flex; gap: 0.5rem; }
  .name { flex: 1; min-width: 0; font-weight: 600; }
  .modes { gap: 0.4rem; }
  .games { display: flex; flex-direction: column; gap: 0.3rem; }
  .note { margin: 0 0 0.8rem; }
  .okt { color: #4ade80; margin: 0; }
  .errt { color: var(--danger); margin: 0; }
</style>
