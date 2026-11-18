// Shared by the level meters: -60 dBFS and below shows as an empty bar.
export const FLOOR = -60;

/** How full a meter bar is for a level in dBFS, 0 to 100. */
export const pct = (db: number) => !Number.isFinite(db) ? 0 : Math.round((Math.min(0, Math.max(FLOOR, db)) - FLOOR) * (100 / -FLOOR));

/** Where a meter turns yellow and red (in % of its length), like OBS: green up to -20 dB, yellow to -9 dB. */
export const YELLOW_AT = pct(-20);
export const RED_AT = pct(-9);

/** A level's left and right channel; a level from an older clipr has only the louder one. */
export const channels = (lr: [number, number] | undefined, both: number): [number, number] => lr ?? [both, both];

/** A level for display: "-∞ dB" at or below the floor. */
export const fmtDb = (db: number) => `${db <= FLOOR ? '-∞' : Math.round(db)} dB`;
