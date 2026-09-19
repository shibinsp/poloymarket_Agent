import { useSyncExternalStore } from "react";

/**
 * One app-wide clock.
 *
 * Every relative timestamp on the page re-renders from this, so they all tick
 * together rather than drifting apart, and a page showing thirty relative
 * times costs one timer rather than thirty.
 */
let now = Date.now();
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | null = null;

function start(): void {
  if (timer !== null) return;
  timer = setInterval(() => {
    now = Date.now();
    for (const l of listeners) l();
  }, 1000);
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  start();
  return () => {
    listeners.delete(l);
    if (listeners.size === 0 && timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  };
}

export function useNow(): number {
  return useSyncExternalStore(
    subscribe,
    () => now,
    () => now,
  );
}
