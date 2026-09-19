/**
 * A legend is present whenever there are two or more series — identity is
 * never carried by colour alone. A single-series chart has no legend: its
 * title already names it.
 *
 * The label wears text ink, never the series colour; the swatch beside it
 * carries identity.
 */
export function Legend({
  items,
}: {
  items: { name: string; color: string }[];
}) {
  if (items.length < 2) return null;
  return (
    <div className="legend">
      {items.map((it) => (
        <span className="legend__item" key={it.name}>
          <span
            className="legend__swatch"
            style={{ background: it.color }}
            aria-hidden="true"
          />
          {it.name}
        </span>
      ))}
    </div>
  );
}
