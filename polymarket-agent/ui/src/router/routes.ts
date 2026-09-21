export interface RouteDef {
  path: string;
  label: string;
  title: string;
}

/** One list driving the nav, the router and document.title. */
export const ROUTES: RouteDef[] = [
  { path: "/overview", label: "Overview", title: "Overview" },
  { path: "/trades", label: "Trades", title: "Trades" },
  { path: "/orders", label: "Orders", title: "Orders and fills" },
  { path: "/risk", label: "Risk", title: "Risk and reconciliation" },
  { path: "/venues", label: "Venues", title: "Venues and platforms" },
  { path: "/cycles", label: "Cycles", title: "Cycles" },
  { path: "/costs", label: "Costs", title: "API costs" },
  { path: "/health", label: "Health", title: "Health" },
  { path: "/settings", label: "Settings", title: "Settings" },
];

export const DEFAULT_PATH = "/overview";

export interface Resolved {
  path: string;
  route: RouteDef | null;
  /** What the operator actually asked for, when it matched nothing. */
  attempted: string | null;
}

/**
 * Turn a location hash into a route.
 *
 * An unknown hash resolves to `route: null` so the app can render a "no such
 * page" view naming what was asked for. Silently redirecting to the overview
 * would hide a broken bookmark or a typo in a link.
 */
export function resolve(hash: string): Resolved {
  let raw = hash.startsWith("#") ? hash.slice(1) : hash;
  // Drop a query string so a future `?filter=` cannot 404 the whole page.
  const q = raw.indexOf("?");
  if (q !== -1) raw = raw.slice(0, q);
  raw = raw.replace(/\/+$/, "").toLowerCase();

  if (raw === "" || raw === "/") {
    return { path: DEFAULT_PATH, route: ROUTES[0], attempted: null };
  }
  const path = raw.startsWith("/") ? raw : "/" + raw;
  const route = ROUTES.find((r) => r.path === path) ?? null;
  return { path, route, attempted: route ? null : path };
}
