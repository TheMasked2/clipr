// Typed client for the clipr web API. Mirrors docs/api.md and crates/core/src/config.rs.

export type State = 'idle' | 'buffering' | 'recording';

export interface Status {
  version: string;
  state: State;
  game: string | null;
  encoder: string | null;
  error: string | null;
  recording_since: number | null;
  uptime_secs: number;
  /** The user paused the replay buffer. */
  paused: boolean;
  /** The microphone is muted in clips (the mute hotkey); clipr starts unmuted. */
  mic_muted?: boolean;
}

export type ClipKind = 'clip' | 'recording';

export interface Clip {
  id: string;
  title: string;
  game: string;
  kind: ClipKind;
  size_bytes: number;
  created: number;
  duration_secs: number | null;
  width: number | null;
  height: number | null;
  favorite: boolean;
  shelves: string[];
  markers: number[];
  url: string;
  thumb: string | null;
  sprite: string | null;
  sprite_frames: number;
}

export interface GameInfo {
  name: string;
  clips: number;
  size_bytes: number;
}

export type Codec = 'h264' | 'hevc' | 'av1';
export type GameMode = 'auto' | 'always';

/** Video settings that replace `video.*` while one game runs; null keeps the normal value. */
export interface GameProfile {
  game: string;
  fps: number | null;
  height: number | null;
  bitrate_kbps: number | null;
}

/** One audio filter, like an OBS filter. Levels in dBFS, times in ms. */
export type FilterKind =
  | { kind: 'noise_suppression'; strength: number }
  | { kind: 'noise_gate'; open_db: number; close_db: number; attack_ms: number; hold_ms: number; release_ms: number }
  | { kind: 'compressor'; threshold_db: number; ratio: number; attack_ms: number; release_ms: number; makeup_db: number }
  | { kind: 'limiter'; ceiling_db: number; release_ms: number }
  | { kind: 'gain'; db: number };
export type AudioFilter = { enabled: boolean } & FilterKind;

export interface Config {
  /** `lan_preview`: other devices may see the live capture preview (the whole screen). */
  web: { port: number; lan: boolean; lan_pin: string; lan_preview: boolean };
  ui: { theme: string; accent: string; setup_done: boolean };
  general: {
    start_with_windows: boolean;
    open_ui_on_start: boolean;
    auto_update: boolean;
    update_beta: boolean;
  };
  privacy: { crash_reports: boolean };
  replay: {
    enabled: boolean;
    clip_secs: number;
    buffer_secs: number;
    sound_on_save: boolean;
    /** 0–100 %: the save sound and the mic mute blip. */
    sound_volume: number;
    toast_on_save: boolean;
  };
  video: { codec: Codec; fps: number; height: number; bitrate_kbps: number; monitor: number; gpu_priority: boolean };
  audio: {
    system: boolean;
    system_volume: number;
    mic: boolean;
    mic_volume: number;
    /** The detected game's own sound on a track of its own. */
    game: boolean;
    apps: string[];
    /** Volume of the game and each app (exe name in lower case), 0–200 %; missing = 100. */
    volumes: Record<string, number>;
    separate_tracks: boolean;
    /** Filters per source: `system`, `mic`, or an app's exe name, in the order they run. */
    filters: Record<string, AudioFilter[]>;
    push_to_talk: string;
    ptt_release_ms: number;
    mic_offset_ms: number;
    system_offset_ms: number;
    bitrate_kbps: number;
  };
  hotkeys: {
    save_clip: string;
    save_long_clip: string;
    toggle_buffer: string;
    toggle_recording: string;
    mark: string;
    toggle_mic: string;
  };
  clips: { folder: string; max_age_days: number; max_total_gb: number; min_free_gb: number };
  games: {
    mode: GameMode;
    /** What "Game only" captures: the game's window, or the whole monitor it's on. */
    capture: 'window' | 'monitor';
    custom: string[];
    blocked: string[];
    auto_record: string[];
    profiles: GameProfile[];
  };
  recording: { max_minutes: number; folder: string };
  discord: DiscordConfig;
  cloud: CloudConfig;
}

export type CloudProvider = 'r2' | 's3' | 'b2' | 'other';
export type UploadTiming = 'after_game' | 'now' | 'manual';

