import { connect, type Status } from './api';

export const app = $state<{ status: Status | null; connected: boolean }>({
  status: null,
  connected: false,
});

let started = false;
export function startConnection(): void {
  if (started) return;
  started = true;
  connect(
    (s) => (app.status = s),
    (up) => (app.connected = up),
  );
}
