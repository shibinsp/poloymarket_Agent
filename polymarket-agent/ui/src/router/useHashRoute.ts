import { useEffect, useSyncExternalStore } from "react";
import { DEFAULT_PATH, resolve, type Resolved } from "./routes";

/**
 * Hash routing, because the server serves exactly one route.
 *
 * `index_handler` answers `/` and nothing else, so a real path like
 * `/trades` would 404 on reload. `useSyncExternalStore` is the right
 * primitive here — it is tear-free under concurrent rendering, which a
 * `useState` + `useEffect` listener is not.
 */
function subscribe(cb: () => void): () => void {
  window.addEventListener("hashchange", cb);
  return () => window.removeEventListener("hashchange", cb);
}

function getSnapshot(): string {
  return window.location.hash || "";
}

export function useHashRoute(): Resolved {
  const hash = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  const resolved = resolve(hash);

  useEffect(() => {
    // Only normalise a genuinely empty hash, and with replaceState so the
    // Back button is not trapped on the landing page.
    if (window.location.hash === "") {
      window.history.replaceState(null, "", "#" + DEFAULT_PATH);
    }
  }, [hash]);

  useEffect(() => {
    document.title = resolved.route
      ? `${resolved.route.title} · Polymarket Agent`
      : "Not found · Polymarket Agent";
  }, [resolved.route]);

  return resolved;
}
