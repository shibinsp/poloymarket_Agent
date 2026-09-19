import { Cartesian, type Series } from "./Cartesian";
import { divergingBarPath, roundedBarPath } from "../../lib/path";
import { Legend } from "./Legend";

const MAX_BAR = 24;
/** Keeps a 2px surface gap between neighbours by trimming width, not stroke. */
const GAP = 2;

/**
 * Columns. Handles one series, several side-by-side, and signed values that
 * hang below a zero rule.
 */
export function ColumnChart({
  categories,
  series,
  formatY,
  formatX,
  height,
  diverging = false,
  ariaLabel,
}: {
  categories: string[];
  series: Series[];
  formatY: (v: number) => string;
  formatX?: (c: string, i: number) => string;
  height?: number;
  diverging?: boolean;
  ariaLabel: string;
}) {
  return (
    <>
      <Cartesian
        categories={categories}
        series={series}
        formatY={formatY}
        formatX={formatX}
        height={height}
        includeZero
        ariaLabel={ariaLabel}
      >
        {({ x, y, plotW, zeroY }) => (
          <g>
            {diverging && (
              <line
                className="chart__baseline"
                x1={0}
                x2={plotW}
                y1={zeroY}
                y2={zeroY}
              />
            )}
            {series.map((s, si) => {
              const slot = Math.min(MAX_BAR, x.bandwidth / series.length);
              const w = Math.max(1, slot - GAP);
              return (
                <g key={s.name}>
                  {s.values.map((v, i) => {
                    if (v === null || !Number.isFinite(v)) return null;
                    const groupLeft =
                      x.at(i) + (x.bandwidth - slot * series.length) / 2;
                    const bx = groupLeft + si * slot;
                    const vy = y(v);
                    const d = diverging
                      ? divergingBarPath(bx, zeroY, vy, w)
                      : roundedBarPath(bx, vy, w, Math.max(0, y(0) - vy));
                    if (!d) return null;
                    const tone =
                      diverging && v < 0 ? "var(--status-critical)" : s.color;
                    return (
                      <path
                        key={i}
                        d={d}
                        fill={diverging ? tone : s.color}
                      >
                        <title>{`${categories[i]}: ${formatY(v)}`}</title>
                      </path>
                    );
                  })}
                </g>
              );
            })}
          </g>
        )}
      </Cartesian>
      <Legend items={series.map((s) => ({ name: s.name, color: s.color }))} />
    </>
  );
}
