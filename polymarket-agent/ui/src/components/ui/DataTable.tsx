import { useMemo, useState, type ReactNode } from "react";
import { cls } from "../../lib/cls";

export interface Column<T> {
  key: string;
  header: string;
  /** Right-aligned and tabular. */
  numeric?: boolean;
  /** Ellipsis-truncated with the full value in `title`. */
  truncate?: boolean;
  render: (row: T) => ReactNode;
  /** Returning null sorts the row to the end regardless of direction. */
  sortValue?: (row: T) => number | string | null;
  title?: (row: T) => string | undefined;
}

export function DataTable<T>({
  rows,
  columns,
  rowKey,
  initialSort,
  initialDesc = true,
  empty,
}: {
  rows: T[];
  columns: Column<T>[];
  rowKey: (row: T, i: number) => string;
  initialSort?: string;
  initialDesc?: boolean;
  empty?: ReactNode;
}) {
  const [sortKey, setSortKey] = useState<string | undefined>(initialSort);
  const [desc, setDesc] = useState(initialDesc);

  const sorted = useMemo(() => {
    const col = columns.find((c) => c.key === sortKey);
    if (!col?.sortValue) return rows;
    const get = col.sortValue;
    // Copy: the caller's array is shared with other views.
    return [...rows].sort((a, b) => {
      const av = get(a);
      const bv = get(b);
      // Missing values sink, whichever way the column is sorted — a blank is
      // not "smallest", it is "unknown".
      if (av === null && bv === null) return 0;
      if (av === null) return 1;
      if (bv === null) return -1;
      const cmp = typeof av === "number" && typeof bv === "number"
        ? av - bv
        : String(av).localeCompare(String(bv));
      return desc ? -cmp : cmp;
    });
  }, [rows, columns, sortKey, desc]);

  if (rows.length === 0 && empty) return <>{empty}</>;

  return (
    <div className="table-wrap">
      <table className="data">
        <thead>
          <tr>
            {columns.map((c) => {
              const active = c.key === sortKey;
              return (
                <th
                  key={c.key}
                  className={cls(c.numeric && "num", c.sortValue && "sortable")}
                  aria-sort={
                    active ? (desc ? "descending" : "ascending") : undefined
                  }
                  onClick={() => {
                    if (!c.sortValue) return;
                    if (active) setDesc((d) => !d);
                    else {
                      setSortKey(c.key);
                      setDesc(true);
                    }
                  }}
                >
                  {c.header}
                  {active ? (desc ? " ↓" : " ↑") : ""}
                </th>
              );
            })}
          </tr>
        </thead>
        <tbody>
          {sorted.map((row, i) => (
            <tr key={rowKey(row, i)}>
              {columns.map((c) => (
                <td
                  key={c.key}
                  className={cls(c.numeric && "num", c.truncate && "truncate")}
                  title={c.title?.(row)}
                >
                  {c.render(row)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
