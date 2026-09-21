/** One function per route. The only place API paths are written down. */
import { get, post, type ApiResult } from "./client";
import { asArray, asObject, asObjectOrNull } from "./narrow";
import type {
  ApiCostRecord,
  CycleRecord,
  DailyEquity,
  FillRecord,
  HaltResponse,
  Health,
  Metrics,
  OrderRecord,
  ReconciliationRun,
  RiskStatus,
  TradeRecord,
  VenueStatus,
} from "./types";

export const DATASET_KEYS = [
  "health",
  "metrics",
  "trades",
  "tradesAll",
  "cycle",
  "cyclesAll",
  "costs",
  "orders",
  "fills",
  "equity",
  "reconciliation",
  "risk",
  "venues",
] as const;
export type DatasetKey = (typeof DATASET_KEYS)[number];

/** Human labels, used by the connection bar and the endpoint matrix. */
export const DATASET_LABELS: Record<DatasetKey, string> = {
  health: "Health",
  metrics: "Metrics",
  trades: "Recent trades",
  tradesAll: "All trades",
  cycle: "Latest cycle",
  cyclesAll: "All cycles",
  costs: "API costs",
  orders: "Orders",
  fills: "Fills",
  equity: "Daily equity",
  reconciliation: "Reconciliation",
  risk: "Risk limits",
  venues: "Venues",
};

export const DATASET_PATHS: Record<DatasetKey, string> = {
  health: "/api/health",
  metrics: "/api/metrics",
  trades: "/api/trades",
  tradesAll: "/api/trades/all",
  cycle: "/api/cycles",
  cyclesAll: "/api/cycles/all",
  costs: "/api/costs",
  orders: "/api/orders",
  fills: "/api/fills",
  equity: "/api/equity",
  reconciliation: "/api/reconciliation",
  risk: "/api/risk",
  venues: "/api/venues",
};

/**
 * The `/all` routes have no server-side limit, so they are polled at a
 * multiple of the base interval rather than every tick. A month of history is
 * ~4,300 cycle rows; re-fetching that every 30 seconds is megabytes of churn
 * for data that changes once per cycle.
 */
export const DATASET_INTERVAL_MULTIPLIER: Record<DatasetKey, number> = {
  health: 1,
  metrics: 1,
  trades: 1,
  cycle: 1,
  tradesAll: 4,
  cyclesAll: 4,
  costs: 4,
  // Capped server-side at 500 rows, so these are cheap enough to poll every
  // tick — and an order changing state is exactly what an operator watching
  // this page is waiting for.
  orders: 1,
  fills: 1,
  risk: 1,
  // One row per day and one per reconciliation pass; neither moves fast.
  equity: 4,
  reconciliation: 4,
  // Fixed at startup: which venues built is decided once and never changes
  // while the process lives. Polled at all only so the page fills in when the
  // dashboard came up before the agent finished initialising.
  venues: 4,
};

export function fetchHealth(signal?: AbortSignal): Promise<ApiResult<Health>> {
  return get({ path: DATASET_PATHS.health, narrow: asObject<Health>, signal });
}

export function fetchOrders(
  signal?: AbortSignal,
): Promise<ApiResult<OrderRecord[]>> {
  return get({
    path: DATASET_PATHS.orders,
    narrow: asArray<OrderRecord>,
    signal,
  });
}

export function fetchFills(
  signal?: AbortSignal,
): Promise<ApiResult<FillRecord[]>> {
  return get({ path: DATASET_PATHS.fills, narrow: asArray<FillRecord>, signal });
}

export function fetchEquity(
  signal?: AbortSignal,
): Promise<ApiResult<DailyEquity[]>> {
  return get({
    path: DATASET_PATHS.equity,
    narrow: asArray<DailyEquity>,
    signal,
  });
}

export function fetchReconciliation(
  signal?: AbortSignal,
): Promise<ApiResult<ReconciliationRun[]>> {
  return get({
    path: DATASET_PATHS.reconciliation,
    narrow: asArray<ReconciliationRun>,
    signal,
  });
}

export function fetchRisk(signal?: AbortSignal): Promise<ApiResult<RiskStatus>> {
  return get({ path: DATASET_PATHS.risk, narrow: asObject<RiskStatus>, signal });
}

export function fetchVenues(signal?: AbortSignal): Promise<ApiResult<VenueStatus[]>> {
  return get({ path: DATASET_PATHS.venues, narrow: asArray<VenueStatus>, signal });
}

/**
 * Stop the agent opening positions. Exits, order polling and reconciliation
 * keep running — this is not a shutdown.
 *
 * The 200 means the flag is set, not that resting orders are already
 * cancelled: the agent does that on its next wake. The response says so.
 */
export function halt(): Promise<ApiResult<HaltResponse>> {
  return post<HaltResponse>("/api/halt");
}

/** Lift the halt, and remove the HALT file so it does not come straight back. */
export function resume(): Promise<ApiResult<HaltResponse>> {
  return post<HaltResponse>("/api/resume");
}

export function fetchMetrics(
  signal?: AbortSignal,
): Promise<ApiResult<Metrics>> {
  return get({
    path: DATASET_PATHS.metrics,
    narrow: asObject<Metrics>,
    signal,
  });
}

export function fetchTrades(
  signal?: AbortSignal,
): Promise<ApiResult<TradeRecord[]>> {
  return get({
    path: DATASET_PATHS.trades,
    narrow: asArray<TradeRecord>,
    signal,
  });
}

export function fetchTradesAll(
  signal?: AbortSignal,
): Promise<ApiResult<TradeRecord[]>> {
  return get({
    path: DATASET_PATHS.tradesAll,
    narrow: asArray<TradeRecord>,
    signal,
  });
}

/** Returns `null` — successfully — when no cycle has completed. */
export function fetchLatestCycle(
  signal?: AbortSignal,
): Promise<ApiResult<CycleRecord | null>> {
  return get({
    path: DATASET_PATHS.cycle,
    narrow: asObjectOrNull<CycleRecord>,
    signal,
  });
}

export function fetchCyclesAll(
  signal?: AbortSignal,
): Promise<ApiResult<CycleRecord[]>> {
  return get({
    path: DATASET_PATHS.cyclesAll,
    narrow: asArray<CycleRecord>,
    signal,
  });
}

export function fetchCosts(
  signal?: AbortSignal,
): Promise<ApiResult<ApiCostRecord[]>> {
  return get({
    path: DATASET_PATHS.costs,
    narrow: asArray<ApiCostRecord>,
    signal,
  });
}
