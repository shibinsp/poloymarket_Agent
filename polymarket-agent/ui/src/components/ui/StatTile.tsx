import type { ReactNode } from "react";
import { cls } from "../../lib/cls";

/**
 * A single number. Per the form guidance this is the right shape for a
 * headline figure — a one-value bar chart communicates less than the value
 * written out.
 */
export function StatTile({
  label,
  value,
  sub,
  tone,
  hero,
  title,
}: {
  label: string;
  value: ReactNode;
  sub?: ReactNode;
  /** Only for signed quantities where direction is the point. */
  tone?: "pos" | "neg" | null;
  hero?: boolean;
  title?: string;
}) {
  return (
    <div title={title}>
      <div className="tile__label">{label}</div>
      <div
        className={cls(
          "tile__value",
          hero && "tile__value--hero",
          tone === "pos" && "is-pos",
          tone === "neg" && "is-neg",
        )}
      >
        {value}
      </div>
      {sub && <div className="tile__sub">{sub}</div>}
    </div>
  );
}

/** Sign of a value, for tiles where direction carries meaning. */
export function toneOf(v: number | null | undefined): "pos" | "neg" | null {
  if (v === null || v === undefined || v === 0) return null;
  return v > 0 ? "pos" : "neg";
}
