// Shapes the 7 raw input levels (0..1) from the core into a waveform.
// Shared by the pill, onboarding practice area and Voice Lab.

export const SILENT_LEVELS: number[] = [0, 0, 0, 0, 0, 0, 0];

/** Smallest bar height as a fraction, so silence still draws a visible baseline. */
const FLOOR = 0.14;

/**
 * Interpolates the 7 levels into `count` bar heights (FLOOR..1). The ends taper a little so the
 * waveform reads as one shape rather than a row of equal meters.
 */
export function waveHeights(levels: number[], count: number): number[] {
  const src = levels.length ? levels : SILENT_LEVELS;
  const out: number[] = [];
  for (let i = 0; i < count; i++) {
    const t = count === 1 ? 0.5 : i / (count - 1);
    const pos = t * (src.length - 1);
    const lo = Math.floor(pos);
    const hi = Math.min(src.length - 1, lo + 1);
    const raw = src[lo] + (src[hi] - src[lo]) * (pos - lo);
    const taper = 0.55 + 0.45 * Math.sin(Math.PI * t);
    const loud = Math.min(1, Math.pow(Math.max(0, raw), 0.6) * 1.5);
    out.push(FLOOR + (1 - FLOOR) * loud * taper);
  }
  return out;
}
