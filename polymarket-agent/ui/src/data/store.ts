/**
 * One poller for the whole app.
 *
 * Pages subscribe to the datasets they need; each tick fetches only what is
 * subscribed. The alternative — a hook per page each with its own timer —
 * gives four uncoordinated clocks on the Overview page alone and four
 * disagreeing "last updated" times, which is precisely the kind of quiet
 * inconsistency a monitoring page must not have.
 */
import type { ApiFailure, ApiResult } from "../api/client";
import {
  DATASET_INTERVAL_MULTIPLIER,
  DATASET_KEYS,
  fetchCosts,
  fetchCyclesAll,
  fetchEquity,
  fetchFills,
  fetchHealth,
  fetchLatestCycle,
  fetchMetrics,
  fetchOrders,
  fetchReconciliation,
  fetchRisk,
  fetchVenues,
  fetchTrades,
  fetchTradesAll,
  type DatasetKey,
} from "../api/endpoints";
import { settings } from "./settings";

export interface DatasetState {
  /** `undefined` means "never loaded" — distinct from a loaded empty list. */
  data: unknown;
  error: ApiFailure | null;
  loading: boolean;
  lastSuccessAt: number | null;
  lastAttemptAt: number | null;
  latencyMs: number | null;
  /** Consecutive network failures, driving the backoff. */
  networkFailures: number;
}

type Fetcher = (signal?: AbortSignal) => Promise<ApiResult<unknown>>;

const FETCHERS: Record<DatasetKey, Fetcher> = {
  health: fetchHealth,
  metrics: fetchMetrics,
  trades: fetchTrades,
  tradesAll: fetchTradesAll,
  cycle: fetchLatestCycle,
  cyclesAll: fetchCyclesAll,
  costs: fetchCosts,
  orders: fetchOrders,
  fills: fetchFills,
  equity: fetchEquity,
  reconciliation: fetchReconciliation,
  risk: fetchRisk,
  venues: fetchVenues,
};

function blank(): DatasetState {
  return {
    data: undefined,
    error: null,
    loading: false,
    lastSuccessAt: null,
    lastAttemptAt: null,
    latencyMs: null,
    networkFailures: 0,
  };
}

let states: Record<DatasetKey, DatasetState> = Object.fromEntries(
  DATASET_KEYS.map((k) => [k, blank()]),
) as Record<DatasetKey, DatasetState>;

const refCounts = new Map<DatasetKey, number>();
const inFlight = new Map<DatasetKey, AbortController>();
const listeners = new Set<() => void>();

let timer: ReturnType<typeof setTimeout> | null = null;
let ticks = 0;
let paused = document.hidden;
let lastVisibilityFetch = 0;

function notify(): void {
  // A fresh object each time so useSyncExternalStore sees the change.
  states = { ...states };
  for (const l of listeners) l();
}

function setState(key: DatasetKey, patch: Partial<DatasetState>): void {
  states[key] = { ...states[key], ...patch };
  notify();
}

/**
 * Back off on repeated network failures so a stopped agent is not hammered for
 * hours, while a single blip still recovers on the next ordinary tick.
 */
function backoffMultiplier(key: DatasetKey): number {
  const n = states[key].networkFailures;
  if (n === 0) return 1;
  if (n === 1) return 1;
  if (n === 2) return 2;
  return 4;
}

async function fetchOne(key: DatasetKey, force: boolean): Promise<void> {
  const existing = inFlight.get(key);
  if (existing) {
    // A scheduled tick never stacks on an in-flight request; a manual refresh
    // replaces it, so the operator's click always visibly does something.
    if (!force) return;
    existing.abort();
  }

  const controller = new AbortController();
  inFlight.set(key, controller);
  setState(key, { loading: true });

  const started = performance.now();
  const result = await FETCHERS[key](controller.signal);
  const latencyMs = Math.round(performance.now() - started);

  // A newer request replaced this one; discard the stale answer.
  if (inFlight.get(key) !== controller) return;
  inFlight.delete(key);

  if (result.ok) {
    setState(key, {
      data: result.value,
      error: null,
      loading: false,
      lastSuccessAt: Date.now(),
      lastAttemptAt: Date.now(),
      latencyMs,
      networkFailures: 0,
    });
    return;
  }

  // An aborted request is not a failure worth reporting.
  if (result.error.kind === "network" && controller.signal.aborted) {
    setState(key, { loading: false });
    return;
  }

  setState(key, {
    error: result.error,
    loading: false,
    lastAttemptAt: Date.now(),
    latencyMs,
    networkFailures:
      result.error.kind === "network" ? states[key].networkFailures + 1 : 0,
  });
}

function activeKeys(): DatasetKey[] {
  return DATASET_KEYS.filter((k) => (refCounts.get(k) ?? 0) > 0);
}

function tick(): void {
  ticks += 1;
  for (const key of activeKeys()) {
    const every =
      DATASET_INTERVAL_MULTIPLIER[key] * backoffMultiplier(key);
    if (ticks % every === 0) void fetchOne(key, false);
  }
  schedule();
}

/**
 * Chained timeouts, never setInterval. With an interval, a fetch slower than
 * the period drifts and then stacks; chaining means "N seconds between ticks"
 * and removes the whole overlap class structurally.
 */
function schedule(): void {
  if (timer !== null) {
    clearTimeout(timer);
    timer = null;
  }
  if (paused) return;
  const seconds = settings.getSnapshot().intervalSeconds;
  if (seconds === 0) return;
  timer = setTimeout(tick, seconds * 1000);
}

function onVisibilityChange(): void {
  paused = document.hidden;
  if (paused) {
    for (const [, c] of inFlight) c.abort();
    inFlight.clear();
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    notify();
    return;
  }
  // Catch up on return, but debounce so alt-tabbing does not hammer.
  const since = Date.now() - lastVisibilityFetch;
  if (since > 5000) {
    lastVisibilityFetch = Date.now();
    for (const key of activeKeys()) void fetchOne(key, true);
  }
  schedule();
  notify();
}

document.addEventListener("visibilitychange", onVisibilityChange);
settings.subscribe(schedule);

export const dataStore = {
  subscribe(l: () => void): () => void {
    listeners.add(l);
    return () => listeners.delete(l);
  },

  getSnapshot(): Record<DatasetKey, DatasetState> {
    return states;
  },

  isPaused(): boolean {
    return paused;
  },

  /**
   * Register interest in a dataset. A key nobody is watching is never fetched;
   * a key that has never loaded fetches immediately rather than waiting up to
   * a full interval for the first tick.
   */
  retain(key: DatasetKey): () => void {
    refCounts.set(key, (refCounts.get(key) ?? 0) + 1);
    if (states[key].data === undefined && !states[key].loading) {
      void fetchOne(key, false);
    }
    schedule();
    return () => {
      const next = (refCounts.get(key) ?? 1) - 1;
      if (next <= 0) refCounts.delete(key);
      else refCounts.set(key, next);
    };
  },

  /** Manual refresh: everything subscribed, or one dataset. */
  refresh(key?: DatasetKey): void {
    const keys = key ? [key] : activeKeys();
    for (const k of keys) void fetchOne(k, true);
    // Reset the chain so the next automatic tick is a full interval away.
    schedule();
  },
};
