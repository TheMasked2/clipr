// Shared by the level meters: -60 dBFS and below shows as an empty bar.
export const FLOOR = -60;

/** How full a meter bar is for a level in dBFS, 0 to 100. */
export const pct = (db: number) => Math.round((Math.min(0, Math.max(FLOOR, db)) - FLOOR) * (100 / -FLOOR));

/** A level for display: "-∞ dB" at or below the floor. */
export const fmtDb = (db: number) => `${db <= FLOOR ? '-∞' : Math.round(db)} dB`;
