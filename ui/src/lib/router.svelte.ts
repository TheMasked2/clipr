export type Route = 'home' | 'gallery' | 'settings' | 'clip';

interface RouteState {
  route: Route;
  id: string;
  /** Returns true when leaving the current page would lose unsaved changes. */
  guard: (() => boolean) | null;
}

function parse(): { route: Route; id: string } {
  const h = location.hash.replace(/^#\/?/, '');
  if (h.startsWith('clip/')) {
    try {
      return { route: 'clip', id: decodeURIComponent(h.slice(5)) };
    } catch {
      return { route: 'gallery', id: '' };
    }
  }
  if (h === 'gallery' || h === 'settings') return { route: h, id: '' };
  // A settings section: #/settings/stream.
  if (h.startsWith('settings/')) return { route: 'settings', id: h.slice(9) };
  return { route: 'home', id: '' };
}

export const router = $state<RouteState>({ ...parse(), guard: null });

export const clipHref = (id: string) => `#/clip/${encodeURIComponent(id)}`;
export const go = (hash: string) => {
  location.hash = hash;
};

let last = location.hash;
let reverting = false;

window.addEventListener('hashchange', () => {
  if (reverting) {
    reverting = false;
    return;
  }
  if (router.guard?.() && !confirm('You have unsaved changes. Leave without saving?')) {
    reverting = true;
    location.hash = last;
    return;
  }
  last = location.hash;
  Object.assign(router, parse());
});

/** Swap the URL for a renamed clip without adding a history entry. */
export function replaceClipRoute(id: string): void {
  history.replaceState(null, '', clipHref(id));
  last = location.hash;
  router.id = id;
}
