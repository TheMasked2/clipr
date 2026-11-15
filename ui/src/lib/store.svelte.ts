import {
  connect,
  getConfig,
  putConfig,
  onUnauthorized,
  type Config,
  type Level,
  type ServerEvent,
  type Status,
} from './api';
import { saveAccent } from './accent';
import { clipHref } from './router.svelte';

export const app = $state<{
  status: Status | null;
  connected: boolean;
  locked: boolean;
  config: Config | null;
  /** Live audio levels; empty when audio isn't running. */
  levels: Level[];
}>({ status: null, connected: false, locked: false, config: null, levels: [] });

// The server stops sending levels when audio stops; clear the meters then.
let levelsTimer: ReturnType<typeof setTimeout> | undefined;
function setLevels(levels: Level[]): void {
  app.levels = levels;
  clearTimeout(levelsTimer);
  if (levels.length) levelsTimer = setTimeout(() => (app.levels = []), 1000);
}

export interface Toast {
  id: number;
  kind: 'ok' | 'err';
  text: string;
  href?: string;
  linkText?: string;
}
export const toasts = $state<Toast[]>([]);
let nextToast = 1;

export function toast(kind: Toast['kind'], text: string, href?: string, linkText?: string): void {
  const t: Toast = { id: nextToast++, kind, text, href, linkText };
  toasts.push(t);
  setTimeout(() => dismissToast(t.id), kind === 'err' ? 9000 : 6000);
}
export function dismissToast(id: number): void {
  const i = toasts.findIndex((t) => t.id === id);
  if (i >= 0) toasts.splice(i, 1);
}
export const toastError = (e: unknown) => toast('err', e instanceof Error ? e.message : String(e));

// Tiny pub/sub so pages can react to server events.
type Listener = (e: ServerEvent) => void;
const listeners = new Set<Listener>();
export function onServerEvent(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

function applyConfig(c: Config): void {
  app.config = c;
  saveAccent(c.ui.accent);
}

export async function loadConfig(): Promise<void> {
  try {
    applyConfig(await getConfig());
  } catch (e) {
    toastError(e);
  }
}

/** PUT the whole config; throws the server's message on failure. */
export async function saveConfig(c: Config): Promise<Config> {
  const saved = await putConfig(c);
  applyConfig(saved);
  return saved;
}

// Confirm dialog (rendered by ConfirmDialog.svelte).
export const confirmState = $state<{
  open: boolean;
  text: string;
  ok: string;
  danger: boolean;
  resolve: ((v: boolean) => void) | null;
}>({ open: false, text: '', ok: 'OK', danger: false, resolve: null });

export function confirmDialog(text: string, ok = 'OK', danger = false): Promise<boolean> {
  return new Promise((resolve) => {
    confirmState.text = text;
    confirmState.ok = ok;
    confirmState.danger = danger;
    confirmState.resolve = resolve;
    confirmState.open = true;
  });
}

let started = false;
export function startConnection(): void {
  if (started) return;
  started = true;
  onUnauthorized(() => (app.locked = true));
  connect({
    onStatus: (s) => (app.status = s),
    onConnected: (up) => {
      app.connected = up;
      if (!up) setLevels([]);
      if (up && !app.config) void loadConfig();
    },
    onEvent: (e) => {
      if (e.type === 'levels') return setLevels(e.levels);
      if (e.type === 'clip_saved') {
        const c = e.clip;
        toast(
          'ok',
          `Saved ${c.kind === 'recording' ? 'recording' : 'clip'}: ${c.title}`,
          clipHref(c.id),
          'Open',
        );
      } else if (e.type === 'clip_failed') {
        toast('err', `Clip failed: ${e.message}`);
      }
      listeners.forEach((l) => l(e));
    },
  });
  void loadConfig();
}
