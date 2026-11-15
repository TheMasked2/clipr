export function fmtTime(s: number): string {
  if (!isFinite(s) || s < 0) s = 0;
  const t = Math.floor(s);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const sec = t % 60;
  const p = (n: number) => String(n).padStart(2, '0');
  return h ? `${h}:${p(m)}:${p(sec)}` : `${m}:${p(sec)}`;
}

export function fmtTimePrecise(s: number): string {
  return `${fmtTime(s)}.${String(Math.floor((s % 1) * 10))}`;
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const u = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${u[i]}`;
}

const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

export function dayLabel(unix: number): string {
  const d = new Date(unix * 1000);
  const diff = Math.round((startOfDay(new Date()) - startOfDay(d)) / 86400000);
  if (diff === 0) return 'Today';
  if (diff === 1) return 'Yesterday';
  return d.toLocaleDateString(undefined, {
    weekday: 'short',
    day: 'numeric',
    month: 'long',
    year: d.getFullYear() === new Date().getFullYear() ? undefined : 'numeric',
  });
}

export function fmtDateTime(unix: number): string {
  return new Date(unix * 1000).toLocaleString(undefined, {
    day: 'numeric',
    month: 'short',
    hour: '2-digit',
    minute: '2-digit',
  });
}
