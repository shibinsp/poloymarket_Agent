import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useChartSize } from "./useChartSize";
import { band, linear, niceTicks, type BandScale, type LinearScale } from "../../lib/scale";

export interface Series {
  name: string;
  color: string;
  /** One value per category; null leaves a gap rather than drawing zero. */
  values: (number | null)[];
}

export interface CartesianRender {
  x: BandScale;
  y: LinearScale;
  plotW: number;
  plotH: number;
  zeroY: number;
}

const MARGIN = { top: 12, right: 16, bottom: 30, left: 56 };
const DEFAULT_HEIGHT = 200;

/**
 * The shared frame: measured sizing, nice axes, a grid, and the hover layer.
 *
 * Rendered at measured CSS pixels rather than by scaling a `viewBox` — a
 * scaled viewBox stretches the text and fattens the strokes along with the
 * geometry, so a wide chart ends up with 3px lines and squashed labels.
 */
export function Cartesian({
  categories,
  series,
  height = DEFAULT_HEIGHT,
  formatY,
  formatX,
  includeZero = true,
  padding = 0.2,
  children,
  ariaLabel,
}: {
  categories: string[];
  series: Series[];
  height?: number;
  formatY: (v: number) => string;
  formatX?: (c: string, i: number) => string;
  /** Money and counts include zero; a bankroll curve does not — forcing 0
   *  flattens a line that lives around 100 into a wobble at the top. */
  includeZero?: boolean;
  padding?: number;
  children: (r: CartesianRender) => ReactNode;
  ariaLabel: string;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const { width } = useChartSize(hostRef);
  const [hover, setHover] = useState<number | null>(null);
  const frame = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (frame.current !== null) cancelAnimationFrame(frame.current);
    },
    [],
  );

  const plotW = Math.max(0, width - MARGIN.left - MARGIN.right);
  const plotH = Math.max(0, height - MARGIN.top - MARGIN.bottom);

  const flat = series.flatMap((s) => s.values);
  let lo = Infinity;
  let hi = -Infinity;
  for (const v of flat) {
    if (v === null || !Number.isFinite(v)) continue;
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  }
  if (lo === Infinity) {
    lo = 0;
    hi = 1;
  }
  if (includeZero) {
    lo = Math.min(lo, 0);
    hi = Math.max(hi, 0);
  } else {
    const pad = (hi - lo) * 0.08 || Math.abs(hi) * 0.05 || 1;
    lo -= pad;
    hi += pad;
  }

  const { ticks, domain } = niceTicks(lo, hi, 5);
  const y = linear(domain, [plotH, 0]);
  const x = band(categories.length, [0, plotW], padding);
  const zeroY = y(0);

  const onMove = useCallback(
    (e: React.PointerEvent<SVGRectElement>) => {
      const rect = e.currentTarget.getBoundingClientRect();
      const px = e.clientX - rect.left;
      if (frame.current !== null) return; // one update per animation frame
      frame.current = requestAnimationFrame(() => {
        frame.current = null;
        setHover(x.indexAt(px));
      });
    },
    [x],
  );

  const onKey = useCallback(
    (e: React.KeyboardEvent<SVGSVGElement>) => {
      if (categories.length === 0) return;
      if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
        e.preventDefault();
        setHover((h) => {
          const base = h ?? 0;
          const next = base + (e.key === "ArrowRight" ? 1 : -1);
          return Math.max(0, Math.min(categories.length - 1, next));
        });
      } else if (e.key === "Escape") {
        setHover(null);
      }
    },
    [categories.length],
  );

  if (width === 0) {
    return <div className="chart" ref={hostRef} style={{ height }} />;
  }

  // Show roughly one label per 70px so they cannot collide.
  const labelEvery = Math.max(1, Math.ceil(categories.length / Math.max(1, Math.floor(plotW / 70))));

  return (
    <div className="chart" ref={hostRef}>
      <svg
        width={width}
        height={height}
        role="img"
        aria-label={ariaLabel}
        tabIndex={0}
        onKeyDown={onKey}
        onBlur={() => setHover(null)}
      >
        <g transform={`translate(${MARGIN.left},${MARGIN.top})`}>
          <g className="chart__grid" aria-hidden="true">
            {ticks.map((t) => (
              <line key={t} x1={0} x2={plotW} y1={y(t)} y2={y(t)} />
            ))}
          </g>

          <g className="chart__axis" aria-hidden="true">
            {ticks.map((t) => (
              <text key={t} x={-8} y={y(t)} textAnchor="end" dominantBaseline="middle">
                {formatY(t)}
              </text>
            ))}
            {categories.map((c, i) =>
              i % labelEvery === 0 ? (
                <text
                  key={c + i}
                  x={x.at(i) + x.bandwidth / 2}
                  y={plotH + 16}
                  textAnchor="middle"
                >
                  {formatX ? formatX(c, i) : c}
                </text>
              ) : null,
            )}
          </g>

          {children({ x, y, plotW, plotH, zeroY })}

          {hover !== null && categories[hover] !== undefined && (
            <line
              className="chart__crosshair"
              x1={x.at(hover) + x.bandwidth / 2}
              x2={x.at(hover) + x.bandwidth / 2}
              y1={0}
              y2={plotH}
              aria-hidden="true"
            />
          )}

          <line className="chart__baseline" x1={0} x2={plotW} y1={plotH} y2={plotH} />

          <rect
            x={0}
            y={0}
            width={plotW}
            height={plotH}
            fill="transparent"
            onPointerMove={onMove}
            onPointerLeave={() => setHover(null)}
          />
        </g>
      </svg>

      {hover !== null && categories[hover] !== undefined && (
        <Tooltip
          left={MARGIN.left + x.at(hover) + x.bandwidth / 2}
          width={width}
          head={categories[hover]}
          rows={series
            .map((s) => ({
              name: s.name,
              color: s.color,
              value: s.values[hover],
            }))
            .filter((r) => r.value !== null && r.value !== undefined)}
          format={formatY}
        />
      )}
    </div>
  );
}

function Tooltip({
  left,
  width,
  head,
  rows,
  format,
}: {
  left: number;
  width: number;
  head: string;
  rows: { name: string; color: string; value: number | null }[];
  format: (v: number) => string;
}) {
  // Flip before the tip would run off the right edge.
  const flip = left > width * 0.6;
  return (
    <div
      className="chart__tip"
      style={
        flip
          ? { right: Math.max(4, width - left + 12), top: 8 }
          : { left: left + 12, top: 8 }
      }
    >
      <div className="chart__tip-head">{head}</div>
      {rows.map((r) => (
        <div className="chart__tip-row" key={r.name}>
          <span
            className="chart__tip-swatch"
            style={{ background: r.color }}
            aria-hidden="true"
          />
          <span className="chart__tip-name">{r.name}</span>
          <span className="chart__tip-val">
            {r.value === null ? "—" : format(r.value)}
          </span>
        </div>
      ))}
    </div>
  );
}
