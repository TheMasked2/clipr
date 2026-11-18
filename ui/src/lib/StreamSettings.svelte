<script lang="ts">
  import type { StreamConfig, StreamService } from './api';
  import Field from './Field.svelte';

  // Going live. `stream` is the settings draft, changed in place. `remote`: this page is open on another device,
  // which never sees the key and can't change where the stream goes (it could send it, key and all, elsewhere).
  let { stream, remote }: { stream: StreamConfig; remote: boolean } = $props();
  // The key stays hidden until asked, so opening Settings while live doesn't show it to viewers.
  let keyShown = $state(false);

  const SERVICES: { v: StreamService; name: string; text: string }[] = [
    { v: 'twitch', name: 'Twitch', text: 'Goes to Twitch over a secure connection (RTMPS).' },
    { v: 'youtube', name: 'YouTube', text: 'Goes to YouTube Live over a secure connection (RTMPS).' },
    { v: 'custom', name: 'Custom', text: 'Any RTMP server: Kick, Owncast, Restream, your own.' },
  ];
  const QUALITIES = [
    { height: 720, fps: 30 },
    { height: 720, fps: 60 },
    { height: 1080, fps: 30 },
    { height: 1080, fps: 60 },
  ];
  /** Twitch turns away more than this. */
  const TWITCH_MAX = 6000;

  function pick(s: StreamService) {
    stream.service = s;
    if (s === 'twitch' && stream.bitrate_kbps > TWITCH_MAX) stream.bitrate_kbps = TWITCH_MAX;
  }
  const bitrateHint = $derived(
    stream.service === 'twitch'
      ? 'kbit/s. Twitch takes up to 6000: 6000 for 1080p60, 4500 for 720p60. Keep it well under your upload speed.'
      : stream.service === 'youtube'
        ? 'kbit/s. YouTube suggests 4500–9000 for 1080p60, 2500–4000 for 720p. Keep it well under your upload speed.'
        : 'kbit/s. Keep it well under your upload speed.',
  );
  const tooHigh = $derived(stream.service === 'twitch' && stream.bitrate_kbps > TWITCH_MAX);
</script>

<p class="muted small note">
  Go live from what clipr captures (your game, or the desktop when no game runs) with the audio mix, using your GPU's
  encoder. Clips and recordings keep working while you stream. Start it with <b>Go live</b> on Home or a hotkey.
</p>
{#if remote}
  <div class="banner warn">The service, server and stream key can only be changed on the PC running clipr. Here you can change the quality.</div>
{/if}

<Field title="Service" stacked>
  <div class="row chips">
    {#each SERVICES as s (s.v)}
      <button class="chip" class:on={stream.service === s.v} onclick={() => pick(s.v)} disabled={remote}>{s.name}</button>
    {/each}
  </div>
  <p class="muted small">{SERVICES.find((s) => s.v === stream.service)?.text}</p>
</Field>

{#if stream.service === 'custom'}
  <Field title="Server" hint="The address the service gives you, without the key: rtmp://… or rtmps://…" stacked>
    <input type="text" bind:value={stream.server} placeholder="rtmp://live.example.com/app" class="wide" spellcheck="false" autocomplete="off" disabled={remote} />
  </Field>
{/if}

{#if !remote}
  <Field title="Stream key" stacked>
    <div class="row key">
      <input type={keyShown ? 'text' : 'password'} bind:value={stream.key} autocomplete="off" spellcheck="false" aria-label="Stream key" />
      <button class="small" onclick={() => (keyShown = !keyShown)}>{keyShown ? 'Hide' : 'Show'}</button>
    </div>
    <p class="muted small">
      {#if stream.service === 'twitch'}
        Find it in your <a href="https://dashboard.twitch.tv/settings/stream" target="_blank" rel="noreferrer">Twitch creator dashboard</a> under Settings → Stream → Primary stream key.
      {:else if stream.service === 'youtube'}
        In <a href="https://studio.youtube.com" target="_blank" rel="noreferrer">YouTube Studio</a>, click Create → Go live; the key is under Stream settings.
      {:else}
        The key (or stream name) your server gives you.
      {/if}
      Anyone with the key can stream to your channel: it's kept in clipr's settings on this PC and never shown to other devices.
    </p>
  </Field>
{/if}

<Field title="Quality" hint="Never more than your clips (Video): with clips at 720p, a 1080p stream goes out at 720p, and with clips at 30 fps, at 30." stacked>
  <div class="row chips">
    {#each QUALITIES as q (`${q.height}p${q.fps}`)}
      <button class="chip" class:on={stream.height === q.height && stream.fps === q.fps} onclick={() => { stream.height = q.height; stream.fps = q.fps; }}>{q.height}p{q.fps}</button>
    {/each}
  </div>
</Field>
<Field title="Bitrate" hint={bitrateHint} error={tooHigh}>
  <input type="number" min="500" max="51000" step="500" bind:value={stream.bitrate_kbps} class="num" />
  {#if tooHigh}<span class="small errt">Twitch turns away more than 6000.</span>{/if}
</Field>

<style>
  .note { margin: 0 0 0.8rem; }
  .chips { gap: 0.4rem; flex-wrap: wrap; }
  .wide { width: min(100%, 32rem); }
  .key { gap: 0.5rem; align-items: center; max-width: 32rem; }
  .key input { flex: 1; min-width: 0; }
  .num { width: 7rem; }
</style>
