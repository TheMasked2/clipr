const KEY = 'clipr.accent';
export const DEFAULT_ACCENT = '#8b5cf6';

export function applyAccent(color: string): void {
  document.documentElement.style.setProperty('--accent', color);
}

export function loadAccent(): string {
  let c = DEFAULT_ACCENT;
  try {
    const s = localStorage.getItem(KEY);
    if (s && /^#[0-9a-f]{6}$/i.test(s)) c = s;
  } catch {
    /* storage unavailable */
  }
  if (c !== DEFAULT_ACCENT) applyAccent(c);
  return c;
}

export function saveAccent(color: string): void {
  applyAccent(color);
  try {
    localStorage.setItem(KEY, color);
  } catch {
    /* storage unavailable */
  }
}

export function resetAccent(): void {
  document.documentElement.style.removeProperty('--accent');
  try {
    localStorage.removeItem(KEY);
  } catch {
    /* storage unavailable */
  }
}
