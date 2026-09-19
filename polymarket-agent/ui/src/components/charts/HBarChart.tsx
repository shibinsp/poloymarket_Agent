/**
 * Horizontal bars for a handful of nominal categories.
 *
 * Plain HTML rather than SVG: the marks are axis-aligned rectangles and the
 * labels are text, both of which a flex row does natively and responsively
 * with no measuring. (An earlier pass built this in SVG with `foreignObject`,
 * which is a portability trap and buys nothing here.)
 *
 * One hue for every bar. Ramping colour across nominal categories implies an
 * ordering that does not exist — "openai" is not more than "anthropic". Every
 * bar is directly labelled with its value, which is also the relief the
 * light-mode palette requires.
 */
export function HBarChart({
  rows,
  formatValue,
  color = "var(--series-1)",
  ariaLabel,
}: {
  rows: { label: string; value: number }[];
  formatValue: (v: number) => string;
  color?: string;
  ariaLabel: string;
}) {
  const max = rows.reduce((m, r) => Math.max(m, r.value), 0);

  return (
    <div role="img" aria-label={ariaLabel}>
      {rows.map((r) => {
        const pct = max > 0 ? (r.value / max) * 100 : 0;
        return (
          <div key={r.label} style={{ marginBottom: 10 }}>
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                gap: 8,
                fontSize: 12,
                marginBottom: 3,
              }}
            >
              <span>{r.label}</span>
              <span className="mono">{formatValue(r.value)}</span>
            </div>
            <div
              style={{
                background: "var(--surface-2)",
                borderRadius: 4,
                height: 10,
                overflow: "hidden",
              }}
            >
              <div
                style={{
                  width: `${pct}%`,
                  height: "100%",
                  background: color,
                  borderRadius: 4,
                }}
              />
            </div>
          </div>
        );
      })}
    </div>
  );
}