export interface CloudConfig {
  enabled: boolean;
  provider: CloudProvider;
  endpoint: string;
  region: string;
  bucket: string;
  access_key_id: string;
  /** Blank on other devices: only the PC running clipr sees it. */
  secret_access_key: string;
  /** The bucket's public address; Discord gets links under it. */
  public_url: string;
  prefix: string;
  upload: UploadTiming;
  max_mbps: number;
  recordings: boolean;
}

/** A clip's cloud state, from `GET /api/cloud/status`. */
export interface CloudStatus {
  state: 'uploaded' | 'queued' | 'uploading' | 'failed';
  url: string | null;
  progress: number | null;
  error: string | null;
}

export interface Upload {
  id: number;
  clip: { id: string; title: string; game: string; duration_secs: number | null; size_bytes: number; kind: ClipKind };
  key: string;
  state: 'queued' | 'uploading' | 'done' | 'failed' | 'cancelled';
  now: boolean;
  error: string | null;
  created: number;
  done_bytes: number;
}

export type PostMode = 'ask' | 'auto' | 'off';

export interface Webhook {
  name: string;
  /** Blank on other devices: only the PC running clipr sees webhook URLs. */
  url: string;
  mode: PostMode;
  games: string[];
}

export interface DiscordConfig {
  webhooks: Webhook[];
  post_during_game: boolean;
  upload_limit_mb: number;
  /** Rich Presence: the game clipr clips, in your Discord profile. */
  presence: boolean;
  /** Who connected through Connect Discord (no token is kept). */
  account: DiscordAccount | null;
}

export interface DiscordAccount {
  id: string;
  username: string;
  name: string | null;
  avatar: string | null;
}

/** What a finished Connect Discord brings: the account, and with `channel` the picked channel's webhook. */
export interface DiscordConnected {
  account: DiscordAccount;
  webhook: { url: string; channel_id: string; guild_id: string | null } | null;
}

export type JobState = 'ask' | 'queued' | 'posting' | 'posted' | 'skipped' | 'failed';

export interface Job {
  id: number;
  clip: { id: string; title: string; game: string; duration_secs: number | null; size_bytes: number; kind: ClipKind };
  webhook: string;
  state: JobState;
  now: boolean;
  error: string | null;
  created: number;
}

export interface SystemInfo {
  gpus?: { name: string; vendor?: string; encoders?: string[] }[];
  monitors?: { index: number; name: string; width: number; height: number; primary: boolean }[];
  known_games?: number;
  /** clipr runs as administrator, which can break game capture. */
  elevated?: boolean;
  /** This page is open on another device (webhook URLs are hidden from it). */
  remote?: boolean;
  /** Access from the local network is on, but its port couldn't be opened. */
  lan_error?: string | null;
  /** This PC's address on the local network. */
  lan_ip?: string | null;
}

export interface CrashReport {
  id: string;
  /** Unix seconds. */
  time: number;
  version: string;
  summary: string;
  /** Markdown, scrubbed of user name, PC name, profile paths and LAN addresses. */
  report: string;
  /** A pre-filled new GitHub issue (shortened to fit a URL). */
  issue_url: string;
  has_dump: boolean;
}

export type UpdateState =
  | 'idle'
  | 'checking'
  | 'up_to_date'
  | 'available'
  | 'downloading'
  | 'ready'
  | 'waiting'
  | 'installing'
  | 'error';

export interface UpdateAsset {
  name: string;
  url: string;
  size: number;
}

export interface UpdateInfo {
  current: string;
  /** Installed with the installer (only then can clipr update itself). */
  installed: boolean;
  state: UpdateState;
  release: {
    version: string;
    page: string;
    notes: string;
    published: string | null;
    prerelease: boolean;
    setup: UpdateAsset | null;
    portable: UpdateAsset | null;
  } | null;
  checked_at: number | null;
  error: string | null;
  progress: { done: number; total: number } | null;
  /** Why it can't install right now ("a game is running"), or null. */
  busy: string | null;
  /** It installs as soon as nothing runs. */
  will_install: boolean;
}

export interface Disk {
  free_bytes: number;
  total_bytes: number;
  clips_bytes: number;
}

/** One audio source's current peak level in dBFS (-60 and below = silence). */
export interface Level {
  /** The source's key in `audio.filters`: 'system', 'mic', or an app's exe name in lower case. */
  source: string;
  name: string;
  /** As the device delivered it, before any filters. */
  db: number;
  /** As it goes into the recording: after the filters, push-to-talk and volume. */
  after: number;
}

