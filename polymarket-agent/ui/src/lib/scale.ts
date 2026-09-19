/** Scales, ticks and decimation for the hand-rolled charts. */

export interface LinearScale {
  (v: number): number;
  invert(px: number): number;
  domain: [number, number];
  range: [number, number];
}

export function linear(
  domain: [number, number],
  range: [number, number],
): LinearScale {
  const [d0, d1] = domain;
  const [r0, r1] = range;
  const span = d1 - d0 || 1;
  const fn = ((v: number) => r0 + ((v - d0) / span) * (r1 - r0)) as LinearScale;
  fn.invert = (px: number) => d0 + ((px - r0) / (r1 - r0 || 1)) * span;
  fn.domain = domain;
  fn.range = range;
  return fn;
}

export interface BandScale {
  at(i: number): number;
  bandwidth: number;
  step: number;
  /** Nearest index for a pixel position, clamped to the data. */
  indexAt(px: number): number;
  count: number;
}

export function band(
  count: number,
  range: [number, number],
  padding = 0.2,
): BandScale {
  const [r0, r1] = range;
  const width = r1 - r0;
  const step = count > 0 ? width / count : width;
  const bandwidth = step * (1 - padding);
  return {
    count,
    step,
    bandwidth,
    at: (i) => r0 + i * step + (step - bandwidth) / 2,
    indexAt: (px) => {
      if (count === 0) return 0;
      const i = Math.floor((px - r0) / step);
      return Math.max(0, Math.min(count - 1, i));
    },
  };
}

/** Min and max, ignoring nulls and NaN — the data is full of both. */
export function extent(values: (number | null | undefined)[]): [number, number] | null {
  let min = Infinity;
  let max = -Infinity;
  for (const v of values) {
    if (v === null || v === undefined || !Number.isFinite(v)) continue;
    if (v < min) min = v;
    if (v > max) max = v;
  }
  return min === Infinity ? null : [min, max];
}

/**
 * Round a domain outward to human numbers and emit the ticks on it, so axes
 * read 0 / 25 / 50 rather than 0 / 23.7 / 47.4.
 */
export function niceTicks(
  min: number,
  max: number,
  target = 5,
): { ticks: number[]; domain: [number, number] } {
  if (!Number.isFinite(min) || !Number.isFinite(max)) {
    return { ticks: [0, 1], domain: [0, 1] };
  }
  if (min === max) {
    const pad = Math.abs(min) * 0.1 || 1;
    min -= pad;
    max += pad;
  }
  const raw = (max - min) / Math.max(1, target);
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const mantissa = raw / mag;
  const step = (mantissa <= 1 ? 1 : mantissa <= 2 ? 2 : mantissa <= 5 ? 5 : 10) * mag;

  const lo = Math.floor(min / step) * step;
  const hi = Math.ceil(max / step) * step;
  const ticks: number[] = [];
  // Accumulate by index, not by repeated addition, to avoid float creep.
  const n = Math.round((hi - lo) / step);
  for (let i = 0; i <= n; i++) ticks.push(lo + i * step);
  return { ticks, domain: [lo, hi] };
}

/**
 * Min/max decimation. A month of cycles is thousands of points on a 900px
 * chart; drawing them all is slow and looks identical. Keeping both extremes
 * of each bucket means spikes survive the reduction instead of being averaged
 * away — which for a bankroll curve is the whole point.
 */
export function decimate<T>(
  rows: T[],
  value: (row: T) => number | null,
  maxPoints: number,
): T[] {
  if (rows.length <= maxPoints || maxPoints < 4) return rows;
  const bucketSize = Math.ceil(rows.length / (maxPoints / 2));
  const out: T[] = [];
  for (let start = 0; start < rows.length; start += bucketSize) {
    const slice = rows.slice(start, start + bucketSize);
    let lo = slice[0];
    let hi = slice[0];
    let loV = Infinity;
    let hiV = -Infinity;
    for (const row of slice) {
      const v = value(row);
      if (v === null || !Number.isFinite(v)) continue;
      if (v < loV) { loV = v; lo = row; }
      if (v > hiV) { hiV = v; hi = row; }
    }
    const first = slice.indexOf(lo) <= slice.indexOf(hi) ? lo : hi;
    const second = first === lo ? hi : lo;
    out.push(first);
    if (second !== first) out.push(second);
  }
  return out;
}
