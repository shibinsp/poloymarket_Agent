import { Cartesian, type Series } from "./Cartesian";
import { areaPath, linePath } from "../../lib/path";
import { Legend } from "./Legend";

/**
 * Line, optionally filled to the baseline.
 *
 * Segments are straight. A smoothed curve through discrete cycle readings
 * draws bankroll values the agent never recorded, which on a money chart is
 * not a stylistic choice.
 */
export function LineChart({
  categories,
  series,
  formatY,
  formatX,
  height,
  area = false,
  includeZero = false,
  ariaLabel,
}: {
  categories: string[];
  series: Series[];
  formatY: (v: number) => string;
  formatX?: (c: string, i: number) => string;
  height?: number;
  area?: boolean;
  includeZero?: boolean;
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
        includeZero={includeZero}
        padding={0}
        ariaLabel={ariaLabel}
      >
        {({ x, y, plotH }) => (
          <g>
            {series.map((s) => {
              const pts: [number, number][] = [];
              s.values.forEach((v, i) => {
                if (v === null || !Number.isFinite(v)) return;
                pts.push([x.at(i) + x.bandwidth / 2, y(v)]);
              });
              if (pts.length === 0) return null;
              return (
                <g key={s.name}>
                  {area && (
                    <path
                      d={areaPath(pts, plotH)}
                      fill={s.color}
                      fillOpacity={0.1}
                      stroke="none"
                    />
                  )}
                  <path
                    d={linePath(pts)}
                    fill="none"
                    stroke={s.color}
                    strokeWidth={2}
                    strokeLinejoin="round"
                    strokeLinecap="round"
                  />
                  {/* A lone reading would otherwise draw nothing at all. */}
                  {pts.length === 1 && (
                    <circle
                      cx={pts[0][0]}
                      cy={pts[0][1]}
                      r={4}
                      fill={s.color}
                      stroke="var(--surface-1)"
                      strokeWidth={2}
                    />
                  )}
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
