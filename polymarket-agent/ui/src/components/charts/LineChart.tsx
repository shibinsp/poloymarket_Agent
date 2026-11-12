import { Cartesian, type Series } from "./Cartesian";
import { areaPath, linePath, splitSegments } from "../../lib/path";
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
              const segments = splitSegments(
                s.values.map((v, i) =>
                  v === null || !Number.isFinite(v)
                    ? null
                    : [x.at(i) + x.bandwidth / 2, y(v)],
                ),
              );
              if (segments.length === 0) return null;
              return (
                <g key={s.name}>
                  {segments.map((pts, i) => (
                    <g key={i}>
                      {area && pts.length > 1 && (
                        <path
                          d={areaPath(pts, plotH)}
                          fill={s.color}
                          fillOpacity={0.1}
                          stroke="none"
                        />
                      )}
                      {pts.length > 1 && (
                        <path
                          d={linePath(pts)}
                          fill="none"
                          stroke={s.color}
                          strokeWidth={2}
                          strokeLinejoin="round"
                          strokeLinecap="round"
                        />
                      )}
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
                  ))}
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
