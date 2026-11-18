// The in-game overlay: clipr opens this page with `?overlay` in a window over the game (crates/clipr/src/ingame.rs).
// The page then shows as a panel over a dimmed game, and its ✕, Esc or a click beside the panel close it.

/** Whether this page runs in the in-game overlay. */
export const inOverlay = new URLSearchParams(location.search).has('overlay');

/** Asks clipr to close the overlay (and give the focus back to the game). */
export function closeOverlay(): void {
  (window as unknown as { ipc?: { postMessage(m: string): void } }).ipc?.postMessage('close');
}
