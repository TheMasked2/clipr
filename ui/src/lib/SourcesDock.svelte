<script lang="ts">
  import { tick } from 'svelte';
  import type { Config } from './api';
  import { app, quickEdit, toastError } from './store.svelte';
  import SourcePanel from './SourcePanel.svelte';

  // What clipr records, like OBS's Sources dock: the picture and every sound, each with its switch and its
  // settings (⚙) right here, instead of somewhere in Settings.
  const MAX_APPS = 4;
  const lower = (s: string) => s.replace(/[A-Z]/g, (c) => c.toLowerCase());

  const cfg = $derived(app.config);
  const st = $derived(app.status);
  let open = $state<string | null>(null);
  let adding = $state(false);
  let newApp = $state('');
  let busy = $state(0);

  async function change(edit: (c: Config) => void): Promise<boolean> {
    busy++;
    try {
      await quickEdit(edit);
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      busy--;
    }
  }

  const apps = $derived(
    (cfg?.audio.apps ?? [])
      .map((a) => a.trim())
      .filter((a, i, all) => a && all.findIndex((b) => lower(b) === lower(a)) === i)
      .map((a) => ({ key: lower(a), label: a.replace(/\.exe$/i, '') })),
  );

  function details(key: string): string {
    if (!cfg) return '';
    const a = cfg.audio;
    const parts: string[] = [];
    const vol = key === 'system' ? a.system_volume : key === 'mic' ? a.mic_volume : (a.volumes[key] ?? 100);
    if (vol !== 100) parts.push(`${vol}%`);
    const n = (a.filters[key] ?? []).filter((f) => f.enabled).length;
    if (n) parts.push(`${n} ${n === 1 ? 'filter' : 'filters'}`);
    if (key === 'mic' && a.push_to_talk.trim()) parts.push(`push-to-talk ${a.push_to_talk.trim()}`);
    if (key === 'mic' && st?.mic_muted) parts.push('muted');
    return parts.join(' · ');
  }

  const video = $derived.by(() => {
    if (!cfg) return { name: 'Video', sub: '' };
    const how = cfg.games.capture === 'window' ? 'game window' : "game's monitor";
    if (cfg.games.mode === 'always') return { name: 'Desktop + games', sub: st?.game ? `${st.game} · ${how}` : cfg.video.monitor ? `monitor ${cfg.video.monitor}` : 'primary monitor' };
    return { name: 'Game capture', sub: st?.game ? `${st.game} · ${how}` : `${how} · waiting for a game` };
  });

  /** Opens a source's panel; cleared first, so the same one opens again even if its dialog was closed some
   * way that didn't report back. */
  async function openPanel(key: string) {
    open = null;
    await tick();
    open = key;
  }

  async function addApp() {
    const exe = newApp.trim();
    if (!exe) return;
    if (/[\\/:]/.test(exe)) {
      toastError(new Error('Type the program\'s file name only, like discord.exe (no folder).'));
      return;
    }
    const name = /\.exe$/i.test(exe) ? exe : `${exe}.exe`;
    if (apps.some((a) => a.key === lower(name))) {
      toastError(new Error(`${name} is already a source.`));
      return;
    }
    // The form stays filled in until the server takes it.
    if (await change((c) => c.audio.apps.push(name))) {
      newApp = '';
      adding = false;
    }
  }
</script>