export type ServerEvent =
  | ({ type: 'status' } & Status)
  | { type: 'clip_saved'; clip: Clip }
  | { type: 'clip_failed'; message: string }
  | { type: 'library' }
  | { type: 'levels'; levels: Level[] }
  | { type: 'discord' }
  | { type: 'cloud' };

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

let unauthorized: () => void = () => {};
export function onUnauthorized(fn: () => void): void {
  unauthorized = fn;
}

async function request<T>(
  method: string,
  url: string,
  body?: unknown,
  noLock = false,
): Promise<T> {
  let r: Response;
  try {
    r = await fetch(url, {
      method,
      headers: body !== undefined ? { 'Content-Type': 'application/json' } : undefined,
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
  } catch {
    throw new ApiError(0, 'Cannot reach clipr. Is it running?');
  }
  if (r.status === 401 && !noLock) unauthorized();
  if (!r.ok) {
    let msg = `${method} ${url} failed (${r.status})`;
    try {
      const j = await r.json();
      if (j && typeof j.error === 'string') msg = j.error;
    } catch {
      /* no JSON body */
    }
    throw new ApiError(r.status, msg);
  }
  if (r.status === 204) return undefined as T;
  const text = await r.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

const qs = (p: Record<string, string | undefined | false>) => {
  const u = new URLSearchParams();
  for (const [k, v] of Object.entries(p)) if (v) u.set(k, v);
  const s = u.toString();
  return s ? `?${s}` : '';
};

export const getStatus = () => request<Status>('GET', '/api/status');
export const getConfig = () => request<Config>('GET', '/api/config');
export const putConfig = (c: Config) => request<Config>('PUT', '/api/config', c);
export const getSystem = () => request<SystemInfo>('GET', '/api/system');
export const getDisk = () => request<Disk>('GET', '/api/disk');
export const getCrashes = () => request<CrashReport[]>('GET', '/api/crashes');
export const dismissCrash = (id: string) => request<void>('POST', '/api/crashes/dismiss', { id });
export const revealCrash = (id: string) => request<void>('POST', '/api/crashes/reveal', { id });
export const getUpdate = () => request<UpdateInfo>('GET', '/api/update');
export const checkUpdate = () => request<void>('POST', '/api/update/check');
export const applyUpdate = () => request<void>('POST', '/api/update/apply');
export const getGames = () => request<GameInfo[]>('GET', '/api/games');
export const getShelves = () => request<string[]>('GET', '/api/shelves');

export interface ClipQuery {
  q?: string;
  game?: string;
  shelf?: string;
  favorite?: boolean;
  kind?: ClipKind;
}
export const listClips = (p: ClipQuery = {}) =>
  request<Clip[]>(
    'GET',
    '/api/clips' +
      qs({ q: p.q, game: p.game, shelf: p.shelf, favorite: p.favorite ? 'true' : undefined, kind: p.kind }),
  );
export const patchClip = (patch: {
  id: string;
  title?: string;
  favorite?: boolean;
  shelves?: string[];
}) => request<Clip>('PATCH', '/api/clips', patch);
export const deleteClip = (id: string) =>
  request<void>('DELETE', '/api/clips?id=' + encodeURIComponent(id));
export const trimClip = (id: string, start: number, end: number, replace: boolean) =>
  request<Clip>('POST', '/api/clips/trim', { id, start, end, replace });
export const reveal = (id?: string) =>
  request<void>('POST', '/api/clips/reveal', id ? { id } : {});
export const cleanup = () =>
  request<{ removed: number; freed_bytes: number }>('POST', '/api/cleanup');

/** What the cleanup would delete in the next `hours`: with these limits, or the saved ones. Deletes nothing. */
export interface CleanupPreview {
  count: number;
  bytes: number;
  /** The first few, oldest first. */
  clips: Clip[];
}
export const cleanupPreview = (limits?: { max_age_days: number; max_total_gb: number }, hours = 24) =>
  request<CleanupPreview>('POST', '/api/cleanup/preview', limits ? { clips: limits, hours } : { hours });

/** Opens the Windows folder picker on the PC and waits; `path` is null when cancelled. Not from other devices. */
export const pickFolder = (title: string, start: string) =>
  request<{ path: string | null }>('POST', '/api/pick-folder', { title, start });

export const saveClip = (seconds?: number) =>
  request<void>('POST', '/api/clip', seconds ? { seconds } : undefined);
export const setBuffer = (on: boolean) => request<void>('POST', '/api/buffer', { on });
export const setRecording = (on: boolean) => request<void>('POST', '/api/recording', { on });
export const markMoment = () => request<void>('POST', '/api/mark');
export const setMicMuted = (muted: boolean) => request<void>('POST', '/api/mic', { muted });
export const testSound = (volume: number) => request<void>('POST', '/api/sound/test', { volume });
/** One second of capture, for Home's performance numbers (see `GET /api/perf`). */
export interface PerfSample {
  at_ms: number;
  fps: number;
  fresh: number;
  late: number;
  dropped: number;
  frame_ms: number;
  kbps: number;
  cpu: number;
  mem_mb: number;
  buffer_secs: number;
  buffer_mb: number;
  capturing: boolean;
}
export const getPerf = () => request<PerfSample[]>('GET', '/api/perf');
/** A page shows level meters: keeps levels coming for 5 s, even while nothing is captured. */
export const watchLevels = () => request<void>('POST', '/api/levels/watch');
/** Sets one source's volume (0–200 %), saved and applied at once. */
export const setVolume = (source: string, volume: number) =>
  request<Config>('POST', '/api/audio/volume', { source, volume });
/** A source's volume in %, by its key. */
export function volumeOf(audio: Config['audio'], source: string): number {
  if (source === 'system') return audio.system_volume;
  if (source === 'mic') return audio.mic_volume;
  return audio.volumes?.[source] ?? 100;
}
export const getPosts = () => request<Job[]>('GET', '/api/discord/posts');
export const discordConnect = (channel: boolean) =>
  request<{ url: string; state: string }>('POST', '/api/discord/connect', { channel });
/** `{ pending: true }` while the user is still on Discord's page. */
export const discordConnectResult = (state: string) =>
  request<DiscordConnected | { pending: true }>('GET', `/api/discord/connect/${encodeURIComponent(state)}`);
export const shareClip = (id: string, webhook: string) =>
  request<{ job: number }>('POST', '/api/discord/share', { id, webhook });
export const answerPost = (job: number, yes: boolean) =>
  request<void>('POST', '/api/discord/answer', { job, yes });
/** Sends a test message: a URL typed in, or a saved webhook by name. */
export const testWebhook = (w: { url?: string; name?: string }) =>
  request<void>('POST', '/api/discord/test', w);
export const getUploads = () => request<Upload[]>('GET', '/api/cloud/uploads');
export const getCloudStatus = () => request<Record<string, CloudStatus>>('GET', '/api/cloud/status');
export const uploadClip = (id: string) => request<{ job: number }>('POST', '/api/cloud/upload', { id });
export const cancelUpload = (job: number) => request<void>('POST', '/api/cloud/cancel', { job });
export const removeFromCloud = (id: string) => request<void>('POST', '/api/cloud/remove', { id });
/** Tries bucket settings (a blank secret means the saved one). */
export const testCloud = (c: CloudConfig) => request<{ message: string }>('POST', '/api/cloud/test', c);
export const login = (pin: string) => request<void>('POST', '/api/login', { pin }, true);

export interface Connection {
  close(): void;
}

export interface Handlers {
  onStatus: (s: Status) => void;
  onConnected: (up: boolean) => void;
  onEvent: (e: ServerEvent) => void;
}

/**
 * Fetches /api/status, opens /api/events and keeps it open, reconnecting with
 * backoff (1s -> 10s).
 */
export function connect(h: Handlers): Connection {
  let closed = false;
  let ws: WebSocket | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let delay = 1000;

  const retry = () => {
    if (closed) return;
    h.onConnected(false);
    timer = setTimeout(open, delay);
    delay = Math.min(delay * 2, 10000);
  };

  const open = () => {
    if (closed) return;
    getStatus()
      .then((s) => !closed && h.onStatus(s))
      .catch(() => {});
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const sock = new WebSocket(`${proto}://${location.host}/api/events`);
    ws = sock;
    sock.onopen = () => {
      delay = 1000;
      h.onConnected(true);
    };
    sock.onmessage = (e) => {
      try {
        const m = JSON.parse(e.data) as ServerEvent;
        if (m.type === 'status') h.onStatus(m);
        else h.onEvent(m);
      } catch {
        /* ignore malformed message */
      }
    };
    sock.onclose = () => {
      if (ws === sock) retry();
    };
    sock.onerror = () => sock.close();
  };

  open();

  return {
    close() {
      closed = true;
      clearTimeout(timer);
      ws?.close();
    },
  };
}
