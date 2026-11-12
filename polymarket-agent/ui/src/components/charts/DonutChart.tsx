import { useState } from "react";

export interface DonutSegment {
  label: string;
  value: number;
  color: string;
  sublabel?: string;
}

export function DonutChart({
  segments,
  centerLabel = "Total",
  centerValue,
  size = 180,
  strokeWidth = 26,
  ariaLabel,
}: {
  segments: DonutSegment[];
  centerLabel?: string;
  centerValue?: string;
  size?: number;
  strokeWidth?: number;
  ariaLabel: string;
}) {
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null);

  const valueOf = (value: number) =>
    Number.isFinite(value) ? Math.max(0, value) : 0;
  const total = segments.reduce((sum, s) => sum + valueOf(s.value), 0);
  const radius = (size - strokeWidth) / 2;
  const center = size / 2;
  const circumference = 2 * Math.PI * radius;

  let cumulativeAngle = 0;

  const renderedSegments = segments.map((seg, idx) => {
    const value = valueOf(seg.value);
    const fraction = total > 0 ? value / total : 0;
    const strokeDasharray = `${fraction * circumference} ${circumference}`;
    const strokeDashoffset = -cumulativeAngle * circumference;
    cumulativeAngle += fraction;

    const isHovered = hoveredIndex === idx;

    return (
      <circle
        key={seg.label}
        cx={center}
        cy={center}
        r={radius}
        fill="transparent"
        stroke={seg.color}
        strokeWidth={isHovered ? strokeWidth + 4 : strokeWidth}
        strokeDasharray={strokeDasharray}
        strokeDashoffset={strokeDashoffset}
        strokeLinecap="butt"
        style={{
          transition: "stroke-width 0.15s ease, filter 0.15s ease",
          cursor: "pointer",
          filter: isHovered ? "brightness(1.15)" : "none",
        }}
        onMouseEnter={() => setHoveredIndex(idx)}
        onMouseLeave={() => setHoveredIndex(null)}
      />
    );
  });

  const activeSegment = hoveredIndex !== null ? segments[hoveredIndex] : null;

  return (
    <div
      role="group"
      aria-label={ariaLabel}
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        gap: "24px",
        flexWrap: "wrap",
        padding: "8px 0",
      }}
    >
      {/* SVG Donut / Pie */}
      <div style={{ position: "relative", width: size, height: size, flexShrink: 0 }}>
        <svg
          width={size}
          height={size}
          style={{ transform: "rotate(-90deg)", overflow: "visible" }}
          aria-hidden="true"
        >
          {total === 0 ? (
            <circle
              cx={center}
              cy={center}
              r={radius}
              fill="transparent"
              stroke="var(--surface-2)"
              strokeWidth={strokeWidth}
            />
          ) : (
            renderedSegments
          )}
        </svg>

        {/* Center Text */}
        <div
          style={{
            position: "absolute",
            inset: 0,
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            pointerEvents: "none",
            textAlign: "center",
            padding: "0 6px",
          }}
        >
          <span style={{ fontSize: "11px", color: "var(--text-muted)", textTransform: "uppercase", letterSpacing: "0.04em" }}>
            {activeSegment ? activeSegment.label : centerLabel}
          </span>
          <span style={{ fontSize: "16px", fontWeight: "700", color: "var(--text-primary)", fontVariantNumeric: "tabular-nums" }}>
            {activeSegment
              ? Number.isFinite(activeSegment.value)
                ? `${total > 0 ? ((valueOf(activeSegment.value) / total) * 100).toFixed(1) : 0}%`
                : "—"
              : (centerValue ?? total.toFixed(0))}
          </span>
          {activeSegment?.sublabel && (
            <span style={{ fontSize: "10px", color: "var(--text-secondary)", marginTop: "2px" }}>
              {activeSegment.sublabel}
            </span>
          )}
        </div>
      </div>

      {/* Legend / Breakdown */}
      <div style={{ display: "flex", flexDirection: "column", gap: "8px", minWidth: "160px" }}>
        {segments.map((seg, idx) => {
          const pct = Number.isFinite(seg.value)
            ? total > 0
              ? ((valueOf(seg.value) / total) * 100).toFixed(1)
              : "0.0"
            : "—";
          const isHovered = hoveredIndex === idx;

          return (
            <button
              type="button"
              className="donut__legend-item"
              aria-label={`${seg.label}: ${pct === "—" ? "unavailable" : `${pct}%`}`}
              aria-pressed={isHovered}
              key={seg.label}
              onMouseEnter={() => setHoveredIndex(idx)}
              onMouseLeave={() => setHoveredIndex(null)}
              onFocus={() => setHoveredIndex(idx)}
              onBlur={() => setHoveredIndex(null)}
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: "12px",
                padding: "4px 8px",
                borderRadius: "var(--radius-sm)",
                background: isHovered ? "var(--surface-2)" : "transparent",
                cursor: "pointer",
                transition: "background 0.12s ease",
                border: 0,
                color: "inherit",
                font: "inherit",
                textAlign: "left",
                width: "100%",
              }}
            >
              <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
                <span
                  style={{
                    width: "10px",
                    height: "10px",
                    borderRadius: "50%",
                    backgroundColor: seg.color,
                    flexShrink: 0,
                  }}
                />
                <span style={{ fontSize: "13px", color: "var(--text-secondary)", fontWeight: isHovered ? "600" : "normal" }}>
                  {seg.label}
                </span>
              </div>
              <div style={{ textAlign: "right" }}>
                <strong style={{ fontSize: "13px", color: "var(--text-primary)", fontVariantNumeric: "tabular-nums" }}>
                  {pct}{pct === "—" ? "" : "%"}
                </strong>
                {seg.sublabel && (
                  <span style={{ display: "block", fontSize: "11px", color: "var(--text-muted)" }}>
                    {seg.sublabel}
                  </span>
                )}
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
}
