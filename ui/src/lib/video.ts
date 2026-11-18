// The Simple video choices, shared by Settings → Video and Home's capture card.

export const QUALITY = [
  { name: 'Low', kbps: 8000 },
  { name: 'Medium', kbps: 15000 },
  { name: 'High', kbps: 25000 },
  { name: 'Ultra', kbps: 40000 },
];

export const HEIGHTS = [
  { v: 720, t: '720p' },
  { v: 1080, t: '1080p' },
  { v: 1440, t: '1440p' },
  { v: 0, t: 'Native (monitor resolution)' },
];

/** The quality preset closest to a bitrate. */
export function nearestQuality(kbps: number): number {
  return QUALITY.reduce((b, p, i) => (Math.abs(p.kbps - kbps) < Math.abs(QUALITY[b].kbps - kbps) ? i : b), 0);
}
