export type State = 'idle' | 'buffering' | 'recording';

export interface Status {
  version: string;
  state: State;
  game: string | null;
  uptime_secs: number;
}

export type ServerEvent = { type: 'status' } & Status;

export async function getStatus(): Promise<Status> {
  const r = await fetch('/api/status');
  if (!r.ok) throw new Error(`GET /api/status: ${r.status}`);
  return (await r.json()) as Status;
}

export interface Connection {
  close(): void;
}

/**
 * Fetches /api/status, opens /api/events and keeps it open, reconnecting with
 * backoff (1s -> 10s). onStatus gets every status; onConnected(false) while down.
 */
export function connect(
  onStatus: (s: Status) => void,
  onConnected: (up: boolean) => void,
): Connection {
  let closed = false;
  let ws: WebSocket | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let delay = 1000;

  const retry = () => {
    if (closed) return;
    onConnected(false);
    timer = setTimeout(open, delay);
    delay = Math.min(delay * 2, 10000);
  };

  const open = () => {
    if (closed) return;
    // Resync via REST, then listen for pushes.
    getStatus()
      .then((s) => !closed && onStatus(s))
      .catch(() => {});
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const sock = new WebSocket(`${proto}://${location.host}/api/events`);
    ws = sock;
    sock.onopen = () => {
      delay = 1000;
      onConnected(true);
    };
    sock.onmessage = (e) => {
      try {
        const m = JSON.parse(e.data) as ServerEvent;
        if (m.type === 'status') onStatus(m);
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
