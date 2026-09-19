import { useEffect, useRef, useState, type RefObject } from "react";

/**
 * Measure a chart's container.
 *
 * Two details that matter more than they look:
 *
 *  - `entry.contentRect` rather than `getBoundingClientRect()`. The latter
 *    forces a synchronous layout on every callback, inside a callback that
 *    fires during layout.
 *  - A 2px hysteresis before committing to state. Without it, a render that
 *    nudges its own container by a sub-pixel re-triggers the observer, which
 *    re-renders, which nudges it again — the classic "ResizeObserver loop
 *    completed with undelivered notifications" feedback loop.
 */
export function useChartSize(
  ref: RefObject<HTMLElement | null>,
): { width: number } {
  const [width, setWidth] = useState(0);
  const committed = useRef(0);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const ro = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.width ?? 0;
      if (Math.abs(next - committed.current) < 2) return;
      committed.current = next;
      setWidth(next);
    });
    ro.observe(el);

    const initial = el.getBoundingClientRect().width;
    committed.current = initial;
    setWidth(initial);

    return () => ro.disconnect();
  }, [ref]);

  return { width };
}