{#snippet sw(on: boolean, label: string, edit: (c: Config) => void)}
  <button class="sw" class:on role="switch" aria-checked={on} aria-label="Record {label}" title={on ? `Recording ${label}: click to turn off` : `Not recording ${label}: click to turn on`} onclick={() => change(edit)} disabled={busy > 0}>
    <span class="knob"></span>
  </button>
{/snippet}
{#snippet gear(key: string, label: string)}
  <button class="gear" onclick={() => openPanel(key)} aria-label="{label} settings" title="{label} settings">⚙</button>
{/snippet}

{#if cfg}
<div class="sources">
  <ul class="list" class:busy={busy > 0}>
    <li>
      <span class="ico" aria-hidden="true">▣</span>
      <span class="what"><b>{video.name}</b><span class="sub">{video.sub}</span></span>
      <span></span>
      {@render gear('video', 'Video')}
    </li>
    <li class:off={!cfg.audio.system}>
      <span class="ico" aria-hidden="true">♪</span>
      <span class="what"><b>System audio</b><span class="sub">{cfg.audio.system ? details('system') || 'everything you hear' : 'off'}</span></span>
      {@render sw(cfg.audio.system, 'system audio', (c) => (c.audio.system = !c.audio.system))}
      {@render gear('system', 'System audio')}
    </li>
    <li class:off={!cfg.audio.mic}>
      <span class="ico" aria-hidden="true">●</span>
      <span class="what"><b>Microphone</b><span class="sub">{cfg.audio.mic ? details('mic') || 'default microphone' : 'off'}</span></span>
      {@render sw(cfg.audio.mic, 'the microphone', (c) => (c.audio.mic = !c.audio.mic))}
      {@render gear('mic', 'Microphone')}
    </li>
    <li class:off={!cfg.audio.game}>
      <span class="ico" aria-hidden="true">♪</span>
      <span class="what"><b>Game audio</b><span class="sub">{cfg.audio.game ? details('game') || 'own track' : 'off'}</span></span>
      {@render sw(cfg.audio.game, 'game audio', (c) => (c.audio.game = !c.audio.game))}
      {@render gear('game', 'Game audio')}
    </li>
    {#each apps as a (a.key)}
      <li>
        <span class="ico" aria-hidden="true">♪</span>
        <span class="what"><b>{a.label}</b><span class="sub">{details(a.key) || 'own track'}</span></span>
        <span></span>
        {@render gear(a.key, a.label)}
      </li>
    {/each}
  </ul>
  <div class="foot">
    {#if adding}
      <form class="add" onsubmit={(e) => { e.preventDefault(); addApp(); }}>
        <!-- svelte-ignore a11y_autofocus -->
        <input type="text" bind:value={newApp} placeholder="discord.exe" aria-label="App to record on its own track" autofocus />
        <button type="submit" class="primary">Add</button>
        <button type="button" onclick={() => { adding = false; newApp = ''; }}>Cancel</button>
      </form>
    {:else}
      <button class="small" onclick={() => (adding = true)} disabled={apps.length >= MAX_APPS} title={apps.length >= MAX_APPS ? `Up to ${MAX_APPS} apps` : 'Record an app (like Discord) on a track of its own'}>+ App</button>
      <span class="muted small">Changes restart capture.</span>
    {/if}
  </div>
</div>
{/if}

<SourcePanel source={open} onclose={() => (open = null)} />

<style>
  .sources { display: flex; flex-direction: column; height: 100%; }
  .list { list-style: none; margin: 0; padding: .2rem 0; flex: 1; min-height: 0; overflow-y: auto; }
  .list.busy { opacity: .7; }
  li { display: grid; grid-template-columns: 1.2rem 1fr auto auto; align-items: center; gap: .5rem; padding: .28rem .7rem; }
  li:hover { background: var(--surface-2); }
  li.off .what { opacity: .5; }
  .ico { color: var(--text-muted); text-align: center; font-size: .85rem; }
  .what { display: flex; flex-direction: column; min-width: 0; line-height: 1.25; }
  .what b { font-size: .88rem; font-weight: 600; }
  .sub { font-size: .74rem; color: var(--text-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .gear { background: none; border: 1px solid transparent; padding: .1rem .35rem; border-radius: 6px; color: var(--text-muted); font-size: 1rem; line-height: 1; }
  .gear:hover { border-color: var(--border); color: var(--text); }
  .sw { width: 2rem; height: 1.1rem; padding: 0; border-radius: 999px; background: var(--surface-2); border: 1px solid var(--border); position: relative; display: block; }
  .sw .knob { position: absolute; top: 1px; left: 1px; width: .8rem; height: .8rem; border-radius: 50%; background: var(--text-muted); transition: transform .15s; }
  .sw.on { background: var(--accent-soft); border-color: var(--accent-border); }
  .sw.on .knob { transform: translateX(.9rem); background: var(--accent); }
  .foot { display: flex; align-items: center; gap: .6rem; padding: .4rem .7rem; border-top: 1px solid var(--border); }
  .foot > button { padding: .15rem .6rem; }
  .add { display: flex; gap: .4rem; width: 100%; }
  .add input { flex: 1; min-width: 0; }
  .add button { padding: .15rem .6rem; }
</style>
