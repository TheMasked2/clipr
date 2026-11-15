// The accent lives in the config (ui.accent). localStorage only keeps a copy so the
// first paint, before /api/config answers, already has the right colour.
const KEY = 'clipr.accent';
export const DEFAULT_ACCENT = '#8b5cf6';

const valid = (s: unknown): s is string => typeof s === 'string' && /^#[0-9a-f]{6}$/i.test(s);

export function applyAccent(color: string): void {
  if (!valid(color) || color.toLowerCase() === DEFAULT_ACCENT)
    document.documentElement.style.removeProperty('--accent');
  else document.documentElement.style.setProperty('--accent', color);
}

/** Fast first paint from the cached value. */
export function loadAccent(): string {
  let c = DEFAULT_ACCENT;
  try {
    const s = localStorage.getItem(KEY);
    if (valid(s)) c = s;
  } catch {
    /* storage unavailable */
  }
  applyAccent(c);
  return c;
}

/** Apply and cache (the caller persists it through the config API). */
export function saveAccent(color: string): void {
  applyAccent(color);
  try {
    localStorage.setItem(KEY, color);
  } catch {
    /* storage unavailable */
  }
}
