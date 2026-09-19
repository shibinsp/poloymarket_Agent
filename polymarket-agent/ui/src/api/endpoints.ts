/** One function per route. The only place API paths are written down. */
import { get, type ApiResult } from "./client";
import { asArray, asObject, asObjectOrNull } from "./narrow";
import type {
  ApiCostRecord,
  CycleRecord,
  Health,
  Metrics,
  TradeRecord,
} from "./types";

export const DATASET_KEYS = [
  "health",
  "metrics",
  "trades",
  "tradesAll",
  "cycle",
  "cyclesAll",
  "costs",
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
};

export const DATASET_PATHS: Record<DatasetKey, string> = {
  health: "/api/health",
  metrics: "/api/metrics",
  trades: "/api/trades",
  tradesAll: "/api/trades/all",
  cycle: "/api/cycles",
  cyclesAll: "/api/cycles/all",
  costs: "/api/costs",
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
};

export function fetchHealth(signal?: AbortSignal): Promise<ApiResult<Health>> {
  return get({ path: DATASET_PATHS.health, narrow: asObject<Health>, signal });
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
