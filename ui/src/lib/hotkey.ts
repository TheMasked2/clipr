// Hotkey strings: Ctrl, Alt, Shift, Win (in that order) joined with '+', then the key.

const NAMED: Record<string, string> = {
  Insert: 'Insert',
  Delete: 'Delete',
  Home: 'Home',
  End: 'End',
  PageUp: 'PageUp',
  PageDown: 'PageDown',
  Pause: 'Pause',
  ScrollLock: 'ScrollLock',
  PrintScreen: 'PrintScreen',
  Space: 'Space',
  Tab: 'Tab',
  ArrowUp: 'Up',
  ArrowDown: 'Down',
  ArrowLeft: 'Left',
  ArrowRight: 'Right',
};

export function keyFromCode(code: string): string | null {
  let m: RegExpMatchArray | null;
  if ((m = code.match(/^F(\d{1,2})$/)) && +m[1] >= 1 && +m[1] <= 24) return `F${m[1]}`;
  if ((m = code.match(/^Key([A-Z])$/))) return m[1];
  if ((m = code.match(/^Digit(\d)$/))) return m[1];
  if ((m = code.match(/^Numpad(\d)$/))) return `Num${m[1]}`;
  return NAMED[code] ?? null;
}

export type Captured =
  | { kind: 'wait' }
  | { kind: 'cancel' }
  | { kind: 'error'; message: string }
  | { kind: 'ok'; value: string };

const MODIFIER_CODES = [
  'ControlLeft', 'ControlRight', 'AltLeft', 'AltRight',
  'ShiftLeft', 'ShiftRight', 'MetaLeft', 'MetaRight',
];

// Keys that may be used without a modifier.
const BARE_OK = /^(F([1-9]|1\d|2[0-4])|Pause|ScrollLock|Insert|Num\d)$/;

export function capture(e: KeyboardEvent): Captured {
  if (MODIFIER_CODES.includes(e.code)) return { kind: 'wait' };
  const mods = [e.ctrlKey && 'Ctrl', e.altKey && 'Alt', e.shiftKey && 'Shift', e.metaKey && 'Win'].filter(
    Boolean,
  ) as string[];
  if (e.code === 'Escape' && !mods.length) return { kind: 'cancel' };
  const key = keyFromCode(e.code);
  if (!key) return { kind: 'error', message: 'That key is not supported as a hotkey.' };
  if (!mods.length && !BARE_OK.test(key))
    return { kind: 'error', message: 'Add Ctrl, Alt, Shift or Win to that key.' };
  return { kind: 'ok', value: [...mods, key].join('+') };
}

/** A hotkey without any modifier: games see it too. */
export const isBare = (v: string) => v !== '' && !v.includes('+');

/** Names of the actions that share a hotkey with another action. */
export function conflicts(keys: Record<string, string>): Set<string> {
  const seen = new Map<string, string[]>();
  for (const [name, v] of Object.entries(keys)) {
    if (!v) continue;
    const k = v.toLowerCase();
    seen.set(k, [...(seen.get(k) ?? []), name]);
  }
  const out = new Set<string>();
  for (const names of seen.values()) if (names.length > 1) names.forEach((n) => out.add(n));
  return out;
}
