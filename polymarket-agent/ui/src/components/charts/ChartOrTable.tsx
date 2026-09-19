import { useState, type ReactNode } from "react";

/**
 * Every chart ships with a table twin.
 *
 * Partly this is the palette's doing: the aqua series slot sits below 3:1
 * against the light surface, and the relief rule for a sub-3:1 mark is
 * visible labels *and* a table view. But it is worth having on all of them —
 * a table is exact where a chart is approximate, it is what you want when
 * copying a number out, and it is the honest fallback if a hand-rolled SVG
 * has a bug.
 */
export function ChartOrTable({
  chart,
  table,
  label,
}: {
  chart: ReactNode;
  table: ReactNode;
  label: string;
}) {
  const [mode, setMode] = useState<"chart" | "table">("chart");
  return (
    <div>
      <div
        className="segmented"
        role="group"
        aria-label={`${label} view`}
        style={{ marginBottom: 8 }}
      >
        <button
          type="button"
          aria-pressed={mode === "chart"}
          onClick={() => setMode("chart")}
        >
          Chart
        </button>
        <button
          type="button"
          aria-pressed={mode === "table"}
          onClick={() => setMode("table")}
        >
          Table
        </button>
      </div>
      {mode === "chart" ? chart : table}
    </div>
  );
}
