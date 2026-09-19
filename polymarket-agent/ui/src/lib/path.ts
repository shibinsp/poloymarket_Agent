/** SVG path builders for the chart marks. */

/**
 * A polyline through the points, with straight segments.
 *
 * Deliberately not smoothed. The previous dashboard used Chart.js with
 * `tension: 0.3`, which draws a curve through bankroll readings and so invents
 * values between cycles that the agent never recorded.
 */
export function linePath(points: [number, number][]): string {
  if (points.length === 0) return "";
  return points
    .map(([x, y], i) => `${i === 0 ? "M" : "L"}${round(x)},${round(y)}`)
    .join(" ");
}

/** The same line closed down to a baseline, for an area fill. */
export function areaPath(points: [number, number][], baselineY: number): string {
  if (points.length === 0) return "";
  const line = linePath(points);
  const first = points[0];
  const last = points[points.length - 1];
  return `${line} L${round(last[0])},${round(baselineY)} L${round(first[0])},${round(baselineY)} Z`;
}

/**
 * A column with a rounded tip and square feet.
 *
 * `<rect rx>` is not usable here: it rounds all four corners, so a bar appears
 * to float off its own baseline. The radius is clamped by both half-width and
 * height so a very short bar degenerates into a flat-topped stub instead of
 * self-intersecting into a blob.
 */
export function roundedBarPath(
  x: number,
  y: number,
  width: number,
  height: number,
  radius = 4,
): string {
  if (height <= 0 || width <= 0) return "";
  const r = Math.min(radius, width / 2, height);
  const x0 = round(x);
  const x1 = round(x + width);
  const y0 = round(y);
  const y1 = round(y + height);
  return [
    `M${x0},${y1}`,
    `L${x0},${round(y + r)}`,
    `Q${x0},${y0} ${round(x + r)},${y0}`,
    `L${round(x + width - r)},${y0}`,
    `Q${x1},${y0} ${x1},${round(y + r)}`,
    `L${x1},${y1}`,
    "Z",
  ].join(" ");
}

/** A bar that may hang below the baseline, for a diverging column. */
export function divergingBarPath(
  x: number,
  baselineY: number,
  valueY: number,
  width: number,
  radius = 4,
): string {
  const up = valueY <= baselineY;
  const height = Math.abs(baselineY - valueY);
  if (height <= 0.5) {
    // A zero-height bar still deserves a visible hairline.
    return `M${round(x)},${round(baselineY)} L${round(x + width)},${round(baselineY)}`;
  }
  if (up) return roundedBarPath(x, valueY, width, height, radius);

  // Mirror: rounded at the bottom, square at the baseline.
  const r = Math.min(radius, width / 2, height);
  const x0 = round(x);
  const x1 = round(x + width);
  const y0 = round(baselineY);
  const y1 = round(valueY);
  return [
    `M${x0},${y0}`,
    `L${x0},${round(valueY - r)}`,
    `Q${x0},${y1} ${round(x + r)},${y1}`,
    `L${round(x + width - r)},${y1}`,
    `Q${x1},${y1} ${x1},${round(valueY - r)}`,
    `L${x1},${y0}`,
    "Z",
  ].join(" ");
}

function round(n: number): number {
  return Math.round(n * 100) / 100;
}
