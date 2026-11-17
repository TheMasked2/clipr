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
  web: { port: number; lan: boolean; lan_pin: string };
  ui: { theme: string; accent: string; setup_done: boolean };
  general: {
    start_with_windows: boolean;
    open_ui_on_start: boolean;
    auto_update: boolean;
    update_beta: boolean;
  };
  privacy: { crash_reports: boolean };
  replay: { enabled: boolean; clip_secs: number; buffer_secs: number; sound_on_save: boolean; toast_on_save: boolean };
  video: { codec: Codec; fps: number; height: number; bitrate_kbps: number; monitor: number; high_priority: boolean };
  audio: {
    system: boolean;
    system_volume: number;
    mic: boolean;
    mic_volume: number;
    apps: string[];
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
  };
  clips: { folder: string; max_age_days: number; max_total_gb: number; min_free_gb: number };
  games: {
    mode: GameMode;
    custom: string[];
    blocked: string[];
    auto_record: string[];
    profiles: GameProfile[];
  };
  recording: { max_minutes: number };
  discord: DiscordConfig;
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
  /** This page is open on another device (webhook URLs are hidden from it). */
  remote?: boolean;
  /** Access from the local network is on, but its port couldn't be opened. */
  lan_error?: string | null;
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
  | { type: 'discord' };

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

export const saveClip = (seconds?: number) =>
  request<void>('POST', '/api/clip', seconds ? { seconds } : undefined);
export const setBuffer = (on: boolean) => request<void>('POST', '/api/buffer', { on });
export const setRecording = (on: boolean) => request<void>('POST', '/api/recording', { on });
export const markMoment = () => request<void>('POST', '/api/mark');
export const getPosts = () => request<Job[]>('GET', '/api/discord/posts');
export const shareClip = (id: string, webhook: string) =>
  request<{ job: number }>('POST', '/api/discord/share', { id, webhook });
export const answerPost = (job: number, yes: boolean) =>
  request<void>('POST', '/api/discord/answer', { job, yes });
/** Sends a test message: a URL typed in, or a saved webhook by name. */
export const testWebhook = (w: { url?: string; name?: string }) =>
  request<void>('POST', '/api/discord/test', w);
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
